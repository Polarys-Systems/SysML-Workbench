//! Hover help: resting the mouse on a keyword or symbol in the code pane, or
//! on a diagram node (its header or a compartment line), shows the matching
//! entry of the embedded SysML quick reference ([`crate::docs`]) in a popup.

use super::Ui;
use super::chrome::Card;
use crate::diagram_view::{Fonts, contains};
use crate::docs::{self, Reference};
use crate::shapes::{Rect, ShapeRenderer};
use crate::source::Highlight;
use crate::text::{FontId, TextRenderer, TextStyle};
use crate::theme::Theme;
use std::time::{Duration, Instant};

/// How long the mouse rests on something before its help appears.
const DELAY: Duration = Duration::from_millis(450);
/// Popup width and gap to the hovered thing, in logical pixels.
const MAX_WIDTH: f32 = 460.0;
const GAP: f32 = 6.0;

/// What is hovered: a code token by byte range, or a diagram node (line 0)
/// or one of its compartment lines (line i + 1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spot {
    Code(usize, usize),
    Node(usize, usize),
}

#[derive(Clone, Copy)]
struct Target {
    spot: Spot,
    entry: usize,
    /// Screen rectangle the popup goes under (or over).
    anchor: Rect,
}

pub(super) struct HoverState {
    target: Option<Target>,
    since: Instant,
    /// Typing, clicking or scrolling hides the popup until the mouse moves.
    suppressed: bool,
}

impl Default for HoverState {
    fn default() -> Self {
        Self { target: None, since: Instant::now(), suppressed: false }
    }
}

impl HoverState {
    pub(super) fn hide(&mut self) {
        self.target = None;
        self.suppressed = true;
    }

    pub(super) fn moved(&mut self) {
        self.suppressed = false;
    }
}

/// One block of the popup, measured before anything is drawn.
struct Block {
    font: FontId,
    size: f32,
    color: [f32; 4],
    text: String,
    wrap: bool,
    /// Gap above the block.
    top: f32,
    /// Drawn on a tinted panel (examples).
    boxed: bool,
    h: f32,
}

impl Ui {
    /// The documented thing under the mouse, if any.
    fn hover_target(&self) -> Option<Target> {
        let p = self.cursor;
        let busy = self.menu.is_open()
            || self.show_shortcuts
            || self.dragging.is_some()
            || self.code.is_dragging()
            || self.view.is_dragging()
            || self.edit.busy()
            || self.placing();
        if busy {
            return None;
        }
        let r = docs::reference();
        match self.card_at(p) {
            Some(Card::Code) => {
                self.panes.code.filter(|&c| contains(c, p))?;
                let (a, b, h, rect) = self.code.token_at(p)?;
                let src = &self.code.editor.text()[a..b];
                let at_delimiter = || {
                    let x = (p[0] - rect[0]) / (rect[2] - rect[0]).max(1.0) * (b - a) as f32;
                    x < 2.0 || x >= (b - a) as f32 - 2.0
                };
                let entry = match h {
                    Highlight::Keyword => r.for_keyword(src),
                    Highlight::Symbol => r.for_symbol(src),
                    // Only the comment delimiters, not the text inside.
                    Highlight::Comment if at_delimiter() => r.lookup(if src.starts_with("//") { "//" } else { "/* */" }),
                    _ => None,
                }?;
                Some(Target { spot: Spot::Code(a, b), entry, anchor: rect })
            }
            Some(Card::Tree) => None,
            None if self.over_chrome(p) => None,
            None => {
                let pane = self.panes.diagram.filter(|&d| contains(d, p))?;
                let n = self.view.hovered?;
                let node = self.doc.diagram.nodes.get(n)?;
                let world = self.view.to_world(p, pane, self.dpi);
                let (entry, line) = match self.doc.diagram.line_at(n, world, &self.config.diagram) {
                    Some(i) => (r.for_line(&node.lines[i]), i + 1),
                    None => (None, 0),
                };
                let (entry, line) = match entry {
                    Some(e) => (e, line),
                    None => (r.for_kind(node.kind_label())?, 0),
                };
                let h = 18.0 * self.dpi;
                Some(Target { spot: Spot::Node(n, line), entry, anchor: [p[0], p[1], p[0] + 1.0, p[1] + h] })
            }
        }
    }

    /// Follows the mouse; called once per frame before drawing.
    pub(super) fn update_hover(&mut self) {
        if self.hover.suppressed {
            return;
        }
        let next = self.hover_target();
        let same = match (self.hover.target, next) {
            (Some(a), Some(b)) => a.spot == b.spot && a.entry == b.entry,
            (a, b) => a.is_none() && b.is_none(),
        };
        if !same {
            self.hover.target = next;
            self.hover.since = Instant::now();
        }
    }

