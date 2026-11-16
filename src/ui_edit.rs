//! Editing the model from the diagram: the palette toolbar, placing new
//! elements, the connect handle, context menus and the inline text fields.
//! Every change becomes a text edit in the code pane (one undo step) and the
//! model is reparsed at once, so code, tree and diagram stay in step.

use super::Ui;
use crate::code_view::{KeyInput, edit_key};
use crate::theme::{Color, Theme};
use crate::diagram_view::{Fonts, contains};
use crate::editor::Editor;
use crate::menu::{Command, Item, ModelCommand, UNTYPED};
use crate::model_edit::{self, Change, ElementInfo, ElementKind, Relation};
use crate::shapes::{Rect, ShapeRenderer, Transform};
use crate::text::{TextRenderer, TextStyle};
use std::time::{Duration, Instant};
use sysml::UsageKind;
use winit::keyboard::{Key, NamedKey};

const DOUBLE_CLICK: Duration = Duration::from_millis(450);
/// A right click that moves less than this (pixels) opens the context menu
/// instead of panning.
const CLICK_SLOP: f32 = 4.0;

/// What an inline field edits.
enum Purpose {
    /// Renames every occurrence of `old`.
    Rename { old: String },
    /// Replaces the declaration `start..end`, which read `original`.
    Declaration { start: usize, end: usize, original: String },
}

/// Where an inline field sits.
enum Anchor {
    /// Over the name of the node with this qualified name.
    Name(String),
    /// Over a compartment line: owner node and the feature's source offset.
    Line(String, usize),
}

struct Field {
    editor: Editor,
    purpose: Purpose,
    anchor: Anchor,
    rect: Rect,
}

/// Data behind the open context menu (its commands index into it).
#[derive(Default)]
struct Context {
    /// Node (qualified name) the menu was opened on.
    node: Option<String>,
    /// Feature (qualified name and offset) of the compartment line clicked.
    feature: Option<(String, usize)>,
    /// Diagram point where the menu was opened.
    world: [f32; 2],
    /// Qualified type names offered by type submenus.
    types: Vec<String>,
    /// Relationships offered after a connect drag, and its ends.
    relations: Vec<Relation>,
    link: Option<(String, String)>,
}

struct Link {
    from: usize,
    at: [f32; 2],
}

#[derive(Default)]
pub(super) struct EditState {
    /// Armed placement tool: the next click in the diagram creates this.
    place: Option<ElementKind>,
    ctx: Context,
    link: Option<Link>,
    field: Option<Field>,
    right_press: Option<[f32; 2]>,
    last_click: Option<(Instant, [f32; 2])>,
    /// Palette buttons (screen rects), one per palette group.
    buttons: Vec<Rect>,
    pub(super) toolbar: Option<Rect>,
    /// Node whose connect handle shows, and the handle's rect.
    handle: Option<(usize, Rect)>,
}

impl EditState {
    pub(super) fn busy(&self) -> bool {
        self.link.is_some()
    }

    /// Drops the armed tool, the connect drag, the handle and any open field
    /// (another view takes the canvas).
    pub(super) fn reset_tools(&mut self) {
        self.place = None;
        self.link = None;
        self.handle = None;
        self.field = None;
        self.right_press = None;
    }
}

impl Ui {
    // ---- applying changes --------------------------------------------------------

    /// Reparses after a diagram edit, keeping every existing node where it
    /// is (and pinned), so the automatic layout does not reshuffle the view.
    fn sync_parse_keep_layout(&mut self) {
        let before: Vec<(String, [f32; 2])> =
            self.doc.diagram.nodes.iter().map(|n| (n.qualified_name.clone(), n.pos)).collect();
        self.sync_parse();
        let mut moved = false;
        for (name, pos) in before {
            if let Some(i) = self.doc.diagram.node_named(&name) {
                let n = &mut self.doc.diagram.nodes[i];
                moved |= !n.pinned || n.pos != pos;
                n.pos = pos;
                n.pinned = true;
            }
        }
        if moved {
            self.save_positions();
        }
    }

    /// Reparses the edited text now (diagram edits need current spans).
    fn sync_parse(&mut self) {
        let selected = self.selected_name();
        self.doc.set_source(self.code.editor.text());
        self.parsed_version = self.code.editor.version();
        self.reparse_at = None;
        self.view.selected = selected.and_then(|n| self.doc.diagram.node_named(&n));
        self.view.hovered = None;
        self.edit.handle = None;
        self.needs_measure = true;
    }

    fn ensure_parsed(&mut self) {
        if self.code.editor.version() != self.parsed_version {
            self.sync_parse();
        }
    }

