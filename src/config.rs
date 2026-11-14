//! User settings: view options, theme and fonts, read from a TOML file.
//! The theme tokens themselves live in `theme.rs`.
//!
//! Every field has a default, so a config file only needs the values it changes.
//! The file is watched and re-applied while the app runs. See `eagleview.toml`.

use serde::Deserialize;
use std::path::{Path, PathBuf};

pub use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    /// The code editor alone (`"code"` is accepted too).
    #[serde(alias = "code")]
    Outline,
    Diagram,
    Split,
}

impl ViewMode {
    pub fn next(self) -> Self {
        match self {
            ViewMode::Split => ViewMode::Diagram,
            ViewMode::Diagram => ViewMode::Outline,
            ViewMode::Outline => ViewMode::Split,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub view: ViewConfig,
    pub diagram: DiagramConfig,
    pub theme: Theme,
    pub fonts: FontConfig,
    pub files: FilesConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewConfig {
    /// `"split"`, `"diagram"` or `"outline"`.
    pub mode: ViewMode,
    /// Outline width as a fraction of the window in split mode.
    pub split: f32,
    pub outline_font_size: f32,
    pub title_font_size: f32,
    pub status_font_size: f32,
    pub show_frame_time: bool,
    /// Diagnostics listed under the header.
    pub max_diagnostics: usize,
    /// Theme preset: `themes/<name>.toml` next to the config file. Colors in
    /// this file's `[theme]` sections override the preset's.
    pub theme: String,
    /// Model tree panel on the right.
    pub tree: bool,
    /// Model tree panel width in logical pixels.
    pub tree_width: f32,
    /// Menu bar, menus and model tree.
    pub ui_font_size: f32,
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            mode: ViewMode::Split,
            split: 0.34,
            outline_font_size: 14.0,
            title_font_size: 20.0,
            status_font_size: 12.5,
            show_frame_time: true,
            max_diagnostics: 4,
            theme: String::new(),
            tree: true,
            tree_width: 300.0,
            ui_font_size: 13.0,
        }
    }
}

/// Which relationships the diagram draws as edges.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EdgeVisibility {
    pub specialization: bool,
    pub typing: bool,
    pub composition: bool,
    pub satisfy: bool,
    pub dependency: bool,
    /// `connect a to b`, `interface … connect`
    pub connection: bool,
    /// `flow from a to b`
    pub flow: bool,
    /// `allocate a to b`
    pub allocation: bool,
}

impl Default for EdgeVisibility {
    fn default() -> Self {
        Self {
            specialization: true,
            typing: true,
            composition: true,
            satisfy: true,
            dependency: true,
            connection: true,
            flow: true,
            allocation: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagramConfig {
    pub font_size: f32,
    /// Show owned features inside each node.
    pub compartments: bool,
    pub max_compartment_lines: usize,
    pub node_min_width: f32,
    pub node_max_width: f32,
    pub node_padding: f32,
    /// Overrides the theme's node radius (`[theme.size.diagram] node_radius`).
    pub corner_radius: Option<f32>,
    pub border_width: f32,
    pub edge_thickness: f32,
    pub arrow_size: f32,
    /// Horizontal and vertical spacing of the automatic layout.
    pub gap: [f32; 2],
    /// Nodes per row before the automatic layout wraps.
    pub max_row: usize,
    pub grid: bool,
    pub grid_spacing: f32,
    /// Snap dragged nodes to the grid.
    pub snap: bool,
    pub zoom_speed: f32,
    pub min_zoom: f32,
    pub max_zoom: f32,
    /// Labels on composition edges (part name and multiplicity).
    pub edge_labels: bool,
    pub edges: EdgeVisibility,
}

impl Default for DiagramConfig {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            compartments: true,
            max_compartment_lines: 8,
            node_min_width: 140.0,
            node_max_width: 340.0,
            node_padding: 10.0,
            corner_radius: None,
            border_width: 1.0,
            edge_thickness: 1.25,
            arrow_size: 9.0,
            gap: [70.0, 90.0],
            max_row: 6,
            grid: true,
            grid_spacing: 40.0,
            snap: false,
            zoom_speed: 1.15,
            min_zoom: 0.1,
            max_zoom: 4.0,
            edge_labels: true,
            edges: EdgeVisibility::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    /// Folder for font file names that are not absolute paths.
    pub dir: PathBuf,
    pub title: String,
    pub body: String,
    pub mono: String,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            dir: "C:/Windows/Fonts".into(),
            title: "segoeuib.ttf".into(),
            body: "segoeui.ttf".into(),
            mono: "consola.ttf".into(),
        }
    }
}

impl FontConfig {
    pub fn path(&self, file: &str) -> PathBuf {
        let p = Path::new(file);
        if p.is_absolute() { p.to_path_buf() } else { self.dir.join(p) }
    }
}

/// Recently opened models, newest first (kept up to date by the File menu).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FilesConfig {
    pub recent: Vec<PathBuf>,
}

pub const MAX_RECENT: usize = 8;

