//! What the canvas shows (general view, interconnection view, state machine
//! and action flow views, requirements table, traceability and allocation
//! matrices, model checks): switching between them, building the diagram
//! views on demand and routing input to the active one.

use super::Ui;
use crate::config::{self, Canvas};
use crate::diagram_view::contains;
use crate::document::Document;
use crate::ibd::{End, Ibd};
use crate::menu::{Command, Item};
use crate::state_view::StateView;
use crate::states::{Hit, StateMachine};
use std::time::{Duration, Instant};
use winit::event::MouseButton;
use winit::keyboard::{Key, NamedKey};

const DOUBLE_CLICK: Duration = Duration::from_millis(450);

/// The two views drawn as a graph of nodes and curved edges.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Graph {
    States,
    Actions,
}

impl Graph {
    fn of(canvas: Canvas) -> Option<Graph> {
        match canvas {
            Canvas::States => Some(Graph::States),
            Canvas::Actions => Some(Graph::Actions),
            _ => None,
        }
    }

    fn canvas(self) -> Canvas {
        match self {
            Graph::States => Canvas::States,
            Graph::Actions => Canvas::Actions,
        }
    }

    fn candidates(self, doc: &Document) -> Vec<String> {
        let index = doc.index();
        match self {
            Graph::States => crate::states::candidates(&index, |id| doc.is_local(&index, id)),
            Graph::Actions => crate::actions::candidates(&index, |id| doc.is_local(&index, id)),
        }
    }

    fn build(self, doc: &Document, root: &str) -> Option<StateMachine> {
        let index = doc.index();
        let id = index.get(root)?;
        Some(match self {
            Graph::States => StateMachine::build(&index, id),
            Graph::Actions => crate::actions::build(&index, id),
        })
    }

    /// "state" / "action", for messages.
    fn noun(self) -> &'static str {
        match self {
            Graph::States => "state",
            Graph::Actions => "action",
        }
    }
}

impl Ui {
    pub(super) fn graph(&self, g: Graph) -> &StateView {
        match g {
            Graph::States => &self.states,
            Graph::Actions => &self.actions,
        }
    }

    pub(super) fn graph_mut(&mut self, g: Graph) -> &mut StateView {
        match g {
            Graph::States => &mut self.states,
            Graph::Actions => &mut self.actions,
        }
    }

    pub(super) fn set_canvas(&mut self, canvas: Canvas) {
        if self.canvas == canvas {
            return;
        }
        // Carry the selection over, so switching views keeps the element in focus.
        let focus = self.focus_name();
        self.canvas = canvas;
        self.view.hovered = None;
        self.ibd.hovered = None;
        self.states.hovered = None;
        self.actions.hovered = None;
        self.edit.reset_tools();
        if canvas == Canvas::Interconnection
            && let Some(name) = focus.clone()
        {
            // Start from the element selected before, if it has an inside.
            self.ensure_ibd();
            if self.ibd.candidates.contains(&name) && self.ibd.current_root() != Some(name.as_str()) {
                self.set_ibd_root(Some(name), true);
            }
        }
        if let Some(g) = Graph::of(canvas)
            && let Some(name) = focus.clone()
        {
            self.ensure_graph(g);
            let v = self.graph(g);
            if v.candidates.contains(&name) && v.current_root() != Some(name.as_str()) {
                self.set_graph_root(g, Some(name), true);
            }
        }
        if !canvas.is_diagram() {
            let keep = self.table().selected.clone();
            self.table_mut().select(keep);
        }
        self.focus_canvas_on(focus.as_deref());
        self.config.view.canvas = canvas;
        self.persist("view", "canvas", config::toml_string(canvas.key()));
    }

    /// The element the active view has selected, for the code and tree to follow.
    pub(super) fn focus_name(&self) -> Option<String> {
        match self.canvas {
            Canvas::General => self.selected_name(),
            Canvas::Interconnection => self.ibd.selected.and_then(|e| self.ibd.name_of(e)).map(str::to_owned),
            c => match Graph::of(c) {
                Some(g) => {
                    let v = self.graph(g);
                    v.selected.and_then(|h| v.name_of(h)).map(str::to_owned)
                }
                None => self.table().selected.clone(),
            },
        }
    }

