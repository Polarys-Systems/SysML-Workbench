//! Hover help: resting the mouse on a keyword or symbol in the code pane
//! shows the matching entry of the embedded SysML quick reference
//! ([`crate::docs`]); resting it on a name (a declaration, a type, a feature
//! chain, a unit) or on a diagram node (its header or a compartment line)
//! describes the model element it stands for ([`crate::element_docs`]).

use super::Ui;
use super::chrome::Card;
use crate::diagram_view::{Fonts, contains};
use crate::docs::{self, Reference};
use crate::document::Tone;
use crate::element_docs::{self, ElementHelp};
use crate::shapes::{Rect, ShapeRenderer};
use crate::source::Highlight;
use crate::text::{FontId, TextRenderer, TextStyle};
use crate::theme::Theme;
use std::time::{Duration, Instant};

/// How long the mouse rests on something before its help appears.
const DELAY: Duration = Duration::from_millis(450);
/// Popup width and gap to the hovered thing, in logical pixels.
const MAX_WIDTH: f32 = 500.0;
const GAP: f32 = 6.0;

/// What is hovered: a code token by byte range, or a diagram node (line 0)
/// or one of its compartment lines (line i + 1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spot {
    Code(usize, usize),
    Node(usize, usize),
}

/// What to look up for a spot. Found every frame, so it is cheap; the help
/// itself is built once, when the probe changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    /// A keyword or symbol: this reference entry.
    Reference(usize),
    /// A name in the code, at this offset.
    Name(usize),
    /// A diagram node, or its compartment line `i` (with the keyword entry
    /// to fall back on).
    Node(usize, Option<usize>, Option<usize>),
}

#[derive(Clone, Copy)]
struct Target {
    spot: Spot,
    probe: Probe,
    /// Screen rectangle the popup goes under (or over).
    anchor: Rect,
}

enum Help {
    Reference(usize),
    Element(Box<ElementHelp>),
}

pub(super) struct HoverState {
    target: Option<Target>,
    help: Option<Help>,
    /// Model generation the help was built from (`None`: not built yet).
    generation: Option<u64>,
    since: Instant,
    /// Typing, clicking or scrolling hides the popup until the mouse moves.
    suppressed: bool,
}

impl Default for HoverState {
    fn default() -> Self {
        Self { target: None, help: None, generation: None, since: Instant::now(), suppressed: false }
    }
}

