//! The table views: the requirements table, the traceability and allocation
//! matrices and the model checks list. Glass cards filling the canvas area,
//! with scrolling, hover and selection (which shows the element in the code
//! and the model tree like a diagram selection does).

use super::Ui;
use super::chrome::glass_card;
use crate::checks::Severity;
use crate::config::Canvas;
use crate::diagram_view::{Fonts, contains};
use crate::requirements::{ReqStatus, TraceKind};
use crate::shapes::{Rect, ShapeRenderer, Transform};
use crate::text::{TextRenderer, TextStyle};
use crate::theme::Theme;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysml::eval::Verdict;

const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// Requirements table columns: title and share of the width.
const COLUMNS: [(&str, f32); 7] = [
    ("Id", 0.06),
    ("Requirement", 0.15),
    ("Text", 0.29),
    ("Subject", 0.11),
    ("Satisfied by", 0.12),
    ("Verified by", 0.11),
    ("Status", 0.16),
];

/// Checks list columns.
const CHECK_COLUMNS: [(&str, f32); 4] = [("Check", 0.18), ("Element", 0.2), ("Finding", 0.48), ("File", 0.14)];

/// Geometry of the last draw, for hit testing.
#[derive(Default, Clone)]
struct Layout {
    card: Rect,
    /// The scrolling rows.
    body: Rect,
    row_h: f32,
    /// Matrix only: the header strip, and each column's screen x range.
    header: Rect,
    cols: Vec<(f32, f32)>,
    max_scroll: f32,
    max_hscroll: f32,
}

/// Scrolling, selection and hover of one table view.
#[derive(Default)]
pub(super) struct ReqState {
    scroll: f32,
    hscroll: f32,
    /// Qualified name of the selected row (or matrix column) element.
    pub(super) selected: Option<String>,
    hovered: Option<(Option<usize>, Option<usize>)>,
    /// Scroll the selected row into view on the next draw.
    reveal: bool,
    layout: Option<Layout>,
}

impl ReqState {
    pub(super) fn select(&mut self, qualified_name: Option<String>) {
        self.selected = qualified_name;
        self.reveal = true;
    }

    pub(super) fn reset(&mut self) {
        *self = ReqState::default();
    }

    pub(super) fn card(&self) -> Option<Rect> {
        self.layout.as_ref().map(|l| l.card)
    }

    /// Scrolls row `i` into view if a reveal is pending.
    fn reveal_row(&mut self, i: Option<usize>, row_h: f32, height: f32) {
        if !std::mem::take(&mut self.reveal) {
            return;
        }
        if let Some(i) = i {
            let (y0, y1) = (i as f32 * row_h, (i + 1) as f32 * row_h);
            if y0 < self.scroll {
                self.scroll = y0;
            } else if y1 > self.scroll + height {
                self.scroll = y1 - height;
            }
        }
    }
}

/// A matrix ready to draw: row labels, column titles and the filled cells.
struct Matrix {
    corner: &'static str,
    /// Label, indent level, color.
    rows: Vec<(String, usize, [f32; 4])>,
    columns: Vec<String>,
    /// Letters and color by (row, column).
    cells: HashMap<(usize, usize), (String, [f32; 4])>,
    empty: &'static str,
}

fn status_color(theme: &Theme, s: ReqStatus) -> [f32; 4] {
    match s {
        ReqStatus::Satisfied => theme.success.0,
        ReqStatus::Violated | ReqStatus::CannotCheck => theme.error.0,
        ReqStatus::Open => theme.kind.definition.fg.0,
        ReqStatus::Undecided | ReqStatus::Definition => theme.text_muted.0,
    }
}

