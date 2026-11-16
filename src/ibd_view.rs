//! The interconnection view pane: which element it shows, its camera,
//! selection and hover, and drawing the parts, ports and links of an [`Ibd`].

use crate::config::DiagramConfig;
use crate::diagram_view::{DiagramView, Fonts, TextMetrics};
use crate::ibd::{End, Ibd, LABEL_GAP, LinkKind, PORT, Side, midpoint, point_along};
use crate::shapes::{Rect, ShapeRenderer};
use crate::text::{TextRenderer, TextStyle};
use crate::theme::Theme;
use sysml::Direction;

#[derive(Default)]
pub struct IbdView {
    pub camera: DiagramView,
    pub ibd: Option<Ibd>,
    /// The element shown (qualified name); `None`: the best candidate.
    pub root: Option<String>,
    /// Elements that can be shown, best first.
    pub candidates: Vec<String>,
    /// Elements shown before the current one (double-click drills down).
    pub back: Vec<String>,
    /// Document generation and root the view was built from.
    pub built: Option<(u64, Option<String>)>,
    pub needs_layout: bool,
    pub selected: Option<End>,
    pub hovered: Option<End>,
}

impl IbdView {
    /// The element at a diagram point: a port, else the deepest part.
    pub fn hit(&self, world: [f32; 2]) -> Option<End> {
        let ibd = self.ibd.as_ref()?;
        ibd.port_at(world).map(End::Port).or_else(|| ibd.part_at(world).map(End::Part))
    }

    /// Qualified name of the declaration behind `end`.
    pub fn name_of(&self, end: End) -> Option<&str> {
        let ibd = self.ibd.as_ref()?;
        Some(match end {
            End::Part(p) => ibd.parts.get(p)?.qualified_name.as_str(),
            End::Port(p) => ibd.ports.get(p)?.qualified_name.as_str(),
        })
    }

    /// The part or port declared as `qualified_name`.
    pub fn find(&self, qualified_name: &str) -> Option<End> {
        let ibd = self.ibd.as_ref()?;
        ibd.ports
            .iter()
            .position(|p| p.qualified_name == qualified_name)
            .map(End::Port)
            .or_else(|| ibd.parts.iter().position(|p| p.qualified_name == qualified_name).map(End::Part))
    }

    /// The root shown: the chosen element, else the best candidate.
    pub fn current_root(&self) -> Option<&str> {
        self.root.as_deref().or(self.candidates.first().map(String::as_str))
    }

    /// Short label for the root chip: `drone1`.
    pub fn root_label(&self) -> String {
        match self.current_root() {
            Some(q) => q.rsplit("::").next().unwrap_or(q).to_string(),
            None => "nothing to show".to_string(),
        }
    }