    /// The help popup, above everything else (overlay pass).
    pub(super) fn draw_hover(&self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, window: [f32; 2], dpi: f32) {
        let Some(t) = self.hover.target else { return };
        if self.hover.suppressed || self.hover.since.elapsed() < DELAY {
            return;
        }
        let r = docs::reference();
        let size = self.config.view.ui_font_size * dpi;
        let small = (size * 0.92).round();
        let pad = (10.0 * dpi).round();
        let margin = 8.0 * dpi;
        let max_w = (MAX_WIDTH * dpi).min(window[0] - 2.0 * margin - 2.0 * pad);
        if max_w < 60.0 * dpi {
            return;
        }
        let mut blocks = blocks(r, t.entry, fonts, theme, size, small, pad);
        let mut w: f32 = 0.0;
        let mut h = 0.0;
        for b in &mut blocks {
            let limit = b.wrap.then_some(max_w);
            let inner = if b.boxed { 2.0 * pad * 0.6 } else { 0.0 };
            let [bw, bh] = text.measure(b.font, b.size, &b.text, limit.map(|m| m - inner));
            b.h = bh + inner;
            w = w.max(bw + inner);
            h += b.top + b.h;
        }
        let w = (w.min(max_w) + 2.0 * pad).ceil();
        let h = (h + 2.0 * pad).ceil().min(window[1] - 2.0 * margin);

        // Under the anchor if it fits, else over it, else as low as fits.
        let gap = GAP * dpi;
        let x = t.anchor[0].min(window[0] - margin - w).max(margin).round();
        let below = t.anchor[3] + gap;
        let above = t.anchor[1] - gap - h;
        let y = if below + h <= window[1] - margin {
            below
        } else if above >= margin {
            above
        } else {
            (window[1] - margin - h).max(margin)
        }
        .round();
        let rect = [x, y, x + w, y + h];

        shapes.begin_overlay();
        text.begin_overlay();
        let radius = theme.size.frame.dialog_radius * dpi * 0.6;
        shapes.shadow([rect[0], rect[1] + 6.0 * dpi, rect[2], rect[3] + 6.0 * dpi], theme.glass.shadow.0, radius, 18.0 * dpi);
        shapes.bordered_rect(rect, theme.bg.panel.0, theme.glass.border.0, radius, dpi.max(1.0));
        let clip = [rect[0] + 1.0, rect[1] + 1.0, rect[2] - 1.0, rect[3] - 1.0];
        shapes.begin_layer(crate::shapes::Transform::IDENTITY, Some(clip));
        text.set_clip(Some(clip));
        let mut by = y + pad;
        for b in &blocks {
            by += b.top;
            if by >= rect[3] {
                break;
            }
            let style = TextStyle::new(b.font, b.size, b.color);
            if b.boxed {
                let inner = pad * 0.6;
                shapes.rect([x + pad, by, x + w - pad, by + b.h], theme.current_line.alpha(2.0), radius * 0.6);
                text.draw(style, [x + pad + inner, by + inner], &b.text);
            } else if b.wrap {
                text.draw_wrapped(style, [x + pad, by], w - 2.0 * pad, &b.text);
            } else {
                text.draw(style, [x + pad, by], &b.text);
            }
            by += b.h;
        }
        text.set_clip(None);
    }
}

/// The popup's content for `entry`: names, category line, summary, syntax,
/// details, example and related entries.
fn blocks(r: &Reference, entry: usize, fonts: Fonts, theme: &Theme, size: f32, small: f32, pad: f32) -> Vec<Block> {
    let e = r.entry(entry);
    let block = |font, size, color: [f32; 4], text: String, top: f32| Block {
        font,
        size,
        color,
        text,
        wrap: true,
        top,
        boxed: false,
        h: 0.0,
    };
    let para = (pad * 0.7).round();
    let mut out = vec![block(fonts.title, size, theme.syntax.keyword.0, e.names.join("  ·  "), 0.0)];
    let mut meta = format!("{} · {}", r.category_title(&e.category), e.kind);
    if let Some(spec) = &e.spec {
        meta += &format!(" · spec §{spec}");
    }
    out.push(block(fonts.body, small, theme.text_muted.0, meta, 2.0));
    out.push(block(fonts.body, size, theme.text.0, e.summary.clone(), para));
    for (i, s) in e.syntax.iter().enumerate() {
        out.push(block(fonts.mono, small, theme.text.0, s.clone(), if i == 0 { para } else { 2.0 }));
    }
    for p in e.paragraphs() {
        out.push(block(fonts.body, small, theme.text_muted.0, p, para));
    }
    if let Some(ex) = e.example.as_deref().map(str::trim_end).filter(|s| !s.trim().is_empty()) {
        out.push(Block { wrap: false, boxed: true, ..block(fonts.mono, small, theme.text.0, ex.to_owned(), para) });
    }
    if !e.see.is_empty() {
        out.push(block(fonts.body, small, theme.text_muted.0, format!("See also: {}", e.see.join(", ")), para));
    }
    out
}
