//! The state machine view pane: which element it shows, its camera,
//! selection and hover, and drawing the states and transitions of a
//! [`StateMachine`].

use crate::config::DiagramConfig;
use crate::diagram_view::{DiagramView, Fonts, TextMetrics};
use crate::shapes::{Rect, ShapeRenderer};
use crate::states::{Hit, PSEUDO, StateKind, StateMachine};
use crate::text::{TextRenderer, TextStyle};
use crate::theme::Theme;

/// How close (in screen pixels) a click must be to a transition line.
const HIT_SLOP: f32 = 6.0;

#[derive(Default)]
pub struct StateView {
    pub camera: DiagramView,
    pub machine: Option<StateMachine>,
    /// The element shown (qualified name); `None`: the best candidate.
    pub root: Option<String>,
    /// Elements that can be shown, best first.
    pub candidates: Vec<String>,
    /// Elements shown before the current one (double-click drills down).
    pub back: Vec<String>,
    /// Document generation and root the view was built from.
    pub built: Option<(u64, Option<String>)>,
    pub needs_layout: bool,
    pub selected: Option<Hit>,
    pub hovered: Option<Hit>,
}

impl StateView {
    pub fn hit(&self, world: [f32; 2]) -> Option<Hit> {
        let tolerance = HIT_SLOP / self.camera.zoom.max(0.05);
        self.machine.as_ref()?.hit(world, tolerance)
    }

    /// Qualified name of the declaration behind `hit`; a transition without
    /// a name gives none.
    pub fn name_of(&self, hit: Hit) -> Option<&str> {
        let sm = self.machine.as_ref()?;
        match hit {
            Hit::State(s) => sm.states.get(s)?.qualified_name.as_deref(),
            Hit::Transition(t) => sm.transitions.get(t)?.qualified_name.as_deref(),
        }
    }

    pub fn find(&self, qualified_name: &str) -> Option<Hit> {
        let sm = self.machine.as_ref()?;
        let q = Some(qualified_name);
        sm.transitions
            .iter()
            .position(|t| t.qualified_name.as_deref() == q)
            .map(Hit::Transition)
            .or_else(|| sm.states.iter().position(|s| s.qualified_name.as_deref() == q).map(Hit::State))
    }

    pub fn current_root(&self) -> Option<&str> {
        self.root.as_deref().or(self.candidates.first().map(String::as_str))
    }

    pub fn root_label(&self) -> String {
        match self.current_root() {
            Some(q) => q.rsplit("::").next().unwrap_or(q).to_string(),
            None => "nothing to show".to_string(),
        }
    }