    /// Applies `change` to the code as one undo step; returns what it created.
    fn apply_change(&mut self, change: Option<Change>, failure: &str) -> Option<String> {
        let Some(change) = change else {
            self.notify(format!("Could not {failure}"));
            return None;
        };
        self.code.editor.apply_edits(change.edits);
        self.sync_parse_keep_layout();
        change.created
    }

    fn src(&self) -> String {
        self.code.editor.text().to_owned()
    }

    fn info(&self, node: usize) -> ElementInfo {
        let n = &self.doc.diagram.nodes[node];
        ElementInfo { is_def: n.is_def, kind: n.kind }
    }

    /// Selects the node `name`, pins it at `world` and saves positions.
    fn put_node(&mut self, name: &str, world: [f32; 2]) {
        if let Some(i) = self.doc.diagram.node_named(name) {
            let n = &mut self.doc.diagram.nodes[i];
            n.pos = world;
            n.pinned = true;
            self.view.selected = Some(i);
            self.save_positions();
        }
    }

    // ---- creating ----------------------------------------------------------------

    /// Creates `kind` at diagram point `world`: as a feature of the node there
    /// (usages dropped on a node), else in the package around the selection.
    fn create_at(&mut self, kind: ElementKind, world: [f32; 2]) {
        self.ensure_parsed();
        let src = self.src();
        let target = self.view.node_at(&self.doc.diagram, world);
        if let (ElementKind::Usage(k), Some(t)) = (kind, target) {
            let owner = self.doc.diagram.nodes[t].qualified_name.clone();
            let created = self.apply_change(model_edit::add_feature(&src, &owner, k, false, None, None), "add the feature");
            if let Some(name) = created {
                self.notify(format!("Added {} to {}", kind.keyword(), owner.rsplit("::").next().unwrap_or(&owner)));
                self.open_rename_line(&owner, &name);
            }
            return;
        }
        let near = self.selected_name();
        let container = model_edit::default_container(&src, near.as_deref());
        let created = self.apply_change(model_edit::create(&src, container.as_deref(), kind, None), "create the element");
        if let Some(name) = created {
            self.put_node(&name, world);
            self.open_rename_name(&name);
            self.notify(format!("Created {} — type a name, Enter applies", kind.keyword()));
        }
    }

    // ---- inline fields -----------------------------------------------------------

    fn open_field(&mut self, text: &str, purpose: Purpose, anchor: Anchor) {
        let mut editor = Editor::single_line();
        editor.set_text(text);
        editor.select_all();
        self.edit.field = Some(Field { editor, purpose, anchor, rect: [0.0; 4] });
        self.code.active = false;
        self.tree.active = false;
    }

    fn open_rename_name(&mut self, node_name: &str) {
        let short = node_name.rsplit("::").next().unwrap_or(node_name).to_owned();
        self.open_field(&short, Purpose::Rename { old: short.clone() }, Anchor::Name(node_name.to_owned()));
    }

    /// Rename field over the compartment line of feature `feature` in `owner`.
    fn open_rename_line(&mut self, owner: &str, feature: &str) {
        let Some(offset) = self.doc.offset_of(feature) else { return };
        let short = feature.rsplit("::").next().unwrap_or(feature).to_owned();
        self.open_field(&short, Purpose::Rename { old: short.clone() }, Anchor::Line(owner.to_owned(), offset));
    }

    fn open_declaration(&mut self, span: sysml::Span, anchor: Anchor) {
        let src = self.src();
        let (start, end) = model_edit::header_range(&src, span);
        let original = src[start..end].to_owned();
        let flat = original.split_whitespace().collect::<Vec<_>>().join(" ");
        self.open_field(&flat, Purpose::Declaration { start, end, original }, anchor);
    }

