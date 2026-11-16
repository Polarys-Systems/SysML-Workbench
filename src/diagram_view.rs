//! Interactive diagram pane: camera, hit testing, dragging and drawing.

use crate::config::DiagramConfig;
use crate::theme::{Color, DiagramSizes, KindPair, Theme};
use crate::document::Tone;
use sysml::eval::Verdict;
use crate::diagram::{Diagram, EdgeKind, Metrics, Node, NodeStyle, PILL_GAP, PILL_PAD, TextRole};
use crate::shapes::{Rect, ShapeRenderer, Transform};
use crate::text::{FontId, TextRenderer, TextStyle};

/// Fonts used by the panes; missing faces fall back to one that loaded.
#[derive(Clone, Copy)]
pub struct Fonts {
    pub title: FontId,
    pub body: FontId,
    pub mono: FontId,
}

impl Fonts {
    fn for_role(&self, role: TextRole) -> FontId {
        match role {
            TextRole::Name => self.title,
            TextRole::Keyword => self.body,
            TextRole::Compartment => self.mono,
        }
    }
}

fn role_scale(sizes: &DiagramSizes, role: TextRole) -> f32 {
    match role {
        TextRole::Keyword => sizes.keyword_scale,
        TextRole::Name => sizes.name_scale,
        TextRole::Compartment => sizes.compartment_scale,
    }
}

/// Measures diagram text at zoom 1 (diagram units = logical pixels).
pub struct TextMetrics<'a> {
    pub text: &'a mut TextRenderer,
    pub fonts: Fonts,
    pub font_size: f32,
    pub sizes: &'a DiagramSizes,
}

impl Metrics for TextMetrics<'_> {
    fn width(&mut self, role: TextRole, s: &str) -> f32 {
        self.text.measure(self.fonts.for_role(role), self.font_size * role_scale(self.sizes, role), s, None)[0]
    }

    fn line_height(&mut self, role: TextRole) -> f32 {
        self.text.line_metrics(self.fonts.for_role(role), self.font_size * role_scale(self.sizes, role)).line_height
    }
}

enum Drag {
    Pan { last: [f32; 2] },
    Node { node: usize, grab: [f32; 2], moved: bool },
}

pub struct DiagramView {
    pub zoom: f32,
    /// Screen offset of the diagram origin from the pane's top-left corner.
    pub pan: [f32; 2],
    pub selected: Option<usize>,
    pub hovered: Option<usize>,
    drag: Option<Drag>,
    /// Fit the diagram to the pane on the next draw.
    pub needs_fit: bool,
    /// The part of the pane not covered by floating panels (screen pixels):
    /// fitting and centering aim at it. `None`: the whole pane.
    pub focus: Option<Rect>,
}

impl Default for DiagramView {
    fn default() -> Self {
        Self { zoom: 1.0, pan: [0.0, 0.0], selected: None, hovered: None, drag: None, needs_fit: true, focus: None }
    }
}

pub fn contains(r: Rect, p: [f32; 2]) -> bool {
    p[0] >= r[0] && p[0] < r[2] && p[1] >= r[1] && p[1] < r[3]
}

fn intersect(a: Rect, b: Rect) -> Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

impl DiagramView {
    pub fn transform(&self, pane: Rect, dpi: f32) -> Transform {
        Transform { scale: self.zoom * dpi, offset: [pane[0] + self.pan[0], pane[1] + self.pan[1]] }
    }

