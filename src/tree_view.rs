//! The model tree on the right: the containment hierarchy of the model, like
//! the explorer of SysON / Sirius Web. Rows expand and collapse; a click
//! selects the element in the code and the diagram.

use crate::config::{Color, Theme};
use crate::diagram_view::{Fonts, contains};
use crate::document::TreeRow;
use crate::shapes::{Rect, ShapeRenderer, Transform};
use crate::text::{TextRenderer, TextStyle};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysml::Category;
use winit::keyboard::{Key, NamedKey};

const DOUBLE_CLICK: Duration = Duration::from_millis(450);
/// Rows at a smaller depth start expanded.
const DEFAULT_OPEN_DEPTH: usize = 2;

/// What a click or key in the tree asks the window to do.
pub enum TreeEvent {
    /// Show the element of `doc.tree[row]` in the code and the diagram.
    Select(usize),
}

#[derive(Clone, Copy, Default)]
struct Layout {
    expand_button: Rect,
    collapse_button: Rect,
    /// Area of the rows, below the header.
    body: Rect,
    row_h: f32,
    indent: f32,
}

#[derive(Default)]
pub struct TreeView {
    /// Expanded state the user chose, by row key; others use the default.
    expanded: HashMap<String, bool>,
    /// Key of the selected row.
    pub selected: Option<String>,
    /// Whether the tree has the keyboard.
    pub active: bool,
    /// Scroll offset in pixels.
    scroll: f32,
    /// Scroll the selected row into view on the next draw.
    reveal: bool,
    hovered: Option<usize>,
    mouse: [f32; 2],
    last_click: Option<(Instant, usize)>,
    /// Rows shown at the last draw (indexes into `doc.tree`).
    visible: Vec<usize>,
    layout: Layout,
}

fn is_open(expanded: &HashMap<String, bool>, row: &TreeRow) -> bool {
    expanded.get(&row.key).copied().unwrap_or(row.depth < DEFAULT_OPEN_DEPTH)
}

/// The rows that are not inside a collapsed row.
fn visible_rows(rows: &[TreeRow], expanded: &HashMap<String, bool>) -> Vec<usize> {
    let mut out = Vec::new();
    let mut hidden_below: Option<usize> = None;
    for (i, r) in rows.iter().enumerate() {
        if let Some(d) = hidden_below {
            if r.depth > d {
                continue;
            }
            hidden_below = None;
        }
        out.push(i);
        if r.has_children && !is_open(expanded, r) {
            hidden_below = Some(r.depth);
        }
    }
    out
}

impl TreeView {
    /// Selects the row with `key`, expanding its owners and scrolling to it.
    pub fn select_key(&mut self, key: Option<String>) {
        if let Some(k) = &key {
            // Owners' keys are the prefixes up to each '/'.
            for (i, _) in k.match_indices('/').skip(1) {
                self.expanded.insert(k[..i].to_owned(), true);
            }
            self.reveal = true;
        }
        self.selected = key;
    }

    pub fn expand_all(&mut self, rows: &[TreeRow], open: bool) {
        for r in rows.iter().filter(|r| r.has_children) {
            self.expanded.insert(r.key.clone(), open);
        }
        if !open {
            self.scroll = 0.0;
        }
        self.reveal = open;
    }

    pub fn reset(&mut self) {
        *self = TreeView { active: self.active, ..Default::default() };
    }

    fn selected_index(&self, rows: &[TreeRow]) -> Option<usize> {
        let key = self.selected.as_deref()?;
        rows.iter().position(|r| r.key == key)
    }

    fn row_at(&self, p: [f32; 2]) -> Option<usize> {
        let l = &self.layout;
        if !contains(l.body, p) || l.row_h <= 0.0 {
            return None;
        }
        let i = ((p[1] - l.body[1] + self.scroll) / l.row_h).floor();
        let row = *self.visible.get(i.max(0.0) as usize)?;
        Some(row)
    }

    // ---- input -----------------------------------------------------------------

    pub fn motion(&mut self, p: [f32; 2]) {
        self.mouse = p;
        self.hovered = self.row_at(p);
    }

    pub fn leave(&mut self) {
        self.hovered = None;
    }

