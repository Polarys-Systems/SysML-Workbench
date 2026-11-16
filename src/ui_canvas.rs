//! What the canvas shows (general view, interconnection view, state machine
//! view, requirements table, traceability and allocation matrices, model
//! checks): switching between them, building the interconnection and state
//! machine views on demand and routing input to the active one.

use super::Ui;
use crate::config::{self, Canvas};
use crate::diagram_view::contains;
use crate::ibd::{End, Ibd};
use crate::menu::{Command, Item};
use crate::states::{Hit, StateMachine};
use std::time::{Duration, Instant};
use winit::event::MouseButton;
use winit::keyboard::{Key, NamedKey};

const DOUBLE_CLICK: Duration = Duration::from_millis(450);

impl Ui {
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
        self.edit.reset_tools();
        match canvas {
            Canvas::Interconnection => {
                // Start from the element selected before, if it has an inside.
                if let Some(name) = focus.clone() {
                    self.ensure_ibd();
                    if self.ibd.candidates.contains(&name) && self.ibd.current_root() != Some(name.as_str()) {
                        self.set_ibd_root(Some(name), true);
                    }
                }
            }
            Canvas::States => {
                if let Some(name) = focus.clone() {
                    self.ensure_states();
                    if self.states.candidates.contains(&name) && self.states.current_root() != Some(name.as_str()) {
                        self.set_state_root(Some(name), true);
                    }
                }
            }
            _ => {}
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
            Canvas::States => self.states.selected.and_then(|h| self.states.name_of(h)).map(str::to_owned),
            _ => self.table().selected.clone(),
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
                Canvas::States => self.states.find(name).map(|h| self.states.selected = Some(h)).is_some(),
                _ => {
                    let hit = self.table_rows().iter().map(|r| r.0).chain(self.table_columns()).any(|q| q == name);
                    if hit {
                        let q = name.to_string();
                        self.table_mut().select(Some(q));
                    }
                    hit
                }
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

    // ---- state machine view --------------------------------------------------------

    /// Rebuilds the state machine view when the model or the element shown changed.
    pub(super) fn ensure_states(&mut self) {
        let key = (self.doc.generation, self.states.root.clone());
        if self.states.built.as_ref() == Some(&key) {
            return;
        }
        let index = self.doc.index();
        self.states.candidates = crate::states::candidates(&index, |id| self.doc.is_local(&index, id));
        if self.states.root.as_ref().is_some_and(|r| index.get(r).is_none()) {
            self.states.root = None;
            self.states.back.clear();
        }
        let selected = self.states.selected.and_then(|h| self.states.name_of(h)).map(str::to_owned);
        let root_changed = self.states.built.as_ref().is_none_or(|(_, r)| *r != self.states.root);
        self.states.machine = self.states.current_root().and_then(|r| index.get(r)).map(|id| StateMachine::build(&index, id));
        self.states.selected = selected.and_then(|n| self.states.find(&n));
        self.states.hovered = None;
        self.states.built = Some((self.doc.generation, self.states.root.clone()));
        self.states.needs_layout = true;
        if root_changed {
            self.states.camera.needs_fit = true;
        }
    }

    pub(super) fn set_state_root(&mut self, root: Option<String>, remember: bool) {
        if root.as_deref() == self.states.current_root() && root.is_some() {
            return;
        }
        if remember && let Some(cur) = self.states.current_root().map(str::to_owned) {
            self.states.back.push(cur);
        }
        self.states.root = root;
        self.states.selected = None;
        self.ensure_states();
    }

    pub(super) fn state_back(&mut self) {
        if let Some(prev) = self.states.back.pop() {
            self.set_state_root(Some(prev), false);
        }
    }

    // ---- element menus -------------------------------------------------------------

    /// The elements the interconnection view can show, for a menu.
    pub(super) fn ibd_menu_items(&self) -> Vec<Item> {
        root_items(&self.ibd.candidates, self.ibd.current_root(), !self.ibd.back.is_empty(), Command::IbdRoot, Command::IbdBack, "No parts with an inside in this file")
    }

    /// The elements the state machine view can show, for a menu.
    pub(super) fn state_menu_items(&self) -> Vec<Item> {
        root_items(&self.states.candidates, self.states.current_root(), !self.states.back.is_empty(), Command::StateRoot, Command::StateBack, "No state machines in this file")
    }

    /// The menu of the "Showing …" chip of the diagram shown.
    pub(super) fn root_menu_items(&self) -> Vec<Item> {
        if self.canvas == Canvas::States { self.state_menu_items() } else { self.ibd_menu_items() }
    }

    /// The element shown by the interconnection or state machine view, for the chip.
    pub(super) fn root_label(&self) -> Option<String> {
        match self.canvas {
            Canvas::Interconnection => Some(self.ibd.root_label()),
            Canvas::States => Some(self.states.root_label()),
            _ => None,
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
            Canvas::States => {
                let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) else { return };
                let world = self.states.camera.to_world(p, pane, self.dpi);
                if button == MouseButton::Left {
                    let hit = self.states.hit(world);
                    // Double click on a typed state: its definition's own view.
                    if self.is_double_click(p) && let Some(Hit::State(i)) = hit {
                        let ty = self.states.machine.as_ref().and_then(|m| m.states.get(i)).filter(|_| i > 0).and_then(|s| s.type_name.clone());
                        if let Some(ty) = ty {
                            if self.states.candidates.contains(&ty) {
                                self.set_state_root(Some(ty), true);
                            } else {
                                self.notify(format!("{} has no inner states to show", ty.rsplit("::").next().unwrap_or(&ty)));
                            }
                            return;
                        }
                    }
                    self.states.selected = hit;
                    // A transition without a name has no element to select: show its line.
                    if let Some(Hit::Transition(t)) = hit
                        && let Some(tr) = self.states.machine.as_ref().and_then(|m| m.transitions.get(t))
                        && tr.qualified_name.is_none()
                        && let Some(offset) = tr.offset
                    {
                        self.code.goto(offset);
                    }
                }
                self.states.camera.press_pan(p);
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
            Canvas::States => {
                if self.states.camera.pan_motion(p) {
                    return;
                }
                self.states.hovered = over(self).and_then(|pane| self.states.hit(self.states.camera.to_world(p, pane, self.dpi)));
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
            Canvas::Interconnection | Canvas::States => {
                let states = self.canvas == Canvas::States;
                match key {
                    Key::Named(NamedKey::Backspace) => {
                        if states { self.state_back() } else { self.ibd_back() }
                        true
                    }
                    Key::Named(NamedKey::Escape) => {
                        if states {
                            self.states.selected = None;
                        } else {
                            self.ibd.selected = None;
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
            Canvas::States => Some(match &self.states.machine {
                Some(m) => {
                    let states = m.states.iter().filter(|s| !s.is_pseudo()).count().saturating_sub(1);
                    let mut s = format!("State machine {} · {states} states · {} transitions", self.states.root_label(), m.transitions.len());
                    if m.unresolved > 0 {
                        s += &format!(" · {} transition ends not found", m.unresolved);
                    }
                    s
                }
                None => "No state definition or state usage with states or transitions in this file".to_owned(),
            }),
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
            .map(|(i, &c)| {
                let keys = ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7"][i];
                Item::radio(format!("{} View", c.label()), Command::Canvas(c), self.canvas == c).shortcut(keys)
            })
            .collect()
    }
}

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
