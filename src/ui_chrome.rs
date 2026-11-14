//! The window chrome: the top bar, the status bar, the glass cards floating
//! over the diagram canvas (code editor, model tree) and the diagram's
//! floating controls (model numbers, zoom). The diagram palette is drawn by
//! `ui_edit.rs`; the canvas itself fills the window behind everything.

use super::{Divider, Panes, Ui};
use crate::config::ViewMode;
use crate::document::Tone;
use crate::diagram_view::{Fonts, contains};
use crate::menu::{Command, Item};
use crate::shapes::{Rect, ShapeRenderer, Transform};
use crate::text::{TextRenderer, TextStyle};
use crate::theme::{Color, Theme};
use winit::window::CursorIcon;

/// Clickable chrome, laid out by the last draw.
#[derive(Default)]
pub(super) struct ChromeState {
    top: Rect,
    status: Rect,
    code_card: Option<Rect>,
    tree_card: Option<Rect>,
    file: Rect,
    modes: Vec<(Rect, ViewMode)>,
    search: Rect,
    problems: Rect,
    settings: Rect,
    help: Rect,
    /// Floating diagram controls: the rows above and below the diagram
    /// (model numbers and zoom; the palette), the zoom group and its buttons.
    controls_row: Option<Rect>,
    palette_row: Option<Rect>,
    stats: Option<Rect>,
    zoom: Option<Rect>,
    zoom_out: Rect,
    zoom_in: Rect,
    fit: Rect,
}

pub(super) enum Card {
    Code,
    Tree,
}

/// What a chrome button does.
enum Hit {
    Run(Command),
    FileMenu(Rect),
    SettingsMenu(Rect),
    ProblemList(Rect),
}

/// A translucent card with a soft shadow and a light top edge.
pub(super) fn glass_card(shapes: &mut ShapeRenderer, theme: &Theme, r: Rect, radius: f32, dpi: f32) {
    let (f, g) = (&theme.size.frame, &theme.glass);
    let o = f.shadow_offset * dpi;
    if g.shadow.0[3] > 0.0 {
        shapes.shadow([r[0], r[1] + o, r[2], r[3] + o], g.shadow.0, radius, f.shadow_blur * dpi);
    }
    if f.backdrop_blur > 0.0 {
        shapes.glass(r, g.fill.0, g.border.0, radius, dpi.max(1.0));
    } else {
        shapes.bordered_rect(r, g.fill.0, g.border.0, radius, dpi.max(1.0));
    }
    if r[2] - r[0] > 2.0 * radius {
        shapes.rect([r[0] + radius, r[1] + dpi, r[2] - radius, r[1] + 2.0 * dpi], g.highlight.0, 0.0);
    }
}

/// A full-width bar: frosted glass, or a translucent fill when blur is off.
fn glass_bar(shapes: &mut ShapeRenderer, theme: &Theme, r: Rect) {
    if theme.size.frame.backdrop_blur > 0.0 {
        shapes.glass(r, theme.glass.bar.0, theme.glass.bar.0, 0.0, 0.0);
    } else {
        shapes.rect(r, theme.glass.bar.0, 0.0);
    }
}

fn center(r: Rect) -> [f32; 2] {
    [(r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5]
}

/// A round icon button; hover tints it.
fn icon_button(shapes: &mut ShapeRenderer, theme: &Theme, r: Rect, hot: bool, dpi: f32) {
    let fill = if hot { theme.glass.control.alpha(1.0) } else { Color::TRANSPARENT };
    shapes.bordered_rect(r, fill, theme.glass.border.0, (r[2] - r[0]) * 0.5, dpi.max(1.0));
}

fn magnifier(shapes: &mut ShapeRenderer, c: [f32; 2], color: [f32; 4], dpi: f32) {
    let r = 5.0 * dpi;
    let (x, y) = (c[0] - 1.5 * dpi, c[1] - 1.5 * dpi);
    shapes.bordered_rect([x - r, y - r, x + r, y + r], Color::TRANSPARENT, color, r, 1.6 * dpi);
    let k = r * 0.72;
    shapes.line([x + k, y + k], [x + k + 4.0 * dpi, y + k + 4.0 * dpi], color, 1.8 * dpi, 0.0);
}

fn gear(shapes: &mut ShapeRenderer, c: [f32; 2], color: [f32; 4], dpi: f32) {
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let (s, co) = a.sin_cos();
        let p = |r: f32| [c[0] + co * r * dpi, c[1] + s * r * dpi];
        shapes.line(p(5.0), p(7.2), color, 2.6 * dpi, 0.0);
    }
    let r = 5.6 * dpi;
    shapes.bordered_rect([c[0] - r, c[1] - r, c[0] + r, c[1] + r], Color::TRANSPARENT, color, r, 2.2 * dpi);
}