    pub fn press(&mut self, rows: &[TreeRow], p: [f32; 2]) -> Option<TreeEvent> {
        self.active = true;
        let l = self.layout;
        if contains(l.expand_button, p) {
            self.expand_all(rows, true);
            return None;
        }
        if contains(l.collapse_button, p) {
            self.expand_all(rows, false);
            return None;
        }
        let row = self.row_at(p)?;
        let r = rows.get(row)?;
        let chevron_x = l.body[0] + l.indent * (r.depth as f32 + 0.35);
        let on_chevron = r.has_children && p[0] < chevron_x + l.indent * 0.9;
        let double = self.last_click.is_some_and(|(t, i)| i == row && t.elapsed() < DOUBLE_CLICK);
        self.last_click = Some((Instant::now(), row));
        if on_chevron || (double && r.has_children) {
            let open = is_open(&self.expanded, r);
            self.expanded.insert(r.key.clone(), !open);
            if on_chevron {
                return None;
            }
        }
        self.selected = Some(r.key.clone());
        Some(TreeEvent::Select(row))
    }

    pub fn wheel(&mut self, lines: f32) {
        self.scroll -= lines * self.layout.row_h * 3.0;
    }

    /// Arrow keys move and fold, Enter shows the element. False for other keys.
    pub fn key(&mut self, rows: &[TreeRow], key: &Key) -> (bool, Option<TreeEvent>) {
        let Key::Named(named) = key else { return (false, None) };
        let visible = visible_rows(rows, &self.expanded);
        if visible.is_empty() {
            return (false, None);
        }
        let cur = self.selected_index(rows).and_then(|s| visible.iter().position(|&v| v == s));
        let page = ((self.layout.body[3] - self.layout.body[1]) / self.layout.row_h.max(1.0)).floor().max(1.0) as usize;
        let go = |tv: &mut TreeView, i: usize| {
            let row = visible[i.min(visible.len() - 1)];
            tv.selected = Some(rows[row].key.clone());
            tv.reveal = true;
            (true, Some(TreeEvent::Select(row)))
        };
        match named {
            NamedKey::ArrowDown => go(self, cur.map_or(0, |c| c + 1)),
            NamedKey::ArrowUp => go(self, cur.map_or(0, |c| c.saturating_sub(1))),
            NamedKey::PageDown => go(self, cur.map_or(0, |c| c + page)),
            NamedKey::PageUp => go(self, cur.map_or(0, |c| c.saturating_sub(page))),
            NamedKey::Home => go(self, 0),
            NamedKey::End => go(self, visible.len() - 1),
            NamedKey::ArrowRight | NamedKey::ArrowLeft => {
                let Some(c) = cur else { return go(self, 0) };
                let row = &rows[visible[c]];
                let open = is_open(&self.expanded, row);
                let right = *named == NamedKey::ArrowRight;
                if row.has_children && open != right {
                    self.expanded.insert(row.key.clone(), right);
                    (true, None)
                } else if right && row.has_children {
                    go(self, c + 1)
                } else if !right && row.depth > 0 {
                    // To the owner: the closest row above with a smaller depth.
                    let owner = (0..c).rev().find(|&i| rows[visible[i]].depth < row.depth).unwrap_or(c);
                    go(self, owner)
                } else {
                    (true, None)
                }
            }
            NamedKey::Enter | NamedKey::Space => match cur {
                Some(c) => (true, Some(TreeEvent::Select(visible[c]))),
                None => go(self, 0),
            },
            _ => (false, None),
        }
    }