    pub fn measure(&mut self, text: &mut TextRenderer, fonts: Fonts, cfg: &DiagramConfig, theme: &Theme) {
        if let Some(sm) = &mut self.machine {
            let mut m = TextMetrics { text, fonts, font_size: cfg.font_size, sizes: &theme.size.diagram };
            sm.layout(&mut m);
        }
        self.needs_layout = false;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, cfg: &DiagramConfig, theme: &Theme, pane: Rect, dpi: f32) {
        let Some(sm) = &self.machine else { return };
        if self.camera.needs_fit {
            self.camera.fit_bounds(sm.bounds(), pane, dpi, cfg);
        }
        let t = self.camera.transform(pane, dpi);
        let s = t.scale;
        shapes.begin_layer(t, Some(pane));
        if cfg.grid && cfg.grid_spacing > 0.0 {
            self.camera.draw_grid(shapes, cfg, theme, pane, dpi);
        }
        let ds = &theme.size.diagram;
        let colors = &theme.node;
        let bw = cfg.border_width;
        let radius = cfg.corner_radius.unwrap_or(ds.node_radius).max(10.0);
        let selected_state = match self.selected {
            Some(Hit::State(i)) => Some(i),
            _ => None,
        };

        // States, owners first so what they hold covers them.
        for (i, st) in sm.states.iter().enumerate() {
            let r = st.rect;
            let hot = selected_state == Some(i);
            let hover = self.hovered == Some(Hit::State(i));
            match st.kind {
                StateKind::Initial => {
                    let c = if hot { theme.accent.0 } else { colors.name.0 };
                    shapes.rect(r, c, PSEUDO * 0.5);
                    continue;
                }
                StateKind::Final => {
                    let c = if hot { theme.accent.0 } else { colors.name.0 };
                    shapes.bordered_rect(r, theme.bg.canvas.0, c, PSEUDO * 0.5, 1.6);
                    let k = 4.5;
                    shapes.rect([r[0] + k, r[1] + k, r[2] - k, r[3] - k], c, PSEUDO * 0.5 - k);
                    continue;
                }
                _ => {}
            }
            let fill = match st.depth {
                0 => colors.fill.alpha(0.82),
                d if d % 2 == 1 => colors.header.0,
                _ => colors.fill.0,
            };
            if st.depth > 0 && colors.shadow.0[3] > 0.0 {
                shapes.shadow([r[0], r[1] + ds.shadow_offset, r[2], r[3] + ds.shadow_offset], colors.shadow.0, radius, ds.shadow_blur.max(1.0));
            }
            let (border, width) = if hot {
                (theme.accent.0, bw * ds.selected_border_scale)
            } else if hover {
                (colors.hover_border.0, bw)
            } else {
                (colors.border.0, bw)
            };
            shapes.bordered_rect(r, fill, border, radius, width);
            if st.header < r[3] - r[1] - 0.5 {
                let y = r[1] + st.header;
                shapes.line([r[0] + width, y], [r[2] - width, y], colors.separator.0, 1.0 / s, 0.0);
            }
            // Concurrent regions are divided by dashed lines.
            if st.parallel {
                for w in st.children.windows(2) {
                    let y = (sm.states[w[0]].rect[3] + sm.states[w[1]].rect[1]) * 0.5;
                    shapes.line([r[0] + 8.0, y], [r[2] - 8.0, y], colors.border.0, bw, ds.edge_dash.max(5.0));
                }
            }
        }

        // Transitions, with an arrowhead at the target.
        let label_size = cfg.font_size * ds.label_scale;
        let th = cfg.edge_thickness;
        for (k, tr) in sm.transitions.iter().enumerate() {
            let hot = self.selected == Some(Hit::Transition(k))
                || selected_state.is_some_and(|sel| sm.is_within(tr.from, sel) || sm.is_within(tr.to, sel));
            let hover = self.hovered == Some(Hit::Transition(k));
            let color = if hot {
                theme.accent.0
            } else if hover {
                colors.hover_border.0
            } else {
                theme.edge.composition.0
            };
            for w in tr.points.windows(2) {
                shapes.line(w[0], w[1], color, th * if hot { 1.6 } else { 1.0 }, 0.0);
            }
            if let [.., a, b] = tr.points[..] {
                let d = [b[0] - a[0], b[1] - a[1]];
                let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
                if len > 1e-3 {
                    let d = [d[0] / len, d[1] / len];
                    let n = [-d[1], d[0]];
                    let size = cfg.arrow_size;
                    let back = |k: f32| [b[0] - d[0] * size + n[0] * size * k, b[1] - d[1] * size + n[1] * size * k];
                    shapes.line(b, back(0.5), color, th * 1.8, 0.0);
                    shapes.line(b, back(-0.5), color, th * 1.8, 0.0);
                }
            }
            if cfg.edge_labels && let Some(r) = tr.label_rect {
                shapes.rect(r, theme.bg.canvas.alpha(0.88), 4.0);
                if hot || hover {
                    shapes.bordered_rect(r, crate::theme::Color::TRANSPARENT, color, 4.0, 1.0 / s);
                }
            }
        }

        // ---- text, after every shape ----
        let to_screen = |p: [f32; 2]| [p[0] * s + t.offset[0], p[1] * s + t.offset[1]];
        let title_size = cfg.font_size * ds.name_scale;
        let kw_size = cfg.font_size * ds.keyword_scale;
        let line_size = cfg.font_size * ds.compartment_scale;
        if line_size * s < 3.0 {
            text.set_clip(None);
            return;
        }
        let pad = 16.0;
        let lh = text.line_metrics(fonts.title, title_size).line_height;
        let llh = text.line_metrics(fonts.mono, line_size).line_height;
        for st in sm.states.iter().filter(|st| !st.is_pseudo()) {
            let r = st.rect;
            let [x0, y0] = to_screen([r[0], r[1]]);
            let [x1, y1] = to_screen([r[2], r[3]]);
            text.set_clip(Some(intersect(pane, [x0, y0, x1, y1])));
            let ty = r[1] + (st.header - lh) * 0.5;
            let name_w = text.measure(fonts.title, title_size, &st.title, None)[0];
            if st.kind == StateKind::Root || st.is_composite() {
                text.draw(TextStyle::new(fonts.title, title_size * s, colors.name.0), to_screen([r[0] + pad, ty]), &st.title);
                let kw = format!("«{}»", st.keyword);
                let kw_w = text.measure(fonts.body, kw_size, &kw, None)[0];
                let klh = text.line_metrics(fonts.body, kw_size).line_height;
                if r[0] + pad + name_w + 12.0 < r[2] - pad - kw_w {
                    text.draw(TextStyle::new(fonts.body, kw_size * s, theme.text_muted.0), to_screen([r[2] - pad - kw_w, r[1] + (st.header - klh) * 0.5]), &kw);
                }
            } else {
                let cx = (r[0] + r[2]) * 0.5 - name_w * 0.5;
                text.draw(TextStyle::new(fonts.title, title_size * s, colors.name.0), to_screen([cx, ty]), &st.title);
            }
            let mut y = r[1] + st.header + 6.0;
            for a in &st.actions {
                text.draw(TextStyle::new(fonts.mono, line_size * s, colors.compartment.0), to_screen([r[0] + pad, y]), a);
                y += llh;
            }
            if st.truncated {
                text.draw(TextStyle::new(fonts.mono, line_size * s, theme.text_muted.0), to_screen([r[0] + pad, r[3] - llh - 8.0]), "… inner states not shown");
            }
        }
        text.set_clip(Some(pane));
        if cfg.edge_labels {
            let blh = text.line_metrics(fonts.body, label_size).line_height;
            for tr in &sm.transitions {
                let (Some(label), Some(r)) = (&tr.label, tr.label_rect) else { continue };
                let w = text.measure(fonts.body, label_size, label, None)[0];
                let at = [(r[0] + r[2]) * 0.5 - w * 0.5, (r[1] + r[3]) * 0.5 - blh * 0.5];
                text.draw(TextStyle::new(fonts.body, label_size * s, theme.edge.label.0), to_screen(at), label);
            }
        }
        text.set_clip(None);
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}