    pub fn to_world(&self, p: [f32; 2], pane: Rect, dpi: f32) -> [f32; 2] {
        let t = self.transform(pane, dpi);
        [(p[0] - t.offset[0]) / t.scale, (p[1] - t.offset[1]) / t.scale]
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Zooms and pans so the whole diagram fits the pane.
    pub fn fit(&mut self, diagram: &Diagram, pane: Rect, dpi: f32, cfg: &DiagramConfig) {
        self.fit_bounds(diagram.bounds(), pane, dpi, cfg);
    }

    /// Zooms and pans so `bounds` (diagram units) fits the pane.
    pub fn fit_bounds(&mut self, bounds: Option<Rect>, pane: Rect, dpi: f32, cfg: &DiagramConfig) {
        self.needs_fit = false;
        let Some(b) = bounds else {
            self.zoom = 1.0;
            self.center_on_point([0.0, 0.0], pane, dpi);
            return;
        };
        let area = self.focus_in(pane);
        let margin = 40.0 * dpi;
        let (pw, ph) = ((area[2] - area[0] - 2.0 * margin).max(1.0), (area[3] - area[1] - 2.0 * margin).max(1.0));
        let (bw, bh) = ((b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0));
        self.zoom = ((pw / bw).min(ph / bh) / dpi).clamp(cfg.min_zoom, cfg.max_zoom.min(1.5));
        self.center_on_point([(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5], pane, dpi);
    }

    /// Where fitting and centering aim: the focus area, or the pane.
    pub fn focus_in(&self, pane: Rect) -> Rect {
        self.focus.map(|f| intersect(f, pane)).filter(|f| f[2] > f[0] && f[3] > f[1]).unwrap_or(pane)
    }

    /// Pans so `world` sits at the center of the focus area.
    pub fn center_on_point(&mut self, world: [f32; 2], pane: Rect, dpi: f32) {
        let s = self.zoom * dpi;
        let a = self.focus_in(pane);
        let c = [(a[0] + a[2]) * 0.5 - pane[0], (a[1] + a[3]) * 0.5 - pane[1]];
        self.pan = [c[0] - world[0] * s, c[1] - world[1] * s];
    }

    /// Zooms by `factor`, keeping the diagram point under `screen` fixed.
    pub fn zoom_at(&mut self, screen: [f32; 2], factor: f32, pane: Rect, dpi: f32, cfg: &DiagramConfig) {
        let world = self.to_world(screen, pane, dpi);
        self.zoom = (self.zoom * factor).clamp(cfg.min_zoom, cfg.max_zoom);
        let s = self.zoom * dpi;
        self.pan = [screen[0] - pane[0] - world[0] * s, screen[1] - pane[1] - world[1] * s];
    }

    /// The topmost node at a diagram point (the selected node draws on top).
    pub fn node_at(&self, diagram: &Diagram, world: [f32; 2]) -> Option<usize> {
        if let Some(s) = self.selected
            && diagram.nodes.get(s).is_some_and(|n| contains(n.rect(), world))
        {
            return Some(s);
        }
        diagram.nodes.iter().rposition(|n| contains(n.rect(), world))
    }

    /// Starts panning from screen point `p`.
    pub fn press_pan(&mut self, p: [f32; 2]) {
        self.drag = Some(Drag::Pan { last: p });
    }

    /// Pans while a pan drag is in progress; false when there is none.
    pub fn pan_motion(&mut self, p: [f32; 2]) -> bool {
        match &mut self.drag {
            Some(Drag::Pan { last }) => {
                self.pan[0] += p[0] - last[0];
                self.pan[1] += p[1] - last[1];
                *last = p;
                true
            }
            _ => false,
        }
    }

    pub fn press(&mut self, diagram: &Diagram, p: [f32; 2], pane: Rect, dpi: f32, pan_only: bool) {
        let world = self.to_world(p, pane, dpi);
        match self.node_at(diagram, world).filter(|_| !pan_only) {
            Some(node) => {
                self.selected = Some(node);
                let pos = diagram.nodes[node].pos;
                self.drag = Some(Drag::Node { node, grab: [world[0] - pos[0], world[1] - pos[1]], moved: false });
            }
            None => {
                if !pan_only {
                    self.selected = None;
                }
                self.drag = Some(Drag::Pan { last: p });
            }
        }
    }

    pub fn motion(&mut self, diagram: &mut Diagram, p: [f32; 2], pane: Rect, dpi: f32, cfg: &DiagramConfig) {
        let world = self.to_world(p, pane, dpi);
        match &mut self.drag {
            Some(Drag::Pan { last }) => {
                self.pan[0] += p[0] - last[0];
                self.pan[1] += p[1] - last[1];
                *last = p;
            }
            Some(Drag::Node { node, grab, moved }) => {
                let mut pos = [world[0] - grab[0], world[1] - grab[1]];
                if cfg.snap && cfg.grid_spacing > 0.0 {
                    pos = pos.map(|v| (v / cfg.grid_spacing).round() * cfg.grid_spacing);
                }
                let n = &mut diagram.nodes[*node];
                if n.pos != pos {
                    n.pos = pos;
                    n.pinned = true;
                    *moved = true;
                }
            }
            None => self.hovered = contains(pane, p).then(|| self.node_at(diagram, world)).flatten(),
        }
    }

    /// Ends a drag; true if a node was moved (positions should be saved).
    pub fn release(&mut self) -> bool {
        matches!(self.drag.take(), Some(Drag::Node { moved: true, .. }))
    }

    // ---- drawing ---------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        diagram: &Diagram,
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        cfg: &DiagramConfig,
        theme: &Theme,
        pane: Rect,
        dpi: f32,
    ) {
        if self.needs_fit {
            self.fit(diagram, pane, dpi, cfg);
        }
        let t = self.transform(pane, dpi);
        let s = t.scale;
        shapes.begin_layer(t, Some(pane));
        if cfg.grid && cfg.grid_spacing > 0.0 {
            self.draw_grid(shapes, cfg, theme, pane, dpi);
        }

        for e in diagram.edges.iter().filter(|e| e.kind.visible(&cfg.edges)) {
            let (a, b) = (&diagram.nodes[e.from], &diagram.nodes[e.to]);
            let color = match e.verdict {
                Some(Verdict::Satisfied) => theme.success,
                Some(Verdict::Violated | Verdict::Error) => theme.error,
                _ => edge_color(theme, e.kind),
            };
            let highlight = self.selected.is_some_and(|sel| sel == e.from || sel == e.to);
            let color = if highlight { theme.accent.0 } else { color.0 };
            draw_edge(shapes, a, b, e.kind, color, cfg, theme.size.diagram.edge_dash);
        }

        let order = (0..diagram.nodes.len()).filter(|&i| Some(i) != self.selected).chain(self.selected);
        let order: Vec<usize> = order.filter(|&i| i < diagram.nodes.len()).collect();
        let metrics_cache: Vec<_> = {
            let mut m = TextMetrics { text: &mut *text, fonts, font_size: cfg.font_size, sizes: &theme.size.diagram };
            order.iter().map(|&i| Diagram::node_layout(&diagram.nodes[i], cfg, &mut m)).collect()
        };
        let colors = &theme.node;
        let ds = &theme.size.diagram;
        let radius = cfg.corner_radius.unwrap_or(ds.node_radius);
        for (k, &i) in order.iter().enumerate() {
            let node = &diagram.nodes[i];
            let layout = &metrics_cache[k];
            let r = node.rect();
            let selected = Some(i) == self.selected;
            if colors.shadow.0[3] > 0.0 {
                let shadow = [r[0], r[1] + ds.shadow_offset, r[2], r[3] + ds.shadow_offset];
                if ds.shadow_blur > 0.0 {
                    shapes.shadow(shadow, colors.shadow.0, radius, ds.shadow_blur);
                } else {
                    shapes.rect(shadow, colors.shadow.0, radius);
                }
            }
            // Selection: a soft accent halo around an accent border.
            if selected && ds.selection_ring > 0.0 {
                let g = ds.selection_ring;
                let halo = [r[0] - g, r[1] - g, r[2] + g, r[3] + g];
                shapes.rect(halo, theme.accent.alpha(ds.selection_ring_alpha), radius + g);
            }
            let (border, width) = if selected {
                (theme.accent.0, cfg.border_width * ds.selected_border_scale)
            } else if Some(i) == self.hovered {
                (colors.hover_border.0, cfg.border_width)
            } else {
                match node.verdict {
                    Some(Verdict::Satisfied) => (theme.success.0, cfg.border_width * ds.selected_border_scale),
                    Some(Verdict::Violated | Verdict::Error) => (theme.error.0, cfg.border_width * ds.selected_border_scale),
                    _ => (colors.border.0, cfg.border_width),
                }
            };
            // Card: the border and header color fill it all, then the body covers
            // everything below the header (square on top, rounded at the bottom).
            let header = if node.external.is_some() { colors.header.alpha(0.45) } else { colors.header.0 };
            shapes.bordered_rect(r, header, border, radius, width);
            if let Some(sep) = layout.separator_y {
                let inset = width.max(1.0 / s);
                let top = r[1] + sep;
                let body = [r[0] + inset, top, r[2] - inset, r[3] - inset];
                if body[3] > body[1] {
                    shapes.rect(body, colors.fill.0, (radius - inset).max(0.0));
                    shapes.rect([body[0], top, body[2], (top + radius).min(body[3])], colors.fill.0, 0.0);
                    shapes.line([body[0], top], [body[2], top], colors.separator.0, 1.0 / s, 0.0);
                    // A second rule between the feature list and the computed values.
                    let (lines, more) = Diagram::visible_lines(node, cfg);
                    if (!lines.is_empty() || more) && !Diagram::visible_values(node, cfg).is_empty() {
                        let y = r[1] + layout.values_y - cfg.node_padding * 0.5;
                        shapes.line([body[0] + cfg.node_padding, y], [body[2] - cfg.node_padding, y], colors.separator.0, 1.0 / s, 0.0);
                    }
                }
            }
            // Kind pill, right-aligned in the header row.
            let pill_w = node.title_widths[0] + 2.0 * PILL_PAD[0];
            let px = (r[2] - cfg.node_padding - pill_w).max(r[0] + cfg.node_padding);
            let pill = [px, r[1] + layout.pill_y, (px + pill_w).min(r[2] - cfg.node_padding), r[1] + layout.pill_y + layout.pill_h];
            shapes.rect(pill, kind_colors(theme, node.style).bg.0, layout.pill_h * 0.5);
        }

        // Text: drawn after every shape, clipped to the pane and its node.
        let to_screen = |p: [f32; 2]| [p[0] * s + t.offset[0], p[1] * s + t.offset[1]];
        let size = |role| cfg.font_size * role_scale(&theme.size.diagram, role) * s;
        if size(TextRole::Compartment) < 3.0 {
            return;
        }
        for (k, &i) in order.iter().enumerate() {
            let node = &diagram.nodes[i];
            let layout = &metrics_cache[k];
            let r = node.rect();
            let [x0, y0] = to_screen([r[0], r[1]]);
            let [x1, y1] = to_screen([r[2], r[3]]);
            let pad = cfg.node_padding;
            let pill_w = node.title_widths[0] + 2.0 * PILL_PAD[0];
            let px = (r[2] - pad - pill_w).max(r[0] + pad);
            // Name, left-aligned, stopping before the pill.
            text.set_clip(Some(intersect(pane, [x0 + pad * 0.5 * s, y0, to_screen([px - PILL_GAP * 0.5, 0.0])[0], y1])));
            let nm = to_screen([r[0] + pad, r[1] + layout.name_y]);
            let name_color = if node.external.is_some() { theme.text_muted.0 } else { theme.node.name.0 };
            text.draw(TextStyle::new(fonts.title, size(TextRole::Name), name_color), nm, &node.name);
            // Kind label, centered in its pill.
            let kw_h = layout.pill_h - 2.0 * PILL_PAD[1];
            text.set_clip(Some(intersect(pane, [x0, y0, x1 - pad * 0.5 * s, y1])));
            let kw = to_screen([px + PILL_PAD[0], r[1] + layout.pill_y + (layout.pill_h - kw_h) * 0.5]);
            let fg = kind_colors(theme, node.style).fg.0;
            text.draw(TextStyle::new(fonts.body, size(TextRole::Keyword), fg), kw, node.kind_label());
            // Compartments.
            text.set_clip(Some(intersect(pane, [x0 + pad * 0.5 * s, y0, x1 - pad * 0.5 * s, y1])));
            let (lines, more) = Diagram::visible_lines(node, cfg);
            let style = TextStyle::new(fonts.mono, size(TextRole::Compartment), theme.node.compartment.0);
            let mut y = r[1] + layout.first_line_y;
            for line in lines.iter().map(String::as_str).chain(more.then_some("…")) {
                text.draw(style, to_screen([r[0] + pad, y]), line);
                y += layout.line_height;
            }
            let mut y = r[1] + layout.values_y;
            for (value, tone) in Diagram::visible_values(node, cfg) {
                let color = match tone {
                    Tone::Value => theme.node.name.0,
                    Tone::Muted => theme.text_muted.0,
                    Tone::Good => theme.success.0,
                    Tone::Bad => theme.error.0,
                };
                text.draw(TextStyle::new(fonts.mono, size(TextRole::Compartment), color), to_screen([r[0] + pad, y]), value);
                y += layout.line_height;
            }
        }

        if cfg.edge_labels {
            text.set_clip(Some(pane));
            let style = TextStyle::new(fonts.body, cfg.font_size * theme.size.diagram.label_scale * s, theme.edge.label.0);
            for e in diagram.edges.iter().filter(|e| e.kind.visible(&cfg.edges)) {
                let Some(label) = &e.label else { continue };
                let (a, b) = (&diagram.nodes[e.from], &diagram.nodes[e.to]);
                let Some((pa, pb)) = endpoints(a, b) else { continue };
                let at = if e.kind == EdgeKind::Composition { 0.7 } else { 0.5 };
                let anchor = [pa[0] + (pb[0] - pa[0]) * at, pa[1] + (pb[1] - pa[1]) * at];
                // Text draws above every node, so leave out labels that would cover one.
                let label_box = [anchor[0], anchor[1] - cfg.font_size, anchor[0] + 6.0 * label.len() as f32, anchor[1]];
                let covers = |n: &Node| {
                    let r = n.rect();
                    label_box[0] < r[2] && label_box[2] > r[0] && label_box[1] < r[3] && label_box[3] > r[1]
                };
                if diagram.nodes.iter().any(covers) {
                    continue;
                }
                text.draw(style, to_screen([anchor[0] + 6.0, anchor[1] - cfg.font_size]), label);
            }
        }
        text.set_clip(None);
    }

    pub fn draw_grid(&self, shapes: &mut ShapeRenderer, cfg: &DiagramConfig, theme: &Theme, pane: Rect, dpi: f32) {
        let s = self.zoom * dpi;
        let [x0, y0] = self.to_world([pane[0], pane[1]], pane, dpi);
        let [x1, y1] = self.to_world([pane[2], pane[3]], pane, dpi);
        let mut step = cfg.grid_spacing;
        while step * s < theme.size.diagram.grid_min_spacing {
            step *= 2.0;
        }
        let color = theme.grid.0;
        let dot = theme.size.diagram.grid_dot * dpi;
        if dot > 0.0 {
            // Dots at the grid crossings, a fixed size on screen.
            let r = dot / s;
            let mut y = (y0 / step).floor() * step;
            while y <= y1 {
                let mut x = (x0 / step).floor() * step;
                while x <= x1 {
                    shapes.rect([x - r, y - r, x + r, y + r], color, r);
                    x += step;
                }
                y += step;
            }
            return;
        }
        let mut x = (x0 / step).floor() * step;
        while x <= x1 {
            shapes.line([x, y0], [x, y1], color, 1.0 / s, 0.0);
            x += step;
        }
        let mut y = (y0 / step).floor() * step;
        while y <= y1 {
            shapes.line([x0, y], [x1, y], color, 1.0 / s, 0.0);
            y += step;
        }
    }
}

fn kind_colors(theme: &Theme, style: NodeStyle) -> KindPair {
    let k = &theme.kind;
    match style {
        NodeStyle::Definition => k.definition,
        NodeStyle::Usage => k.usage,
        NodeStyle::Behavior => k.behavior,
        NodeStyle::Requirement => k.requirement,
    }
}

fn edge_color(theme: &Theme, kind: EdgeKind) -> Color {
    let e = &theme.edge;
    match kind {
        EdgeKind::Specialization => e.specialization,
        EdgeKind::Typing => e.typing,
        EdgeKind::Composition => e.composition,
        EdgeKind::Satisfy => e.satisfy,
        EdgeKind::Dependency => e.dependency,
        EdgeKind::Connection => e.connection,
        EdgeKind::Flow => e.flow,
        EdgeKind::Allocation => e.allocation,
    }
}

/// Where the segment from `from` towards `to` leaves `rect`.
fn border_point(rect: Rect, from: [f32; 2], to: [f32; 2]) -> [f32; 2] {
    let d = [to[0] - from[0], to[1] - from[1]];
    let (hx, hy) = ((rect[2] - rect[0]) * 0.5, (rect[3] - rect[1]) * 0.5);
    let tx = if d[0].abs() > 1e-6 { hx / d[0].abs() } else { f32::INFINITY };
    let ty = if d[1].abs() > 1e-6 { hy / d[1].abs() } else { f32::INFINITY };
    let t = tx.min(ty).min(1.0);
    [from[0] + d[0] * t, from[1] + d[1] * t]
}

/// Edge endpoints on the two nodes' borders; `None` if they overlap.
fn endpoints(a: &Node, b: &Node) -> Option<([f32; 2], [f32; 2])> {
    let (ca, cb) = (a.center(), b.center());
    let pa = border_point(a.rect(), ca, cb);
    let pb = border_point(b.rect(), cb, ca);
    let d = [pb[0] - pa[0], pb[1] - pa[1]];
    // Overlapping nodes: the border points cross over.
    let forward = d[0] * (cb[0] - ca[0]) + d[1] * (cb[1] - ca[1]);
    (forward > 1.0).then_some((pa, pb))
}

fn draw_edge(shapes: &mut ShapeRenderer, a: &Node, b: &Node, kind: EdgeKind, color: [f32; 4], cfg: &DiagramConfig, dash: f32) {
    let Some((pa, pb)) = endpoints(a, b) else { return };
    let len = ((pb[0] - pa[0]).powi(2) + (pb[1] - pa[1]).powi(2)).sqrt();
    let dir = [(pb[0] - pa[0]) / len, (pb[1] - pa[1]) / len];
    let nrm = [-dir[1], dir[0]];
    let th = cfg.edge_thickness;
    let size = cfg.arrow_size;
    let at = |p: [f32; 2], along: f32, across: f32| {
        [p[0] + dir[0] * along + nrm[0] * across, p[1] + dir[1] * along + nrm[1] * across]
    };
    let open_arrow = |shapes: &mut ShapeRenderer| {
        shapes.line(pb, at(pb, -size, size * 0.55), color, th, 0.0);
        shapes.line(pb, at(pb, -size, -size * 0.55), color, th, 0.0);
    };
    match kind {
        EdgeKind::Specialization => {
            // Hollow triangle pointing at the general element.
            let base = at(pb, -size * 1.2, 0.0);
            let (l, r) = (at(pb, -size * 1.2, size * 0.7), at(pb, -size * 1.2, -size * 0.7));
            shapes.line(pa, base, color, th, 0.0);
            shapes.line(l, r, color, th, 0.0);
            shapes.line(l, pb, color, th, 0.0);
            shapes.line(r, pb, color, th, 0.0);
        }
        EdgeKind::Composition => {
            // Diamond on the owner's side.
            let far = at(pa, size * 2.0, 0.0);
            let (l, r) = (at(pa, size, size * 0.55), at(pa, size, -size * 0.55));
            for (p, q) in [(pa, l), (l, far), (far, r), (r, pa)] {
                shapes.line(p, q, color, th, 0.0);
            }
            shapes.line(far, pb, color, th, 0.0);
            open_arrow(shapes);
        }
        EdgeKind::Connection => {
            // Plain solid line with a small end dot on each side.
            shapes.line(pa, pb, color, th * 1.3, 0.0);
            let r = size * 0.3;
            for p in [pa, pb] {
                shapes.rect([p[0] - r, p[1] - r, p[0] + r, p[1] + r], color, r);
            }
        }
        EdgeKind::Flow => {
            // Solid line with a filled arrowhead (two strokes, thick).
            shapes.line(pa, pb, color, th, 0.0);
            shapes.line(pb, at(pb, -size, size * 0.5), color, th * 2.2, 0.0);
            shapes.line(pb, at(pb, -size, -size * 0.5), color, th * 2.2, 0.0);
        }
        EdgeKind::Typing | EdgeKind::Satisfy | EdgeKind::Dependency | EdgeKind::Allocation => {
            shapes.line(pa, pb, color, th, dash);
            open_arrow(shapes);
        }
    }
}
