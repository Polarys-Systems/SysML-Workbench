//! Window contents: menu bar, header, code editor pane, diagram pane, model
//! tree and the dividers between them, plus input routing, the menu commands,
//! saving and live reloading of the model and config files.

use crate::code_view::{CodeView, Focus, KeyInput, Mods};
use crate::config::{self, Config, FontConfig, MAX_RECENT, ViewConfig, ViewMode};
use crate::theme::Theme;
use crate::diagram::{Diagram, EdgeKind};
use crate::diagram_view::{DiagramView, Fonts, TextMetrics, contains};
use crate::dialog::{self, SaveChoice};
use crate::document::{self, Document};
use crate::menu::{Command, Item, Menu, MenuBar};
use crate::shapes::{PASS_CHROME, Rect, ShapeRenderer, Transform};
use crate::text::{TextRenderer, TextStyle};
use crate::tree_view::{TreeEvent, TreeView};
use std::path::{Path, PathBuf};

#[path = "ui_edit.rs"]
mod edit;
#[path = "ui_chrome.rs"]
mod chrome;
use chrome::glass_card;
use std::time::{Duration, Instant, SystemTime};
use winit::event::MouseButton;
use winit::keyboard::{Key, NamedKey};
use winit::window::CursorIcon;

const POLL_INTERVAL: Duration = Duration::from_millis(400);
const MESSAGE_TIME: Duration = Duration::from_secs(4);
/// Pause in typing before the model is reparsed and the diagram rebuilt.
const REPARSE_DELAY: Duration = Duration::from_millis(150);
/// Without native dialogs, a discarding action must be repeated within this time.
const CONFIRM_TIME: Duration = Duration::from_secs(5);
/// Half width of the divider's grab area, in logical pixels.
const DIVIDER_GRAB: f32 = 4.0;
/// Exported diagram images: pixels per diagram unit (2 = twice the on-screen
/// size at zoom 1), blank border in diagram units, and the most pixels allowed.
const EXPORT_SCALE: f32 = 2.0;
const EXPORT_MARGIN: f32 = 24.0;
const EXPORT_MAX_PIXELS: f32 = 40_000_000.0;
/// Written when settings change and no config file was found.
const CONFIG_NAME: &str = "eagleview.toml";

/// Monospaced fonts offered in Settings > Code Font: (name, file).
const CODE_FONTS: [(&str, &str); 7] = [
    ("Consolas", "consola.ttf"),
    ("Cascadia Mono", "CascadiaMono.ttf"),
    ("Cascadia Code", "CascadiaCode.ttf"),
    ("Courier New", "cour.ttf"),
    ("Lucida Console", "lucon.ttf"),
    ("JetBrains Mono", "JetBrainsMono-Regular.ttf"),
    ("Fira Code", "FiraCode-Regular.ttf"),
];
/// Interface fonts offered in Settings > Interface Font: (name, regular, bold).
const UI_FONTS: [(&str, &str, &str); 7] = [
    ("Segoe UI", "segoeui.ttf", "segoeuib.ttf"),
    ("Arial", "arial.ttf", "arialbd.ttf"),
    ("Calibri", "calibri.ttf", "calibrib.ttf"),
    ("Tahoma", "tahoma.ttf", "tahomabd.ttf"),
    ("Verdana", "verdana.ttf", "verdanab.ttf"),
    ("Georgia", "georgia.ttf", "georgiab.ttf"),
    ("Bahnschrift", "bahnschrift.ttf", "bahnschrift.ttf"),
];

const SHORTCUTS: [(&str, &str); 23] = [
    ("Ctrl+N / Ctrl+O", "New / open a model"),
    ("Ctrl+S / Ctrl+Shift+S", "Save / save as"),
    ("Ctrl+E", "Export the diagram as PNG"),
    ("Ctrl+W", "Close the model"),
    ("F5", "Reload from disk"),
    ("Ctrl+Tab", "Cycle split / diagram / code view"),
    ("Ctrl+B", "Show or hide the model tree"),
    ("Ctrl+= / Ctrl+- / Ctrl+0", "Code font size"),
    ("Alt+letter, F10", "Open the menus"),
    ("Ctrl+F / Ctrl+H", "Find / replace"),
    ("F3 / Shift+F3", "Next / previous match"),
    ("F2", "Rename symbol"),
    ("Shift+Alt+F", "Format document"),
    ("Ctrl+D / Ctrl+Shift+L", "Next occurrence / all occurrences"),
    ("Ctrl+Alt+Up/Down", "Add a cursor"),
    ("Ctrl+/", "Toggle line comment"),
    ("Tree: arrows, Enter", "Move, fold and show elements"),
    ("Diagram: F", "Fit"),
    ("Diagram: L", "Restore the automatic layout"),
    ("Diagram: G / C", "Grid / compartments"),
    ("Diagram: + / -", "Zoom"),
    ("Diagram: drag", "Move nodes, pan the background"),
    ("Esc", "Clear the selection, close menus"),
];

#[derive(Clone, Copy, Default)]
struct Panes {
    /// Left and width of the area shared by the code and diagram panes.
    split_x: f32,
    split_w: f32,
    window: [f32; 2],
    code: Option<Rect>,
    divider: Option<Rect>,
    diagram: Option<Rect>,
    tree_divider: Option<Rect>,
    tree: Option<Rect>,
    /// Right edge of the model tree card (its width is dragged from the left edge).
    tree_right: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Divider {
    Split,
    Tree,
}

/// Actions that would throw away unsaved edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Discard {
    Close,
    Reload,
    Open,
}

pub struct Ui {
    config: Config,
    config_path: Option<PathBuf>,
    config_modified: Option<SystemTime>,
    config_error: Option<String>,
    doc: Document,
    code: CodeView,
    /// Editor version the document was last parsed from.
    parsed_version: u64,
    /// When the pending reparse is due.
    reparse_at: Option<Instant>,
    view: DiagramView,
    tree: TreeView,
    menu: MenuBar,
    show_shortcuts: bool,
    /// Editing the model from the diagram.
    edit: edit::EditState,
    /// Top bar, status bar and floating cards.
    chrome: chrome::ChromeState,
    mode: ViewMode,
    split: f32,
    fonts: Option<Fonts>,
    fonts_dirty: bool,
    needs_measure: bool,
    /// Qualified name of the selected node at the last draw.
    marked: Option<String>,
    cursor: [f32; 2],
    mods: Mods,
    dragging: Option<Divider>,
    panes: Panes,
    message: Option<(String, Instant)>,
    armed: Option<(Discard, Instant)>,
    last_poll: Instant,
    dpi: f32,
    pub frame_ms: f32,
    /// Owner window of native dialogs.
    pub owner: dialog::Owner,
    /// File > Exit was chosen and confirmed.
    pub exit_requested: bool,
    /// PNG file the diagram should be exported to before the next frame.
    export_request: Option<PathBuf>,
}

