//! The menu bar and its drop-down menus with submenus, check and radio items
//! and keyboard navigation. The window rebuilds the menus from its state every
//! frame; popups draw in the overlay pass, above the panes.

use crate::config::ViewMode;
use crate::theme::{MenuSizes, Theme};
use crate::diagram::EdgeKind;
use crate::diagram_view::{Fonts, contains};
use crate::model_edit::ElementKind;
use sysml::UsageKind;
use crate::shapes::{Rect, ShapeRenderer};
use crate::text::{TextRenderer, TextStyle};
use winit::keyboard::{Key, NamedKey};

/// `MenuBar::open` value of a context menu.
const CONTEXT: usize = usize::MAX;

/// Everything a menu item can do; the window carries it out.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Command {
    New,
    Open,
    OpenRecent(usize),
    ClearRecent,
    Save,
    SaveAs,
    ExportPng,
    Reload,
    Close,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Find,
    Replace,
    Rename,
    Format,
    ToggleComment,
    Mode(ViewMode),
    ToggleTree,
    ToggleFrameTime,
    ZoomIn,
    ZoomOut,
    Fit,
    ExpandTree,
    CollapseTree,
    ToggleGrid,
    ToggleSnap,
    ToggleCompartments,
    ToggleEdgeLabels,
    ToggleValues,
    ToggleEdge(EdgeKind),
    ResetLayout,
    Theme(usize),
    CodeFont(usize),
    UiFont(usize),
    PickCodeFont,
    CodeSize(i8),
    DiagramSize(i8),
    UiSize(i8),
    ResetSizes,
    EditSettings,
    ReloadSettings,
    Shortcuts,
    /// Show the n-th problem in the code editor.
    GotoProblem(u16),
    About,
    /// Model editing from the diagram.
    Model(ModelCommand),
}

pub enum ItemKind {
    Action(Command),
    Check(Command, bool),
    Radio(Command, bool),
    Submenu(Vec<Item>),
    Separator,
}

pub struct Item {
    pub label: String,
    pub shortcut: &'static str,
    pub kind: ItemKind,
    pub enabled: bool,
}

impl Item {
    pub fn action(label: impl Into<String>, shortcut: &'static str, cmd: Command) -> Self {
        Item { label: label.into(), shortcut, kind: ItemKind::Action(cmd), enabled: true }
    }
    pub fn check(label: impl Into<String>, shortcut: &'static str, cmd: Command, on: bool) -> Self {
        Item { label: label.into(), shortcut, kind: ItemKind::Check(cmd, on), enabled: true }
    }
    pub fn radio(label: impl Into<String>, cmd: Command, on: bool) -> Self {
        Item { label: label.into(), shortcut: "", kind: ItemKind::Radio(cmd, on), enabled: true }
    }
    pub fn submenu(label: impl Into<String>, items: Vec<Item>) -> Self {
        let enabled = !items.is_empty();
        Item { label: label.into(), shortcut: "", kind: ItemKind::Submenu(items), enabled }
    }
    pub fn separator() -> Self {
        Item { label: String::new(), shortcut: "", kind: ItemKind::Separator, enabled: false }
    }
    pub fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }
    fn command(&self) -> Option<Command> {
        match self.kind {
            ItemKind::Action(c) | ItemKind::Check(c, _) | ItemKind::Radio(c, _) => Some(c),
            _ => None,
        }
    }
    fn selectable(&self) -> bool {
        self.enabled && !matches!(self.kind, ItemKind::Separator)
    }
}

pub struct Menu {
    pub title: &'static str,
    pub items: Vec<Item>,
}

/// One open popup: its rectangle and its items' rectangles.
#[derive(Clone, Default)]
struct Popup {
    rect: Rect,
    items: Vec<Rect>,
}

#[derive(Default)]
pub struct MenuBar {
    menus: Vec<Menu>,
    /// Open top-level menu.
    open: Option<usize>,
    /// Highlighted item per open level: `path[0]` in the drop-down, `path[1]`
    /// in its submenu, ... A submenu is open when its item is highlighted.
    path: Vec<Option<usize>>,
    /// Submenus open on hover; keyboard navigation opens them with Right.
    titles: Vec<Rect>,
    popups: Vec<Popup>,
    bar: Rect,
    mouse: [f32; 2],
    /// A context menu (open == [`CONTEXT`]): its items and top-left corner.
    context: Option<(Vec<Item>, [f32; 2])>,
    /// The context menu opens upwards: its bottom-left corner is at the point.
    context_above: bool,
    /// The highlight follows the keyboard rather than the mouse.
    keyboard: bool,
}