fn severity_color(theme: &Theme, s: Severity) -> [f32; 4] {
    match s {
        // Amber, apart from the red of syntax errors.
        Severity::Warning => theme.edge.allocation.0,
        Severity::Info => theme.accent.0,
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn with_file(name: String, file: Option<&str>) -> String {
    match file {
        Some(f) => format!("{name}  ({f})"),
        None => name,
    }
}

impl Ui {
    /// Where the tables go: the canvas area between the control rows' tops and bottoms.
    pub(super) fn table_area(&self) -> Option<Rect> {
        let (row, palette) = (self.chrome_controls_row()?, self.chrome_palette_row()?);
        let gap = (self.config.theme.size.frame.gap * self.dpi).round();
        Some([row[0], row[3] + gap, row[2], palette[3]])
    }

    /// The state of the table view shown.
    pub(super) fn table(&self) -> &ReqState {
        match self.canvas {
            Canvas::Allocation => &self.alloc_table,
            Canvas::Checks => &self.check_table,
            _ => &self.reqs,
        }
    }

    pub(super) fn table_mut(&mut self) -> &mut ReqState {
        match self.canvas {
            Canvas::Allocation => &mut self.alloc_table,
            Canvas::Checks => &mut self.check_table,
            _ => &mut self.reqs,
        }
    }

    /// Qualified name of each row of the table shown, with its file (0: this one).
    pub(super) fn table_rows(&self) -> Vec<(&str, usize)> {
        let doc = &self.doc;
        match self.canvas {
            Canvas::Allocation => doc.allocations.rows.iter().map(|r| (r.qualified_name.as_str(), r.source)).collect(),
            Canvas::Checks => doc.checks.iter().map(|f| (f.element.as_str(), f.source)).collect(),
            _ => doc.requirements.rows.iter().map(|r| (r.qualified_name.as_str(), r.source)).collect(),
        }
    }

    /// Qualified name of each matrix column (none for lists).
    pub(super) fn table_columns(&self) -> Vec<&str> {
        let doc = &self.doc;
        match self.canvas {
            Canvas::Traceability => doc.requirements.columns.iter().map(|c| c.qualified_name.as_str()).collect(),
            Canvas::Allocation => doc.allocations.columns.iter().map(|c| c.qualified_name.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    fn is_matrix(&self) -> bool {
        matches!(self.canvas, Canvas::Traceability | Canvas::Allocation)
    }

    /// Row (and matrix column) under a screen point.
    fn req_hit(&self, p: [f32; 2]) -> Option<(Option<usize>, Option<usize>)> {
        let t = self.table();
        let l = t.layout.as_ref()?;
        let col = l.cols.iter().position(|&(a, b)| p[0] >= a.max(l.body[0]) && p[0] < b && b > a);
        if contains(l.header, p) {
            return Some((None, col));
        }
        if !contains(l.body, p) {
            return None;
        }
        let r = ((p[1] - l.body[1] + t.scroll) / l.row_h).floor() as usize;
        (r < self.table_rows().len()).then_some((Some(r), col))
    }

    pub(super) fn reqs_press(&mut self, p: [f32; 2]) -> bool {
        if !self.table().card().is_some_and(|c| contains(c, p)) {
            return false;
        }
        let hit = self.req_hit(p);
        let double = self.canvas_click.is_some_and(|(t, q)| t.elapsed() < DOUBLE_CLICK && (q[0] - p[0]).abs() < 5.0 && (q[1] - p[1]).abs() < 5.0);
        self.canvas_click = Some((Instant::now(), p));
        let row = hit.and_then(|h| h.0).and_then(|r| self.table_rows().get(r).map(|&(q, s)| (q.to_owned(), s)));
        // Double click on an element of another file: open that file there.
        if double
            && let Some((name, source)) = row.clone().filter(|r| r.1 != 0)
            && let Some(path) = self.doc.source_path(source).map(|p| p.to_path_buf())
        {
            self.canvas_click = None;
            self.open(path);
            if let Some(offset) = self.doc.offset_of(&name) {
                self.table_mut().select(Some(name));
                self.code.goto(offset);
            }
            return true;
        }
        let pick = match hit {
            Some((Some(_), _)) => row.map(|r| r.0),
            Some((None, Some(c))) if self.is_matrix() => self.table_columns().get(c).map(|c| c.to_string()),
            _ => None,
        };
        // A finding points at its line, which may be inside the element.
        if self.canvas == Canvas::Checks
            && let Some(f) = hit.and_then(|h| h.0).and_then(|r| self.doc.checks.get(r)).filter(|f| f.source == 0)
        {
            let offset = f.offset;
            self.code.goto(offset);
        }
        self.table_mut().selected = pick;
        true
    }

    pub(super) fn reqs_motion(&mut self, p: [f32; 2]) {
        let h = self.req_hit(p);
        self.table_mut().hovered = h;
    }

    pub(super) fn reqs_wheel(&mut self, lines: f32, horizontal: bool) {
        let t = self.table_mut();
        let Some(l) = &t.layout else { return };
        let step = l.row_h * 1.5;
        if horizontal {
            t.hscroll = (t.hscroll - lines * step).clamp(0.0, l.max_hscroll);
        } else {
            t.scroll = (t.scroll - lines * step).clamp(0.0, l.max_scroll);
        }
    }

    /// Up / Down move the selection; true if handled.
    pub(super) fn reqs_key(&mut self, down: bool) -> bool {
        let rows: Vec<String> = self.table_rows().iter().map(|r| r.0.to_owned()).collect();
        if rows.is_empty() {
            return false;
        }
        let cur = self.table().selected.as_ref().and_then(|q| rows.iter().position(|r| r == q));
        // Rows of the same element (several findings) are skipped together.
        let next = match (cur, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1..rows.len()).find(|&k| rows[k] != rows[i]).unwrap_or(i),
            (Some(i), false) => (0..i).rev().find(|&k| rows[k] != rows[i]).unwrap_or(i),
        };
        let name = rows[next].clone();
        if self.canvas == Canvas::Checks
            && let Some(f) = self.doc.checks.get(next).filter(|f| f.source == 0)
        {
            let offset = f.offset;
            self.code.goto(offset);
        }
        self.table_mut().select(Some(name));
        true
    }

    pub(super) fn draw_requirements(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) {
        let Some(card) = self.table_area().filter(|a| a[2] - a[0] > 120.0 * dpi && a[3] - a[1] > 80.0 * dpi) else {
            self.table_mut().layout = None;
            return;
        };
        let f = &theme.size.frame;
        shapes.begin_layer(Transform::IDENTITY, None);
        glass_card(shapes, theme, card, f.card_radius * dpi, dpi);
        let size = self.config.view.ui_font_size * dpi;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let pad = (14.0 * dpi).round();

        // Summary line: totals, and the legend of a matrix.
        let sy = card[1] + pad;
        text.set_clip(Some(card));
        let title = self.canvas.label();
        text.draw(TextStyle::new(fonts.title, size * 1.08, theme.text.0), [card[0] + pad, sy], title);
        let mut x = card[0] + pad + text.measure(fonts.title, size * 1.08, title, None)[0] + 16.0 * dpi;
        let (chips, legend) = self.summary(theme);
        let cy = sy + lh * 0.5 + dpi;
        for (i, (label, color)) in chips.iter().enumerate() {
            if i > 0 {
                let d = 3.5 * dpi;
                shapes.rect([x - d, cy - d, x + d, cy + d], *color, d);
                x += 10.0 * dpi;
            }
            text.draw(TextStyle::new(fonts.body, size, if i == 0 { theme.text_muted.0 } else { theme.text.0 }), [x, sy + dpi], label);
            x += text.measure(fonts.body, size, label, None)[0] + 16.0 * dpi;
        }
        if let Some(legend) = legend {
            let w = text.measure(fonts.body, size * 0.92, legend, None)[0];
            if x + 20.0 * dpi < card[2] - pad - w {
                text.draw(TextStyle::new(fonts.body, size * 0.92, theme.text_muted.0), [card[2] - pad - w, sy + 2.0 * dpi], legend);
            }
        }
        let top = sy + lh + pad * 0.8;

        let empty = match self.canvas {
            Canvas::Allocation if self.doc.allocations.rows.is_empty() => Some(
                "No actions or allocations in this project yet. Declare `action def`s, then say which part carries them out with `perform action a : A;` in the part, or `allocate A to part;`.",
            ),
            Canvas::Checks if self.doc.checks.is_empty() => {
                Some("No findings: ports are connected, requirements are traced and decided, actions are allocated, definitions are used.")
            }
            Canvas::Requirements | Canvas::Traceability if self.doc.requirements.rows.is_empty() => Some(
                "No requirements in this model yet. Declare `requirement def` and `requirement` elements, then relate them with `satisfy R by x;` or `verify R;`.",
            ),
            _ => None,
        };
        if let Some(msg) = empty {
            text.draw_wrapped(TextStyle::new(fonts.body, size, theme.text_muted.0), [card[0] + pad, top + pad], card[2] - card[0] - 2.0 * pad, msg);
            text.set_clip(None);
            self.table_mut().layout = Some(Layout { card, body: [0.0; 4], row_h: 1.0, ..Default::default() });
            return;
        }
        text.set_clip(None);
        match self.canvas {
            Canvas::Traceability => {
                let m = self.trace_matrix(theme);
                self.draw_matrix(&m, shapes, text, fonts, theme, dpi, card, top, size, lh, pad);
            }
            Canvas::Allocation => {
                let m = self.allocation_matrix(theme);
                self.draw_matrix(&m, shapes, text, fonts, theme, dpi, card, top, size, lh, pad);
            }
            Canvas::Checks => self.draw_checks(shapes, text, fonts, theme, dpi, card, top, size, lh, pad),
            _ => self.draw_table(shapes, text, fonts, theme, dpi, card, top, size, lh, pad),
        }
    }

    /// Totals for the summary line, and a legend.
    fn summary(&self, theme: &Theme) -> (Vec<(String, [f32; 4])>, Option<&'static str>) {
        let reqs = &self.doc.requirements;
        match self.canvas {
            Canvas::Allocation => {
                let a = &self.doc.allocations;
                let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
                let mut chips = vec![(format!("{} · {}", plural(a.rows.len(), "function", "functions"), plural(a.columns.len(), "part", "parts")), theme.text_muted.0)];
                let open = (0..a.rows.len()).filter(|&r| a.unallocated(r)).count();
                if open > 0 {
                    chips.push((format!("{open} unallocated"), theme.error.0));
                }
                if a.unresolved > 0 {
                    chips.push((format!("{} allocation ends not found", a.unresolved), theme.edge.allocation.0));
                }
                (chips, Some("A allocate   P perform   E exhibit"))
            }
            Canvas::Checks => {
                let n = |s: Severity| self.doc.checks.iter().filter(|f| f.severity == s).count();
                let total = self.doc.checks.len();
                let mut chips = vec![(format!("{total} {}", if total == 1 { "finding" } else { "findings" }), theme.text_muted.0)];
                for (s, word) in [(Severity::Warning, "warnings"), (Severity::Info, "notes")] {
                    if n(s) > 0 {
                        chips.push((format!("{} {word}", n(s)), severity_color(theme, s)));
                    }
                }
                (chips, None)
            }
            _ => {
                let count = |s: ReqStatus| reqs.rows.iter().filter(|r| r.status == s).count();
                let usages = reqs.rows.iter().filter(|r| !r.is_def).count();
                let mut chips: Vec<(String, [f32; 4])> = vec![(format!("{usages} requirements · {} definitions", reqs.rows.len() - usages), theme.text_muted.0)];
                for s in [ReqStatus::Satisfied, ReqStatus::Violated, ReqStatus::Undecided, ReqStatus::CannotCheck, ReqStatus::Open] {
                    let n = count(s);
                    if n > 0 {
                        chips.push((format!("{n} {}", s.label()), status_color(theme, s)));
                    }
                }
                if self.canvas == Canvas::Traceability {
                    let untraced = (0..reqs.rows.len()).filter(|&r| reqs.untraced(r)).count();
                    if untraced > 0 {
                        chips.push((format!("{untraced} untraced"), theme.error.0));
                    }
                    return (chips, Some("S satisfy   V verify   D dependency"));
                }
                (chips, None)
            }
        }
    }

    fn trace_matrix(&self, theme: &Theme) -> Matrix {
        let reqs = &self.doc.requirements;
        let rows = reqs
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let name = if r.id.is_empty() { r.name.clone() } else { format!("{}  {}", r.id, r.name) };
                let color = if reqs.untraced(i) {
                    theme.error.0
                } else if r.is_def {
                    theme.text_muted.0
                } else {
                    theme.text.0
                };
                (with_file(name, r.file.as_deref()), r.depth, color)
            })
            .collect();
        let mut cells = HashMap::new();
        for r in 0..reqs.rows.len() {
            for c in 0..reqs.columns.len() {
                let cs: Vec<_> = reqs.cell(r, c).collect();
                if cs.is_empty() {
                    continue;
                }
                let letters: String = cs.iter().map(|c| c.kind.letter()).collect();
                let fill = match cs.iter().find(|c| c.kind == TraceKind::Satisfy).and_then(|c| c.verdict) {
                    Some(Verdict::Satisfied) => theme.success.0,
                    Some(Verdict::Violated | Verdict::Error) => theme.error.0,
                    // Satisfied by it, but the constraints cannot be decided.
                    _ if cs.iter().any(|c| c.kind == TraceKind::Satisfy) => theme.text_muted.0,
                    _ if cs.iter().any(|c| c.kind == TraceKind::Verify) => theme.accent.0,
                    _ => theme.edge.dependency.0,
                };
                cells.insert((r, c), (letters, fill));
            }
        }
        Matrix {
            corner: "Requirement",
            rows,
            columns: reqs.columns.iter().map(|c| c.name.clone()).collect(),
            cells,
            empty: "Nothing satisfies, verifies or depends on these requirements yet.",
        }
    }

    fn allocation_matrix(&self, theme: &Theme) -> Matrix {
        let a = &self.doc.allocations;
        let rows = a
            .rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let color = if a.unallocated(i) { theme.error.0 } else { theme.text.0 };
                (with_file(format!("{}  ·  {}", r.name, r.keyword), r.file.as_deref()), 0, color)
            })
            .collect();
        let mut cells: HashMap<(usize, usize), (String, [f32; 4])> = HashMap::new();
        for c in &a.cells {
            // Explicit allocations in the allocation color, perform / exhibit in the accent.
            let color = if c.kind == crate::allocations::AllocKind::Allocate { theme.edge.allocation.0 } else { theme.accent.0 };
            let e = cells.entry((c.row, c.col)).or_insert((String::new(), color));
            e.0.push(c.kind.letter());
        }
        Matrix {
            corner: "Function",
            rows,
            columns: a.columns.iter().map(|c| with_file(c.name.clone(), c.file.as_deref())).collect(),
            cells,
            empty: "No part performs, exhibits or is allocated these functions yet.",
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_table(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32, card: Rect, top: f32, size: f32, lh: f32, pad: f32) {
        let reqs = &self.doc.requirements;
        let inner = [card[0] + pad * 0.5, card[2] - pad * 0.5];
        let width = inner[1] - inner[0];
        let mut xs = Vec::new();
        let mut x = inner[0];
        for (_, share) in COLUMNS {
            xs.push((x, x + width * share));
            x += width * share;
        }
        let header = [card[0], top, card[2], top + lh + pad * 0.8];
        shapes.rect([card[0] + dpi, header[3] - dpi, card[2] - dpi, header[3]], theme.border.0, 0.0);
        text.set_clip(Some(header));
        let hy = header[1] + (header[3] - header[1] - lh) * 0.5;
        for ((title, _), &(x0, _)) in COLUMNS.iter().zip(&xs) {
            text.draw(TextStyle::new(fonts.title, size * 0.92, theme.text_muted.0), [x0 + pad * 0.5, hy], title);
        }
        let body = [card[0] + dpi, header[3], card[2] - dpi, card[3] - pad * 0.5];
        let row_h = (lh * 2.0 + pad * 0.8).round();
        let total = row_h * reqs.rows.len() as f32;
        let max_scroll = (total - (body[3] - body[1])).max(0.0);
        let sel_row = self.reqs.selected.as_ref().and_then(|q| reqs.rows.iter().position(|r| &r.qualified_name == q));
        self.reqs.reveal_row(sel_row, row_h, body[3] - body[1]);
        self.reqs.scroll = self.reqs.scroll.clamp(0.0, max_scroll);
        let scroll = self.reqs.scroll;
        shapes.begin_layer(Transform::IDENTITY, Some(body));
        let first = (scroll / row_h).floor() as usize;
        let shown = ((body[3] - body[1]) / row_h).ceil() as usize + 1;
        let hovered_row = self.reqs.hovered.and_then(|h| h.0);
        for (i, r) in reqs.rows.iter().enumerate().skip(first).take(shown) {
            let y = body[1] + i as f32 * row_h - scroll;
            let row = [body[0], y, body[2], y + row_h];
            if self.reqs.selected.as_deref() == Some(r.qualified_name.as_str()) {
                shapes.rect(row, theme.selection_row.0, 0.0);
            } else if hovered_row == Some(i) {
                shapes.rect(row, theme.hover_row.0, 0.0);
            }
            shapes.rect([row[0] + pad * 0.5, row[3] - dpi, row[2] - pad * 0.5, row[3]], theme.border.alpha(0.5), 0.0);
            let ty = y + pad * 0.4;
            let cell = |k: usize| {
                let (a, b) = xs[k];
                intersect(body, [a + pad * 0.5, y, b - pad * 0.3, y + row_h])
            };
            let body_style = TextStyle::new(fonts.body, size, theme.text.0);
            let muted = TextStyle::new(fonts.body, size, theme.text_muted.0);
            let put = |text: &mut TextRenderer, k: usize, style: TextStyle, s: &str, wrap: bool| {
                let c = cell(k);
                text.set_clip(Some(c));
                if wrap {
                    text.draw_wrapped(style, [c[0], ty], c[2] - c[0], s);
                } else {
                    text.draw(style, [c[0], ty], s);
                }
            };
            put(text, 0, TextStyle::new(fonts.mono, size * 0.95, theme.kind.requirement.fg.0), if r.id.is_empty() { "—" } else { &r.id }, false);
            // Name, indented under the requirement it is nested in, with its definition below.
            let indent = r.depth as f32 * 12.0 * dpi;
            let c = cell(1);
            text.set_clip(Some(c));
            text.draw(TextStyle::new(fonts.title, size, theme.text.0), [c[0] + indent, ty], &r.name);
            let mut second = if r.is_def { "requirement def".to_string() } else { r.ty.as_ref().map_or("requirement".to_string(), |t| format!(": {t}")) };
            if let Some(f) = &r.file {
                second += &format!("  · {f}");
            }
            text.draw(TextStyle::new(fonts.body, size * 0.9, theme.text_muted.0), [c[0] + indent, ty + lh], &second);
            if r.text.is_empty() {
                put(text, 2, TextStyle::new(fonts.body, size, theme.text_muted.alpha(0.6)), "no text (add a doc comment)", false);
            } else {
                put(text, 2, body_style, &r.text, true);
            }
            put(text, 3, muted, r.subject.as_deref().unwrap_or("—"), true);
            let sat = if r.satisfied_by.is_empty() { "—".to_string() } else { r.satisfied_by.join(", ") };
            put(text, 4, if r.satisfied_by.is_empty() { muted } else { body_style }, &sat, true);
            let ver = if r.verified_by.is_empty() { "—".to_string() } else { r.verified_by.join(", ") };
            put(text, 5, if r.verified_by.is_empty() { muted } else { body_style }, &ver, true);
            // Status pill, with the compared values under it.
            let c = cell(6);
            let label = r.status.label();
            let color = status_color(theme, r.status);
            let small = size * 0.9;
            let w = text.measure(fonts.title, small, label, None)[0] + 14.0 * dpi;
            let slh = text.line_metrics(fonts.title, small).line_height;
            let pill = [c[0], ty, (c[0] + w).min(c[2]), ty + slh + 4.0 * dpi];
            shapes.rect(pill, [color[0], color[1], color[2], 0.16], (pill[3] - pill[1]) * 0.5);
            text.set_clip(Some(c));
            text.draw(TextStyle::new(fonts.title, small, color), [pill[0] + 7.0 * dpi, ty + 2.0 * dpi], label);
            if !r.detail.is_empty() {
                text.draw(TextStyle::new(fonts.mono, size * 0.82, theme.text_muted.0), [c[0], ty + lh + 3.0 * dpi], &r.detail);
            }
        }
        text.set_clip(None);
        self.reqs.layout = Some(Layout { card, body, row_h, header, cols: xs, max_scroll, max_hscroll: 0.0 });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_checks(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32, card: Rect, top: f32, size: f32, lh: f32, pad: f32) {
        let findings = &self.doc.checks;
        let inner = [card[0] + pad * 0.5, card[2] - pad * 0.5];
        let width = inner[1] - inner[0];
        let mut xs = Vec::new();
        let mut x = inner[0];
        for (_, share) in CHECK_COLUMNS {
            xs.push((x, x + width * share));
            x += width * share;
        }
        let header = [card[0], top, card[2], top + lh + pad * 0.8];
        shapes.rect([card[0] + dpi, header[3] - dpi, card[2] - dpi, header[3]], theme.border.0, 0.0);
        text.set_clip(Some(header));
        let hy = header[1] + (header[3] - header[1] - lh) * 0.5;
        for ((title, _), &(x0, _)) in CHECK_COLUMNS.iter().zip(&xs) {
            text.draw(TextStyle::new(fonts.title, size * 0.92, theme.text_muted.0), [x0 + pad * 0.5, hy], title);
        }
        let body = [card[0] + dpi, header[3], card[2] - dpi, card[3] - pad * 0.5];
        let row_h = (lh + pad * 0.9).round();
        let max_scroll = (row_h * findings.len() as f32 - (body[3] - body[1])).max(0.0);
        let t = &mut self.check_table;
        let sel_row = t.selected.as_ref().and_then(|q| findings.iter().position(|f| &f.element == q));
        t.reveal_row(sel_row, row_h, body[3] - body[1]);
        t.scroll = t.scroll.clamp(0.0, max_scroll);
        let scroll = t.scroll;
        let hovered_row = t.hovered.and_then(|h| h.0);
        let selected = t.selected.clone();
        shapes.begin_layer(Transform::IDENTITY, Some(body));
        let first = (scroll / row_h).floor() as usize;
        let shown = ((body[3] - body[1]) / row_h).ceil() as usize + 1;
        for (i, f) in findings.iter().enumerate().skip(first).take(shown) {
            let y = body[1] + i as f32 * row_h - scroll;
            let row = [body[0], y, body[2], y + row_h];
            if selected.as_deref() == Some(f.element.as_str()) {
                shapes.rect(row, theme.selection_row.0, 0.0);
            } else if hovered_row == Some(i) {
                shapes.rect(row, theme.hover_row.0, 0.0);
            }
            shapes.rect([row[0] + pad * 0.5, row[3] - dpi, row[2] - pad * 0.5, row[3]], theme.border.alpha(0.5), 0.0);
            let ty = y + (row_h - lh) * 0.5;
            let cell = |k: usize| {
                let (a, b) = xs[k];
                intersect(body, [a + pad * 0.5, y, b - pad * 0.3, y + row_h])
            };
            // Severity dot and the check's name.
            let color = severity_color(theme, f.severity);
            let c = cell(0);
            let d = 4.0 * dpi;
            let cy = y + row_h * 0.5;
            shapes.rect([c[0], cy - d, c[0] + 2.0 * d, cy + d], color, d);
            text.set_clip(Some(c));
            text.draw(TextStyle::new(fonts.title, size * 0.95, theme.text.0), [c[0] + 2.0 * d + 8.0 * dpi, ty], f.rule.label());
            let c = cell(1);
            text.set_clip(Some(c));
            let short = f.element.rsplit("::").next().unwrap_or(&f.element);
            let mlh = text.line_metrics(fonts.mono, size * 0.92).line_height;
            text.draw(TextStyle::new(fonts.mono, size * 0.92, theme.kind.usage.fg.0), [c[0], (y + (row_h - mlh) * 0.5).round()], short);
            let c = cell(2);
            text.set_clip(Some(c));
            text.draw(TextStyle::new(fonts.body, size, theme.text.0), [c[0], ty], &f.message);
            let c = cell(3);
            text.set_clip(Some(c));
            text.draw(TextStyle::new(fonts.body, size * 0.92, theme.text_muted.0), [c[0], ty], f.file.as_deref().unwrap_or("this file"));
        }
        text.set_clip(None);
        self.check_table.layout = Some(Layout { card, body, row_h, header, cols: xs, max_scroll, max_hscroll: 0.0 });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_matrix(&mut self, m: &Matrix, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32, card: Rect, top: f32, size: f32, lh: f32, pad: f32) {
        let row_names: Vec<String> = self.table_rows().iter().map(|r| r.0.to_owned()).collect();
        let col_names: Vec<String> = self.table_columns().iter().map(|c| c.to_string()).collect();
        let first_w = m
            .rows
            .iter()
            .map(|(label, depth, _)| text.measure(fonts.body, size, label, None)[0] + *depth as f32 * 12.0 * dpi)
            .fold(140.0 * dpi, f32::max)
            .min((card[2] - card[0]) * 0.4)
            + 2.0 * pad;
        let widths: Vec<f32> = m.columns.iter().map(|c| (text.measure(fonts.title, size * 0.92, c, None)[0] + pad * 1.4).clamp(52.0 * dpi, 170.0 * dpi)).collect();
        let grid_x0 = card[0] + first_w;
        let grid_w = card[2] - pad * 0.5 - grid_x0;
        let total_w: f32 = widths.iter().sum();
        let max_hscroll = (total_w - grid_w).max(0.0);
        let t = self.table_mut();
        t.hscroll = t.hscroll.clamp(0.0, max_hscroll);
        let mut cols = Vec::new();
        let mut x = grid_x0 - t.hscroll;
        for w in &widths {
            cols.push((x, x + w));
            x += w;
        }
        let header = [card[0], top, card[2], top + lh + pad];
        let body = [card[0] + dpi, header[3], card[2] - dpi, card[3] - pad * 0.5];
        let grid_clip = [grid_x0, header[1], body[2], body[3]];
        let row_h = (lh + pad * 0.7).round();
        let max_scroll = (row_h * m.rows.len() as f32 - (body[3] - body[1])).max(0.0);
        let sel_row = t.selected.as_ref().and_then(|q| row_names.iter().position(|r| r == q));
        t.reveal_row(sel_row, row_h, body[3] - body[1]);
        t.scroll = t.scroll.clamp(0.0, max_scroll);
        let scroll = t.scroll;
        let (hover_row, hover_col) = t.hovered.unwrap_or((None, None));
        let sel = t.selected.clone();
        let sel = sel.as_deref();

        // Highlights: hovered row and column, selected row or column.
        shapes.begin_layer(Transform::IDENTITY, Some(body));
        for (i, name) in row_names.iter().enumerate().take(m.rows.len()) {
            let y = body[1] + i as f32 * row_h - scroll;
            if y > body[3] || y + row_h < body[1] {
                continue;
            }
            let row = [body[0], y, body[2], y + row_h];
            if sel == Some(name.as_str()) {
                shapes.rect(row, theme.selection_row.0, 0.0);
            } else if hover_row == Some(i) {
                shapes.rect(row, theme.hover_row.0, 0.0);
            }
            shapes.rect([row[0] + pad * 0.5, row[3] - dpi, row[2] - pad * 0.5, row[3]], theme.border.alpha(0.4), 0.0);
        }
        shapes.begin_layer(Transform::IDENTITY, Some(grid_clip));
        for (c, &(x0, x1)) in cols.iter().enumerate() {
            let column = [x0, header[1], x1, body[3]];
            if sel.is_some() && sel == col_names.get(c).map(String::as_str) {
                shapes.rect(column, theme.selection_row.0, 0.0);
            } else if hover_col == Some(c) {
                shapes.rect(column, theme.hover_row.alpha(0.7), 0.0);
            }
            shapes.rect([x0, header[1] + pad * 0.3, x0 + dpi, body[3]], theme.border.alpha(0.4), 0.0);
        }
        shapes.begin_layer(Transform::IDENTITY, None);
        shapes.rect([card[0] + dpi, header[3] - dpi, card[2] - dpi, header[3]], theme.border.0, 0.0);

        // Column headers.
        let hy = header[1] + (header[3] - header[1] - lh) * 0.5;
        text.set_clip(Some([card[0], header[1], grid_x0, header[3]]));
        text.draw(TextStyle::new(fonts.title, size * 0.92, theme.text_muted.0), [card[0] + pad, hy], m.corner);
        for (c, &(x0, x1)) in cols.iter().enumerate() {
            text.set_clip(Some(intersect(grid_clip, [x0, header[1], x1, header[3]])));
            text.draw(TextStyle::new(fonts.title, size * 0.92, theme.text.0), [x0 + pad * 0.7, hy], &m.columns[c]);
        }

        // Rows: label, then the cells.
        let chip = (row_h * 0.64).round();
        let first = (scroll / row_h).floor() as usize;
        let shown = ((body[3] - body[1]) / row_h).ceil() as usize + 1;
        for (i, (label, depth, color)) in m.rows.iter().enumerate().skip(first).take(shown) {
            let y = body[1] + i as f32 * row_h - scroll;
            let ty = y + (row_h - lh) * 0.5;
            text.set_clip(Some(intersect(body, [card[0], y, grid_x0 - pad * 0.3, y + row_h])));
            let indent = *depth as f32 * 12.0 * dpi;
            text.draw(TextStyle::new(fonts.body, size, *color), [card[0] + pad + indent, ty], label);
            for (c, &(x0, x1)) in cols.iter().enumerate() {
                if x1 < grid_x0 || x0 > body[2] {
                    continue;
                }
                let Some((letters, fill)) = m.cells.get(&(i, c)) else { continue };
                let w = text.measure(fonts.title, size * 0.85, letters, None)[0];
                let cw = chip.max(w + 8.0 * dpi);
                let cx = ((x0 + x1) * 0.5 - cw * 0.5).round();
                let cyy = (y + (row_h - chip) * 0.5).round();
                let r = [cx, cyy, cx + cw, cyy + chip];
                shapes.begin_layer(Transform::IDENTITY, Some(intersect(grid_clip, body)));
                shapes.rect(r, [fill[0], fill[1], fill[2], 0.22], 4.0 * dpi);
                shapes.bordered_rect(r, crate::theme::Color::TRANSPARENT, *fill, 4.0 * dpi, dpi);
                text.set_clip(Some(intersect(intersect(grid_clip, body), r)));
                let clh = text.line_metrics(fonts.title, size * 0.85).line_height;
                text.draw(TextStyle::new(fonts.title, size * 0.85, *fill), [((x0 + x1) * 0.5 - w * 0.5).round(), (cyy + (chip - clh) * 0.5).round()], letters);
            }
        }
        if m.columns.is_empty() {
            text.set_clip(Some(card));
            text.draw(TextStyle::new(fonts.body, size, theme.text_muted.0), [grid_x0 + pad, body[1] + pad], m.empty);
        }
        text.set_clip(None);
        self.table_mut().layout = Some(Layout { card, body, row_h, header, cols, max_scroll, max_hscroll });
    }
}