    pub(super) fn commit_field(&mut self) {
        let Some(field) = self.edit.field.take() else { return };
        let new = field.editor.text().trim().to_owned();
        match field.purpose {
            Purpose::Rename { old } => {
                if new.is_empty() || new == old {
                    return;
                }
                // Keep the renamed node where it is.
                let anchor_node = match &field.anchor {
                    Anchor::Name(n) => Some(n.clone()),
                    Anchor::Line(..) => None,
                };
                let kept = anchor_node
                    .as_deref()
                    .and_then(|n| self.doc.diagram.node_named(n))
                    .map(|i| (self.doc.diagram.nodes[i].pos, self.doc.diagram.nodes[i].pinned));
                let edits = model_edit::rename(&self.src(), &old, &new);
                if edits.is_empty() {
                    return;
                }
                let count = edits.len();
                self.apply_change(Some(Change { edits, created: None }), "rename");
                if let (Some(old_q), Some((pos, pinned))) = (anchor_node, kept) {
                    let new_q = match old_q.rsplit_once("::") {
                        Some((owner, _)) => format!("{owner}::{}", new),
                        None => new.clone(),
                    };
                    if let Some(i) = self.doc.diagram.node_named(&new_q) {
                        self.doc.diagram.nodes[i].pos = pos;
                        self.doc.diagram.nodes[i].pinned = pinned;
                        self.view.selected = Some(i);
                        self.save_positions();
                    }
                }
                self.notify(format!("Renamed {count} occurrence{} of {old} to {new}", if count == 1 { "" } else { "s" }));
            }
            Purpose::Declaration { start, end, original } => {
                if new.is_empty() || new == original.split_whitespace().collect::<Vec<_>>().join(" ") {
                    return;
                }
                if self.code.editor.text().get(start..end) != Some(original.as_str()) {
                    self.notify("The code changed meanwhile; the declaration was not applied");
                    return;
                }
                let edit = crate::editor::Edit { start, end, text: new };
                self.apply_change(Some(Change { edits: vec![edit], created: None }), "change the declaration");
                if let Some(d) = self.doc.diagnostics.first() {
                    let text = d.text.clone();
                    self.notify(format!("Applied, but the model now has a problem: {text} (Ctrl+Z undoes)"));
                }
            }
        }
    }

    // ---- context menus -----------------------------------------------------------

    fn type_items(&mut self, kind: Option<UsageKind>, make: impl Fn(u16) -> ModelCommand, with_untyped: bool) -> Vec<Item> {
        let mut items = Vec::new();
        if with_untyped {
            items.push(Item::action("(no type)", "", Command::Model(make(UNTYPED))));
        }
        for (q, name) in model_edit::types_for(&self.src(), kind) {
            let i = self.edit.ctx.types.len() as u16;
            self.edit.ctx.types.push(q);
            items.push(Item::action(name, "", Command::Model(make(i))));
        }
        items
    }

    fn add_feature_menu(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        for k in model_edit::FEATURE_KINDS {
            let label = ElementKind::Usage(k).label();
            let sub = self.type_items(Some(k), |i| ModelCommand::AddFeature(k, false, i), true);
            items.push(Item::submenu(label, sub));
        }
        items.push(Item::separator());
        let refs = self.type_items(Some(UsageKind::Part), |i| ModelCommand::AddFeature(UsageKind::Part, true, i), true);
        items.push(Item::submenu("Reference Part", refs));
        items
    }

    fn palette_items(create_here: bool) -> Vec<Item> {
        model_edit::palette()
            .into_iter()
            .map(|(group, kinds)| {
                let items = kinds
                    .into_iter()
                    .map(|k| {
                        let cmd = if create_here { ModelCommand::CreateHere(k) } else { ModelCommand::Place(k) };
                        Item::action(k.label(), "", Command::Model(cmd))
                    })
                    .collect();
                Item::submenu(group, items)
            })
            .collect()
    }

    /// Right click in the diagram at screen point `p`.
    fn open_diagram_menu(&mut self, p: [f32; 2]) {
        let Some(pane) = self.panes.diagram else { return };
        self.ensure_parsed();
        let world = self.view.to_world(p, pane, self.dpi);
        self.edit.ctx = Context { world, ..Default::default() };
        let node = self.view.node_at(&self.doc.diagram, world);
        let Some(node) = node else {
            let mut items = vec![Item::submenu("New Here", Self::palette_items(true))];
            items.push(Item::action("New Package", "", Command::Model(ModelCommand::CreateHere(ElementKind::Package))));
            items.push(Item::separator());
            items.push(Item::action("Fit Diagram", "F", Command::Fit));
            items.push(Item::action("Restore Automatic Layout", "L", Command::ResetLayout));
            self.menu.open_context(items, p);
            return;
        };
        self.view.selected = Some(node);
        if self.refuse_external(node) {
            return;
        }
        let n = &self.doc.diagram.nodes[node];
        let qname = n.qualified_name.clone();
        let (is_def, kind) = (n.is_def, n.kind);
        self.edit.ctx.node = Some(qname.clone());
        // A compartment line: menu for that feature.
        if let Some(line) = self.doc.diagram.line_at(node, world, &self.config.diagram) {
            let offset = self.doc.diagram.nodes[node].line_spans[line].start;
            let feature = self.doc.elements.iter().find(|e| e.span.start == offset).and_then(|e| e.qualified_name.clone());
            let finfo = feature.as_deref().and_then(|f| model_edit::info_of(&self.src(), f));
            self.edit.ctx.feature = feature.clone().map(|f| (f, offset));
            let mut items = Vec::new();
            if feature.is_some() {
                items.push(Item::action("Rename", "F2", Command::Model(ModelCommand::Rename)));
            }
            items.push(Item::action("Edit Declaration…", "double-click", Command::Model(ModelCommand::EditDeclaration)));
            if let Some(fi) = finfo {
                let mut types = self.type_items(Some(fi.kind), ModelCommand::SetType, false);
                if types.is_empty() {
                    types = self.type_items(None, ModelCommand::SetType, false);
                }
                items.push(Item::submenu("Set Type", types));
            }
            items.push(Item::separator());
            items.push(Item::action("Show in Code", "", Command::Model(ModelCommand::ShowInCode)));
            items.push(Item::action("Delete Feature", "Del", Command::Model(ModelCommand::Delete)));
            self.menu.open_context(items, p);
            return;
        }
        let mut items = vec![
            Item::action("Rename", "F2", Command::Model(ModelCommand::Rename)),
            Item::action("Edit Declaration…", "", Command::Model(ModelCommand::EditDeclaration)),
            Item::separator(),
            Item::submenu("Add Feature", self.add_feature_menu()),
        ];
        if is_def {
            let bases = self.type_items(Some(kind), ModelCommand::Specialize, false);
            let mut all = bases;
            if all.is_empty() {
                all = self.type_items(None, ModelCommand::Specialize, false);
            }
            items.push(Item::submenu("Specializes", all));
        } else {
            let mut types = self.type_items(Some(kind), ModelCommand::SetType, false);
            if types.is_empty() {
                types = self.type_items(None, ModelCommand::SetType, false);
            }
            items.push(Item::submenu("Defined By (Type)", types));
        }
        items.push(Item::separator());
        items.push(Item::action("Show in Code", "", Command::Model(ModelCommand::ShowInCode)));
        items.push(Item::action("Delete", "Del", Command::Model(ModelCommand::Delete)));
        self.menu.open_context(items, p);
    }