    // ---- drawing -----------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        rows: &[TreeRow],
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        font_size: f32,
        theme: &Theme,
        pane: Rect,
        dpi: f32,
    ) {
        let size = font_size * dpi;
        let small = size * 0.85;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let row_h = (lh * 1.45).round();
        let pad = (10.0 * dpi).round();
        let header_h = (lh * 2.0).round();
        let indent = (size * 1.25).round();
        let header = [pane[0], pane[1], pane[2], pane[1] + header_h];
        let body = [pane[0], header[3], pane[2], pane[3]];

        // Header: title and expand / collapse all.
        shapes.begin_layer(Transform::IDENTITY, None);
        shapes.rect(pane, theme.panel.0, 0.0);
        shapes.rect(header, theme.header.0, 0.0);
        shapes.rect([header[0], header[3] - dpi, header[2], header[3]], theme.divider.0, 0.0);
        let ty = header[1] + (header_h - text.line_metrics(fonts.title, small).line_height) * 0.5;
        text.set_clip(Some(header));
        text.draw(TextStyle::new(fonts.title, small, theme.dim.0), [pane[0] + pad, ty], "MODEL");
        let btn = (header_h * 0.62).round();
        let by = header[1] + ((header_h - btn) * 0.5).round();
        let collapse_button = [pane[2] - pad - btn, by, pane[2] - pad, by + btn];
        let expand_button = [collapse_button[0] - btn - dpi * 4.0, by, collapse_button[0] - dpi * 4.0, by + btn];
        for (r, plus) in [(expand_button, true), (collapse_button, false)] {
            let hot = contains(r, self.mouse);
            if hot {
                shapes.rect(r, theme.hover_row.alpha(2.5), 4.0 * dpi);
            }
            let c = if hot { theme.text.0 } else { theme.dim.0 };
            let (cx, cy, s) = ((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5, btn * 0.22);
            // A box with + (expand all) or − (collapse all).
            shapes.bordered_rect([cx - s * 1.6, cy - s * 1.6, cx + s * 1.6, cy + s * 1.6], [0.0; 4], c, 2.0 * dpi, dpi);
            shapes.line([cx - s * 0.8, cy], [cx + s * 0.8, cy], c, 1.2 * dpi, 0.0);
            if plus {
                shapes.line([cx, cy - s * 0.8], [cx, cy + s * 0.8], c, 1.2 * dpi, 0.0);
            }
        }
        let count = format!("{}", rows.len());
        let cw = text.measure(fonts.body, small, &count, None)[0];
        text.draw(TextStyle::new(fonts.body, small, theme.dim.alpha(0.8)), [expand_button[0] - pad * 0.6 - cw, ty], &count);
        text.set_clip(None);

        self.visible = visible_rows(rows, &self.expanded);
        self.layout = Layout { expand_button, collapse_button, body, row_h, indent };

        // Scrolling.
        let view_h = body[3] - body[1];
        let content_h = self.visible.len() as f32 * row_h + pad;
        if self.reveal {
            self.reveal = false;
            if let Some(i) = self.selected_index(rows).and_then(|s| self.visible.iter().position(|&v| v == s)) {
                let y = i as f32 * row_h;
                if y < self.scroll || y + row_h > self.scroll + view_h {
                    self.scroll = y - view_h / 3.0;
                }
            }
        }
        self.scroll = self.scroll.min(content_h - view_h).max(0.0);

        shapes.begin_layer(Transform::IDENTITY, Some(body));
        text.set_clip(Some(body));
        if rows.is_empty() {
            text.draw(TextStyle::new(fonts.body, size, theme.dim.0), [body[0] + pad, body[1] + pad], "No elements");
        }
        let first = (self.scroll / row_h).floor().max(0.0) as usize;
        let selected = self.selected_index(rows);
        let badge = (lh * 0.95).round();
        for (vi, &ri) in self.visible.iter().enumerate().skip(first) {
            let y = body[1] + vi as f32 * row_h - self.scroll;
            if y > body[3] {
                break;
            }
            let r = &rows[ri];
            let row_rect = [body[0], y, body[2], y + row_h];
            if selected == Some(ri) {
                let c = if self.active { theme.selection_row.0 } else { theme.selection_row.alpha(0.6) };
                shapes.rect(row_rect, c, 0.0);
                if self.active {
                    shapes.rect([body[0], y, body[0] + 2.0 * dpi, y + row_h], theme.selection.0, 0.0);
                }
            } else if self.hovered == Some(ri) {
                shapes.rect(row_rect, theme.hover_row.0, 0.0);
            }
            // Indentation guides.
            for d in 0..r.depth {
                let x = (body[0] + pad * 0.5 + indent * (d as f32 + 0.5)).round();
                shapes.rect([x, y, x + dpi.max(1.0), y + row_h], theme.divider.alpha(0.6), 0.0);
            }
            let x0 = body[0] + pad * 0.5 + indent * r.depth as f32;
            let cy = y + row_h * 0.5;
            if r.has_children {
                let open = is_open(&self.expanded, r);
                let (cx, s) = (x0 + indent * 0.5, size * 0.2);
                let c = theme.dim.0;
                let t = 1.4 * dpi;
                if open {
                    shapes.line([cx - s, cy - s * 0.5], [cx, cy + s * 0.5], c, t, 0.0);
                    shapes.line([cx, cy + s * 0.5], [cx + s, cy - s * 0.5], c, t, 0.0);
                } else {
                    shapes.line([cx - s * 0.5, cy - s], [cx + s * 0.5, cy], c, t, 0.0);
                    shapes.line([cx + s * 0.5, cy], [cx - s * 0.5, cy + s], c, t, 0.0);
                }
            }
            // Kind badge: definitions filled, usages outlined (like SysML's
            // def / usage distinction), colored by category.
            let color = category_color(theme, r.category);
            let bx = x0 + indent;
            let b = [bx, cy - badge * 0.5, bx + badge, cy + badge * 0.5];
            let letter = badge_letter(r);
            let letter_size = small * 0.92;
            let lw = text.measure(fonts.title, letter_size, &letter, None)[0];
            let llh = text.line_metrics(fonts.title, letter_size).line_height;
            let lp = [(b[0] + b[2] - lw) * 0.5, (b[1] + b[3] - llh) * 0.5];
            match r.category {
                Category::Package => {
                    // A folder: tab and body.
                    shapes.rect([b[0], b[1] + badge * 0.05, b[0] + badge * 0.5, b[1] + badge * 0.3], color.0, 1.5 * dpi);
                    shapes.rect([b[0], b[1] + badge * 0.2, b[2], b[3] - badge * 0.05], color.0, 2.0 * dpi);
                }
                _ if r.is_definition => {
                    shapes.rect(b, color.0, 3.0 * dpi);
                    text.draw(TextStyle::new(fonts.title, letter_size, theme.panel.0), lp, &letter);
                }
                Category::Comment | Category::Import | Category::Expression => {
                    text.draw(TextStyle::new(fonts.title, letter_size, color.0), lp, &letter);
                }
                _ => {
                    shapes.bordered_rect(b, color.alpha(0.14), color.0, badge * 0.5, 1.2 * dpi);
                    text.draw(TextStyle::new(fonts.title, letter_size, color.0), lp, &letter);
                }
            }
            // Label, then the kind keyword dimmed.
            let tx = b[2] + pad * 0.6;
            let ty = cy - lh * 0.5;
            let (label, label_color) = if r.label.is_empty() { (&r.keyword, theme.text.0) } else { (&r.label, theme.text.0) };
            text.draw(TextStyle::new(fonts.body, size, label_color), [tx, ty], label);
            if !r.label.is_empty() && !r.keyword.is_empty() {
                let w = text.measure(fonts.body, size, label, None)[0];
                let ky = cy - text.line_metrics(fonts.body, small).line_height * 0.5;
                text.draw(TextStyle::new(fonts.body, small, theme.dim.0), [tx + w + pad * 0.6, ky], &r.keyword);
            }
        }
        text.set_clip(None);

        // Scrollbar.
        if content_h > view_h {
            let h = (view_h * view_h / content_h).max(row_h);
            let t = self.scroll / (content_h - view_h);
            let y = body[1] + t * (view_h - h);
            let w = 4.0 * dpi;
            shapes.rect([body[2] - w - 2.0 * dpi, y, body[2] - 2.0 * dpi, y + h], theme.dim.alpha(0.35), w * 0.5);
        }
    }
}

fn category_color(theme: &Theme, c: Category) -> Color {
    let o = &theme.outline;
    match c {
        Category::Package => o.package,
        Category::Import => o.import,
        Category::Comment => o.comment,
        Category::Definition => theme.node.definition,
        Category::Usage => theme.node.usage,
        Category::Relationship => o.relationship,
        Category::Behavior => theme.node.behavior,
        Category::Expression => o.expression,
    }
}

/// First letter of the kind (`part` → P, `attribute def` → A); a few symbols
/// for kinds without a useful letter.
fn badge_letter(r: &TreeRow) -> String {
    match r.category {
        Category::Comment => "\u{201C}".into(),
        Category::Import => "\u{2192}".into(),
        Category::Expression => "=".into(),
        _ => {
            let word = r.keyword.split_whitespace().rfind(|w| *w != "def" && *w != "abstract").unwrap_or("");
            word.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "·".into())
        }
    }
}