impl MenuBar {
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn close(&mut self) {
        self.open = None;
        self.context = None;
        self.path.clear();
    }

    pub fn set_menus(&mut self, menus: Vec<Menu>) {
        self.menus = menus;
        if self.open.is_some_and(|o| o != CONTEXT && o >= self.menus.len()) {
            self.close();
        }
    }

    /// Opens a pop-up menu with its top-left corner at `at` (screen pixels).
    pub fn open_context(&mut self, items: Vec<Item>, at: [f32; 2]) {
        self.open_context_at(items, at, false);
    }

    /// A context menu whose bottom-left corner is at `at` (for controls at the
    /// bottom of the window).
    pub fn open_context_above(&mut self, items: Vec<Item>, at: [f32; 2]) {
        self.open_context_at(items, at, true);
    }

    fn open_context_at(&mut self, items: Vec<Item>, at: [f32; 2], above: bool) {
        if items.is_empty() {
            return;
        }
        self.context = Some((items, at));
        self.context_above = above;
        self.open = Some(CONTEXT);
        self.keyboard = false;
        self.path = vec![None];
    }

    fn open_menu(&mut self, i: usize, keyboard: bool) {
        self.context = None;
        self.open = Some(i);
        self.keyboard = keyboard;
        let first = keyboard.then(|| first_selectable(&self.menus[i].items, 0, 1)).flatten();
        self.path = vec![first];
    }

    /// Items of the popup at `level` along the current path.
    fn items_at(&self, level: usize) -> Option<&[Item]> {
        let open = self.open?;
        let mut items: &[Item] = if open == CONTEXT { &self.context.as_ref()?.0 } else { &self.menus.get(open)?.items };
        for l in 0..level {
            match &items.get(self.path.get(l).copied().flatten()?)?.kind {
                ItemKind::Submenu(sub) => items = sub,
                _ => return None,
            }
        }
        Some(items)
    }

    /// Opens the menu whose title starts with `letter` (Alt+letter).
    pub fn open_mnemonic(&mut self, letter: &str) -> bool {
        let found = self.menus.iter().position(|m| m.title.to_lowercase().starts_with(letter));
        match found {
            Some(i) => {
                self.open_menu(i, true);
                true
            }
            None => false,
        }
    }

    // ---- mouse -----------------------------------------------------------------

    /// True while the mouse is over the bar or an open popup.
    pub fn hit(&self, p: [f32; 2]) -> bool {
        contains(self.bar, p) || (self.open.is_some() && self.popups.iter().any(|pp| contains(pp.rect, p)))
    }

    pub fn motion(&mut self, p: [f32; 2]) {
        if p == self.mouse {
            return;
        }
        self.mouse = p;
        let Some(open) = self.open else { return };
        self.keyboard = false;
        if let Some(t) = self.titles.iter().position(|r| contains(*r, p)) {
            if t != open && open != CONTEXT {
                self.open_menu(t, false);
            }
            return;
        }
        // The deepest popup under the mouse takes the highlight.
        for level in (0..self.popups.len()).rev() {
            let popup = &self.popups[level];
            if !contains(popup.rect, p) {
                continue;
            }
            let hit = popup.items.iter().position(|r| contains(*r, p));
            let items = self.items_at(level).unwrap_or(&[]);
            let hit = hit.filter(|&i| items.get(i).is_some_and(Item::selectable));
            let opens_sub = hit.is_some_and(|i| matches!(items[i].kind, ItemKind::Submenu(_)));
            if self.path.get(level).copied().flatten() == hit && self.path.len() > level + 1 {
                return; // already showing this item's submenu
            }
            self.path.truncate(level + 1);
            self.path.resize(level + 1, None);
            self.path[level] = hit;
            // Hovering a submenu item opens it.
            if opens_sub {
                self.path.push(None);
            }
            return;
        }
    }

    /// Handles a click. Returns `(consumed, command)`: a click outside an open
    /// menu closes it and is consumed too.
    pub fn press(&mut self, p: [f32; 2]) -> (bool, Option<Command>) {
        self.mouse = p;
        if let Some(t) = self.titles.iter().position(|r| contains(*r, p)) {
            if self.open == Some(t) {
                self.close();
            } else {
                self.open_menu(t, false);
            }
            return (true, None);
        }
        if self.open.is_none() {
            return (contains(self.bar, p), None);
        }
        for level in (0..self.popups.len()).rev() {
            if !contains(self.popups[level].rect, p) {
                continue;
            }
            let Some(i) = self.popups[level].items.iter().position(|r| contains(*r, p)) else { return (true, None) };
            return (true, self.activate(level, i));
        }
        self.close();
        (true, None)
    }