fn chevron_down(shapes: &mut ShapeRenderer, c: [f32; 2], color: [f32; 4], dpi: f32) {
    let s = 3.5 * dpi;
    shapes.line([c[0] - s, c[1] - s * 0.5], [c[0], c[1] + s * 0.5], color, 1.5 * dpi, 0.0);
    shapes.line([c[0], c[1] + s * 0.5], [c[0] + s, c[1] - s * 0.5], color, 1.5 * dpi, 0.0);
}

/// A key cap (`Ctrl+S`) for hints; returns its width.
#[allow(clippy::too_many_arguments)]
fn key_cap(shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, x: f32, cy: f32, label: &str, size: f32, dpi: f32) -> f32 {
    let size = size * 0.9;
    let [w, _] = text.measure(fonts.body, size, label, None);
    let lh = text.line_metrics(fonts.body, size).line_height;
    let (pad, h) = (5.0 * dpi, (lh + 2.0 * dpi).round());
    let r = [x, (cy - h * 0.5).round(), x + w + 2.0 * pad, (cy - h * 0.5).round() + h];
    shapes.bordered_rect(r, theme.glass.control.0, theme.glass.border.0, 4.0 * dpi, dpi.max(1.0));
    text.draw(TextStyle::new(fonts.body, size, theme.text_muted.0), [x + pad, (cy - lh * 0.5).round()], label);
    r[2] - r[0]
}

impl Ui {
    /// Places the cards over the canvas: the code card on the left, the model
    /// tree card on the right, the diagram canvas behind the whole window.
    pub(super) fn layout_panes(&mut self, width: f32, height: f32, dpi: f32) {
        let f = &self.config.theme.size.frame;
        let gap = (f.gap * dpi).round();
        let top_h = (f.top_bar_height * dpi).round();
        let status_h = (f.status_bar_height * dpi).round();
        let header_h = (f.card_header_height * dpi).round();
        let min_w = f.pane_min_width * dpi;
        let (top, bottom) = (top_h + gap, height - status_h - gap);
        let grip = (3.0 * dpi).round();

        let mut panes = Panes { window: [width, height], ..Default::default() };
        self.chrome.top = [0.0, 0.0, width, top_h];
        self.chrome.status = [0.0, height - status_h, width, height];
        self.chrome.code_card = None;
        self.chrome.tree_card = None;

        let mut right = width - gap;
        if self.config.view.tree {
            let max = (width * 0.6).max(min_w);
            let tw = (self.config.view.tree_width * dpi).clamp(min_w, max).round();
            let card = [width - gap - tw, top, width - gap, bottom];
            self.chrome.tree_card = Some(card);
            panes.tree = Some([card[0] + 4.0 * dpi, card[1], card[2] - 4.0 * dpi, card[3] - 8.0 * dpi]);
            panes.tree_divider = Some([card[0] - grip, top, card[0] + grip, bottom]);
            right = card[0] - gap;
        }
        panes.tree_right = width - gap;
        let left = gap;
        panes.split_x = left;
        panes.split_w = (right - left).max(1.0);
        let code_card = match self.mode {
            ViewMode::Outline => Some([left, top, right, bottom]),
            ViewMode::Diagram => None,
            ViewMode::Split => {
                let w = (panes.split_w * self.split).round().clamp(min_w.min(panes.split_w), panes.split_w);
                Some([left, top, left + w, bottom])
            }
        };
        let mut free = [left, top, right, bottom];
        if let Some(card) = code_card {
            self.chrome.code_card = Some(card);
            panes.code = Some([card[0] + dpi, card[1] + header_h, card[2] - dpi, card[3] - (f.card_radius * 0.5 * dpi).round()]);
            if self.mode == ViewMode::Split {
                panes.divider = Some([card[2] - grip, top, card[2] + grip, bottom]);
            }
            free[0] = card[2] + gap;
        }
        if self.mode != ViewMode::Outline {
            // The canvas spans the window; fitting aims below the controls row.
            panes.diagram = Some([0.0, 0.0, width, height]);
            let row_h = (f.control_height * dpi).round() + 2.0 * (f.toolbar_pad * dpi).round();
            self.chrome.controls_row = Some([free[0], top, free[2], top + row_h]);
            self.chrome.palette_row = Some([free[0], bottom - row_h, free[2], bottom]);
            self.view.focus = Some([free[0], top + row_h + gap, free[2], bottom - row_h - gap]);
        } else {
            self.chrome.controls_row = None;
            self.chrome.palette_row = None;
            self.view.focus = None;
        }
        self.panes = panes;
    }

