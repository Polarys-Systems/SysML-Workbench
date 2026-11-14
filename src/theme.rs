//! Design tokens: every color, size and type-scale ratio the panes draw with.
//!
//! A theme is a preset file in `themes/` (`[view] theme = "<name>"`) whose
//! `[theme]` tables fill this struct; `eagleview.toml` can refine any token in
//! its own `[theme]` sections. Every token has a default, and the defaults are
//! the `github-dark` preset, so a preset only lists what it changes.
//!
//! Sizes are logical pixels (multiplied by the display scale when drawn) unless
//! the token is a [`Len`], which may also be relative to the font: `"1.45lh"`
//! (times the line height) or `"1.25em"` (times the font size).
//!
//! Font families and the base font sizes are user settings (`[fonts]`, `[view]`,
//! `[diagram] font_size`), changed from the menus and Ctrl+= / Ctrl+-; the theme
//! only holds ratios to those base sizes.

use serde::Deserialize;

/// sRGB RGBA, 0..1. Written in the file as `"#rrggbb"` or `"#rrggbbaa"`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub [f32; 4]);

impl Color {
    pub const TRANSPARENT: [f32; 4] = [0.0; 4];

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0])
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0])
    }

    pub fn parse(s: &str) -> Option<Self> {
        let hex = s.strip_prefix('#')?;
        if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|b| b as f32 / 255.0);
        let alpha = if hex.len() == 8 { byte(6)? } else { 1.0 };
        Some(Self([byte(0)?, byte(2)?, byte(4)?, alpha]))
    }

    /// The color with its alpha multiplied by `a`.
    pub fn alpha(self, a: f32) -> [f32; 4] {
        let [r, g, b, alpha] = self.0;
        [r, g, b, alpha * a]
    }

    /// Linear RGB (for clear colors; shaders convert sRGB themselves).
    pub fn linear(self) -> [f32; 4] {
        let f = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        let [r, g, b, a] = self.0;
        [f(r), f(g), f(b), a]
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color {s:?}, expected \"#rrggbb\" or \"#rrggbbaa\"")))
    }
}

/// A length: logical pixels (`12`), or relative to the text it sits next to:
/// `"1.45lh"` (line heights) or `"1.25em"` (font sizes).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Px(f32),
    Lh(f32),
    Em(f32),
}

impl Len {
    /// Physical pixels for text of physical `font_size` and `line_height`.
    pub fn px(self, dpi: f32, font_size: f32, line_height: f32) -> f32 {
        match self {
            Len::Px(v) => v * dpi,
            Len::Lh(k) => k * line_height,
            Len::Em(k) => k * font_size,
        }
    }
}

impl<'de> Deserialize<'de> for Len {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(i64),
            Float(f64),
            Text(String),
        }
        let bad = |s: &str| serde::de::Error::custom(format!("invalid length {s:?}, expected pixels (12), \"1.5lh\" or \"1.2em\""));
        match Raw::deserialize(d)? {
            Raw::Int(v) => Ok(Len::Px(v as f32)),
            Raw::Float(v) => Ok(Len::Px(v as f32)),
            Raw::Text(s) => {
                let t = s.trim();
                let (num, unit) = t.split_at(t.len().saturating_sub(2));
                let k: f32 = num.trim().parse().map_err(|_| bad(&s))?;
                match unit {
                    "lh" => Ok(Len::Lh(k)),
                    "em" => Ok(Len::Em(k)),
                    "px" => Ok(Len::Px(k)),
                    _ => Err(bad(&s)),
                }
            }
        }
    }
}

// ---- colors -----------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub bg: Surfaces,
    /// Pane borders, dividers, field and button outlines, indentation guides.
    pub border: Color,
    pub text: Color,
    /// Secondary text: hints, counts, kind keywords, line numbers.
    pub text_muted: Color,
    /// Text drawn on an `accent` fill (highlighted menu item, hot handle).
    pub text_on_accent: Color,
    pub error: Color,
    /// Selection and focus: selected node border, highlighted edges, active
    /// divider, focused field, highlighted menu item.
    pub accent: Color,
    /// Background of the selected tree row / marked code line (usually translucent).
    pub selection_row: Color,
    /// Background of the row under the mouse (usually translucent).
    pub hover_row: Color,
    pub grid: Color,
    /// Code editor: text cursor, selected text, find matches and the cursor's line.
    pub cursor: Color,
    pub text_selection: Color,
    pub find_match: Color,
    pub current_line: Color,
    /// Syntax colors of the code editor.
    pub syntax: SyntaxColors,
    /// Foreground / background pair per kind of element (tree badges, node keywords).
    pub kind: KindColors,
    pub node: NodeColors,
    pub edge: EdgeColors,
    pub size: Sizes,
    /// Floating cards and bars over the canvas.
    pub glass: GlassColors,
    /// "No problems" and other good news.
    pub success: Color,
}