    pub fn measure(&mut self, text: &mut TextRenderer, fonts: Fonts, cfg: &DiagramConfig, theme: &Theme) {
        if let Some(ibd) = &mut self.ibd {
            let mut m = TextMetrics { text, fonts, font_size: cfg.font_size, sizes: &theme.size.diagram };
            ibd.layout(&mut m);
        }
        self.needs_layout = false;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, cfg: &DiagramConfig, theme: &Theme, pane: Rect, dpi: f32) {
        let Some(ibd) = &self.ibd else { return };
        if self.camera.needs_fit {
            self.camera.fit_bounds(ibd.bounds(), pane, dpi, cfg);
        }
        let t = self.camera.transform(pane, dpi);
        let s = t.scale;
        shapes.begin_layer(t, Some(pane));
        if cfg.grid && cfg.grid_spacing > 0.0 {
            self.camera.draw_grid(shapes, cfg, theme, pane, dpi);
        }
        let ds = &theme.size.diagram;
        let colors = &theme.node;
        let radius = cfg.corner_radius.unwrap_or(ds.node_radius);
        let bw = cfg.border_width;
        let selected_part = self.selected.map(|e| ibd.end_part(e));

        // Parts, owners first so their children cover them.
        for (i, part) in ibd.parts.iter().enumerate() {
            let r = part.rect;
            let fill = match part.depth {
                0 => colors.fill.alpha(0.82),
                d if d % 2 == 1 => colors.header.0,
                _ => colors.fill.0,
            };
            if part.depth > 0 && colors.shadow.0[3] > 0.0 {
                shapes.shadow([r[0], r[1] + ds.shadow_offset, r[2], r[3] + ds.shadow_offset], colors.shadow.0, radius, ds.shadow_blur.max(1.0));
            }
            let (border, width) = if selected_part == Some(i) && matches!(self.selected, Some(End::Part(_))) {
                (theme.accent.0, bw * ds.selected_border_scale)
            } else if self.hovered == Some(End::Part(i)) {
                (colors.hover_border.0, bw)
            } else {
                (colors.border.0, bw)
            };
            if part.multiple {
                let o = 6.0;
                shapes.bordered_rect([r[0] + o, r[1] + o, r[2] + o, r[3] + o], fill, colors.border.0, radius, bw);
            }
            if part.is_ref {
                shapes.rect(r, fill, radius);
                let dash = ds.edge_dash.max(4.0);
                let (a, b, c, d) = ([r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]);
                for (p, q) in [(a, b), (b, c), (c, d), (d, a)] {
                    shapes.line(p, q, border, width, dash);
                }
            } else {
                shapes.bordered_rect(r, fill, border, radius, width);
            }
            let y = r[1] + part.header;
            shapes.line([r[0] + width, y], [r[2] - width, y], colors.separator.0, 1.0 / s, 0.0);
        }

        // Links, under the ports.
        let label_size = cfg.font_size * ds.label_scale;
        for link in &ibd.links {
            let base = match link.kind {
                LinkKind::Flow => theme.edge.flow,
                LinkKind::Binding => theme.edge.typing,
                LinkKind::Connection | LinkKind::Interface => theme.edge.connection,
            };
            let hot = selected_part.is_some_and(|p| ibd.touches(link.from, p) || ibd.touches(link.to, p));
            let color = if hot { theme.accent.0 } else { base.0 };
            let th = cfg.edge_thickness * if link.kind == LinkKind::Interface { 1.8 } else { 1.0 };
            let dash = if link.kind == LinkKind::Binding { ds.edge_dash.max(4.0) } else { 0.0 };
            for w in link.points.windows(2) {
                shapes.line(w[0], w[1], color, th, dash);
            }
            if link.kind == LinkKind::Flow
                && let Some((m, d)) = point_along(&link.points, 0.78)
            {
                let a = cfg.arrow_size;
                let n = [-d[1], d[0]];
                let back = |k: f32| [m[0] - d[0] * a + n[0] * a * k, m[1] - d[1] * a + n[1] * a * k];
                shapes.line(m, back(0.5), color, th * 2.2, 0.0);
                shapes.line(m, back(-0.5), color, th * 2.2, 0.0);
            }
            // A chip behind the label keeps it readable over lines and parts.
            if cfg.edge_labels
                && let (Some(label), Some((m, _))) = (&link.label, midpoint(&link.points))
            {
                let [w, h] = text.measure(fonts.body, label_size, label, None);
                let at = label_anchor(m, w, h, link.lane);
                shapes.rect([at[0] - 4.0, at[1] - 1.0, at[0] + w + 4.0, at[1] + h + 1.0], theme.bg.canvas.alpha(0.85), 4.0);
            }
        }

        // Ports straddle their owner's border.
        for (i, port) in ibd.ports.iter().enumerate() {
            let c = port.center;
            let h = PORT * 0.5;
            let r = [c[0] - h, c[1] - h, c[0] + h, c[1] + h];
            let fg = theme.kind.usage.fg;
            let hot = self.hovered == Some(End::Port(i)) || self.selected == Some(End::Port(i));
            let border = if hot { theme.accent.0 } else { fg.0 };
            let (fill, glyph) = if port.conjugated { (colors.fill.0, fg.0) } else { (fg.0, colors.fill.0) };
            shapes.bordered_rect(r, fill, border, 2.0, bw.max(1.0) * if hot { 1.6 } else { 1.0 });
            // Arrow for the direction of what flows through it.
            let out = port.side_normal();
            let dir = match port.direction {
                Some(Direction::In) => Some([-out[0], -out[1]]),
                Some(Direction::Out) => Some(out),
                _ => None,
            };
            if let Some(d) = dir {
                let k = h * 0.55;
                let tip = [c[0] + d[0] * k, c[1] + d[1] * k];
                let n = [-d[1], d[0]];
                let tail = |sgn: f32| [c[0] - d[0] * k * 0.6 + n[0] * k * sgn, c[1] - d[1] * k * 0.6 + n[1] * k * sgn];
                shapes.line(tail(1.0), tip, glyph, 1.4, 0.0);
                shapes.line(tail(-1.0), tip, glyph, 1.4, 0.0);
            }
        }

        // ---- text, after every shape ----
        let to_screen = |p: [f32; 2]| [p[0] * s + t.offset[0], p[1] * s + t.offset[1]];
        let title_size = cfg.font_size * ds.name_scale;
        let kw_size = cfg.font_size * ds.keyword_scale;
        let port_size = cfg.font_size * ds.compartment_scale;
        if port_size * s < 3.0 {
            text.set_clip(None);
            return;
        }
        let pad = 16.0;
        for part in &ibd.parts {
            let r = part.rect;
            let [x0, y0] = to_screen([r[0], r[1]]);
            let [x1, y1] = to_screen([r[2], r[3]]);
            text.set_clip(Some(intersect(pane, [x0, y0, x1, y1])));
            let lh = text.line_metrics(fonts.title, title_size).line_height;
            let ty = r[1] + (part.header - lh) * 0.5;
            text.draw(TextStyle::new(fonts.title, title_size * s, colors.name.0), to_screen([r[0] + pad, ty]), &part.title);
            let kw = format!("«{}»", part.keyword);
            let kw_w = text.measure(fonts.body, kw_size, &kw, None)[0];
            let klh = text.line_metrics(fonts.body, kw_size).line_height;
            let name_w = text.measure(fonts.title, title_size, &part.title, None)[0];
            if r[0] + pad + name_w + 12.0 < r[2] - pad - kw_w {
                text.draw(TextStyle::new(fonts.body, kw_size * s, theme.text_muted.0), to_screen([r[2] - pad - kw_w, r[1] + (part.header - klh) * 0.5]), &kw);
            }
            if part.truncated {
                let plh = text.line_metrics(fonts.mono, port_size).line_height;
                text.draw(TextStyle::new(fonts.mono, port_size * s, theme.text_muted.0), to_screen([r[0] + pad, r[3] - plh - 8.0]), "… inner parts not shown");
            }
        }
        text.set_clip(Some(pane));
        let plh = text.line_metrics(fonts.mono, port_size).line_height;
        for port in &ibd.ports {
            let c = port.center;
            let w = port.label_width;
            let off = PORT * 0.5 + LABEL_GAP;
            let at = match port.side {
                Side::Left => [c[0] + off, c[1] - plh * 0.5],
                Side::Right => [c[0] - off - w, c[1] - plh * 0.5],
                Side::Top => [c[0] - w * 0.5, c[1] + off],
                Side::Bottom => [c[0] - w * 0.5, c[1] - off - plh],
            };
            text.draw(TextStyle::new(fonts.mono, port_size * s, colors.compartment.0), to_screen(at), &port.label);
        }
        if cfg.edge_labels {
            for link in &ibd.links {
                let (Some(label), Some((m, _))) = (&link.label, midpoint(&link.points)) else { continue };
                let [w, h] = text.measure(fonts.body, label_size, label, None);
                let at = label_anchor(m, w, h, link.lane);
                text.draw(TextStyle::new(fonts.body, label_size * s, theme.edge.label.0), to_screen(at), label);
            }
        }
        text.set_clip(None);
    }
}

/// Top-left of a link label.
fn label_anchor(m: [f32; 2], w: f32, h: f32, lane: f32) -> [f32; 2] {
    // Centered on the link, parallel links stacked.
    [m[0] - w * 0.5, m[1] - h * 0.5 + lane * (h + 6.0)]
}

fn intersect(a: Rect, b: Rect) -> Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

impl crate::ibd::Port {
    fn side_normal(&self) -> [f32; 2] {
        match self.side {
            Side::Left => [-1.0, 0.0],
            Side::Right => [1.0, 0.0],
            Side::Top => [0.0, -1.0],
            Side::Bottom => [0.0, 1.0],
        }
    }
}