    /// True over anything floating above the canvas.
    pub(super) fn over_chrome(&self, p: [f32; 2]) -> bool {
        let c = &self.chrome;
        let on = |r: Option<Rect>| r.is_some_and(|r| contains(r, p));
        contains(c.top, p)
            || contains(c.status, p)
            || on(c.code_card)
            || on(c.tree_card)
            || on(c.stats)
            || on(c.zoom)
            || on(self.edit.toolbar)
    }

    fn hit(&self, p: [f32; 2]) -> Option<Hit> {
        let c = &self.chrome;
        if let Some(&(_, m)) = c.modes.iter().find(|(r, _)| contains(*r, p)) {
            return Some(Hit::Run(Command::Mode(m)));
        }
        let hits = [
            (Some(c.file), Hit::FileMenu(c.file)),
            (Some(c.search), Hit::Run(Command::Find)),
            (Some(c.problems), Hit::ProblemList(c.problems)),
            (Some(c.settings), Hit::SettingsMenu(c.settings)),
            (Some(c.help), Hit::Run(Command::Shortcuts)),
            (c.zoom.map(|_| c.zoom_out), Hit::Run(Command::ZoomOut)),
            (c.zoom.map(|_| c.zoom_in), Hit::Run(Command::ZoomIn)),
            (c.zoom.map(|_| c.fit), Hit::Run(Command::Fit)),
        ];
        hits.into_iter().find(|(r, _)| r.is_some_and(|r| contains(r, p))).map(|(_, h)| h)
    }


    /// The card under `p`.
    pub(super) fn card_at(&self, p: [f32; 2]) -> Option<Card> {
        if self.chrome.code_card.is_some_and(|r| contains(r, p)) {
            Some(Card::Code)
        } else if self.chrome.tree_card.is_some_and(|r| contains(r, p)) {
            Some(Card::Tree)
        } else {
            None
        }
    }

    /// A press on the bars or floating controls; true if it was consumed.
    pub(super) fn chrome_press(&mut self, p: [f32; 2], left: bool) -> bool {
        let below = |r: Rect, dpi: f32| [r[0], r[3] + 6.0 * dpi];
        let hit = if left { self.hit(p) } else { None };
        match hit {
            Some(Hit::Run(cmd)) => self.run(cmd),
            Some(Hit::FileMenu(r)) => {
                let items = self.build_menus().into_iter().next().map(|m| m.items).unwrap_or_default();
                self.menu.open_context(items, below(r, self.dpi));
            }
            Some(Hit::SettingsMenu(r)) => {
                let items = self.build_menus().into_iter().find(|m| m.title == "Settings").map(|m| m.items).unwrap_or_default();
                self.menu.open_context(items, below(r, self.dpi));
            }
            Some(Hit::ProblemList(r)) => {
                let items: Vec<Item> = self
                    .doc
                    .diagnostics
                    .iter()
                    .take(30)
                    .enumerate()
                    .map(|(i, d)| Item::action(d.text.clone(), "", Command::GotoProblem(i as u16)))
                    .collect();
                if items.is_empty() {
                    self.notify("No problems");
                } else {
                    self.menu.open_context(items, below(r, self.dpi));
                }
            }
            None => return contains(self.chrome.top, p) || contains(self.chrome.status, p) || self.chrome.stats.is_some_and(|r| contains(r, p)) || self.chrome.zoom.is_some_and(|r| contains(r, p)),
        }
        true
    }

    // ---- drawing -----------------------------------------------------------------