/// Translucent cards and bars floating over the diagram canvas.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GlassColors {
    /// Code and model cards, diagram controls (translucent).
    pub fill: Color,
    /// Top bar and status bar (translucent).
    pub bar: Color,
    pub border: Color,
    /// One-pixel light edge along the top of each card.
    pub highlight: Color,
    /// Soft shadow under the cards.
    pub shadow: Color,
    /// Track of the mode switch, search field and chips.
    pub control: Color,
    /// The selected segment of the mode switch.
    pub control_active: Color,
    /// Two soft color glows on the canvas that the glass picks up (alpha 00: off).
    pub glow: Color,
    pub glow_alt: Color,
}

impl Default for GlassColors {
    fn default() -> Self {
        Self {
            fill: Color::rgba(0x16, 0x1b, 0x22, 0xd9),
            bar: Color::rgba(0x01, 0x04, 0x09, 0xcc),
            border: Color::rgb(0x30, 0x36, 0x3d),
            highlight: Color::rgba(0xff, 0xff, 0xff, 0x0d),
            shadow: Color::rgba(0x00, 0x00, 0x00, 0x99),
            control: Color::rgba(0xff, 0xff, 0xff, 0x0d),
            control_active: Color::rgb(0x30, 0x36, 0x3d),
            glow: Color::rgba(0, 0, 0, 0),
            glow_alt: Color::rgba(0, 0, 0, 0),
        }
    }
}

/// Background surfaces, from the window outwards.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Surfaces {
    /// Window background behind the panels.
    pub app: Color,
    /// Code editor and model tree.
    pub panel: Color,
    /// Diagram canvas (also the PNG export background).
    pub canvas: Color,
    /// Menu bar, header, toolbars, pane headers, popups and dialogs.
    pub bar: Color,
}

impl Default for Surfaces {
    fn default() -> Self {
        Self {
            app: Color::rgb(0x01, 0x04, 0x09),
            panel: Color::rgb(0x0d, 0x11, 0x17),
            canvas: Color::rgb(0x0d, 0x11, 0x17),
            bar: Color::rgb(0x01, 0x04, 0x09),
        }
    }
}

// Defaults follow GitHub's Primer "dark" palette: neutral surfaces and borders,
// with color reserved for small text accents so nothing glows.
impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Surfaces::default(),
            border: Color::rgb(0x30, 0x36, 0x3d),
            text: Color::rgb(0xe6, 0xed, 0xf3),
            text_muted: Color::rgb(0x8b, 0x94, 0x9e),
            text_on_accent: Color::rgb(0xff, 0xff, 0xff),
            error: Color::rgb(0xf8, 0x51, 0x49),
            accent: Color::rgb(0x1f, 0x6f, 0xeb),
            selection_row: Color([0x38 as f32 / 255.0, 0x8b as f32 / 255.0, 0xfd as f32 / 255.0, 0.15]),
            hover_row: Color([0xb1 as f32 / 255.0, 0xba as f32 / 255.0, 0xc4 as f32 / 255.0, 0.06]),
            grid: Color::rgb(0x15, 0x1a, 0x21),
            cursor: Color::rgb(0x2f, 0x81, 0xf7),
            text_selection: Color::rgba(0x38, 0x8b, 0xfd, 0x55),
            find_match: Color::rgba(0xbb, 0x80, 0x09, 0x66),
            current_line: Color::rgba(0x6e, 0x76, 0x81, 0x1a),
            syntax: SyntaxColors::default(),
            kind: KindColors::default(),
            node: NodeColors::default(),
            edge: EdgeColors::default(),
            size: Sizes::default(),
            glass: GlassColors::default(),
            success: Color::rgb(0x3f, 0xb9, 0x50),
        }
    }
}