    /// Selects `qualified_name` (or its closest owner shown) in the active
    /// non-general view, after a click in the code or the tree.
    pub(super) fn focus_canvas_on(&mut self, qualified_name: Option<&str>) {
        let Some(mut name) = qualified_name else { return };
        loop {
            let found = match self.canvas {
                Canvas::General => return,
                Canvas::Interconnection => self.ibd.find(name).map(|e| self.ibd.selected = Some(e)).is_some(),
                c => match Graph::of(c) {
                    Some(g) => {
                        let hit = self.graph(g).find(name);
                        if hit.is_some() {
                            self.graph_mut(g).selected = hit;
                        }
                        hit.is_some()
                    }
                    None => {
                        let hit = self.table_rows().iter().map(|r| r.0).chain(self.table_columns()).any(|q| q == name);
                        if hit {
                            let q = name.to_string();
                            self.table_mut().select(Some(q));
                        }
                        hit
                    }
                },
            };
            if found {
                return;
            }
            match name.rfind("::") {
                Some(i) => name = &name[..i],
                None => return,
            }
        }
    }

    // ---- interconnection view ------------------------------------------------------

    /// Rebuilds the interconnection view when the model or the element shown changed.
    pub(super) fn ensure_ibd(&mut self) {
        let key = (self.doc.generation, self.ibd.root.clone());
        if self.ibd.built.as_ref() == Some(&key) {
            return;
        }
        let index = self.doc.index();
        self.ibd.candidates = crate::ibd::candidates(&index, |id| self.doc.is_local(&index, id));
        if self.ibd.root.as_ref().is_some_and(|r| index.get(r).is_none()) {
            // Renamed or deleted: fall back to the best candidate.
            self.ibd.root = None;
            self.ibd.back.clear();
        }
        let selected = self.ibd.selected.and_then(|e| self.ibd.name_of(e)).map(str::to_owned);
        let root_changed = self.ibd.built.as_ref().is_none_or(|(_, r)| *r != self.ibd.root);
        self.ibd.ibd = self.ibd.current_root().and_then(|r| index.get(r)).map(|id| Ibd::build(&index, id));
        self.ibd.selected = selected.and_then(|n| self.ibd.find(&n));
        self.ibd.hovered = None;
        self.ibd.built = Some((self.doc.generation, self.ibd.root.clone()));
        self.ibd.needs_layout = true;
        if root_changed {
            self.ibd.camera.needs_fit = true;
        }
    }

    /// Shows `root` (`None`: the best candidate); `remember` keeps the
    /// current element for Backspace.
    pub(super) fn set_ibd_root(&mut self, root: Option<String>, remember: bool) {
        if root.as_deref() == self.ibd.current_root() && root.is_some() {
            return;
        }
        if remember && let Some(cur) = self.ibd.current_root().map(str::to_owned) {
            self.ibd.back.push(cur);
        }
        self.ibd.root = root;
        self.ibd.selected = None;
        self.ensure_ibd();
    }

    pub(super) fn ibd_back(&mut self) {
        if let Some(prev) = self.ibd.back.pop() {
            self.set_ibd_root(Some(prev), false);
        }
    }

    // ---- state machine and action flow views ---------------------------------------

    /// Rebuilds a graph view when the model or the element shown changed.
    pub(super) fn ensure_graph(&mut self, g: Graph) {
        let generation = self.doc.generation;
        if self.graph(g).built.as_ref().is_some_and(|(gen_, r)| *gen_ == generation && *r == self.graph(g).root) {
            return;
        }
        let candidates = g.candidates(&self.doc);
        let doc = &self.doc;
        let v = match g {
            Graph::States => &mut self.states,
            Graph::Actions => &mut self.actions,
        };
        v.candidates = candidates;
        if v.root.as_ref().is_some_and(|r| doc.index().get(r).is_none()) {
            // Renamed or deleted: fall back to the best candidate.
            v.root = None;
            v.back.clear();
        }
        let selected = v.selected.and_then(|h| v.name_of(h)).map(str::to_owned);
        let root_changed = v.built.as_ref().is_none_or(|(_, r)| *r != v.root);
        v.machine = v.current_root().and_then(|r| g.build(doc, r));
        v.selected = selected.and_then(|n| v.find(&n));
        v.hovered = None;
        v.built = Some((generation, v.root.clone()));
        v.needs_layout = true;
        if root_changed {
            v.camera.needs_fit = true;
        }
    }