    fn activate(&mut self, level: usize, i: usize) -> Option<Command> {
        let item = self.items_at(level)?.get(i)?;
        if !item.selectable() {
            return None;
        }
        if let ItemKind::Submenu(sub) = &item.kind {
            let first = first_selectable(sub, 0, 1);
            self.path.truncate(level + 1);
            self.path[level] = Some(i);
            self.path.push(if self.keyboard { first } else { None });
            return None;
        }
        let cmd = item.command();
        self.close();
        cmd
    }

    // ---- keyboard --------------------------------------------------------------

    /// Keys while a menu is open. Returns `(consumed, command)`.
    pub fn key(&mut self, key: &Key) -> (bool, Option<Command>) {
        let Some(open) = self.open else { return (false, None) };
        self.keyboard = true;
        if self.path.is_empty() {
            self.path.push(None);
        }
        let level = self.path.len() - 1;
        let items_len = self.items_at(level).map_or(0, <[Item]>::len);
        let current = self.path[level];
        let n = self.menus.len();
        match key {
            Key::Named(NamedKey::Escape) => {
                if level > 0 {
                    self.path.pop();
                } else {
                    self.close();
                }
            }
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp) => {
                let down = matches!(key, Key::Named(NamedKey::ArrowDown));
                let step = if down { 1 } else { items_len.saturating_sub(1) };
                let start = match current {
                    Some(c) => (c + step) % items_len.max(1),
                    None if down => 0,
                    None => items_len.saturating_sub(1),
                };
                let items = self.items_at(level).unwrap_or(&[]);
                self.path[level] = first_selectable(items, start, if down { 1 } else { -1 });
            }
            Key::Named(NamedKey::ArrowRight) => {
                let opens_sub = current
                    .and_then(|c| self.items_at(level)?.get(c))
                    .is_some_and(|i| i.selectable() && matches!(i.kind, ItemKind::Submenu(_)));
                if opens_sub {
                    return (true, self.activate(level, current.unwrap()));
                }
                if open != CONTEXT {
                    self.open_menu((open + 1) % n, true);
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if level > 0 {
                    self.path.pop();
                } else {
                    if open == CONTEXT {
                        self.close();
                    } else {
                        self.open_menu((open + n - 1) % n, true);
                    }
                }
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(c) = current {
                    return (true, self.activate(level, c));
                }
            }
            Key::Named(NamedKey::Alt) => self.close(),
            Key::Character(c) => {
                // First letter of an item, like Windows menus without underlines.
                let c = c.to_lowercase();
                let items = self.items_at(level).unwrap_or(&[]);
                let start = current.map_or(0, |i| i + 1);
                let found = (0..items.len())
                    .map(|k| (start + k) % items.len())
                    .find(|&k| items[k].selectable() && items[k].label.to_lowercase().starts_with(&c));
                if let Some(k) = found {
                    self.path[level] = Some(k);
                }
            }
            _ => {}
        }
        (true, None)
    }

    // ---- drawing -----------------------------------------------------------------

    /// Width the menu titles need at UI font size `size` (physical pixels).
    pub fn titles_width(&self, text: &mut TextRenderer, fonts: Fonts, size: f32, sz: &MenuSizes, dpi: f32) -> f32 {
        let pad = (sz.pad * dpi).round();
        self.menus.iter().map(|m| text.measure(fonts.body, size, m.title, None)[0] + pad * 1.6).sum::<f32>() + pad * 0.4
    }