impl HoverState {
    pub(super) fn hide(&mut self) {
        self.target = None;
        self.help = None;
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
    /// Drawn on a tinted panel (examples, declarations, member lists).
    boxed: bool,
    /// A fact row: the label drawn in the label column, the text after it.
    label: Option<String>,
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
                let parsed = self.parsed_version == self.code.editor.version();
                let probe = match h {
                    Highlight::Keyword => Probe::Reference(r.for_keyword(src)?),
                    Highlight::Symbol => Probe::Reference(r.for_symbol(src)?),
                    // Only the comment delimiters, not the text inside.
                    Highlight::Comment if at_delimiter() => {
                        Probe::Reference(r.lookup(if src.starts_with("//") { "//" } else { "/* */" })?)
                    }
                    Highlight::Name | Highlight::Type | Highlight::Definition if parsed => Probe::Name(a),
                    _ => return None,
                };
                Some(Target { spot: Spot::Code(a, b), probe, anchor: rect })
            }
            Some(Card::Tree) => None,
            None if self.over_chrome(p) => None,
            None => {
                let pane = self.panes.diagram.filter(|&d| contains(d, p))?;
                let n = self.view.hovered?;
                let node = self.doc.diagram.nodes.get(n)?;
                let world = self.view.to_world(p, pane, self.dpi);
                let line = self.doc.diagram.line_at(n, world, &self.config.diagram);
                let fallback = line.and_then(|i| r.for_line(&node.lines[i])).or_else(|| r.for_kind(node.kind_label()));
                let h = 18.0 * self.dpi;
                Some(Target {
                    spot: Spot::Node(n, line.map_or(0, |i| i + 1)),
                    probe: Probe::Node(n, line, fallback),
                    anchor: [p[0], p[1], p[0] + 1.0, p[1] + h],
                })
            }
        }
    }

    /// Builds the help for a probe (resolving names, so not every frame).
    fn build_help(&self, probe: Probe) -> Option<Help> {
        let src = self.code.editor.text();
        let element = |h: Option<ElementHelp>| h.map(|h| Help::Element(Box::new(h)));
        match probe {
            Probe::Reference(i) => Some(Help::Reference(i)),
            Probe::Name(offset) => element(element_docs::at_offset(&self.doc, src, offset)),
            Probe::Node(n, line, fallback) => {
                let node = self.doc.diagram.nodes.get(n)?;
                let found = match line {
                    Some(i) => node.line_spans.get(i).and_then(|&s| element_docs::for_feature(&self.doc, src, &node.qualified_name, s)),
                    None => element_docs::for_qualified_name(&self.doc, src, &node.qualified_name),
                };
                element(found).or(fallback.map(Help::Reference))
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
            (Some(a), Some(b)) => a.spot == b.spot && a.probe == b.probe,
            (a, b) => a.is_none() && b.is_none(),
        };
        if !same {
            self.hover.target = next;
            self.hover.since = Instant::now();
            self.hover.help = None;
            self.hover.generation = None;
        }
        // Built once the mouse has rested, and again if the model changed meanwhile.
        if let Some(t) = self.hover.target
            && self.hover.since.elapsed() >= DELAY
            && self.hover.generation != Some(self.doc.generation)
        {
            self.hover.help = self.build_help(t.probe);
            self.hover.generation = Some(self.doc.generation);
        }
    }

    /// The help popup, above everything else (overlay pass).
    pub(super) fn draw_hover(&self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, window: [f32; 2], dpi: f32) {
        let (Some(t), Some(help)) = (self.hover.target, self.hover.help.as_ref()) else { return };
        if self.hover.suppressed || self.hover.since.elapsed() < DELAY {
            return;
        }
        let size = self.config.view.ui_font_size * dpi;
        let small = (size * 0.92).round();
        let pad = (10.0 * dpi).round();
        let margin = 8.0 * dpi;
        let max_w = (MAX_WIDTH * dpi).min(window[0] - 2.0 * margin - 2.0 * pad);
        if max_w < 60.0 * dpi {
            return;
        }
        let style = Style { fonts, theme, size, small, pad };
        let mut blocks = match help {
            Help::Reference(i) => reference_blocks(docs::reference(), *i, &style),
            Help::Element(e) => element_blocks(e, &style),
        };
        // Fact labels share one column.
        let label_w = blocks
            .iter()
            .filter_map(|b| b.label.as_deref())
            .map(|l| text.measure(fonts.body, small, l, None)[0])
            .fold(0.0f32, f32::max);
        let label_col = if label_w > 0.0 { (label_w + pad).ceil() } else { 0.0 };
        let mut w: f32 = 0.0;
        let mut h = 0.0;
        for b in &mut blocks {
            let inner = if b.boxed { 2.0 * pad * 0.6 } else { 0.0 };
            let indent = if b.label.is_some() { label_col } else { 0.0 };
            let limit = b.wrap.then_some(max_w - inner - indent);
            let [bw, bh] = text.measure(b.font, b.size, &b.text, limit);
            b.h = bh + inner;
            w = w.max(bw + inner + indent);
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
                if b.wrap {
                    text.draw_wrapped(style, [x + pad + inner, by + inner], w - 2.0 * pad - 2.0 * inner, &b.text);
                } else {
                    text.draw(style, [x + pad + inner, by + inner], &b.text);
                }
            } else if let Some(label) = &b.label {
                text.draw(TextStyle::new(fonts.body, small, theme.text_muted.0), [x + pad, by], label);
                text.draw_wrapped(style, [x + pad + label_col, by], w - 2.0 * pad - label_col, &b.text);
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

/// Fonts, colors and sizes the popup content is laid out with.
struct Style<'a> {
    fonts: Fonts,
    theme: &'a Theme,
    size: f32,
    small: f32,
    pad: f32,
}

impl Style<'_> {
    fn block(&self, font: FontId, size: f32, color: [f32; 4], text: String, top: f32) -> Block {
        Block { font, size, color, text, wrap: true, top, boxed: false, label: None, h: 0.0 }
    }

    fn para(&self) -> f32 {
        (self.pad * 0.7).round()
    }

    fn tone(&self, tone: Tone) -> [f32; 4] {
        match tone {
            Tone::Value => self.theme.text.0,
            Tone::Muted => self.theme.text_muted.0,
            Tone::Good => self.theme.success.0,
            Tone::Bad => self.theme.error.0,
        }
    }
}

/// `Library  definitions specialize Parts::Part, usages subset Parts::parts`.
fn library_line(e: &docs::Entry) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(d) = &e.library_def {
        parts.push(format!("definitions specialize {d}"));
    }
    if let Some(u) = &e.library_usage {
        parts.push(format!("usages subset {u}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The popup's content for a reference entry: names, category line,
/// summary, syntax, details, library types, example and related entries.
fn reference_blocks(r: &Reference, entry: usize, s: &Style) -> Vec<Block> {
    let e = r.entry(entry);
    let (fonts, theme) = (s.fonts, s.theme);
    let para = s.para();
    let mut out = vec![s.block(fonts.title, s.size, theme.syntax.keyword.0, e.names.join("  ·  "), 0.0)];
    let mut meta = format!("{} · {}", r.category_title(&e.category), e.kind);
    if let Some(spec) = &e.spec {
        meta += &format!(" · spec §{spec}");
    }
    out.push(s.block(fonts.body, s.small, theme.text_muted.0, meta, 2.0));
    out.push(s.block(fonts.body, s.size, theme.text.0, e.summary.clone(), para));
    for (i, syn) in e.syntax.iter().enumerate() {
        out.push(s.block(fonts.mono, s.small, theme.text.0, syn.clone(), if i == 0 { para } else { 2.0 }));
    }
    for p in e.paragraphs() {
        out.push(s.block(fonts.body, s.small, theme.text_muted.0, p, para));
    }
    if let Some(lib) = library_line(e) {
        out.push(Block { label: Some("Library".into()), ..s.block(fonts.mono, s.small, theme.text.0, lib, para) });
    }
    if let Some(ex) = e.example.as_deref().map(str::trim_end).filter(|s| !s.trim().is_empty()) {
        out.push(Block { wrap: false, boxed: true, ..s.block(fonts.mono, s.small, theme.text.0, ex.to_owned(), para) });
    }
    if !e.see.is_empty() {
        out.push(s.block(fonts.body, s.small, theme.text_muted.0, format!("See also: {}", e.see.join(", ")), para));
    }
    out
}

/// The popup's content for a model element: name, kind and location, the
/// declaration, facts, documentation, owned features and its keyword.
fn element_blocks(e: &ElementHelp, s: &Style) -> Vec<Block> {
    let (fonts, theme) = (s.fonts, s.theme);
    let para = s.para();
    let mut out = vec![s.block(fonts.title, s.size, theme.syntax.definition.0, e.title.clone(), 0.0)];
    out.push(s.block(fonts.body, s.small, theme.text_muted.0, e.meta.clone(), 2.0));
    if !e.signature.is_empty() {
        out.push(Block { boxed: true, ..s.block(fonts.mono, s.small, theme.text.0, e.signature.clone(), para) });
    }
    for (i, f) in e.facts.iter().enumerate() {
        let top = if i == 0 { para } else { 3.0 };
        out.push(Block { label: Some(f.label.to_owned()), ..s.block(fonts.mono, s.small, s.tone(f.tone), f.value.clone(), top) });
    }
    if !e.doc.is_empty() {
        if let Some(from) = &e.doc_from {
            out.push(s.block(fonts.body, s.small, theme.text_muted.0, format!("Documentation of {from}:"), para));
        }
        for (i, p) in e.doc.iter().enumerate() {
            let top = if i == 0 && e.doc_from.is_some() { 2.0 } else { para };
            out.push(s.block(fonts.body, s.size, theme.text.0, p.clone(), top));
        }
    }
    if !e.members.is_empty() {
        out.push(s.block(fonts.body, s.small, theme.text_muted.0, "Members".into(), para));
        out.push(Block { wrap: false, boxed: true, ..s.block(fonts.mono, s.small, theme.text.0, e.members.join("\n"), 2.0) });
    }
    if let Some(k) = e.keyword {
        let entry = docs::reference().entry(k);
        out.push(s.block(fonts.body, s.small, theme.text_muted.0, format!("{}: {}", entry.names[0], entry.summary), para));
    }
    out
}