impl Ui {
    pub fn new(model: PathBuf, config_path: Option<PathBuf>) -> Self {
        let (config, config_error) = match &config_path {
            Some(p) => match Config::load(p) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(e)),
            },
            None => (Config::default(), None),
        };
        let doc = Document::load(model);
        let code = CodeView::new(&doc.source);
        let mut ui = Self {
            mode: config.view.mode,
            split: config.view.split,
            config_modified: config_path.as_deref().and_then(document::modified),
            config,
            config_path,
            config_error,
            parsed_version: code.editor.version(),
            reparse_at: None,
            doc,
            code,
            view: DiagramView::default(),
            tree: TreeView::default(),
            menu: MenuBar::default(),
            show_shortcuts: false,
            edit: edit::EditState::default(),
            chrome: chrome::ChromeState::default(),
            fonts: None,
            fonts_dirty: true,
            needs_measure: true,
            marked: None,
            cursor: [0.0, 0.0],
            mods: Mods::default(),
            dragging: None,
            panes: Panes::default(),
            message: None,
            armed: None,
            last_poll: Instant::now(),
            dpi: 1.0,
            frame_ms: 0.0,
            owner: 0,
            exit_requested: false,
            export_request: None,
        };
        if ui.doc.load_error.is_none() {
            ui.add_recent(ui.doc.path.clone());
        }
        ui
    }

    fn modified(&self) -> bool {
        self.code.editor.is_modified()
    }

    pub fn title(&self) -> String {
        let dot = if self.modified() { " ●" } else { "" };
        format!("EagleView - {}{dot}", self.doc.path.display())
    }

    /// Blur radius of the frosted glass in physical pixels (0: off).
    pub fn backdrop_blur(&self, dpi: f32) -> f32 {
        self.config.theme.size.frame.backdrop_blur * dpi
    }

    /// Linear clear color for the frame.
    pub fn clear_color(&self) -> [f32; 4] {
        self.config.theme.bg.canvas.linear()
    }

    fn divider_at(&self, p: [f32; 2]) -> Option<Divider> {
        let grab = DIVIDER_GRAB * self.dpi;
        let near = |d: Option<Rect>| d.is_some_and(|d| contains([d[0] - grab, d[1], d[2] + grab, d[3]], p));
        if near(self.panes.tree_divider) {
            Some(Divider::Tree)
        } else if near(self.panes.divider) {
            Some(Divider::Split)
        } else {
            None
        }
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        let p = self.cursor;
        if self.menu.is_open() || self.show_shortcuts || self.menu.hit(p) {
            CursorIcon::Default
        } else if self.dragging.is_some() || self.divider_at(p).is_some() {
            CursorIcon::ColResize
        } else if let Some(icon) = self.chrome_cursor_icon(p) {
            icon
        } else if self.placing() && !self.over_chrome(p) && self.panes.diagram.is_some_and(|d| contains(d, p)) {
            CursorIcon::Crosshair
        } else if self.panes.code.is_some() && (self.code.wants_text_cursor(p) || self.code.is_dragging()) {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        }
    }

    fn notify(&mut self, message: impl Into<String>) {
        self.message = Some((message.into(), Instant::now()));
    }

    /// True when `action` may discard the edits: nothing is unsaved, the user
    /// saved or chose to discard them (or, without native dialogs, repeated
    /// the action after being warned).
    fn confirm_discard(&mut self, action: Discard, what: &str) -> bool {
        if !self.modified() {
            return true;
        }
        if dialog::HAS_PICKERS {
            return match dialog::ask_save(self.owner, &self.doc.file_name()) {
                SaveChoice::Save => {
                    self.save();
                    !self.modified()
                }
                SaveChoice::Discard => true,
                SaveChoice::Cancel => false,
            };
        }
        if self.armed.is_some_and(|(a, t)| a == action && t.elapsed() < CONFIRM_TIME) {
            self.armed = None;
            return true;
        }
        self.armed = Some((action, Instant::now()));
        self.notify(format!("Unsaved changes: Ctrl+S saves them, {what} again discards them"));
        false
    }

    /// Window close: false keeps the window open.
    pub fn close_requested(&mut self) -> bool {
        self.confirm_discard(Discard::Close, "close")
    }

    pub fn set_modifiers(&mut self, mods: Mods) {
        self.mods = mods;
    }

    // ---- settings file -----------------------------------------------------------

    /// Writes `key = value` (a TOML literal) into the config file, keeping its
    /// comments. Creates `eagleview.toml` when there is no config file yet.
    fn persist(&mut self, section: &str, key: &str, value: String) {
        let path = self.config_path.clone().unwrap_or_else(|| PathBuf::from(CONFIG_NAME));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let new = config::set_toml_value(&text, section, key, &value);
        if new == text {
            return;
        }
        match std::fs::write(&path, new) {
            Ok(()) => {
                // Our own write: do not reload it as an outside edit.
                self.config_modified = document::modified(&path);
                self.config_path = Some(path);
            }
            Err(e) => self.notify(format!("Could not save settings to {}: {e}", path.display())),
        }
    }

    fn persist_bool(&mut self, section: &str, key: &str, on: bool) {
        self.persist(section, key, on.to_string());
    }

    fn persist_num(&mut self, section: &str, key: &str, v: f32) {
        // Two decimals are plenty for sizes and fractions.
        let v = (v * 100.0).round() / 100.0;
        self.persist(section, key, format!("{v}"));
    }

    fn add_recent(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let recent = &mut self.config.files.recent;
        if recent.first() == Some(&path) {
            return;
        }
        recent.retain(|p| p != &path);
        recent.insert(0, path);
        recent.truncate(MAX_RECENT);
        self.persist_recent();
    }

    fn persist_recent(&mut self) {
        let items: Vec<String> =
            self.config.files.recent.iter().map(|p| config::toml_string(&p.to_string_lossy())).collect();
        self.persist("files", "recent", format!("[{}]", items.join(", ")));
    }

    // ---- files -----------------------------------------------------------------

    pub fn open(&mut self, path: PathBuf) {
        if !self.confirm_discard(Discard::Open, "drop the file") {
            return;
        }
        self.doc = Document::load(path);
        self.after_new_document();
        if self.doc.load_error.is_none() {
            self.add_recent(self.doc.path.clone());
        } else {
            let path = self.doc.path.clone();
            self.config.files.recent.retain(|p| p != &path);
        }
    }

    fn after_new_document(&mut self) {
        self.code.open(&self.doc.source);
        self.parsed_version = self.code.editor.version();
        self.reparse_at = None;
        self.view = DiagramView::default();
        self.tree.reset();
        self.marked = None;
        self.needs_measure = true;
    }

    fn open_dialog(&mut self) {
        let start = (!self.doc.untitled).then(|| self.doc.path.clone());
        if let Some(path) = dialog::open_file(self.owner, start.as_deref()) {
            self.open(path);
        }
    }

    /// File > New and File > Close: an empty, untitled model.
    fn close_document(&mut self) {
        if !self.confirm_discard(Discard::Close, "close") {
            return;
        }
        self.doc = Document::untitled();
        self.after_new_document();
    }

    /// Re-reads the model from disk, keeping the camera, the selection and the cursors.
    fn reload(&mut self) {
        if self.doc.untitled {
            return;
        }
        let selected = self.selected_name();
        self.doc = Document::load(self.doc.path.clone());
        self.code.reload(&self.doc.source);
        self.parsed_version = self.code.editor.version();
        self.reparse_at = None;
        self.view.selected = selected.and_then(|n| self.doc.diagram.node_named(&n));
        self.view.hovered = None;
        self.needs_measure = true;
    }

    fn save(&mut self) {
        if self.doc.untitled {
            self.save_as();
            return;
        }
        let text = self.code.editor.text().to_owned();
        match self.doc.save(&text) {
            Ok(()) => {
                self.code.editor.mark_saved();
                self.armed = None;
                self.notify(format!("Saved {}", self.doc.file_name()));
            }
            Err(e) => self.notify(format!("Could not save {}: {e}", self.doc.path.display())),
        }
    }

    fn save_as(&mut self) {
        if !dialog::HAS_PICKERS {
            self.notify("Save As needs a file dialog, which this platform does not have");
            return;
        }
        let Some(path) = dialog::save_file(self.owner, Some(&self.doc.path)) else { return };
        let text = self.code.editor.text().to_owned();
        match self.doc.save_as(path, &text) {
            Ok(()) => {
                self.code.editor.mark_saved();
                self.armed = None;
                self.save_positions();
                self.add_recent(self.doc.path.clone());
                self.notify(format!("Saved {}", self.doc.file_name()));
            }
            Err(e) => self.notify(format!("Could not save: {e}")),
        }
    }

    /// File > Export Diagram as PNG: asks for the file; the window renders it
    /// before the next frame (see [`Self::draw_export`]).
    fn export_dialog(&mut self) {
        if self.doc.diagram.nodes.is_empty() {
            self.notify("The diagram is empty: nothing to export");
            return;
        }
        let default = self.doc.path.with_extension("png");
        // Without a file dialog the image goes next to the model.
        let path = if dialog::HAS_PICKERS {
            let Some(path) = dialog::save_png(self.owner, Some(&default)) else { return };
            path
        } else {
            default
        };
        self.export_request = Some(path);
    }

    /// Exports to `path` before the next frame (for `--export-png`).
    pub fn request_export(&mut self, path: PathBuf) {
        self.export_request = Some(path);
    }

    pub fn take_export_request(&mut self) -> Option<PathBuf> {
        self.export_request.take()
    }

    /// Queues the whole diagram for an export image, without the selection,
    /// hover highlight or grid, and returns the image size in pixels. Each side
    /// is at most `max_side`; large diagrams are scaled down to fit.
    pub fn draw_export(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, max_side: u32) -> Option<[u32; 2]> {
        self.prepare_fonts_and_layout(text);
        let fonts = self.fonts?;
        let b = self.doc.diagram.bounds()?;
        let (w, h) = (b[2] - b[0] + 2.0 * EXPORT_MARGIN, b[3] - b[1] + 2.0 * EXPORT_MARGIN);
        let max = max_side as f32;
        let scale = EXPORT_SCALE.min(max / w).min(max / h).min((EXPORT_MAX_PIXELS / (w * h)).sqrt());
        let size = [((w * scale).ceil() as u32).clamp(1, max_side), ((h * scale).ceil() as u32).clamp(1, max_side)];
        let mut view = DiagramView::default();
        view.zoom = scale;
        view.pan = [(EXPORT_MARGIN - b[0]) * scale, (EXPORT_MARGIN - b[1]) * scale];
        view.needs_fit = false;
        let mut cfg = self.config.diagram.clone();
        cfg.grid = false;
        let pane = [0.0, 0.0, size[0] as f32, size[1] as f32];
        view.draw(&self.doc.diagram, shapes, text, fonts, &cfg, &self.config.theme, pane, 1.0);
        text.set_clip(None);
        Some(size)
    }

    /// Shows how an export went (`None`: nothing to draw, else the image size
    /// or the error) in the status line and returns that message.
    pub fn export_finished(&mut self, path: &Path, result: Option<Result<[u32; 2], String>>) -> String {
        let message = match result {
            None => "The diagram is empty: nothing to export".to_owned(),
            Some(Ok([w, h])) => format!("Exported {} ({w} × {h} px)", path.display()),
            Some(Err(e)) => format!("Could not export {}: {e}", path.display()),
        };
        self.notify(message.clone());
        message
    }

    fn selected_name(&self) -> Option<String> {
        self.view.selected.and_then(|i| self.doc.diagram.nodes.get(i)).map(|n| n.qualified_name.clone())
    }

    /// Parses the edited text once typing pauses; the diagram follows.
    fn reparse_if_due(&mut self) {
        let version = self.code.editor.version();
        if version == self.parsed_version {
            self.reparse_at = None;
            return;
        }
        let due = *self.reparse_at.get_or_insert_with(|| Instant::now() + REPARSE_DELAY);
        if Instant::now() < due || self.view.is_dragging() {
            return;
        }
        let selected = self.selected_name();
        self.doc.set_source(self.code.editor.text());
        self.parsed_version = version;
        self.reparse_at = None;
        self.view.selected = selected.and_then(|n| self.doc.diagram.node_named(&n));
        self.view.hovered = None;
        self.needs_measure = true;
    }

    fn reload_config(&mut self) {
        let Some(path) = self.config_path.clone() else { return };
        self.config_modified = document::modified(&path);
        match Config::load(&path) {
            Ok(new) => {
                // Only settings that changed in the file override runtime choices.
                if new.view.mode != self.config.view.mode {
                    self.mode = new.view.mode;
                }
                if new.view.split != self.config.view.split {
                    self.split = new.view.split;
                }
                if new.fonts != self.config.fonts {
                    self.fonts_dirty = true;
                }
                self.config = new;
                self.config_error = None;
                self.needs_measure = true;
                self.notify("Settings applied");
            }
            Err(e) => self.config_error = Some(e),
        }
    }

    /// Picks up edits to the model and config files made by other programs.
    pub fn poll_files(&mut self) {
        self.reparse_if_due();
        if self.last_poll.elapsed() < POLL_INTERVAL || self.view.is_dragging() {
            return;
        }
        self.last_poll = Instant::now();
        if self.config_path.as_deref().and_then(document::modified) != self.config_modified {
            self.reload_config();
        }
        if self.doc.changed_on_disk() {
            if self.modified() {
                // Keep the user's edits; warn once for this change.
                self.doc.modified = document::modified(&self.doc.path);
                self.notify("The file changed on disk; F5 reloads it (discarding your edits)");
            } else {
                self.reload();
            }
        }
    }

    fn save_positions(&mut self) {
        if self.doc.untitled {
            return;
        }
        let path = Diagram::layout_path(&self.doc.path);
        match self.doc.diagram.save_positions(&path) {
            Ok(()) => {}
            Err(e) => self.notify(format!("Could not save {}: {e}", path.display())),
        }
    }

    fn reset_layout(&mut self) {
        for n in &mut self.doc.diagram.nodes {
            n.pinned = false;
        }
        self.save_positions();
        self.doc.diagram.auto_layout(&self.config.diagram);
        self.view.needs_fit = true;
        self.notify("Automatic layout restored");
    }

    // ---- fonts -------------------------------------------------------------------

    /// Folders searched for the fonts the Settings menu offers.
    fn font_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.config.fonts.dir.clone()];
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(Path::new(&local).join("Microsoft").join("Windows").join("Fonts"));
        }
        dirs
    }

    /// How to write `file` in the config: a bare name when it is in the
    /// configured folder, else its full path. `None` if it is not installed.
    fn font_value(&self, file: &str) -> Option<String> {
        self.font_dirs().iter().enumerate().find(|(_, d)| d.join(file).is_file()).map(|(i, d)| {
            if i == 0 { file.to_owned() } else { d.join(file).to_string_lossy().into_owned() }
        })
    }

    fn available_code_fonts(&self) -> Vec<(&'static str, String)> {
        CODE_FONTS.iter().filter_map(|(name, file)| Some((*name, self.font_value(file)?))).collect()
    }

    fn available_ui_fonts(&self) -> Vec<(&'static str, String, String)> {
        UI_FONTS
            .iter()
            .filter_map(|(name, regular, bold)| {
                let r = self.font_value(regular)?;
                let b = self.font_value(bold).unwrap_or_else(|| r.clone());
                Some((*name, r, b))
            })
            .collect()
    }

    fn set_code_font(&mut self, value: String) {
        self.config.fonts.mono = value.clone();
        self.persist("fonts", "mono", config::toml_string(&value));
        self.fonts_dirty = true;
    }

    // ---- menus -------------------------------------------------------------------

    fn build_menus(&self) -> Vec<Menu> {
        let v = &self.config.view;
        let d = &self.config.diagram;

        let diagram = self.mode != ViewMode::Outline;

        let recent: Vec<Item> = self
            .config
            .files
            .recent
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let dir = p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                Item::action(format!("{}  {name}    {dir}", i + 1), "", Command::OpenRecent(i))
            })
            .chain(
                (!self.config.files.recent.is_empty())
                    .then(|| [Item::separator(), Item::action("Clear Recent", "", Command::ClearRecent)])
                    .into_iter()
                    .flatten(),
            )
            .collect();
        let file = vec![
            Item::action("New", "Ctrl+N", Command::New),
            Item::action("Open…", "Ctrl+O", Command::Open).enabled(dialog::HAS_PICKERS),
            Item::submenu("Open Recent", recent),
            Item::separator(),
            Item::action("Save", "Ctrl+S", Command::Save),
            Item::action("Save As…", "Ctrl+Shift+S", Command::SaveAs).enabled(dialog::HAS_PICKERS),
            Item::action("Reload from Disk", "F5", Command::Reload).enabled(!self.doc.untitled),
            Item::separator(),
            Item::action("Export Diagram as PNG…", "Ctrl+E", Command::ExportPng)
                .enabled(!self.doc.diagram.nodes.is_empty()),
            Item::separator(),
            Item::action("Close", "Ctrl+W", Command::Close),
            Item::action("Exit", "Alt+F4", Command::Exit),
        ];
        let edit = vec![
            Item::action("Undo", "Ctrl+Z", Command::Undo),
            Item::action("Redo", "Ctrl+Y", Command::Redo),
            Item::separator(),
            Item::action("Cut", "Ctrl+X", Command::Cut),
            Item::action("Copy", "Ctrl+C", Command::Copy),
            Item::action("Paste", "Ctrl+V", Command::Paste),
            Item::action("Select All", "Ctrl+A", Command::SelectAll),
            Item::separator(),
            Item::action("Find", "Ctrl+F", Command::Find),
            Item::action("Replace", "Ctrl+H", Command::Replace),
            Item::separator(),
            Item::action("Rename Symbol", "F2", Command::Rename),
            Item::action("Format Document", "Shift+Alt+F", Command::Format),
            Item::action("Toggle Line Comment", "Ctrl+/", Command::ToggleComment),
        ];
        let view = vec![
            Item::radio("Split: Code and Diagram", Command::Mode(ViewMode::Split), self.mode == ViewMode::Split),
            Item::radio("Diagram Only", Command::Mode(ViewMode::Diagram), self.mode == ViewMode::Diagram),
            Item::radio("Code Only", Command::Mode(ViewMode::Outline), self.mode == ViewMode::Outline),
            Item::separator(),
            Item::check("Model Tree", "Ctrl+B", Command::ToggleTree, v.tree),
            Item::action("Expand All in Tree", "", Command::ExpandTree).enabled(v.tree),
            Item::action("Collapse All in Tree", "", Command::CollapseTree).enabled(v.tree),
            Item::separator(),
            Item::action("Zoom In", "+", Command::ZoomIn).enabled(diagram),
            Item::action("Zoom Out", "-", Command::ZoomOut).enabled(diagram),
            Item::action("Fit Diagram", "F", Command::Fit).enabled(diagram),
            Item::separator(),
            Item::check("Frame Time", "", Command::ToggleFrameTime, v.show_frame_time),
        ];
        let e = &d.edges;
        let edges = vec![
            Item::check("Specialization", "", Command::ToggleEdge(EdgeKind::Specialization), e.specialization),
            Item::check("Typing", "", Command::ToggleEdge(EdgeKind::Typing), e.typing),
            Item::check("Composition", "", Command::ToggleEdge(EdgeKind::Composition), e.composition),
            Item::check("Satisfy", "", Command::ToggleEdge(EdgeKind::Satisfy), e.satisfy),
            Item::check("Connection", "", Command::ToggleEdge(EdgeKind::Connection), e.connection),
            Item::check("Flow", "", Command::ToggleEdge(EdgeKind::Flow), e.flow),
            Item::check("Allocation", "", Command::ToggleEdge(EdgeKind::Allocation), e.allocation),
            Item::check("Dependency", "", Command::ToggleEdge(EdgeKind::Dependency), e.dependency),
        ];
        let diagram_menu = vec![
            Item::check("Grid", "G", Command::ToggleGrid, d.grid),
            Item::check("Snap to Grid", "", Command::ToggleSnap, d.snap),
            Item::check("Compartments", "C", Command::ToggleCompartments, d.compartments),
            Item::check("Edge Labels", "", Command::ToggleEdgeLabels, d.edge_labels),
            Item::submenu("Relationships Shown", edges),
            Item::separator(),
            Item::action("Restore Automatic Layout", "L", Command::ResetLayout),
        ];

        let themes: Vec<Item> = config::theme_names(self.config_path.as_deref())
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let on = v.theme == name;
                Item::radio(pretty_name(&name), Command::Theme(i), on)
            })
            .collect();
        let mono = &self.config.fonts.mono;
        let mut code_fonts: Vec<Item> = self
            .available_code_fonts()
            .into_iter()
            .enumerate()
            .map(|(i, (name, value))| Item::radio(name, Command::CodeFont(i), &value == mono))
            .collect();
        code_fonts.push(Item::separator());
        code_fonts.push(Item::action("Other Font File…", "", Command::PickCodeFont).enabled(dialog::HAS_PICKERS));
        let body = &self.config.fonts.body;
        let ui_fonts: Vec<Item> = self
            .available_ui_fonts()
            .into_iter()
            .enumerate()
            .map(|(i, (name, regular, _))| Item::radio(name, Command::UiFont(i), &regular == body))
            .collect();
        let sizes = vec![
            Item::action(format!("Code: Larger  ({})", v.outline_font_size), "Ctrl+=", Command::CodeSize(1)),
            Item::action("Code: Smaller", "Ctrl+-", Command::CodeSize(-1)),
            Item::separator(),
            Item::action(format!("Diagram: Larger  ({})", d.font_size), "", Command::DiagramSize(1)),
            Item::action("Diagram: Smaller", "", Command::DiagramSize(-1)),
            Item::separator(),
            Item::action(format!("Interface: Larger  ({})", v.ui_font_size), "", Command::UiSize(1)),
            Item::action("Interface: Smaller", "", Command::UiSize(-1)),
            Item::separator(),
            Item::action("Reset All Sizes", "Ctrl+0", Command::ResetSizes),
        ];
        let settings = vec![
            Item::submenu("Theme", themes),
            Item::submenu("Code Font", code_fonts),
            Item::submenu("Interface Font", ui_fonts),
            Item::submenu("Font Sizes", sizes),
            Item::separator(),
            Item::action("Open Settings File", "", Command::EditSettings),
            Item::action("Reload Settings", "", Command::ReloadSettings).enabled(self.config_path.is_some()),
        ];
        let help = vec![
            Item::action("Keyboard Shortcuts", "", Command::Shortcuts),
            Item::action("About EagleView", "", Command::About),
        ];

        vec![
            Menu { title: "File", items: file },
            Menu { title: "Edit", items: edit },
            Menu { title: "View", items: view },
            Menu { title: "Diagram", items: diagram_menu },
            Menu { title: "Settings", items: settings },
            Menu { title: "Help", items: help },
        ]
    }

    /// Sends an editing shortcut to the code pane, showing it if needed.
    fn code_shortcut(&mut self, key: Key, ctrl: bool, shift: bool, alt: bool, editor_focus: bool) {
        if self.mode == ViewMode::Diagram {
            self.mode = ViewMode::Split;
        }
        self.code.active = true;
        self.tree.active = false;
        if editor_focus {
            self.code.focus = Focus::Editor;
        }
        let input = KeyInput { key: &key, text: None, mods: Mods { ctrl, shift, alt } };
        self.code.key(&input);
        if let Some(n) = self.code.notice.take() {
            self.notify(n);
        }
    }

    fn zoom_center(&mut self, factor: f32) {
        if let Some(pane) = self.panes.diagram {
            let a = self.view.focus_in(pane);
            let c = [(a[0] + a[2]) * 0.5, (a[1] + a[3]) * 0.5];
            self.view.zoom_at(c, factor, pane, self.dpi, &self.config.diagram);
        }
    }

    fn set_mode(&mut self, mode: ViewMode) {
        self.mode = mode;
        let name = match mode {
            ViewMode::Split => "split",
            ViewMode::Diagram => "diagram",
            ViewMode::Outline => "code",
        };
        self.config.view.mode = mode;
        self.persist("view", "mode", config::toml_string(name));
    }

    fn change_size(&mut self, which: Command) {
        let step = |v: &mut f32, d: i8, lo: f32, hi: f32| *v = (*v + d as f32).clamp(lo, hi);
        match which {
            Command::CodeSize(d) => {
                step(&mut self.config.view.outline_font_size, d, 6.0, 48.0);
                self.persist_num("view", "outline_font_size", self.config.view.outline_font_size);
            }
            Command::DiagramSize(d) => {
                step(&mut self.config.diagram.font_size, d, 6.0, 40.0);
                self.persist_num("diagram", "font_size", self.config.diagram.font_size);
                self.needs_measure = true;
            }
            Command::UiSize(d) => {
                step(&mut self.config.view.ui_font_size, d, 8.0, 28.0);
                self.persist_num("view", "ui_font_size", self.config.view.ui_font_size);
            }
            _ => {
                let def = ViewConfig::default();
                self.config.view.outline_font_size = def.outline_font_size;
                self.config.view.ui_font_size = def.ui_font_size;
                self.config.diagram.font_size = crate::config::DiagramConfig::default().font_size;
                self.persist_num("view", "outline_font_size", def.outline_font_size);
                self.persist_num("view", "ui_font_size", def.ui_font_size);
                self.persist_num("diagram", "font_size", self.config.diagram.font_size);
                self.needs_measure = true;
                self.notify("Font sizes reset");
            }
        }
    }

    fn run(&mut self, cmd: Command) {
        let ch = |c: &str| Key::Character(c.into());
        match cmd {
            Command::New | Command::Close => self.close_document(),
            Command::Open => self.open_dialog(),
            Command::OpenRecent(i) => {
                if let Some(p) = self.config.files.recent.get(i).cloned() {
                    if p.is_file() {
                        self.open(p);
                    } else {
                        self.notify(format!("{} no longer exists", p.display()));
                        self.config.files.recent.remove(i);
                        self.persist_recent();
                    }
                }
            }
            Command::ClearRecent => {
                self.config.files.recent.clear();
                self.persist_recent();
            }
            Command::Save => self.save(),
            Command::SaveAs => self.save_as(),
            Command::ExportPng => self.export_dialog(),
            Command::Reload => {
                if self.confirm_discard(Discard::Reload, "F5") {
                    self.reload();
                    self.notify("Reloaded from disk");
                }
            }
            Command::Exit => {
                if self.confirm_discard(Discard::Close, "Exit") {
                    self.exit_requested = true;
                }
            }
            Command::Undo => self.code_shortcut(ch("z"), true, false, false, true),
            Command::Redo => self.code_shortcut(ch("y"), true, false, false, true),
            Command::Cut => self.code_shortcut(ch("x"), true, false, false, true),
            Command::Copy => self.code_shortcut(ch("c"), true, false, false, true),
            Command::Paste => self.code_shortcut(ch("v"), true, false, false, true),
            Command::SelectAll => self.code_shortcut(ch("a"), true, false, false, true),
            Command::Find => self.code_shortcut(ch("f"), true, false, false, true),
            Command::Replace => self.code_shortcut(ch("h"), true, false, false, true),
            Command::Rename => self.code_shortcut(Key::Named(NamedKey::F2), false, false, false, true),
            Command::ToggleComment => self.code_shortcut(ch("/"), true, false, false, true),
            Command::Format => {
                self.code.format();
                if let Some(n) = self.code.notice.take() {
                    self.notify(n);
                }
            }
            Command::Mode(m) => self.set_mode(m),
            Command::ToggleTree => {
                self.config.view.tree = !self.config.view.tree;
                if !self.config.view.tree {
                    self.tree.active = false;
                }
                self.persist_bool("view", "tree", self.config.view.tree);
            }
            Command::ExpandTree => self.tree.expand_all(&self.doc.tree, true),
            Command::CollapseTree => self.tree.expand_all(&self.doc.tree, false),
            Command::ToggleFrameTime => {
                self.config.view.show_frame_time = !self.config.view.show_frame_time;
                self.persist_bool("view", "show_frame_time", self.config.view.show_frame_time);
            }
            Command::ZoomIn => self.zoom_center(1.25),
            Command::ZoomOut => self.zoom_center(0.8),
            Command::Fit => self.view.needs_fit = true,
            Command::ToggleGrid => {
                self.config.diagram.grid = !self.config.diagram.grid;
                self.persist_bool("diagram", "grid", self.config.diagram.grid);
            }
            Command::ToggleSnap => {
                self.config.diagram.snap = !self.config.diagram.snap;
                self.persist_bool("diagram", "snap", self.config.diagram.snap);
            }
            Command::ToggleCompartments => {
                self.config.diagram.compartments = !self.config.diagram.compartments;
                self.needs_measure = true;
                self.persist_bool("diagram", "compartments", self.config.diagram.compartments);
            }
            Command::ToggleEdgeLabels => {
                self.config.diagram.edge_labels = !self.config.diagram.edge_labels;
                self.persist_bool("diagram", "edge_labels", self.config.diagram.edge_labels);
            }
            Command::ToggleEdge(kind) => {
                let e = &mut self.config.diagram.edges;
                let (flag, key) = match kind {
                    EdgeKind::Specialization => (&mut e.specialization, "specialization"),
                    EdgeKind::Typing => (&mut e.typing, "typing"),
                    EdgeKind::Composition => (&mut e.composition, "composition"),
                    EdgeKind::Satisfy => (&mut e.satisfy, "satisfy"),
                    EdgeKind::Dependency => (&mut e.dependency, "dependency"),
                    EdgeKind::Connection => (&mut e.connection, "connection"),
                    EdgeKind::Flow => (&mut e.flow, "flow"),
                    EdgeKind::Allocation => (&mut e.allocation, "allocation"),
                };
                *flag = !*flag;
                let on = *flag;
                self.persist_bool("diagram.edges", key, on);
            }
            Command::ResetLayout => self.reset_layout(),
            Command::Theme(i) => {
                if let Some(name) = config::theme_names(self.config_path.as_deref()).get(i).cloned() {
                    self.persist("view", "theme", config::toml_string(&name));
                    self.reload_config();
                    if self.config_error.is_none() {
                        self.notify(format!("Theme: {}", pretty_name(&name)));
                    }
                }
            }
            Command::CodeFont(i) => {
                if let Some((name, value)) = self.available_code_fonts().get(i).cloned() {
                    self.set_code_font(value);
                    self.notify(format!("Code font: {name}"));
                }
            }
            Command::PickCodeFont => {
                let dir = self.config.fonts.dir.clone();
                if let Some(p) = dialog::pick_font(self.owner, Some(&dir)) {
                    let value = match p.parent() {
                        Some(d) if d == dir => p.file_name().unwrap().to_string_lossy().into_owned(),
                        _ => p.to_string_lossy().into_owned(),
                    };
                    self.set_code_font(value);
                }
            }
            Command::UiFont(i) => {
                if let Some((name, regular, bold)) = self.available_ui_fonts().get(i).cloned() {
                    self.config.fonts.body = regular.clone();
                    self.config.fonts.title = bold.clone();
                    self.persist("fonts", "body", config::toml_string(&regular));
                    self.persist("fonts", "title", config::toml_string(&bold));
                    self.fonts_dirty = true;
                    self.notify(format!("Interface font: {name}"));
                }
            }
            Command::CodeSize(_) | Command::DiagramSize(_) | Command::UiSize(_) | Command::ResetSizes => self.change_size(cmd),
            Command::EditSettings => {
                if self.config_path.is_none() {
                    // Create the file so there is something to edit.
                    self.persist_bool("view", "tree", self.config.view.tree);
                }
                if let Some(p) = self.config_path.clone() {
                    let p = std::path::absolute(&p).unwrap_or(p);
                    if !dialog::open_external(self.owner, &p) {
                        self.notify(format!("Could not open {}", p.display()));
                    }
                }
            }
            Command::ReloadSettings => self.reload_config(),
            Command::Shortcuts => self.show_shortcuts = true,
            Command::GotoProblem(i) => {
                if let Some(offset) = self.doc.diagnostics.get(usize::from(i)).map(|d| d.span.start) {
                    if self.mode == ViewMode::Diagram {
                        self.mode = ViewMode::Split;
                    }
                    self.code.goto(offset);
                    self.code.active = true;
                    self.tree.active = false;
                }
            }
            Command::About => self.notify(concat!(
                "EagleView ",
                env!("CARGO_PKG_VERSION"),
                " — a SysML v2 editor and viewer, rendered with Vulkan"
            )),
            Command::Model(m) => self.run_model(m),
        }
    }

    // ---- input -----------------------------------------------------------------

    pub fn cursor_moved(&mut self, p: [f32; 2]) {
        self.cursor = p;
        self.menu.motion(p);
        if self.menu.is_open() || self.show_shortcuts {
            return;
        }
        match self.dragging {
            Some(Divider::Split) => {
                if self.panes.split_w > 0.0 {
                    self.split = ((p[0] - self.panes.split_x) / self.panes.split_w).clamp(0.12, 0.85);
                }
                return;
            }
            Some(Divider::Tree) => {
                let w = (self.panes.tree_right - p[0]) / self.dpi;
                let min = self.config.theme.size.frame.pane_min_width;
                let max = (self.panes.window[0] / self.dpi * 0.6).max(min);
                self.config.view.tree_width = w.clamp(min, max);
                return;
            }
            None => {}
        }
        if self.code.is_dragging() {
            self.code.motion(p);
            return;
        }
        if self.panes.tree.is_some_and(|r| contains(r, p)) {
            self.tree.motion(p);
        } else {
            self.tree.leave();
        }
        if self.edit.busy() {
            self.edit_motion(p);
            return;
        }
        if let Some(pane) = self.panes.diagram {
            // Drags continue under the cards; hovering does not reach through them.
            if self.view.is_dragging() || !self.over_chrome(p) {
                self.view.motion(&mut self.doc.diagram, p, pane, self.dpi, &self.config.diagram);
            } else {
                self.view.hovered = None;
            }
        }
        self.edit_motion(p);
    }

    pub fn cursor_left(&mut self) {
        self.view.hovered = None;
        self.tree.leave();
    }

    pub fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
        let p = self.cursor;
        if !pressed {
            match self.dragging.take() {
                Some(Divider::Split) => self.persist_num("view", "split", self.split),
                Some(Divider::Tree) => self.persist_num("view", "tree_width", self.config.view.tree_width.round()),
                None => {}
            }
            self.code.release();
            if self.view.release() {
                self.save_positions();
            }
            if matches!(button, MouseButton::Left | MouseButton::Right) {
                self.edit_release(button == MouseButton::Left, p);
            }
            return;
        }
        if self.show_shortcuts {
            self.show_shortcuts = false;
            return;
        }
        if button == MouseButton::Left || self.menu.is_open() {
            let (consumed, cmd) = self.menu.press(p);
            if let Some(cmd) = cmd {
                self.run(cmd);
            }
            if consumed {
                return;
            }
        }
        let left = button == MouseButton::Left;
        if self.press_field(p) {
            return;
        }
        // Floating chrome first, then the card resize grips, the cards, and
        // last the canvas behind them.
        if self.chrome_press(p, left) {
            return;
        }
        if left && let Some(d) = self.divider_at(p) {
            self.dragging = Some(d);
            return;
        }
        match self.card_at(p) {
            Some(chrome::Card::Tree) => {
                self.code.active = false;
                if left
                    && self.panes.tree.is_some_and(|r| contains(r, p))
                    && let Some(TreeEvent::Select(row)) = self.tree.press(&self.doc.tree, p)
                {
                    self.show_row(row);
                }
                return;
            }
            Some(chrome::Card::Code) => {
                self.tree.active = false;
                if left && self.panes.code.is_some_and(|r| contains(r, p)) && self.code.press(p, self.mods) {
                    self.select_at_cursor();
                }
                return;
            }
            None => {}
        }
        if matches!(button, MouseButton::Left | MouseButton::Right) && self.edit_press(left, p) {
            self.code.active = false;
            self.tree.active = false;
            return;
        }
        if let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) {
            self.code.active = false;
            self.tree.active = false;
            let before = self.view.selected;
            self.view.press(&self.doc.diagram, p, pane, self.dpi, button != MouseButton::Left);
            if self.view.selected != before && self.view.selected.is_some() {
                let name = self.selected_name();
                self.marked = name.clone();
                let offset = name.as_deref().and_then(|n| self.doc.offset_of(n));
                self.code.mark_offset(offset, true);
                self.sync_tree_to(name.as_deref());
            }
        }
    }

    /// Selects the tree row of the element named `qualified_name`.
    fn sync_tree_to(&mut self, qualified_name: Option<&str>) {
        let Some(name) = qualified_name else { return };
        let row = self.doc.elements.iter().position(|e| e.qualified_name.as_deref() == Some(name));
        if let Some(r) = row.and_then(|r| self.doc.tree.get(r)) {
            self.tree.select_key(Some(r.key.clone()));
        }
    }

    /// Shows the element of tree row `row`: the code cursor goes to its
    /// declaration and the diagram selects and centers its node.
    fn show_row(&mut self, row: usize) {
        let Some(el) = self.doc.elements.get(row) else { return };
        let (offset, name) = (el.span.start, el.qualified_name.clone());
        self.code.goto(offset);
        self.code.active = false;
        let node = name.as_deref().and_then(|q| self.doc.diagram.node_for(q));
        self.view.selected = node;
        self.marked = self.selected_name();
        self.code.mark_offset(Some(offset), true);
        if let (Some(n), Some(pane)) = (node, self.panes.diagram) {
            self.view.center_on_point(self.doc.diagram.nodes[n].center(), pane, self.dpi);
        }
    }

    /// Selects the diagram node and tree row for the element under the text cursor.
    fn select_at_cursor(&mut self) {
        let head = self.code.editor.primary().head;
        let node = self.doc.element_at(head).and_then(|q| self.doc.diagram.node_for(q));
        self.view.selected = node;
        self.marked = self.selected_name();
        self.code.mark_offset(self.marked.as_deref().and_then(|n| self.doc.offset_of(n)), false);
        if let (Some(n), Some(pane)) = (node, self.panes.diagram) {
            self.view.center_on_point(self.doc.diagram.nodes[n].center(), pane, self.dpi);
        }
        if let Some(r) = self.doc.row_at(head).and_then(|r| self.doc.tree.get(r)) {
            self.tree.select_key(Some(r.key.clone()));
        }
    }

    /// `lines` > 0 scrolls up / zooms in.
    pub fn wheel(&mut self, lines: f32) {
        let p = self.cursor;
        if self.menu.is_open() || self.show_shortcuts {
            return;
        }
        match self.card_at(p) {
            Some(chrome::Card::Tree) => {
                self.tree.wheel(lines);
                self.tree.motion(p);
            }
            Some(chrome::Card::Code) => self.code.wheel(lines, self.mods.shift),
            None if self.over_chrome(p) => {}
            None => {
                if let Some(pane) = self.panes.diagram.filter(|&r| contains(r, p)) {
                    let factor = self.config.diagram.zoom_speed.max(1.001).powf(lines);
                    self.view.zoom_at(p, factor, pane, self.dpi, &self.config.diagram);
                }
            }
        }
    }

    pub fn key(&mut self, key: &Key, text: Option<&str>) {
        let input = KeyInput { key, text, mods: self.mods };
        let m = self.mods;
        let ch = match key {
            Key::Character(c) => c.to_lowercase(),
            _ => String::new(),
        };
        if self.show_shortcuts {
            self.show_shortcuts = false;
            return;
        }
        // An open menu takes every key.
        if self.menu.is_open() {
            let (_, cmd) = self.menu.key(key);
            if let Some(cmd) = cmd {
                self.run(cmd);
            }
            return;
        }
        // The diagram's inline field and editing tools.
        if self.edit_key(key, text) {
            return;
        }
        // Alt+letter opens a menu, unless a text field other than the editor has the keyboard.
        let editor_has_keys = self.code.focus == Focus::Editor || !self.code.active;
        if m.alt && !m.ctrl && !m.shift && !ch.is_empty() && editor_has_keys && self.menu.open_mnemonic(&ch) {
            return;
        }
        if *key == Key::Named(NamedKey::F10) && !m.ctrl {
            self.menu.open_mnemonic("f");
            return;
        }
        // Shortcuts that work everywhere.
        if m.ctrl && !m.alt {
            let cmd = match ch.as_str() {
                "s" if m.shift => Some(Command::SaveAs),
                "s" => Some(Command::Save),
                "e" => Some(Command::ExportPng),
                "o" => Some(Command::Open),
                "n" => Some(Command::New),
                "w" => Some(Command::Close),
                "b" => Some(Command::ToggleTree),
                "=" | "+" => Some(Command::CodeSize(1)),
                "-" => Some(Command::CodeSize(-1)),
                "0" => Some(Command::ResetSizes),
                _ => None,
            };
            if let Some(cmd) = cmd {
                self.run(cmd);
                return;
            }
        }
        if m.ctrl && *key == Key::Named(NamedKey::Tab) {
            self.mode = self.mode.next();
            return;
        }
        if *key == Key::Named(NamedKey::F5) {
            self.run(Command::Reload);
            return;
        }
        if self.tree.active && self.panes.tree.is_some() {
            let (handled, event) = self.tree.key(&self.doc.tree, key);
            if let Some(TreeEvent::Select(row)) = event {
                self.show_row(row);
            }
            if handled {
                return;
            }
            if *key == Key::Named(NamedKey::Escape) {
                self.tree.active = false;
            }
            return;
        }
        let code_visible = self.panes.code.is_some();
        if self.mode == ViewMode::Outline {
            self.code.active = true;
        }
        if code_visible && self.code.active {
            if self.code.key(&input) {
                if let Some(n) = self.code.notice.take() {
                    self.notify(n);
                }
                return;
            }
            if let Some(n) = self.code.notice.take() {
                self.notify(n);
            }
            if *key == Key::Named(NamedKey::Escape) {
                self.view.selected = None;
            }
            return;
        }
        self.diagram_key(key);
    }

    fn diagram_key(&mut self, key: &Key) {
        if self.diagram_edit_key(key) {
            return;
        }
        if self.mods.ctrl || self.mods.alt {
            return;
        }
        match key {
            Key::Named(NamedKey::Tab) => self.mode = self.mode.next(),
            Key::Named(NamedKey::Escape) => self.view.selected = None,
            Key::Character(c) => match c.as_str() {
                "f" | "F" => self.view.needs_fit = true,
                "l" | "L" => self.reset_layout(),
                "g" | "G" => self.config.diagram.grid = !self.config.diagram.grid,
                "c" | "C" => {
                    self.config.diagram.compartments = !self.config.diagram.compartments;
                    self.needs_measure = true;
                }
                "+" | "=" => self.zoom_center(1.25),
                "-" => self.zoom_center(0.8),
                _ => {}
            },
            _ => {}
        }
    }

    // ---- drawing -----------------------------------------------------------------

    fn prepare_fonts_and_layout(&mut self, text: &mut TextRenderer) {
        if self.fonts_dirty {
            self.fonts_dirty = false;
            let (fonts, errors) = load_fonts(text, &self.config.fonts);
            if fonts.is_some() || self.fonts.is_none() {
                self.fonts = fonts;
            }
            if !errors.is_empty() {
                self.notify(errors.join("; "));
            }
            self.needs_measure = true;
        }
        if let (true, Some(fonts)) = (self.needs_measure, self.fonts) {
            self.needs_measure = false;
            let cfg = &self.config.diagram;
            let mut metrics = TextMetrics { text, fonts, font_size: cfg.font_size, sizes: &self.config.theme.size.diagram };
            self.doc.diagram.measure(cfg, &mut metrics);
            self.doc.diagram.auto_layout(cfg);
            if let Some(s) = self.view.selected
                && s >= self.doc.diagram.nodes.len()
            {
                self.view.selected = None;
            }
        }
    }

    pub fn draw(&mut self, shapes: &mut ShapeRenderer, text: &mut TextRenderer, width: f32, height: f32, dpi: f32) {
        self.dpi = dpi;
        self.prepare_fonts_and_layout(text);
        let Some(fonts) = self.fonts else { return };
        if self.message.as_ref().is_some_and(|(_, t)| t.elapsed() > MESSAGE_TIME) {
            self.message = None;
        }
        if let Some(n) = self.code.notice.take() {
            self.notify(n);
        }
        // Keep the code pane's marked line on the selected element as the text changes.
        let selected = self.selected_name();
        if selected != self.marked {
            self.marked = selected.clone();
            let offset = selected.as_deref().and_then(|n| self.doc.offset_of(n));
            self.code.mark_offset(offset, true);
            self.sync_tree_to(selected.as_deref());
        } else if self.reparse_at.is_none() {
            let offset = selected.as_deref().and_then(|n| self.doc.offset_of(n));
            self.code.mark_offset(offset, false);
        }
        let menus = self.build_menus();
        self.menu.set_menus(menus);
        let theme = self.config.theme.clone();
        self.layout_panes(width, height, dpi);

        // ---- canvas: the diagram fills the window behind everything ----
        shapes.begin_layer(Transform::IDENTITY, None);
        // Ambient glows for the glass cards to pick up.
        let glass = &theme.glass;
        for (c, cx, cy, rx, ry) in [(glass.glow, 0.42, 0.22, 0.34, 0.30), (glass.glow_alt, 0.70, 0.88, 0.34, 0.30)] {
            if c.0[3] > 0.0 {
                let (x, y, w, h) = (width * cx, height * cy, width * rx, height * ry);
                let r = [x - w * 0.5, y - h * 0.5, x + w * 0.5, y + h * 0.5];
                shapes.shadow(r, c.0, (w.min(h)) * 0.5, w.max(h) * 0.6);
            }
        }
        if let Some(pane) = self.panes.diagram {
            self.view.draw(&self.doc.diagram, shapes, text, fonts, &self.config.diagram, &theme, pane, dpi);
        }
        text.set_clip(None);

        // ---- chrome: cards, floating controls, bars ----
        shapes.begin_pass(PASS_CHROME);
        text.begin_pass(PASS_CHROME);
        self.draw_cards(shapes, text, fonts, &theme, dpi);
        self.draw_diagram_controls(shapes, text, fonts, &theme, dpi);
        self.draw_status_bar(shapes, text, fonts, &theme, dpi);
        let menu_strip = self.draw_top_bar(shapes, text, fonts, &theme, dpi);

        // Diagram editing aids (the inline field goes to the overlay pass),
        // then the menus and the shortcuts sheet on top of everything.
        self.draw_edit_overlay(shapes, text, fonts, &theme, dpi);
        shapes.begin_layer(Transform::IDENTITY, None);
        self.menu.draw(shapes, text, fonts, self.config.view.ui_font_size, &theme, menu_strip, [width, height], dpi);
        if self.show_shortcuts {
            self.draw_shortcuts(shapes, text, fonts, &theme, width, height, dpi);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_shortcuts(
        &self,
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        theme: &Theme,
        width: f32,
        height: f32,
        dpi: f32,
    ) {
        shapes.begin_overlay();
        text.begin_overlay();
        shapes.rect([0.0, 0.0, width, height], theme.node.shadow.alpha(1.5), 0.0);
        let size = self.config.view.ui_font_size * dpi;
        let lh = text.line_metrics(fonts.body, size).line_height;
        let frame = &theme.size.frame;
        let row = frame.dialog_row_height.px(dpi, size, lh).round();
        let pad = (frame.dialog_pad * dpi).round();
        let key_w = SHORTCUTS.iter().map(|(k, _)| text.measure(fonts.title, size, k, None)[0]).fold(0.0, f32::max);
        let what_w = SHORTCUTS.iter().map(|(_, w)| text.measure(fonts.body, size, w, None)[0]).fold(0.0, f32::max);
        let title_size = size * frame.dialog_title_scale;
        let title_lh = text.line_metrics(fonts.title, title_size).line_height;
        let w = (key_w + what_w + pad * 4.0).min(width - 2.0 * pad);
        let h = (title_lh + pad * 3.0 + row * SHORTCUTS.len() as f32).min(height - 2.0 * pad);
        let x = ((width - w) * 0.5).round();
        let y = ((height - h) * 0.5).round().max(pad);
        let r = [x, y, x + w, y + h];
        let radius = frame.dialog_radius * dpi;
        shapes.shadow([r[0], r[1] + 10.0 * dpi, r[2], r[3] + 10.0 * dpi], theme.glass.shadow.0, radius, 30.0 * dpi);
        shapes.bordered_rect(r, theme.bg.panel.0, theme.glass.border.0, radius, dpi.max(1.0));
        text.set_clip(Some(r));
        text.draw(TextStyle::new(fonts.title, title_size, theme.text.0), [x + pad, y + pad], "Keyboard Shortcuts");
        let mut ry = y + pad * 2.0 + title_lh;
        for (k, what) in SHORTCUTS {
            text.draw(TextStyle::new(fonts.title, size, theme.text.0), [x + pad, ry], k);
            text.draw(TextStyle::new(fonts.body, size, theme.text_muted.0), [x + pad * 2.0 + key_w, ry], what);
            ry += row;
        }
        text.set_clip(None);
    }
}

/// `github-light` → `GitHub Light`.
fn pretty_name(name: &str) -> String {
    name.split(['-', '_'])
        .map(|w| match w {
            "github" => "GitHub".to_owned(),
            _ => {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Loads the configured fonts; a face that fails falls back to one that loaded.
fn load_fonts(text: &mut TextRenderer, cfg: &FontConfig) -> (Option<Fonts>, Vec<String>) {
    let mut errors = Vec::new();
    let mut load = |file: &str| match text.load_font(cfg.path(file)) {
        Ok(id) => Some(id),
        Err(e) => {
            errors.push(format!("font {file}: {e}"));
            None
        }
    };
    let (title, body, mono) = (load(&cfg.title), load(&cfg.body), load(&cfg.mono));
    let fonts = body.or(title).or(mono).map(|any| Fonts {
        title: title.unwrap_or(any),
        body: body.unwrap_or(any),
        mono: mono.unwrap_or(any),
    });
    (fonts, errors)
}