    /// The code and model cards with their contents.
    pub(super) fn draw_cards(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) {
        let f = &theme.size.frame;
        let radius = f.card_radius * dpi;
        let ui_size = self.config.view.ui_font_size * dpi;
        if let (Some(card), Some(pane)) = (self.chrome.code_card, self.panes.code) {
            shapes.begin_layer(Transform::IDENTITY, None);
            glass_card(shapes, theme, card, radius, dpi);
            // Header: title, file name, cursor position.
            let header = [card[0], card[1], card[2], pane[1]];
            let pad = 16.0 * dpi;
            let cy = (header[1] + header[3]) * 0.5;
            let lh = text.line_metrics(fonts.title, ui_size).line_height;
            text.set_clip(Some(header));
            text.draw(TextStyle::new(fonts.title, ui_size, theme.text.0), [card[0] + pad, (cy - lh * 0.5).round()], "Source");
            let tw = text.measure(fonts.title, ui_size, "Source", None)[0];
            let name = self.doc.file_name();
            text.draw(TextStyle::new(fonts.body, ui_size, theme.text_muted.0), [card[0] + pad + tw + 8.0 * dpi, (cy - lh * 0.5).round()], &name);
            let small = ui_size * f.hint_scale;
            let pos = self.code.status();
            let pw = text.measure(fonts.body, small, &pos, None)[0];
            let slh = text.line_metrics(fonts.body, small).line_height;
            text.draw(TextStyle::new(fonts.body, small, theme.text_muted.0), [card[2] - pad - pw, (cy - slh * 0.5).round()], &pos);
            text.set_clip(None);
            shapes.rect([card[0] + dpi, header[3] - dpi, card[2] - dpi, header[3]], theme.border.0, 0.0);
            let diags: Vec<(usize, usize)> = self.doc.diagnostics.iter().map(|d| (d.span.start, d.span.end)).collect();
            let notes: Vec<(usize, &str, [f32; 4])> = self
                .doc
                .annotations
                .iter()
                .map(|a| {
                    let color = match a.tone {
                        Tone::Value => theme.text_muted.0,
                        Tone::Muted => theme.text_muted.alpha(0.55),
                        Tone::Good => theme.success.0,
                        Tone::Bad => theme.error.0,
                    };
                    (a.offset, a.text.as_str(), color)
                })
                .collect();
            self.code.draw(shapes, text, fonts, self.config.view.outline_font_size, theme, pane, dpi, &diags, &notes);
            text.set_clip(None);
        }
        if let (Some(card), Some(pane)) = (self.chrome.tree_card, self.panes.tree) {
            shapes.begin_layer(Transform::IDENTITY, None);
            glass_card(shapes, theme, card, radius, dpi);
            self.tree.draw(&self.doc.tree, shapes, text, fonts, self.config.view.ui_font_size, theme, pane, dpi);
            text.set_clip(None);
        }
        // Resize grips: an accent edge while hovered or dragged.
        shapes.begin_layer(Transform::IDENTITY, None);
        for (d, which) in [(self.panes.divider, Divider::Split), (self.panes.tree_divider, Divider::Tree)] {
            let Some(d) = d else { continue };
            let active = self.dragging == Some(which) || (self.dragging.is_none() && self.divider_at(self.cursor) == Some(which));
            if active {
                let x = ((d[0] + d[2]) * 0.5).round();
                shapes.rect([x - dpi, d[1] + radius, x + dpi, d[3] - radius], theme.accent.0, dpi);
            }
        }
    }