/// Code editor text color per token category.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SyntaxColors {
    pub package: Color,
    pub import: Color,
    pub comment: Color,
    pub definition: Color,
    pub usage: Color,
    pub relationship: Color,
    pub behavior: Color,
    pub expression: Color,
    /// Reserved words.
    pub keyword: Color,
}

impl Default for SyntaxColors {
    fn default() -> Self {
        Self {
            // GitHub's dark syntax colors.
            keyword: Color::rgb(0xff, 0x7b, 0x72),
            package: Color::rgb(0x79, 0xc0, 0xff),
            import: Color::rgb(0x8b, 0x94, 0x9e),
            comment: Color::rgb(0x8b, 0x94, 0x9e),
            definition: Color::rgb(0xd2, 0xa8, 0xff),
            usage: Color::rgb(0xe6, 0xed, 0xf3),
            relationship: Color::rgb(0x7e, 0xe7, 0x87),
            behavior: Color::rgb(0xff, 0xa6, 0x57),
            expression: Color::rgb(0xa5, 0xd6, 0xff),
        }
    }
}

/// A kind's colors: `fg` for its letter, keyword and outline, `bg` for the
/// tinted fill behind them. Written as `{ fg = "#...", bg = "#..." }`.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindPair {
    pub fg: Color,
    pub bg: Color,
}

impl KindPair {
    /// `fg` with a 14% tint of itself as `bg`.
    const fn tinted(r: u8, g: u8, b: u8) -> Self {
        Self { fg: Color::rgb(r, g, b), bg: Color::rgba(r, g, b, 0x24) }
    }
}

/// Colors per kind of model element (the categories the tree and diagram
/// distinguish).
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KindColors {
    pub package: KindPair,
    pub import: KindPair,
    pub comment: KindPair,
    pub definition: KindPair,
    pub usage: KindPair,
    pub relationship: KindPair,
    pub behavior: KindPair,
    pub requirement: KindPair,
    pub expression: KindPair,
}

impl Default for KindColors {
    fn default() -> Self {
        Self {
            package: KindPair::tinted(0x79, 0xc0, 0xff),
            import: KindPair::tinted(0x8b, 0x94, 0x9e),
            comment: KindPair::tinted(0x8b, 0x94, 0x9e),
            definition: KindPair::tinted(0xd2, 0x99, 0x22),
            usage: KindPair::tinted(0x44, 0x93, 0xf8),
            relationship: KindPair::tinted(0x7e, 0xe7, 0x87),
            behavior: KindPair::tinted(0xab, 0x7d, 0xf8),
            requirement: KindPair::tinted(0x3f, 0xb9, 0x50),
            expression: KindPair::tinted(0xa5, 0xd6, 0xff),
        }
    }
}

/// Diagram nodes are drawn as cards: a header band (kind and name) over a body
/// listing owned features, with one neutral border. The element kind shows
/// in the `«keyword»` label, colored by `[theme.kind]`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeColors {
    /// Header band behind the keyword and name.
    pub header: Color,
    /// Body behind the feature list.
    pub fill: Color,
    pub border: Color,
    /// Border of the node under the mouse.
    pub hover_border: Color,
    pub separator: Color,
    pub name: Color,
    pub compartment: Color,
    /// Drop shadow under nodes, menus and dialogs; set alpha to 00 to turn it off.
    pub shadow: Color,
}