impl Config {
    #[cfg(test)]
    pub fn from_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Reads `path`; when `[view] theme` names a preset, its colors are the
    /// base that the file's own `[theme]` sections refine.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut value: toml::Table = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let preset = value
            .get("view")
            .and_then(|v| v.get("theme"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if let Some(name) = preset {
            let file = theme_path(Some(path), &name);
            let theme_text = std::fs::read_to_string(&file).map_err(|e| format!("theme {name:?}: {}: {e}", file.display()))?;
            let mut preset: toml::Table =
                toml::from_str(&theme_text).map_err(|e| format!("{}: {e}", file.display()))?;
            if let Some(own) = value.remove("theme") {
                merge(preset.entry("theme").or_insert_with(|| toml::Value::Table(Default::default())), own);
            }
            if let Some(t) = preset.remove("theme") {
                value.insert("theme".into(), t);
            }
        }
        value.try_into().map_err(|e: toml::de::Error| format!("{}: {e}", path.display()))
    }
}

/// Deep-merges `over` into `base`; tables merge key by key, other values replace.
fn merge(base: &mut toml::Value, over: toml::Value) {
    match (base, over) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Folder holding the theme presets: `themes/` next to the config file, else
/// the one in the source tree.
pub fn themes_dir(config: Option<&Path>) -> PathBuf {
    let beside = config.and_then(|c| c.parent()).map(|d| d.join("themes"));
    beside.filter(|d| d.is_dir()).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes"))
}

pub fn theme_path(config: Option<&Path>, name: &str) -> PathBuf {
    themes_dir(config).join(format!("{name}.toml"))
}

/// Names of the available theme presets, sorted.
pub fn theme_names(config: Option<&Path>) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(themes_dir(config))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

/// Sets `key = value` in `[section]` of a TOML document, keeping comments,
/// the layout of everything else and a trailing comment on the line itself.
/// `value` is a TOML literal (`"text"`, `12`, `true`, `["a"]`). A missing key
/// is added at the end of its section; a missing section at the end.
pub fn set_toml_value(text: &str, section: &str, key: &str, value: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let header = |l: &str| {
        let t = l.trim();
        (t.starts_with('[') && !t.starts_with("[[")).then(|| t[1..].split(']').next().unwrap_or("").trim().to_owned())
    };
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 2);
    let (mut current, mut done, mut last_in_section) = (String::new(), false, None);
    for line in &lines {
        if let Some(h) = header(line) {
            current = h;
        } else if !done && current == section {
            let t = line.trim_start();
            let is_key = t.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='));
            if is_key {
                let indent = &line[..line.len() - t.len()];
                let comment = trailing_comment(t);
                let pad = comment.map(|c| {
                    // Keep the comment column where it was if the new value fits.
                    let col = t.len() - c.len();
                    let used = key.len() + 3 + value.len();
                    format!("{}{c}", " ".repeat(col.saturating_sub(used).max(1)))
                });
                out.push(format!("{indent}{key} = {value}{}", pad.unwrap_or_default()));
                done = true;
                continue;
            }
            if !t.is_empty() && !t.starts_with('#') {
                last_in_section = Some(out.len());
            }
        }
        if !done && header(line).is_some() && current == section && last_in_section.is_none() {
            last_in_section = Some(out.len());
        }
        out.push(line.to_string());
    }
    if !done {
        let entry = format!("{key} = {value}");
        match last_in_section {
            Some(i) => out.insert(i + 1, entry),
            None => {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push(format!("[{section}]"));
                out.push(entry);
            }
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The `# comment` ending a `key = value` line, ignoring `#` inside strings.
fn trailing_comment(line: &str) -> Option<&str> {
    let mut quote = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '#') => return Some(&line[i..]),
            _ => {}
        }
    }
    None
}

/// A TOML string literal.
pub fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_defaults() {
        let c = Config::from_toml("[view]\nmode = \"diagram\"\n[theme.node]\nname = \"#ff000080\"").unwrap();
        assert_eq!(c.view.mode, ViewMode::Diagram);
        assert_eq!(c.view.split, ViewConfig::default().split);
        assert_eq!(c.theme.node.name.0, [1.0, 0.0, 0.0, 128.0 / 255.0]);
    }

    #[test]
    fn rejects_typos_and_bad_colors() {
        assert!(Config::from_toml("[view]\nmdoe = \"split\"").is_err());
        assert!(Config::from_toml("[theme]\ntext = \"red\"").is_err());
    }

    #[test]
    fn sample_config_parses() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        Config::load(&root.join("eagleview.toml")).unwrap();
        for name in theme_names(Some(&root.join("eagleview.toml"))) {
            Config::load(&theme_path(None, &name)).unwrap();
        }
    }

    #[test]
    fn set_value_keeps_comments() {
        let src = "# top\n[view]\nmode = \"split\"    # comment\nsplit = 0.3\n\n[fonts]\nmono = \"a.ttf\"\n";
        let out = set_toml_value(src, "view", "mode", "\"code\"");
        assert!(out.contains("mode = \"code\"     # comment"), "{out}");
        let out = set_toml_value(&out, "view", "tree", "false");
        assert!(out.contains("split = 0.3\ntree = false\n"), "{out}");
        let out = set_toml_value(&out, "files", "recent", "[\"x\"]");
        assert!(out.ends_with("[files]\nrecent = [\"x\"]\n"), "{out}");
        let c = Config::from_toml(&out).unwrap();
        assert!(!c.view.tree);
        assert_eq!(c.fonts.mono, "a.ttf");
    }
}