    /// Model numbers and the zoom group in the row above the diagram.
    pub(super) fn draw_diagram_controls(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) {
        self.edit.toolbar = None;
        self.chrome.stats = None;
        self.chrome.zoom = None;
        let (Some(row), Some(palette_row)) = (self.chrome.controls_row, self.chrome.palette_row) else { return };
        let f = &theme.size.frame;
        let gap = (f.gap * dpi).round();
        let pad = (f.toolbar_pad * dpi).round();
        let ch = (f.control_height * dpi).round();
        let size = self.config.view.ui_font_size * dpi;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let radius = f.control_radius * dpi + pad;

        self.edit.toolbar = Some(self.draw_toolbar(shapes, text, fonts, theme, palette_row, dpi));

        // Zoom group, right-aligned: − 100% + | fit.
        let pct = format!("{:.0}%", self.view.zoom * 100.0);
        let pw = text.measure(fonts.body, size, "000%", None)[0] + 8.0 * dpi;
        let zw = ch * 3.0 + pw + pad * 4.0 + dpi;
        let zoom = [row[2] - zw, row[1], row[2], row[3]];
        if zoom[0] > row[0] + gap {
            shapes.begin_layer(Transform::IDENTITY, None);
            glass_card(shapes, theme, zoom, radius, dpi);
            let button = |x: f32, w: f32| [x, zoom[1] + pad, x + w, zoom[3] - pad];
            let out = button(zoom[0] + pad, ch);
            let label = button(out[2] + pad, pw);
            let inn = button(label[2] + pad, ch);
            // One pixel of room for the separator before "fit".
            let fit = button(inn[2] + pad + dpi, ch);
            for r in [out, inn, fit] {
                if contains(r, self.cursor) {
                    shapes.rect(r, theme.hover_row.alpha(3.0), f.control_radius * dpi);
                }
            }
            let (c, s, t) = (theme.text.0, 5.0 * dpi, 1.6 * dpi);
            let [ox, oy] = center(out);
            shapes.line([ox - s, oy], [ox + s, oy], c, t, 0.0);
            let [ix, iy] = center(inn);
            shapes.line([ix - s, iy], [ix + s, iy], c, t, 0.0);
            shapes.line([ix, iy - s], [ix, iy + s], c, t, 0.0);
            let sep_x = (inn[2] + pad * 0.5 + dpi * 0.5).round();
            shapes.rect([sep_x - dpi * 0.5, zoom[1] + pad * 2.5, sep_x + dpi * 0.5, zoom[3] - pad * 2.5], theme.glass.border.0, 0.0);
            // Fit: four corner brackets.
            let [fx, fy] = center(fit);
            let (a, b) = (6.0 * dpi, 2.5 * dpi);
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let corner = [fx + sx * a, fy + sy * a];
                shapes.line(corner, [corner[0] - sx * b, corner[1]], c, t, 0.0);
                shapes.line(corner, [corner[0], corner[1] - sy * b], c, t, 0.0);
            }
            let w = text.measure(fonts.body, size, &pct, None)[0];
            text.draw(TextStyle::new(fonts.body, size, theme.text.0), [(center(label)[0] - w * 0.5).round(), (center(label)[1] - lh * 0.5).round()], &pct);
            self.chrome.zoom = Some(zoom);
            self.chrome.zoom_out = out;
            self.chrome.zoom_in = inn;
            self.chrome.fit = fit;
        }