impl Default for NodeColors {
    fn default() -> Self {
        Self {
            header: Color::rgb(0x16, 0x1b, 0x22),
            fill: Color::rgb(0x0d, 0x11, 0x17),
            border: Color::rgb(0x30, 0x36, 0x3d),
            hover_border: Color::rgb(0x6e, 0x76, 0x81),
            separator: Color::rgb(0x21, 0x26, 0x2d),
            name: Color::rgb(0xe6, 0xed, 0xf3),
            compartment: Color::rgb(0xb1, 0xba, 0xc4),
            shadow: Color([0x01 as f32 / 255.0, 0x04 as f32 / 255.0, 0x09 as f32 / 255.0, 0.4]),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EdgeColors {
    pub specialization: Color,
    pub typing: Color,
    pub composition: Color,
    pub satisfy: Color,
    pub dependency: Color,
    pub connection: Color,
    pub flow: Color,
    pub allocation: Color,
    pub label: Color,
}

impl Default for EdgeColors {
    fn default() -> Self {
        Self {
            // Primer accents blended about 60% into the canvas: distinct, not bright.
            specialization: Color::rgb(0x8b, 0x94, 0x9e),
            typing: Color::rgb(0x6e, 0x76, 0x81),
            composition: Color::rgb(0x3a, 0x6a, 0xa2),
            satisfy: Color::rgb(0x30, 0x87, 0x3f),
            dependency: Color::rgb(0x6e, 0x4f, 0xa9),
            connection: Color::rgb(0x8b, 0x94, 0x9e),
            flow: Color::rgb(0x3a, 0x86, 0x9a),
            allocation: Color::rgb(0x9a, 0x6a, 0x2e),
            label: Color::rgb(0x8b, 0x94, 0x9e),
        }
    }
}

// ---- sizes ------------------------------------------------------------------------

/// Spacing, radii, stroke widths and type-scale ratios, grouped by area.
/// `*_scale` tokens multiply that area's base font size.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sizes {
    pub frame: FrameSizes,
    pub menu: MenuSizes,
    pub code: CodeSizes,
    pub tree: TreeSizes,
    pub diagram: DiagramSizes,
}

/// Window frame: the top bar, status bar and the cards floating over the
/// canvas (code editor, model tree, diagram controls), plus dialogs.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FrameSizes {
    pub top_bar_height: f32,
    pub status_bar_height: f32,
    /// Space between the cards, and between them and the window edges.
    pub gap: f32,
    pub card_radius: f32,
    /// Title row of the code and model cards.
    pub card_header_height: f32,
    /// Frosted glass: blur radius of the canvas seen through cards and bars
    /// (0: plain translucent fill).
    pub backdrop_blur: f32,
    /// Soft shadow under cards: blur radius and downward offset.
    pub shadow_blur: f32,
    pub shadow_offset: f32,
    /// Buttons, chips, the mode switch and the search field.
    pub control_height: f32,
    pub control_radius: f32,
    /// Narrowest a card can be dragged.
    pub pane_min_width: f32,
    /// Inner padding of the diagram palette and other floating button groups.
    pub toolbar_pad: f32,
    pub toolbar_button_radius: f32,
    /// Status bar text and shortcut hints, relative to the UI font size.
    pub hint_scale: f32,
    /// Frame time readout (absolute font size).
    pub stats_font_size: f32,
    /// Keyboard shortcuts sheet.
    pub dialog_pad: f32,
    pub dialog_radius: f32,
    pub dialog_row_height: Len,
    pub dialog_title_scale: f32,
    /// Accent bar marking the selected tree row and the code line of the
    /// selected element.
    pub marker_width: f32,
    /// Text caret in the code editor and inline fields.
    pub caret_width: f32,
}

impl Default for FrameSizes {
    fn default() -> Self {
        Self {
            top_bar_height: 60.0,
            status_bar_height: 30.0,
            gap: 12.0,
            card_radius: 14.0,
            card_header_height: 44.0,
            backdrop_blur: 28.0,
            shadow_blur: 18.0,
            shadow_offset: 6.0,
            control_height: 34.0,
            control_radius: 10.0,
            pane_min_width: 200.0,
            toolbar_pad: 4.0,
            toolbar_button_radius: 8.0,
            hint_scale: 0.92,
            stats_font_size: 11.0,
            dialog_pad: 18.0,
            dialog_radius: 12.0,
            dialog_row_height: Len::Lh(1.35),
            dialog_title_scale: 1.3,
            marker_width: 2.0,
            caret_width: 1.5,
        }
    }
}

/// Menu bar titles and drop-down / context menus.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MenuSizes {
    pub pad: f32,
    pub row_height: Len,
    pub separator_height: Len,
    /// Space left of the labels for check marks.
    pub gutter: Len,
    pub min_width: f32,
    pub radius: f32,
    /// Highlight behind a menu item or menu bar title.
    pub item_radius: f32,
    pub item_inset: f32,
    /// Gap between a menu bar title's highlight and the bar's edges.
    pub title_inset: f32,
    pub shadow: f32,
    pub shortcut_scale: f32,
}

impl Default for MenuSizes {
    fn default() -> Self {
        Self {
            pad: 10.0,
            row_height: Len::Lh(1.55),
            separator_height: Len::Lh(0.6),
            gutter: Len::Em(1.9),
            min_width: 180.0,
            radius: 6.0,
            item_radius: 4.0,
            item_inset: 4.0,
            title_inset: 3.0,
            shadow: 8.0,
            shortcut_scale: 0.92,
        }
    }
}