    /// Runs a command chosen in a diagram menu.
    pub(super) fn run_model(&mut self, cmd: ModelCommand) {
        self.ensure_parsed();
        let src = self.src();
        let ctx_node = self.edit.ctx.node.clone();
        let ty = |ui: &Ui, i: u16| (i != UNTYPED).then(|| ui.edit.ctx.types.get(i as usize).cloned()).flatten();
        match cmd {
            ModelCommand::Place(kind) => {
                if self.mode == crate::config::ViewMode::Outline {
                    self.mode = crate::config::ViewMode::Split;
                }
                self.edit.place = Some(kind);
                self.notify(format!("Click in the diagram to place a {} (on a node: adds it as a feature) · Esc cancels", kind.keyword()));
            }
            ModelCommand::CreateHere(kind) => self.create_at(kind, self.edit.ctx.world),
            ModelCommand::AddFeature(kind, is_ref, t) => {
                let Some(owner) = ctx_node else { return };
                let t = ty(self, t);
                let created = self.apply_change(model_edit::add_feature(&src, &owner, kind, is_ref, None, t.as_deref()), "add the feature");
                if let Some(name) = created {
                    self.open_rename_line(&owner, &name);
                }
            }
            ModelCommand::SetType(t) => {
                let target = self.edit.ctx.feature.as_ref().map(|f| f.0.clone()).or(ctx_node);
                if let (Some(target), Some(t)) = (target, ty(self, t)) {
                    self.apply_change(model_edit::set_type(&src, &target, &t), "set the type");
                }
            }
            ModelCommand::Specialize(t) => {
                if let (Some(target), Some(t)) = (ctx_node, ty(self, t)) {
                    self.apply_change(model_edit::specialize(&src, &target, &t), "add the specialization");
                }
            }
            ModelCommand::Relate(i) => {
                let Some((from, to)) = self.edit.ctx.link.clone() else { return };
                let Some(&rel) = self.edit.ctx.relations.get(i as usize) else { return };
                let created = self.apply_change(model_edit::relate(&src, rel, &from, &to), "draw the relationship");
                if let Some(name) = created {
                    // An owned or referenced feature: let the user name it.
                    self.open_rename_line(&from, &name);
                }
            }
            ModelCommand::Rename => {
                if let Some((f, _)) = self.edit.ctx.feature.clone() {
                    let owner = ctx_node.unwrap_or_default();
                    self.open_rename_line(&owner, &f);
                } else if let Some(n) = ctx_node {
                    self.open_rename_name(&n);
                }
            }
            ModelCommand::EditDeclaration => {
                if let Some((_, offset)) = self.edit.ctx.feature.clone() {
                    let owner = ctx_node.unwrap_or_default();
                    if let Some(span) = self.span_at(offset) {
                        self.open_declaration(span, Anchor::Line(owner, offset));
                    }
                } else if let Some(n) = ctx_node {
                    if let Some(i) = self.doc.diagram.node_named(&n) {
                        let span = self.doc.diagram.nodes[i].span;
                        self.open_declaration(span, Anchor::Name(n));
                    }
                }
            }
            ModelCommand::Delete => {
                if let Some((f, offset)) = self.edit.ctx.feature.clone() {
                    if let Some(span) = self.span_at(offset) {
                        self.apply_change(Some(Change { edits: vec![model_edit::delete(&src, span)], created: None }), "delete");
                        self.notify(format!("Deleted {} (Ctrl+Z undoes)", f.rsplit("::").next().unwrap_or(&f)));
                    }
                } else if let Some(n) = ctx_node {
                    self.delete_node(&n);
                }
            }
            ModelCommand::ShowInCode => {
                let offset = self.edit.ctx.feature.as_ref().map(|f| f.1).or_else(|| ctx_node.and_then(|n| self.doc.offset_of(&n)));
                if let Some(o) = offset {
                    if self.mode == crate::config::ViewMode::Diagram {
                        self.mode = crate::config::ViewMode::Split;
                    }
                    self.code.goto(o);
                    self.code.active = true;
                }
            }
        }
    }