    pub(super) fn set_graph_root(&mut self, g: Graph, root: Option<String>, remember: bool) {
        let v = self.graph_mut(g);
        if root.as_deref() == v.current_root() && root.is_some() {
            return;
        }
        if remember && let Some(cur) = v.current_root().map(str::to_owned) {
            v.back.push(cur);
        }
        v.root = root;
        v.selected = None;
        self.ensure_graph(g);
    }

    pub(super) fn graph_back(&mut self, g: Graph) {
        if let Some(prev) = self.graph_mut(g).back.pop() {
            self.set_graph_root(g, Some(prev), false);
        }
    }

    /// The "Show Element" commands of a graph view.
    pub(super) fn run_graph_root(&mut self, g: Graph, i: u16) {
        if let Some(q) = self.graph(g).candidates.get(usize::from(i)).cloned() {
            self.set_canvas(g.canvas());
            self.set_graph_root(g, Some(q), true);
        }
    }

    // ---- element menus -------------------------------------------------------------

    /// The elements the interconnection view can show, for a menu.
    pub(super) fn ibd_menu_items(&self) -> Vec<Item> {
        root_items(&self.ibd.candidates, self.ibd.current_root(), !self.ibd.back.is_empty(), Command::IbdRoot, Command::IbdBack, "No parts with an inside in this file")
    }

    /// The elements a graph view can show, for a menu.
    pub(super) fn graph_menu_items(&self, g: Graph) -> Vec<Item> {
        let v = self.graph(g);
        match g {
            Graph::States => root_items(&v.candidates, v.current_root(), !v.back.is_empty(), Command::StateRoot, Command::StateBack, "No state machines in this file"),
            Graph::Actions => root_items(&v.candidates, v.current_root(), !v.back.is_empty(), Command::ActionRoot, Command::ActionBack, "No actions with steps in this file"),
        }
    }

    /// The menu of the "Showing …" chip of the diagram shown.
    pub(super) fn root_menu_items(&self) -> Vec<Item> {
        match Graph::of(self.canvas) {
            Some(g) => self.graph_menu_items(g),
            None => self.ibd_menu_items(),
        }
    }

    /// The element shown by the interconnection, state or action view, for the chip.
    pub(super) fn root_label(&self) -> Option<String> {
        match self.canvas {
            Canvas::Interconnection => Some(self.ibd.root_label()),
            c => Graph::of(c).map(|g| self.graph(g).root_label()),
        }
    }

    /// Whether the chip's view has somewhere to go back to.
    pub(super) fn root_has_back(&self) -> bool {
        match Graph::of(self.canvas) {
            Some(g) => !self.graph(g).back.is_empty(),
            None => !self.ibd.back.is_empty(),
        }
    }

    // ---- input -----------------------------------------------------------------------

    fn is_double_click(&mut self, p: [f32; 2]) -> bool {
        let double = self.canvas_click.is_some_and(|(t, q)| t.elapsed() < DOUBLE_CLICK && (q[0] - p[0]).abs() < 5.0 && (q[1] - p[1]).abs() < 5.0);
        self.canvas_click = if double { None } else { Some((Instant::now(), p)) };
        double
    }