/// Code editor and its find / replace / rename bar.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodeSizes {
    pub pad: f32,
    /// Find-bar fields are this much taller than a code line.
    pub field_extra_height: f32,
    pub field_radius: f32,
    /// Find-bar labels and buttons, relative to the code font size.
    pub label_scale: f32,
    pub scrollbar_width: f32,
    /// Gap between the scrollbar and the pane edge.
    pub scrollbar_inset: f32,
    pub scrollbar_min_length: f32,
    /// Wavelength of the zigzag under problems.
    pub squiggle: f32,
    /// Text inset inside find-bar fields.
    pub field_pad: f32,
}

impl Default for CodeSizes {
    fn default() -> Self {
        Self {
            pad: 10.0,
            field_extra_height: 8.0,
            field_radius: 3.0,
            label_scale: 0.92,
            scrollbar_width: 3.0,
            scrollbar_inset: 2.0,
            scrollbar_min_length: 20.0,
            squiggle: 2.0,
            field_pad: 6.0,
        }
    }
}

/// Model tree.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TreeSizes {
    pub row_height: Len,
    pub header_height: Len,
    pub pad: f32,
    pub indent: Len,
    /// Kind badge (square) size.
    pub badge: Len,
    pub badge_radius: f32,
    /// Header title, element count and kind keywords, relative to the UI font size.
    pub small_scale: f32,
    pub button_radius: f32,
    pub scrollbar_width: f32,
    pub scrollbar_inset: f32,
}

impl Default for TreeSizes {
    fn default() -> Self {
        Self {
            row_height: Len::Lh(1.45),
            header_height: Len::Px(44.0),
            pad: 10.0,
            indent: Len::Em(1.25),
            badge: Len::Lh(0.95),
            badge_radius: 3.0,
            small_scale: 0.85,
            button_radius: 4.0,
            scrollbar_width: 4.0,
            scrollbar_inset: 2.0,
        }
    }
}

/// Diagram canvas. Lengths are diagram units (they zoom with the diagram),
/// except `grid_min_spacing` and `handle`, which are screen pixels.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagramSizes {
    /// Node text relative to `[diagram] font_size`. These change node sizes.
    pub keyword_scale: f32,
    pub name_scale: f32,
    pub compartment_scale: f32,
    pub label_scale: f32,
    pub shadow_offset: f32,
    /// Node corner radius (`[diagram] corner_radius` overrides it).
    pub node_radius: f32,
    /// Halo around the selected node: width (0: none) and accent opacity.
    pub selection_ring: f32,
    pub selection_ring_alpha: f32,
    /// Blur of the node shadow (0: a hard offset copy of the node).
    pub shadow_blur: f32,
    /// Grid dot radius in screen pixels (0: grid lines).
    pub grid_dot: f32,
    /// Selected node border, as a multiple of `[diagram] border_width`.
    pub selected_border_scale: f32,
    /// Dash length of typing, satisfy, dependency and allocation edges.
    pub edge_dash: f32,
    /// Grid lines closer than this on screen are thinned out.
    pub grid_min_spacing: f32,
    /// The "+" handle for drawing relationships (half its width).
    pub handle: f32,
    pub field_radius: f32,
}