    /// A node from another file cannot be edited through this file's text:
    /// says so and returns true for such a node.
    fn refuse_external(&mut self, node: usize) -> bool {
        let Some(n) = self.doc.diagram.nodes.get(node) else { return false };
        let Some(file) = n.external.clone() else { return false };
        let name = n.name.clone();
        self.notify(format!("{name} is declared in {file}: open that file to edit it"));
        true
    }

    fn span_at(&self, offset: usize) -> Option<sysml::Span> {
        self.doc.elements.iter().find(|e| e.span.start == offset).map(|e| e.span)
    }

    fn delete_node(&mut self, name: &str) {
        self.ensure_parsed();
        let Some(i) = self.doc.diagram.node_named(name) else { return };
        let span = self.doc.diagram.nodes[i].span;
        let edit = model_edit::delete(&self.src(), span);
        self.view.selected = None;
        self.apply_change(Some(Change { edits: vec![edit], created: None }), "delete");
        self.notify(format!("Deleted {} (Ctrl+Z undoes)", name.rsplit("::").next().unwrap_or(name)));
    }

    // ---- input -----------------------------------------------------------------

    /// Keys for the inline field and the diagram editing tools; true if used.
    pub(super) fn edit_key(&mut self, key: &Key, text: Option<&str>) -> bool {
        if self.edit.field.is_some() {
            match key {
                Key::Named(NamedKey::Enter | NamedKey::Tab) => self.commit_field(),
                Key::Named(NamedKey::Escape) => self.edit.field = None,
                _ => {
                    let input = KeyInput { key, text, mods: self.mods };
                    let mut clip = None;
                    let field = self.edit.field.as_mut().unwrap();
                    edit_key(&mut field.editor, &input, 1, &mut clip);
                }
            }
            return true;
        }
        if *key == Key::Named(NamedKey::Escape) && (self.edit.place.is_some() || self.edit.link.is_some()) {
            self.edit.place = None;
            self.edit.link = None;
            return true;
        }
        false
    }

    /// Diagram keys: Delete, F2, Ctrl+Z / Ctrl+Y. True if used.
    pub(super) fn diagram_edit_key(&mut self, key: &Key) -> bool {
        let m = self.mods;
        let ch = match key {
            Key::Character(c) => c.to_lowercase(),
            _ => String::new(),
        };
        if m.ctrl && !m.alt && (ch == "z" || ch == "y") {
            let redo = ch == "y" || m.shift;
            if if redo { self.code.editor.redo() } else { self.code.editor.undo() } {
                self.sync_parse_keep_layout();
            }
            return true;
        }
        let Some(name) = self.selected_name() else { return false };
        if matches!(key, Key::Named(NamedKey::Delete | NamedKey::F2)) && self.view.selected.is_some_and(|n| self.refuse_external(n)) {
            return true;
        }
        match key {
            Key::Named(NamedKey::Delete) => {
                self.delete_node(&name);
                true
            }
            Key::Named(NamedKey::F2) => {
                self.open_rename_name(&name);
                true
            }
            _ => false,
        }
    }

    /// A press in the diagram area. True if editing consumed it.
    /// A press outside the open inline field applies it; true if the press
    /// is on the field.
    pub(super) fn press_field(&mut self, p: [f32; 2]) -> bool {
        if let Some(f) = &self.edit.field {
            if contains(f.rect, p) {
                return true;
            }
            self.commit_field();
        }
        false
    }