        // Model numbers at the left of the row, when they fit beside the zoom group.
        let edges = self.doc.diagram.edges.iter().filter(|e| e.kind.visible(&self.config.diagram.edges)).count();
        let problems = self.doc.diagnostics.len();
        let stats = [
            (self.doc.elements.len(), "elements"),
            (self.doc.diagram.nodes.len(), "nodes"),
            (edges, "relations"),
            (problems, if problems == 1 { "problem" } else { "problems" }),
        ];
        let blocks: Vec<(String, f32, f32)> = stats
            .iter()
            .map(|(n, what)| {
                let n = n.to_string();
                let nw = text.measure(fonts.title, size, &n, None)[0];
                let ww = text.measure(fonts.body, size, what, None)[0];
                (n, nw, ww)
            })
            .collect();
        let inner = 14.0 * dpi;
        let sw = blocks.iter().map(|(_, nw, ww)| nw + ww + 5.0 * dpi + 2.0 * inner).sum::<f32>() + pad * 2.0;
        let space = [row[0], self.chrome.zoom.map_or(row[2], |z| z[0] - gap)];
        if space[1] - space[0] >= sw {
            let x0 = space[0];
            let card = [x0, row[1], x0 + sw, row[3]];
            shapes.begin_layer(Transform::IDENTITY, None);
            glass_card(shapes, theme, card, radius, dpi);
            let ty = (center(card)[1] - lh * 0.5).round();
            let mut x = card[0] + pad;
            for (i, ((n, nw, ww), (_, what))) in blocks.iter().zip(stats).enumerate() {
                if i > 0 {
                    shapes.rect([x - dpi * 0.5, card[1] + pad * 3.0, x + dpi * 0.5, card[3] - pad * 3.0], theme.glass.border.0, 0.0);
                }
                let bad = i == 3 && problems > 0;
                let color = if bad { theme.error.0 } else { theme.text.0 };
                text.draw(TextStyle::new(fonts.title, size, color), [x + inner, ty], n);
                text.draw(TextStyle::new(fonts.body, size, theme.text_muted.0), [x + inner + nw + 5.0 * dpi, ty], what);
                x += nw + ww + 5.0 * dpi + 2.0 * inner;
            }
            self.chrome.stats = Some(card);
        }
    }

    /// The top bar: brand, file, menus, mode switch, search, problems, settings, help.
    pub(super) fn draw_top_bar(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) -> Rect {
        let f = &theme.size.frame;
        let bar = self.chrome.top;
        let size = self.config.view.ui_font_size * dpi;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let ch = (f.control_height * dpi).round();
        let cy = ((bar[1] + bar[3]) * 0.5).round();
        let ctl = |x0: f32, w: f32| [x0, cy - ch * 0.5, x0 + w, cy + ch * 0.5];
        let ty = (cy - lh * 0.5).round();
        let mouse = self.cursor;

        shapes.begin_layer(Transform::IDENTITY, None);
        glass_bar(shapes, theme, bar);
        shapes.rect([bar[0], bar[3] - dpi, bar[2], bar[3]], theme.glass.border.0, 0.0);
        text.set_clip(Some(bar));

        // Brand: mark and two-line name.
        let mut x = 20.0 * dpi;
        let m = (30.0 * dpi).round();
        let mark = [x, cy - m * 0.5, x + m, cy + m * 0.5];
        shapes.rect(mark, theme.accent.0, 9.0 * dpi);
        // An eye: the "view" in EagleView.
        let [mx, my] = center(mark);
        let (ew, eh) = (8.0 * dpi, 4.6 * dpi);
        shapes.line([mx - ew, my], [mx, my - eh], theme.text_on_accent.0, 1.7 * dpi, 0.0);
        shapes.line([mx, my - eh], [mx + ew, my], theme.text_on_accent.0, 1.7 * dpi, 0.0);
        shapes.line([mx - ew, my], [mx, my + eh], theme.text_on_accent.0, 1.7 * dpi, 0.0);
        shapes.line([mx, my + eh], [mx + ew, my], theme.text_on_accent.0, 1.7 * dpi, 0.0);
        let pr = 2.4 * dpi;
        shapes.rect([mx - pr, my - pr, mx + pr, my + pr], theme.text_on_accent.0, pr);
        x = mark[2] + 10.0 * dpi;
        let name_size = size * 1.1;
        let small = size * 0.82;
        let (nlh, slh) = (text.line_metrics(fonts.title, name_size).line_height, text.line_metrics(fonts.body, small).line_height);
        let y0 = (cy - (nlh + slh) * 0.5 + dpi).round();
        text.draw(TextStyle::new(fonts.title, name_size, theme.text.0), [x, y0], "EagleView");
        text.draw(TextStyle::new(fonts.body, small, theme.text_muted.0), [x, y0 + nlh - dpi], "SysML v2 studio");
        x += text.measure(fonts.title, name_size, "EagleView", None)[0].max(text.measure(fonts.body, small, "SysML v2 studio", None)[0]) + 18.0 * dpi;
        shapes.rect([x, cy - 12.0 * dpi, x + dpi, cy + 12.0 * dpi], theme.glass.border.0, 0.0);
        x += dpi + 10.0 * dpi;

        // File selector: opens the File menu.
        let file = self.doc.file_name();
        let dirty = if self.modified() { " ●" } else { "" };
        let label = format!("{file}{dirty}");
        let fw = text.measure(fonts.title, size, &label, None)[0];
        let fr = ctl(x, fw + 38.0 * dpi);
        if contains(fr, mouse) {
            shapes.rect(fr, theme.glass.control.0, f.control_radius * dpi);
        }
        text.draw(TextStyle::new(fonts.title, size, theme.text.0), [fr[0] + 12.0 * dpi, ty], &label);
        chevron_down(shapes, [fr[2] - 14.0 * dpi, cy], theme.text_muted.0, dpi);
        self.chrome.file = fr;
        x = fr[2] + 6.0 * dpi;

        // Menu titles (drawn last so their popups cover everything).
        let msz = &theme.size.menu;
        let menus_w = self.menu.titles_width(text, fonts, size, msz, dpi);
        let menus = [x, cy - ch * 0.5, x + menus_w, cy + ch * 0.5];
        let left_end = menus[2];

        // Right side, from the window edge inwards.
        let mut rx = bar[2] - 20.0 * dpi;
        let help = ctl(rx - ch, ch);
        rx = help[0] - 8.0 * dpi;
        let settings = ctl(rx - ch, ch);
        rx = settings[0] - 12.0 * dpi;
        let problems = self.doc.diagnostics.len();
        let plabel = match problems {
            0 => "No problems".to_owned(),
            1 => "1 problem".to_owned(),
            n => format!("{n} problems"),
        };
        let plw = text.measure(fonts.body, size, &plabel, None)[0];
        let pchip = ctl(rx - plw - 36.0 * dpi, plw + 36.0 * dpi);
        rx = pchip[0] - 8.0 * dpi;
        let sw = (230.0 * dpi).round();
        let search = ctl(rx - sw, sw);
        rx = search[0];

        for (r, hot) in [(help, contains(help, mouse)), (settings, contains(settings, mouse))] {
            icon_button(shapes, theme, r, hot, dpi);
        }
        let qw = text.measure(fonts.title, size * 1.05, "?", None)[0];
        let qlh = text.line_metrics(fonts.title, size * 1.05).line_height;
        text.draw(TextStyle::new(fonts.title, size * 1.05, theme.text_muted.0), [(center(help)[0] - qw * 0.5).round(), (cy - qlh * 0.5).round()], "?");
        gear(shapes, center(settings), theme.text_muted.0, dpi);

        let chip_fill = if contains(pchip, mouse) { theme.hover_row.alpha(3.0) } else { theme.glass.control.0 };
        shapes.bordered_rect(pchip, chip_fill, theme.glass.border.0, ch * 0.5, dpi.max(1.0));
        let dot = 3.5 * dpi;
        let dc = [pchip[0] + 16.0 * dpi, cy];
        let dot_color = if problems == 0 { theme.success.0 } else { theme.error.0 };
        shapes.rect([dc[0] - dot, dc[1] - dot, dc[0] + dot, dc[1] + dot], dot_color, dot);
        text.draw(TextStyle::new(fonts.body, size, theme.text.0), [dc[0] + 10.0 * dpi, ty], &plabel);

        let search_fill = if contains(search, mouse) { theme.hover_row.alpha(3.0) } else { theme.glass.control.0 };
        shapes.bordered_rect(search, search_fill, theme.glass.border.0, f.control_radius * dpi, dpi.max(1.0));
        magnifier(shapes, [search[0] + 18.0 * dpi, cy], theme.text_muted.0, dpi);
        text.draw(TextStyle::new(fonts.body, size, theme.text_muted.0), [search[0] + 34.0 * dpi, ty], "Search code");
        let kw = text.measure(fonts.body, size * 0.9, "Ctrl F", None)[0] + 10.0 * dpi;
        key_cap(shapes, text, fonts, theme, search[2] - kw - 8.0 * dpi, cy, "Ctrl F", size, dpi);

        // Mode switch: centered, or just right of the menus when space is short.
        let labels = [("Split", ViewMode::Split), ("Code", ViewMode::Outline), ("Diagram", ViewMode::Diagram)];
        let seg_w: Vec<f32> = labels.iter().map(|(l, _)| (text.measure(fonts.title, size, l, None)[0] + 32.0 * dpi).round()).collect();
        let inset = 3.0 * dpi;
        let total = seg_w.iter().sum::<f32>() + inset * 2.0;
        let mut sx = ((bar[2] - total) * 0.5).round();
        sx = sx.max(left_end + 16.0 * dpi).min(rx - 16.0 * dpi - total);
        self.chrome.modes.clear();
        if sx >= left_end + 8.0 * dpi {
            let track = ctl(sx, total);
            shapes.rect(track, theme.glass.control.0, ch * 0.5);
            let mut x = track[0] + inset;
            for ((label, mode), w) in labels.into_iter().zip(seg_w) {
                let r = [x, track[1] + inset, x + w, track[3] - inset];
                let on = self.mode == mode;
                if on {
                    shapes.shadow([r[0], r[1] + dpi, r[2], r[3] + dpi], theme.glass.shadow.alpha(0.6), (r[3] - r[1]) * 0.5, 3.0 * dpi);
                    shapes.rect(r, theme.glass.control_active.0, (r[3] - r[1]) * 0.5);
                } else if contains(r, mouse) {
                    shapes.rect(r, theme.hover_row.alpha(2.0), (r[3] - r[1]) * 0.5);
                }
                let (font, color) = if on { (fonts.title, theme.text.0) } else { (fonts.body, theme.text_muted.0) };
                let w = text.measure(font, size, label, None)[0];
                text.draw(TextStyle::new(font, size, color), [(center(r)[0] - w * 0.5).round(), ty], label);
                self.chrome.modes.push((r, mode));
                x += r[2] - r[0];
            }
        }
        text.set_clip(None);
        self.chrome.search = search;
        self.chrome.problems = pchip;
        self.chrome.settings = settings;
        self.chrome.help = help;
        menus
    }

    /// The status bar: what is going on at the left, shortcut hints at the right.
    pub(super) fn draw_status_bar(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) {
        let f = &theme.size.frame;
        let bar = self.chrome.status;
        let size = self.config.view.ui_font_size * dpi * f.hint_scale;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let cy = ((bar[1] + bar[3]) * 0.5).round();
        let ty = (cy - lh * 0.5).round();
        shapes.begin_layer(Transform::IDENTITY, None);
        glass_bar(shapes, theme, bar);
        shapes.rect([bar[0], bar[1], bar[2], bar[1] + dpi], theme.glass.border.0, 0.0);

        let code_active = self.code.active && self.mode != ViewMode::Diagram;
        let hints: &[(&str, &str)] = if self.tree.active {
            &[("↑ ↓", "move"), ("← →", "fold"), ("Enter", "show"), ("Ctrl+B", "hide tree")]
        } else if code_active {
            &[("Ctrl+S", "save"), ("Ctrl+F", "find"), ("Ctrl+H", "replace"), ("F2", "rename"), ("Shift+Alt+F", "format"), ("Ctrl+Tab", "view")]
        } else {
            &[("Tab", "view"), ("F", "fit"), ("L", "re-layout"), ("G", "grid"), ("C", "compartments"), ("Ctrl+S", "save")]
        };
        // Hints from the right; drop the last ones when the bar is too narrow.
        let pad = 16.0 * dpi;
        let mut x = bar[2] - pad;
        let mut shown = Vec::new();
        for (k, what) in hints.iter().rev() {
            let kw = text.measure(fonts.body, size * 0.9, k, None)[0] + 10.0 * dpi;
            let ww = text.measure(fonts.body, size, what, None)[0];
            let w = kw + 6.0 * dpi + ww;
            if x - w < bar[2] * 0.45 {
                break;
            }
            x -= w;
            shown.push((x, *k, *what, kw));
            x -= 16.0 * dpi;
        }
        let hints_left = x;
        for (hx, k, what, kw) in shown {
            key_cap(shapes, text, fonts, theme, hx, cy, k, size, dpi);
            text.draw(TextStyle::new(fonts.body, size, theme.text_muted.0), [hx + kw + 6.0 * dpi, ty], what);
        }

        // Left: message, errors, the diagram's current hint.
        let (hint, busy) = self.edit_hint();
        let (msg, color) = if let Some((m, _)) = &self.message {
            (m.clone(), theme.accent.0)
        } else if let Some(e) = &self.doc.load_error {
            (e.clone(), theme.error.0)
        } else if let Some(e) = &self.config_error {
            (format!("settings: {e}"), theme.error.0)
        } else if busy {
            (hint, theme.accent.0)
        } else if self.mode == ViewMode::Outline || code_active {
            (format!("{} elements · {} nodes", self.doc.elements.len(), self.doc.diagram.nodes.len()), theme.text_muted.0)
        } else {
            (hint, theme.text_muted.0)
        };
        let mut left = msg;
        if self.config.view.show_frame_time {
            left = format!("{left}  ·  {:.2} ms/frame", self.frame_ms);
        }
        text.set_clip(Some([bar[0] + pad, bar[1], hints_left - pad, bar[3]]));
        text.draw(TextStyle::new(fonts.body, size, color), [bar[0] + pad, ty], &left);
        text.set_clip(None);
    }

    pub(super) fn chrome_cursor_icon(&self, p: [f32; 2]) -> Option<CursorIcon> {
        if self.menu.is_open() || self.show_shortcuts {
            return None;
        }
        self.hit(p).map(|_| CursorIcon::Pointer)
    }
}