impl Default for DiagramSizes {
    fn default() -> Self {
        Self {
            keyword_scale: 0.85,
            name_scale: 1.1,
            compartment_scale: 0.92,
            label_scale: 0.8,
            shadow_offset: 2.0,
            node_radius: 6.0,
            selection_ring: 0.0,
            selection_ring_alpha: 0.14,
            shadow_blur: 0.0,
            grid_dot: 0.0,
            selected_border_scale: 2.0,
            edge_dash: 6.0,
            grid_min_spacing: 14.0,
            handle: 9.0,
            field_radius: 4.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_parse() {
        #[derive(Deserialize)]
        struct T {
            a: Len,
            b: Len,
            c: Len,
            d: Len,
        }
        let t: T = toml::from_str("a = 28\nb = 12.5\nc = \"1.45lh\"\nd = \"1.25em\"").unwrap();
        assert_eq!((t.a, t.b, t.c, t.d), (Len::Px(28.0), Len::Px(12.5), Len::Lh(1.45), Len::Em(1.25)));
        assert_eq!(t.a.px(2.0, 26.0, 30.0), 56.0);
        assert_eq!(t.c.px(2.0, 26.0, 20.0), 29.0);
        assert!(toml::from_str::<T>("a = \"3cm\"\nb = 1\nc = 1\nd = 1").is_err());
    }

    /// `top` composited over `bottom` (opaque result).
    fn over(top: Color, bottom: [f32; 4]) -> [f32; 4] {
        let a = top.0[3];
        let mix = |i: usize| top.0[i] * a + bottom[i] * (1.0 - a);
        [mix(0), mix(1), mix(2), 1.0]
    }

    /// WCAG 2 contrast ratio of two opaque sRGB colors.
    fn contrast(a: [f32; 4], b: [f32; 4]) -> f32 {
        let lum = |c: [f32; 4]| {
            let f = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2])
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Every text color of the light theme on every surface it is drawn on.
    #[test]
    fn light_text_contrast() {
        let t = crate::config::Config::load(&crate::config::theme_path(None, "eagleview-light")).unwrap().theme;
        let panel = t.bg.panel.0;
        let surfaces = [
            ("bg.panel", panel),
            ("bg.app", t.bg.app.0),
            ("bg.canvas", t.bg.canvas.0),
            ("bg.bar", t.bg.bar.0),
            ("current_line", over(t.current_line, panel)),
            ("selection_row", over(t.selection_row, panel)),
            ("glass.fill", over(t.glass.fill, t.bg.canvas.0)),
            ("glass.bar", over(t.glass.bar, t.bg.canvas.0)),
            ("glass.control", over(t.glass.control, panel)),
            ("glass.control_active", over(t.glass.control_active, panel)),
        ];
        let s = &t.syntax;
        let mut pairs: Vec<(String, [f32; 4], [f32; 4])> = Vec::new();
        for (name, fg) in [("text", t.text), ("text_muted", t.text_muted), ("error", t.error)] {
            for (sn, bg) in surfaces {
                pairs.push((format!("{name} on {sn}"), fg.0, bg));
            }
        }
        let syntax = [s.package, s.import, s.comment, s.definition, s.usage, s.relationship, s.behavior, s.expression, s.keyword];
        for (i, c) in syntax.into_iter().enumerate() {
            for (sn, bg) in &surfaces[4..5] {
                pairs.push((format!("syntax[{i}] on {sn}"), c.0, *bg));
            }
            pairs.push((format!("syntax[{i}] on bg.panel"), c.0, panel));
        }
        let k = &t.kind;
        for (name, p) in [
            ("package", k.package),
            ("import", k.import),
            ("comment", k.comment),
            ("definition", k.definition),
            ("usage", k.usage),
            ("relationship", k.relationship),
            ("behavior", k.behavior),
            ("requirement", k.requirement),
            ("expression", k.expression),
        ] {
            pairs.push((format!("kind.{name}.fg on its bg"), p.fg.0, over(p.bg, panel)));
            pairs.push((format!("kind.{name}.fg on bg.panel"), p.fg.0, panel));
        }
        let n = &t.node;
        for (name, fg) in [("node.name", n.name), ("node.compartment", n.compartment)] {
            pairs.push((format!("{name} on node.header"), fg.0, over(n.header, t.bg.canvas.0)));
            pairs.push((format!("{name} on node.fill"), fg.0, over(n.fill, t.bg.canvas.0)));
        }
        pairs.push(("edge.label on bg.canvas".into(), t.edge.label.0, t.bg.canvas.0));
        let failures: Vec<String> = pairs
            .iter()
            .map(|(n, fg, bg)| (n, contrast(*fg, *bg)))
            .filter(|(_, r)| *r < 4.5)
            .map(|(n, r)| format!("{n}: {r:.2}"))
            .collect();
        assert!(failures.is_empty(), "below 4.5:1:\n{}", failures.join("\n"));
    }

    #[test]
    fn kind_pairs_are_tables() {
        let t: Theme = toml::from_str("[kind.usage]\nfg = \"#3b6ff5\"\nbg = \"#eef3ff\"").unwrap();
        assert_eq!(t.kind.usage.fg, Color::rgb(0x3b, 0x6f, 0xf5));
        assert_eq!(t.kind.package.fg, KindColors::default().package.fg);
    }
}