    pub(super) fn edit_press(&mut self, left: bool, p: [f32; 2]) -> bool {
        // A click outside an open field applies it.
        if let Some(f) = &self.edit.field {
            if contains(f.rect, p) {
                return true;
            }
            self.commit_field();
        }
        if let Some(i) = self.edit.buttons.iter().position(|r| contains(*r, p)) {
            if left {
                let (_, kinds) = model_edit::palette().into_iter().nth(i).unwrap();
                let items = kinds.into_iter().map(|k| Item::action(k.label(), "", Command::Model(ModelCommand::Place(k)))).collect();
                let r = self.edit.buttons[i];
                self.menu.open_context_above(items, [r[0], r[1] - 6.0 * self.dpi]);
            }
            return true;
        }
        if self.edit.toolbar.is_some_and(|t| contains(t, p)) {
            return true;
        }
        let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) else { return false };
        if !left {
            self.edit.right_press = Some(p);
            return false; // the view still pans with a right drag
        }
        if let Some(kind) = self.edit.place.take() {
            let world = self.view.to_world(p, pane, self.dpi);
            self.create_at(kind, world);
            return true;
        }
        if let Some((node, r)) = self.edit.handle
            && contains(r, p)
        {
            self.edit.link = Some(Link { from: node, at: p });
            return true;
        }
        // Double click: rename a node, or edit a feature's declaration.
        let double = self.edit.last_click.is_some_and(|(t, q)| t.elapsed() < DOUBLE_CLICK && (q[0] - p[0]).abs() < 5.0 && (q[1] - p[1]).abs() < 5.0);
        self.edit.last_click = Some((Instant::now(), p));
        if double {
            self.edit.last_click = None;
            self.ensure_parsed();
            let world = self.view.to_world(p, pane, self.dpi);
            if let Some(node) = self.view.node_at(&self.doc.diagram, world) {
                if self.refuse_external(node) {
                    return true;
                }
                let n = &self.doc.diagram.nodes[node];
                let name = n.qualified_name.clone();
                if let Some(line) = self.doc.diagram.line_at(node, world, &self.config.diagram) {
                    let span = n.line_spans[line];
                    self.open_declaration(span, Anchor::Line(name, span.start));
                } else {
                    self.open_rename_name(&name);
                }
                return true;
            }
        }
        false
    }

    pub(super) fn edit_motion(&mut self, p: [f32; 2]) {
        if self.edit.right_press.is_some_and(|q| (q[0] - p[0]).abs() > CLICK_SLOP || (q[1] - p[1]).abs() > CLICK_SLOP) {
            self.edit.right_press = None;
        }
        if let Some(link) = &mut self.edit.link {
            link.at = p;
            return;
        }
        // The handle follows the hovered node and stays while the mouse is on it.
        let keep = self.edit.handle.is_some_and(|(_, r)| contains(r, p));
        if !keep {
            self.edit.handle = None;
            if let (Some(n), Some(pane)) = (self.view.hovered, self.panes.diagram)
                && !self.view.is_dragging()
                && self.edit.place.is_none()
                && self.doc.diagram.nodes.get(n).is_some_and(|n| n.external.is_none())
            {
                self.edit.handle = Some((n, self.handle_rect(n, pane)));
            }
        }
    }

    fn handle_rect(&self, node: usize, pane: Rect) -> Rect {
        let t = self.view.transform(pane, self.dpi);
        let r = self.doc.diagram.nodes[node].rect();
        let (x, y) = (r[2] * t.scale + t.offset[0], (r[1] + r[3]) * 0.5 * t.scale + t.offset[1]);
        let s = self.config.theme.size.diagram.handle * self.dpi;
        [x - s, y - s, x + s, y + s]
    }

    /// A release: finishes a connect drag or opens the right-click menu.
    pub(super) fn edit_release(&mut self, left: bool, p: [f32; 2]) {
        if !left {
            if self.edit.right_press.take().is_some() {
                self.open_diagram_menu(p);
            }
            return;
        }
        let Some(link) = self.edit.link.take() else { return };
        let Some(pane) = self.panes.diagram else { return };
        let world = self.view.to_world(p, pane, self.dpi);
        let Some(to) = self.view.node_at(&self.doc.diagram, world).filter(|&t| t != link.from) else { return };
        if self.refuse_external(to) {
            return;
        }
        let relations = model_edit::relations_between(self.info(link.from), self.info(to));
        let names = (self.doc.diagram.nodes[link.from].qualified_name.clone(), self.doc.diagram.nodes[to].qualified_name.clone());
        let items = relations
            .iter()
            .enumerate()
            .map(|(i, r)| Item::action(r.label(), "", Command::Model(ModelCommand::Relate(i as u8))))
            .collect();
        self.edit.ctx = Context { relations, link: Some(names), ..Default::default() };
        self.menu.open_context(items, p);
    }

    pub(super) fn placing(&self) -> bool {
        self.edit.place.is_some()
    }

    // ---- drawing -----------------------------------------------------------------

    /// The diagram palette: a floating glass group of "+ Structure" style
    /// buttons, centered in `row` (chrome pass). Returns its rect.
    pub(super) fn draw_toolbar(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, row: Rect, dpi: f32) -> Rect {
        let size = self.config.view.ui_font_size * dpi;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let fs = &theme.size.frame;
        let pad = (fs.toolbar_pad * dpi).round();
        let h = (fs.control_height * dpi).round();
        let widths: Vec<f32> = model_edit::palette()
            .iter()
            .map(|(group, _)| (text.measure(fonts.body, size, group, None)[0] + size * 1.2 + 26.0 * dpi).round())
            .collect();
        let w = widths.iter().sum::<f32>() + pad * (widths.len() as f32 + 1.0);
        let x0 = ((row[0] + row[2] - w) * 0.5).round().max(row[0]);
        let bar = [x0, row[1], x0 + w, row[1] + h + 2.0 * pad];
        shapes.begin_layer(Transform::IDENTITY, None);
        super::glass_card(shapes, theme, bar, fs.control_radius * dpi + pad, dpi);
        text.set_clip(Some(bar));
        self.edit.buttons.clear();
        let mut x = bar[0] + pad;
        let ty = ((bar[1] + bar[3]) * 0.5 - lh * 0.5).round();
        for ((group, _), bw) in model_edit::palette().into_iter().zip(widths) {
            let r = [x, bar[1] + pad, x + bw, bar[3] - pad];
            if contains(r, self.cursor) {
                shapes.rect(r, theme.hover_row.alpha(3.0), fs.control_radius * dpi);
            }
            // "+" icon in the accent color.
            let (cx, cy, s) = (r[0] + 12.0 * dpi + size * 0.3, (r[1] + r[3]) * 0.5, size * 0.3);
            shapes.line([cx - s, cy], [cx + s, cy], theme.accent.0, 1.6 * dpi, 0.0);
            shapes.line([cx, cy - s], [cx, cy + s], theme.accent.0, 1.6 * dpi, 0.0);
            text.draw(TextStyle::new(fonts.body, size, theme.text.0), [cx + s + 7.0 * dpi, ty], group);
            self.edit.buttons.push(r);
            x = r[2] + pad;
        }
        text.set_clip(None);
        bar
    }

    /// What the diagram is waiting for, for the status bar; `true` while a
    /// placement or an inline edit is in progress.
    pub(super) fn edit_hint(&self) -> (String, bool) {
        match (&self.edit.place, &self.edit.field) {
            (_, Some(_)) => ("Enter applies · Esc cancels".to_owned(), true),
            (Some(k), _) => (format!("Click to place a {} · on a node it becomes a feature · Esc cancels", k.keyword()), true),
            _ => ("Drag + on a node's edge to relate · Right-click add, type, delete · Double-click edit".to_owned(), false),
        }
    }

    /// Handle, connect line and the inline field (overlay pass, under menus).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_edit_overlay(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) {
        let Some(pane) = self.panes.diagram else {
            self.edit.field = None;
            return;
        };
        shapes.begin_layer(Transform::IDENTITY, Some(pane));
        // Connect handle.
        if let Some((node, _)) = self.edit.handle
            && node < self.doc.diagram.nodes.len()
            && self.edit.link.is_none()
        {
            let r = self.handle_rect(node, pane);
            self.edit.handle = Some((node, r));
            let hot = contains(r, self.cursor);
            let fill = if hot { theme.accent.0 } else { theme.bg.bar.0 };
            shapes.bordered_rect(r, fill, theme.accent.0, (r[2] - r[0]) * 0.5, 1.5 * dpi);
            let (cx, cy, s) = ((r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5, (r[2] - r[0]) * 0.25);
            let c = if hot { theme.text_on_accent.0 } else { theme.accent.0 };
            shapes.line([cx - s, cy], [cx + s, cy], c, 1.5 * dpi, 0.0);
            shapes.line([cx, cy - s], [cx, cy + s], c, 1.5 * dpi, 0.0);
        }
        // Rubber band while connecting, and the target node's outline.
        if let Some(link) = &self.edit.link
            && link.from < self.doc.diagram.nodes.len()
        {
            let from = self.handle_rect(link.from, pane);
            let a = [(from[0] + from[2]) * 0.5, (from[1] + from[3]) * 0.5];
            shapes.line(a, link.at, theme.accent.0, 2.0 * dpi, 7.0 * dpi);
            let world = self.view.to_world(link.at, pane, dpi);
            if let Some(t) = self.view.node_at(&self.doc.diagram, world).filter(|&t| t != link.from) {
                let tr = self.view.transform(pane, dpi);
                let r = self.doc.diagram.nodes[t].rect();
                let s = [r[0] * tr.scale + tr.offset[0], r[1] * tr.scale + tr.offset[1], r[2] * tr.scale + tr.offset[0], r[3] * tr.scale + tr.offset[1]];
                shapes.bordered_rect([s[0] - 3.0, s[1] - 3.0, s[2] + 3.0, s[3] + 3.0], Color::TRANSPARENT, theme.accent.0, 6.0 * dpi, 2.0 * dpi);
            }
        }

        // Inline field.
        let Some(field) = &self.edit.field else { return };
        let tr = self.view.transform(pane, dpi);
        let cfg = &self.config.diagram;
        let row = match &field.anchor {
            Anchor::Name(n) => self.doc.diagram.node_named(n).map(|i| {
                let n = &self.doc.diagram.nodes[i];
                (n.rect(), n.pos[1] + n.rows[0], n.rows[1])
            }),
            Anchor::Line(owner, offset) => self.doc.diagram.node_named(owner).and_then(|i| {
                let n = &self.doc.diagram.nodes[i];
                let line = n.line_spans.iter().position(|s| s.start == *offset)?;
                let line = line.min(cfg.max_compartment_lines.saturating_sub(1));
                Some((n.rect(), n.pos[1] + n.rows[2] + line as f32 * n.rows[3], n.rows[3]))
            }),
        };
        let Some((node_rect, y, h)) = row else {
            // Not shown in the diagram (e.g. compartments hidden): edit at the pane top.
            self.edit.field.as_mut().unwrap().rect = [pane[0] + 20.0 * dpi, pane[1] + 10.0 * dpi, pane[0] + 420.0 * dpi, pane[1] + 40.0 * dpi];
            return self.draw_field(shapes, text, fonts, theme, dpi);
        };
        let s = tr.scale;
        let x0 = node_rect[0] * s + tr.offset[0];
        let x1 = (node_rect[2] * s + tr.offset[0]).max(x0 + 260.0 * dpi);
        let fy0 = y * s + tr.offset[1] - 4.0 * dpi;
        let fy1 = (y + h) * s + tr.offset[1] + 4.0 * dpi;
        let min_h = text.line_metrics(fonts.body, 12.0 * dpi).line_height + 8.0 * dpi;
        let fy1 = fy1.max(fy0 + min_h);
        self.edit.field.as_mut().unwrap().rect = [x0 - 4.0 * dpi, fy0, x1 + 4.0 * dpi, fy1];
        self.draw_field(shapes, text, fonts, theme, dpi);
    }

    fn draw_field(&self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, fonts: Fonts, theme: &Theme, dpi: f32) {
        let Some(field) = &self.edit.field else { return };
        let r = field.rect;
        shapes.begin_overlay();
        text.begin_overlay();
        shapes.rect([r[0], r[1] + 2.0 * dpi, r[2] + 3.0 * dpi, r[3] + 5.0 * dpi], theme.node.shadow.0, 6.0 * dpi);
        shapes.bordered_rect(r, theme.bg.panel.0, theme.accent.0, theme.size.diagram.field_radius * dpi, 1.5 * dpi);
        let size = ((r[3] - r[1]) * 0.55).clamp(11.0 * dpi, 22.0 * dpi);
        let lh = text.line_metrics(fonts.body, size).line_height;
        let pad = 6.0 * dpi;
        let ed = &field.editor;
        let t = ed.text();
        let sel = ed.primary();
        let w = |s: &str, text: &mut TextRenderer| text.measure(fonts.body, size, s, None)[0];
        let caret = w(&t[..sel.head.min(t.len())], text);
        let avail = r[2] - r[0] - 2.0 * pad;
        let scroll = (caret - avail + pad).max(0.0);
        let x = r[0] + pad - scroll;
        let y = ((r[1] + r[3]) * 0.5 - lh * 0.5).round();
        let clip = [r[0] + 2.0, r[1], r[2] - 2.0, r[3]];
        shapes.begin_layer(Transform::IDENTITY, Some(clip));
        if !sel.is_empty() {
            let (a, b) = (w(&t[..sel.start()], text), w(&t[..sel.end()], text));
            shapes.rect([x + a, y, x + b, y + lh], theme.text_selection.0, 0.0);
        }
        shapes.rect([x + caret, y, x + caret + theme.size.frame.caret_width * dpi, y + lh], theme.cursor.0, 0.0);
        text.set_clip(Some(clip));
        text.draw(TextStyle::new(fonts.body, size, theme.text.0), [x, y], t);
        text.set_clip(None);
    }
}