    /// Draws the bar into `bar` (base pass) and the open popups (overlay pass).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        font_size: f32,
        theme: &Theme,
        bar: Rect,
        window: [f32; 2],
        dpi: f32,
    ) {
        let size = font_size * dpi;
        let sz = &theme.size.menu;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let pad = (sz.pad * dpi).round();
        // The titles sit on the caller's top bar; `bar` is the strip they use.
        self.bar = bar;
        self.titles.clear();
        let mut x = bar[0] + pad * 0.4;
        let ty = ((bar[1] + bar[3]) * 0.5 - lh * 0.5).round();
        for (i, m) in self.menus.iter().enumerate() {
            let w = text.measure(fonts.body, size, m.title, None)[0] + pad * 1.6;
            let r = [x, bar[1] + sz.title_inset * dpi, x + w, bar[3] - sz.title_inset * dpi];
            let hot = contains(r, self.mouse);
            if self.open == Some(i) {
                shapes.rect(r, theme.selection_row.0, sz.item_radius * dpi);
            } else if hot {
                shapes.rect(r, theme.hover_row.alpha(2.0), sz.item_radius * dpi);
            }
            let color = if hot || self.open == Some(i) { theme.text.0 } else { theme.text_muted.0 };
            text.draw(TextStyle::new(fonts.body, size, color), [x + pad * 0.8, ty], m.title);
            self.titles.push(r);
            x += w;
        }

        self.popups.clear();
        let Some(open) = self.open else { return };
        shapes.begin_overlay();
        text.begin_overlay();
        let origin_at = match &self.context {
            Some((_, at)) if open == CONTEXT => *at,
            _ => [self.titles[open][0], bar[3] + 6.0 * dpi],
        };
        let mut origin = origin_at;
        let mut parent_left = origin_at[0];
        // One popup per open level; a submenu opens beside its item.
        for level in 0..self.path.len().max(1) {
            let Some(items) = self.items_at(level) else { break };
            let mut popup = layout_popup(text, fonts, size, &theme.size.menu, items, origin, window, dpi, (level > 0).then_some(parent_left));
            if level == 0 && open == CONTEXT && self.context_above {
                // Opened from a control at the bottom: the popup ends at `origin`.
                let up = [origin[0], (origin[1] - (popup.rect[3] - popup.rect[1])).max(0.0)];
                popup = layout_popup(text, fonts, size, &theme.size.menu, items, up, window, dpi, None);
            }
            let sel = self.path.get(level).copied().flatten();
            draw_popup(shapes, text, fonts, size, theme, items, &popup, sel, dpi);
            if let Some(r) = sel.and_then(|s| popup.items.get(s)) {
                origin = [popup.rect[2] - 2.0 * dpi, r[1] - 5.0 * dpi];
                parent_left = popup.rect[0];
            }
            self.popups.push(popup);
        }
    }
}

/// Index of the first selectable item from `start`, stepping by `dir` (±1) and wrapping.
fn first_selectable(items: &[Item], start: usize, dir: isize) -> Option<usize> {
    let n = items.len() as isize;
    (0..n).map(|k| (start as isize + k * dir).rem_euclid(n.max(1)) as usize).find(|&i| items[i].selectable())
}

#[allow(clippy::too_many_arguments)]
fn layout_popup(
    text: &mut TextRenderer,
    fonts: Fonts,
    size: f32,
    sz: &MenuSizes,
    items: &[Item],
    origin: [f32; 2],
    window: [f32; 2],
    dpi: f32,
    parent_left: Option<f32>,
) -> Popup {
    let lh = text.line_metrics(fonts.body, size).line_height;
    let row_h = sz.row_height.px(dpi, size, lh).round();
    let sep_h = sz.separator_height.px(dpi, size, lh).round();
    let pad = (sz.pad * dpi).round();
    let gutter = sz.gutter.px(dpi, size, lh).round();
    let mut label_w: f32 = 0.0;
    let mut short_w: f32 = 0.0;
    for it in items {
        label_w = label_w.max(text.measure(fonts.body, size, &it.label, None)[0]);
        if !it.shortcut.is_empty() {
            short_w = short_w.max(text.measure(fonts.body, size * sz.shortcut_scale, it.shortcut, None)[0]);
        }
    }
    let width = (gutter + label_w + if short_w > 0.0 { pad * 3.0 + short_w } else { 0.0 } + pad * 3.0).max(sz.min_width * dpi).round();
    let height: f32 = items.iter().map(|i| if matches!(i.kind, ItemKind::Separator) { sep_h } else { row_h }).sum::<f32>() + pad * 0.8;
    let mut x = origin[0];
    if x + width > window[0] {
        // Submenus flip to the left of their parent; drop-downs shift left.
        x = match parent_left {
            Some(left) => (left - width + 2.0 * dpi).max(0.0),
            None => (window[0] - width).max(0.0),
        };
    }
    let y = origin[1].min((window[1] - height).max(0.0));
    let rect = [x.round(), y.round(), (x + width).round(), (y + height).round()];
    let mut iy = rect[1] + pad * 0.4;
    let rects = items
        .iter()
        .map(|i| {
            let h = if matches!(i.kind, ItemKind::Separator) { sep_h } else { row_h };
            let r = [rect[0] + sz.item_inset * dpi, iy, rect[2] - sz.item_inset * dpi, iy + h];
            iy += h;
            r
        })
        .collect();
    Popup { rect, items: rects }
}