    /// A press on the canvas of a view other than the general one.
    pub(super) fn canvas_press(&mut self, button: MouseButton, p: [f32; 2]) {
        match self.canvas {
            Canvas::General => {}
            Canvas::Interconnection => {
                let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) else { return };
                let world = self.ibd.camera.to_world(p, pane, self.dpi);
                if button == MouseButton::Left {
                    let hit = self.ibd.hit(world);
                    // Double click on a typed part: its definition's own interconnection view.
                    if self.is_double_click(p) && let Some(End::Part(i)) = hit {
                        let ty = self.ibd.ibd.as_ref().and_then(|d| d.parts.get(i)).filter(|_| i > 0).and_then(|part| part.type_name.clone());
                        if let Some(ty) = ty {
                            if self.ibd.candidates.contains(&ty) {
                                self.set_ibd_root(Some(ty), true);
                            } else {
                                self.notify(format!("{} has no inner parts to show", ty.rsplit("::").next().unwrap_or(&ty)));
                            }
                            return;
                        }
                    }
                    self.ibd.selected = hit;
                }
                self.ibd.camera.press_pan(p);
            }
            c if Graph::of(c).is_some() => {
                let g = Graph::of(c).expect("checked");
                let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) else { return };
                let world = self.graph(g).camera.to_world(p, pane, self.dpi);
                if button == MouseButton::Left {
                    let hit = self.graph(g).hit(world);
                    // Double click on a typed state or action: its definition's own view.
                    if self.is_double_click(p) && let Some(Hit::State(i)) = hit {
                        let ty = self.graph(g).machine.as_ref().and_then(|m| m.states.get(i)).filter(|_| i > 0).and_then(|s| s.type_name.clone());
                        if let Some(ty) = ty {
                            if self.graph(g).candidates.contains(&ty) {
                                self.set_graph_root(g, Some(ty), true);
                            } else {
                                let what = if g == Graph::States { "inner states" } else { "steps" };
                                self.notify(format!("{} has no {what} to show", ty.rsplit("::").next().unwrap_or(&ty)));
                            }
                            return;
                        }
                    }
                    self.graph_mut(g).selected = hit;
                    // A transition or succession without a name has no element to select: show its line.
                    if let Some(Hit::Transition(t)) = hit
                        && let Some(tr) = self.graph(g).machine.as_ref().and_then(|m| m.transitions.get(t))
                        && tr.qualified_name.is_none()
                        && let Some(offset) = tr.offset
                    {
                        self.code.goto(offset);
                    }
                }
                self.graph_mut(g).camera.press_pan(p);
            }
            _ => {
                if button == MouseButton::Left {
                    self.reqs_press(p);
                }
            }
        }
    }

    pub(super) fn canvas_motion(&mut self, p: [f32; 2]) {
        let over = |ui: &Ui| ui.panes.diagram.filter(|&pane| contains(pane, p) && !ui.over_chrome(p));
        match self.canvas {
            Canvas::General => {}
            Canvas::Interconnection => {
                if self.ibd.camera.pan_motion(p) {
                    return;
                }
                self.ibd.hovered = over(self).and_then(|pane| self.ibd.hit(self.ibd.camera.to_world(p, pane, self.dpi)));
            }
            c if Graph::of(c).is_some() => {
                let g = Graph::of(c).expect("checked");
                if self.graph_mut(g).camera.pan_motion(p) {
                    return;
                }
                let v = self.graph(g);
                let hovered = over(self).and_then(|pane| v.hit(v.camera.to_world(p, pane, self.dpi)));
                self.graph_mut(g).hovered = hovered;
            }
            _ => self.reqs_motion(p),
        }
    }

    pub(super) fn canvas_wheel(&mut self, lines: f32, p: [f32; 2]) {
        if !self.canvas.is_diagram() {
            self.reqs_wheel(lines, self.mods.shift);
            return;
        }
        if self.canvas == Canvas::General {
            return;
        }
        if let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) {
            let factor = self.config.diagram.zoom_speed.max(1.001).powf(lines);
            let (dpi, cfg) = (self.dpi, self.config.diagram.clone());
            self.camera_mut().zoom_at(p, factor, pane, dpi, &cfg);
        }
    }

    /// Keys for the active view; true if handled.
    pub(super) fn canvas_key(&mut self, key: &Key) -> bool {
        match self.canvas {
            Canvas::General => false,
            Canvas::Interconnection | Canvas::States | Canvas::Actions => {
                let g = Graph::of(self.canvas);
                match key {
                    Key::Named(NamedKey::Backspace) => {
                        match g {
                            Some(g) => self.graph_back(g),
                            None => self.ibd_back(),
                        }
                        true
                    }
                    Key::Named(NamedKey::Escape) => {
                        match g {
                            Some(g) => self.graph_mut(g).selected = None,
                            None => self.ibd.selected = None,
                        }
                        true
                    }
                    Key::Character(c) if matches!(c.as_str(), "f" | "F") => {
                        self.camera_mut().needs_fit = true;
                        true
                    }
                    _ => false,
                }
            }
            _ => match key {
                Key::Named(NamedKey::ArrowDown) => self.reqs_key(true),
                Key::Named(NamedKey::ArrowUp) => self.reqs_key(false),
                Key::Named(NamedKey::Escape) => {
                    self.table_mut().selected = None;
                    true
                }
                _ => false,
            },
        }
    }

    /// The left part of the status bar for views other than the general one.
    pub(super) fn canvas_status(&self) -> Option<String> {
        match self.canvas {
            Canvas::General => None,
            Canvas::Interconnection => Some(match &self.ibd.ibd {
                Some(d) => {
                    let mut s = format!(
                        "Interconnection of {} · {} parts · {} ports · {} connections",
                        self.ibd.root_label(),
                        d.parts.len().saturating_sub(1),
                        d.ports.len(),
                        d.links.len()
                    );
                    if d.unresolved > 0 {
                        s += &format!(" · {} connector ends not found", d.unresolved);
                    }
                    s
                }
                None => "No part or part definition with inner parts, ports or connections in this file".to_owned(),
            }),
            c @ (Canvas::States | Canvas::Actions) => {
                let g = Graph::of(c).expect("graph canvas");
                let v = self.graph(g);
                Some(match &v.machine {
                    Some(m) => {
                        let nodes = m.states.iter().filter(|s| !s.is_pseudo()).count().saturating_sub(1);
                        let (what, nodes_word, edges_word) = match g {
                            Graph::States => ("State machine", "states", "transitions"),
                            Graph::Actions => ("Action flow of", "actions", "flows"),
                        };
                        let mut s = format!("{what} {} · {nodes} {nodes_word} · {} {edges_word}", v.root_label(), m.transitions.len());
                        if m.unresolved > 0 {
                            s += &format!(" · {} {} ends not found", m.unresolved, if g == Graph::States { "transition" } else { "flow" });
                        }
                        s
                    }
                    None => format!("No {} definition or usage with {} in this file", g.noun(), if g == Graph::States { "states or transitions" } else { "steps" }),
                })
            }
            Canvas::Requirements => Some("Click a requirement to show it in the code · ↑ ↓ move · Ctrl+E exports CSV".to_owned()),
            Canvas::Traceability => Some("S satisfy · V verify · D dependency · red rows are untraced · Shift+wheel scrolls sideways".to_owned()),
            Canvas::Allocation => Some("A allocate · P perform · E exhibit · red rows: nothing carries them out · Ctrl+E exports CSV".to_owned()),
            Canvas::Checks => Some("Click a finding to show it in the code · ↑ ↓ move · double-click opens another file · Ctrl+E exports CSV".to_owned()),
        }
    }

    /// Menu items to pick the view.
    pub(super) fn canvas_items(&self) -> Vec<Item> {
        Canvas::ALL
            .iter()
            .enumerate()
            .map(|(i, &c)| Item::radio(format!("{} View", c.label()), Command::Canvas(c), self.canvas == c).shortcut(SHORTCUT_KEYS[i]))
            .collect()
    }
}

/// Ctrl+1 … Ctrl+8, one per canvas in [`Canvas::ALL`] order.
const SHORTCUT_KEYS: [&str; 8] = ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7", "Ctrl+8"];

/// A radio item per candidate, then Back.
fn root_items(candidates: &[String], current: Option<&str>, can_go_back: bool, pick: fn(u16) -> Command, back: Command, none: &str) -> Vec<Item> {
    let mut items: Vec<Item> = candidates.iter().take(60).enumerate().map(|(i, q)| Item::radio(q.clone(), pick(i as u16), current == Some(q.as_str()))).collect();
    if items.is_empty() {
        items.push(Item::action(none, "", back).enabled(false));
    }
    if can_go_back {
        items.push(Item::separator());
        items.push(Item::action("Back", "Backspace", back));
    }
    items
}