#[allow(clippy::too_many_arguments)]
fn draw_popup(
    shapes: &mut ShapeRenderer,
    text: &mut TextRenderer,
    fonts: Fonts,
    size: f32,
    theme: &Theme,
    items: &[Item],
    popup: &Popup,
    highlighted: Option<usize>,
    dpi: f32,
) {
    let r = popup.rect;
    let sz = &theme.size.menu;
    let radius = sz.radius * dpi;
    let shadow = sz.shadow * dpi;
    shapes.shadow([r[0], r[1] + shadow * 0.5, r[2], r[3] + shadow * 0.5], theme.glass.shadow.0, radius, shadow * 1.5);
    shapes.bordered_rect(r, theme.bg.bar.0, theme.border.0, radius, dpi.max(1.0));
    let lh = text.line_metrics(fonts.body, size).line_height;
    let pad = (sz.pad * dpi).round();
    let gutter = sz.gutter.px(dpi, size, lh).round();
    let short_size = size * sz.shortcut_scale;
    for (i, (it, ir)) in items.iter().zip(&popup.items).enumerate() {
        if matches!(it.kind, ItemKind::Separator) {
            let y = ((ir[1] + ir[3]) * 0.5).round();
            shapes.rect([ir[0] + pad * 0.4, y, ir[2] - pad * 0.4, y + dpi.max(1.0)], theme.border.0, 0.0);
            continue;
        }
        let hot = highlighted == Some(i) && it.enabled;
        if hot {
            shapes.rect(*ir, theme.accent.alpha(0.9), sz.item_radius * dpi);
        }
        let color = if !it.enabled {
            theme.text_muted.alpha(0.6)
        } else if hot {
            theme.text_on_accent.0
        } else {
            theme.text.0
        };
        let cy = (ir[1] + ir[3]) * 0.5;
        let ty = (cy - lh * 0.5).round();
        text.draw(TextStyle::new(fonts.body, size, color), [ir[0] + gutter, ty], &it.label);
        let mark_x = ir[0] + gutter * 0.5;
        let t = 1.6 * dpi;
        match it.kind {
            ItemKind::Check(_, true) => {
                let s = size * 0.28;
                shapes.line([mark_x - s, cy], [mark_x - s * 0.3, cy + s * 0.7], color, t, 0.0);
                shapes.line([mark_x - s * 0.3, cy + s * 0.7], [mark_x + s, cy - s * 0.8], color, t, 0.0);
            }
            ItemKind::Radio(_, true) => {
                let s = size * 0.17;
                shapes.rect([mark_x - s, cy - s, mark_x + s, cy + s], color, s);
            }
            ItemKind::Submenu(_) => {
                let (x, s) = (ir[2] - pad, size * 0.22);
                shapes.line([x - s * 0.5, cy - s], [x + s * 0.5, cy], color, t * 0.9, 0.0);
                shapes.line([x + s * 0.5, cy], [x - s * 0.5, cy + s], color, t * 0.9, 0.0);
            }
            _ => {}
        }
        if !it.shortcut.is_empty() {
            let w = text.measure(fonts.body, short_size, it.shortcut, None)[0];
            let sy = (cy - text.line_metrics(fonts.body, short_size).line_height * 0.5).round();
            let c = if hot { theme.text_on_accent.alpha(0.85) } else { theme.text_muted.0 };
            text.draw(TextStyle::new(fonts.body, short_size, c), [ir[2] - pad - w, sy], it.shortcut);
        }
    }
}

/// No type chosen (a plain `part p;`).
pub const UNTYPED: u16 = u16::MAX;

/// Diagram editing commands. Indexes refer to lists the window prepared when
/// it opened the menu (types, relationships).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ModelCommand {
    /// Arm the placement tool: the next click in the diagram creates it.
    Place(ElementKind),
    /// Create at the position where the context menu was opened.
    CreateHere(ElementKind),
    /// Add a feature of this kind (ref or not) typed by `types[i]`.
    AddFeature(UsageKind, bool, u16),
    SetType(u16),
    Specialize(u16),
    /// Draw `relations[i]` from the link source to its target.
    Relate(u8),
    Rename,
    EditDeclaration,
    Delete,
    ShowInCode,
}
