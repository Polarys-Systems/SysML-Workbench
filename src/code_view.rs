//! The code pane: an editor for the SysML source with line numbers, syntax
//! highlighting, diagnostics, multiple cursors, a find / replace bar and a
//! rename box. Editing logic lives in [`crate::editor`]; this is input and drawing.

use crate::clipboard;
use crate::theme::Theme;
use crate::diagram_view::{Fonts, contains};
use crate::editor::{Editor, FindOptions, Motion, find_matches};
use crate::shapes::{Rect, ShapeRenderer, Transform};
use crate::source::{self, Highlight};
use crate::text::{TextRenderer, TextStyle};
use std::time::{Duration, Instant};
use winit::keyboard::{Key, NamedKey};

const BLINK: Duration = Duration::from_millis(530);
const MULTI_CLICK: Duration = Duration::from_millis(450);

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

pub struct KeyInput<'a> {
    pub key: &'a Key,
    /// Text the key produces, if any.
    pub text: Option<&'a str>,
    pub mods: Mods,
}

impl KeyInput<'_> {
    /// Lower-case character of a character key.
    fn ch(&self) -> Option<String> {
        match self.key {
            Key::Character(c) => Some(c.to_lowercase()),
            _ => None,
        }
    }
    fn named(&self) -> Option<NamedKey> {
        match self.key {
            Key::Named(n) => Some(*n),
            _ => None,
        }
    }
    fn is(&self, ctrl: bool, shift: bool, alt: bool, c: &str) -> bool {
        self.mods.ctrl == ctrl && self.mods.shift == shift && self.mods.alt == alt && self.ch().as_deref() == Some(c)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Editor,
    Find,
    Replace,
    Rename,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Granularity {
    Char,
    Word,
    Line,
}

struct Drag {
    unit: Granularity,
    /// The range first clicked (a word or line for double / triple clicks).
    anchor: (usize, usize),
}

struct FindBar {
    open: bool,
    show_replace: bool,
    query: Editor,
    with: Editor,
    opts: FindOptions,
    matches: Vec<(usize, usize)>,
    /// (text version, query version, options) the matches were computed for.
    key: Option<(u64, u64, FindOptions)>,
    /// Where the search started, so typing a query finds the next match from there.
    origin: usize,
}

struct Rename {
    field: Editor,
    old: String,
}

/// A text field's rectangle and horizontal text offset, for mouse hits.
#[derive(Clone, Copy, Default)]
struct FieldGeom {
    rect: Rect,
    text_x: f32,
}

#[derive(Clone, Copy, Default)]
struct Layout {
    /// Gutter and text, below the bar.
    code: Rect,
    /// Left edge of the text column (x of column 0 when not scrolled).
    text_x: f32,
    /// y of line 0 when not scrolled.
    origin_y: f32,
    line_height: f32,
    advance: f32,
    visible: usize,
    bar: Option<Rect>,
    find: FieldGeom,
    replace: Option<FieldGeom>,
    rename: Option<FieldGeom>,
    case_button: Rect,
    word_button: Rect,
    close_button: Rect,
    replace_button: Rect,
    replace_all_button: Rect,
}

pub struct CodeView {
    pub editor: Editor,
    pub focus: Focus,
    /// Whether the pane (rather than the diagram) has the keyboard.
    pub active: bool,
    /// Vertical scroll in lines, horizontal in pixels.
    scroll: [f32; 2],
    /// Scroll the primary cursor into view on the next draw.
    reveal_cursor: bool,
    /// Line of the element selected in the diagram, highlighted and scrolled to.
    pub marked_line: Option<usize>,
    reveal_line: Option<usize>,
    find: FindBar,
    rename: Option<Rename>,
    drag: Option<Drag>,
    mouse: [f32; 2],
    last_click: Option<(Instant, [f32; 2], u32)>,
    blink: Instant,
    /// Exact text of the last whole-line copy, to paste it as lines again.
    line_clip: Option<String>,
    highlights: (u64, Vec<(usize, usize, Highlight)>),
    layout: Layout,
    /// Message for the status line.
    pub notice: Option<String>,
}

impl CodeView {
    pub fn new(text: &str) -> Self {
        Self {
            editor: Editor::new(text),
            focus: Focus::Editor,
            active: true,
            scroll: [0.0, 0.0],
            reveal_cursor: false,
            marked_line: None,
            reveal_line: None,
            find: FindBar {
                open: false,
                show_replace: false,
                query: Editor::single_line(),
                with: Editor::single_line(),
                opts: FindOptions::default(),
                matches: Vec::new(),
                key: None,
                origin: 0,
            },
            rename: None,
            drag: None,
            mouse: [0.0, 0.0],
            last_click: None,
            blink: Instant::now(),
            line_clip: None,
            highlights: (u64::MAX, Vec::new()),
            layout: Layout::default(),
            notice: None,
        }
    }

    /// A fresh file: new text, cursor at the top.
    pub fn open(&mut self, text: &str) {
        let query = std::mem::replace(&mut self.find.query, Editor::single_line());
        *self = Self::new(text);
        self.find.query = query;
    }

    /// The same file re-read from disk: cursors keep their line and column.
    pub fn reload(&mut self, text: &str) {
        self.editor.set_text(text);
        self.editor.mark_saved();
    }

    fn touch(&mut self) {
        self.blink = Instant::now();
        self.reveal_cursor = true;
    }

    /// Highlights the line containing `offset` (the diagram selection's
    /// declaration), scrolling to it with `reveal`.
    pub fn mark_offset(&mut self, offset: Option<usize>, reveal: bool) {
        let line = offset.map(|o| self.editor.line_of(o.min(self.editor.text().len())));
        self.marked_line = line;
        if reveal {
            self.reveal_line = line;
        }
    }

    /// Puts the cursor at `offset` and scrolls it into view (tree navigation).
    pub fn goto(&mut self, offset: usize) {
        self.editor.set_cursor(offset.min(self.editor.text().len()));
        self.focus = Focus::Editor;
        self.touch();
    }

    /// "Ln 3, Col 9 (12 selected) · 2 cursors"
    pub fn status(&self) -> String {
        let p = self.editor.primary();
        let (line, col) = self.editor.line_col(p.head);
        let mut s = format!("Ln {}, Col {}", line + 1, col + 1);
        let selected: usize = self.editor.selections().iter().map(|s| self.editor.selected_text(*s).chars().count()).sum();
        if selected > 0 {
            s += &format!(" ({selected} selected)");
        }
        let n = self.editor.selections().len();
        if n > 1 {
            s += &format!(" · {n} cursors");
        }
        s
    }

    // ---- mouse -----------------------------------------------------------------

    /// Should the mouse show a text cursor at `p`?
    pub fn wants_text_cursor(&self, p: [f32; 2]) -> bool {
        let l = &self.layout;
        let in_field = |f: Option<FieldGeom>| f.is_some_and(|f| contains(f.rect, p));
        (contains(l.code, p) && p[0] >= l.text_x) || in_field(self.find.open.then_some(l.find)) || in_field(l.replace) || in_field(l.rename)
    }

    fn line_at_y(&self, y: f32) -> usize {
        let l = &self.layout;
        let line = ((y - l.origin_y) / l.line_height + self.scroll[1]).floor();
        (line.max(0.0) as usize).min(self.editor.line_count() - 1)
    }

    fn offset_at_point(&self, p: [f32; 2]) -> usize {
        let l = &self.layout;
        let line = self.line_at_y(p[1]);
        let col = ((p[0] - l.text_x + self.scroll[0]) / l.advance).round().max(0.0) as usize;
        self.editor.offset_at(line, col)
    }

    fn field_offset(&self, ed: &Editor, f: FieldGeom, x: f32) -> usize {
        let col = ((x - f.text_x) / self.layout.advance).round().max(0.0) as usize;
        ed.offset_at(0, col)
    }

    /// Returns true for a plain click into the text (the caller selects the
    /// element there in the diagram).
    pub fn press(&mut self, p: [f32; 2], mods: Mods) -> bool {
        self.mouse = p;
        self.active = true;
        self.blink = Instant::now();
        let l = self.layout;
        if let Some(bar) = l.bar
            && contains(bar, p)
        {
            self.press_bar(p, mods);
            return false;
        }
        if !contains(l.code, p) {
            return false;
        }
        if self.rename.is_some() {
            self.rename = None;
        }
        self.focus = Focus::Editor;

        let count = match self.last_click {
            Some((t, q, n)) if t.elapsed() < MULTI_CLICK && (q[0] - p[0]).abs() < 5.0 && (q[1] - p[1]).abs() < 5.0 => n % 3 + 1,
            _ => 1,
        };
        self.last_click = Some((Instant::now(), p, count));

        let at = self.offset_at_point(p);
        let in_gutter = p[0] < l.text_x;
        let unit = if in_gutter || count == 3 {
            Granularity::Line
        } else if count == 2 {
            Granularity::Word
        } else {
            Granularity::Char
        };
        let range = self.unit_range(unit, at);
        let ed = &mut self.editor;
        if mods.shift && !in_gutter {
            let anchor = ed.primary().anchor;
            ed.set_primary(anchor, at);
            self.drag = Some(Drag { unit: Granularity::Char, anchor: (anchor, anchor) });
            return false;
        }
        if mods.alt {
            ed.add_selection(range.0, range.1);
        } else {
            ed.set_selection(range.0, range.1);
        }
        self.drag = Some(Drag { unit, anchor: range });
        count == 1 && !mods.alt && !in_gutter
    }

    fn unit_range(&self, unit: Granularity, at: usize) -> (usize, usize) {
        match unit {
            Granularity::Char => (at, at),
            Granularity::Word => self.editor.word_at(at),
            Granularity::Line => self.editor.line_range(self.editor.line_of(at)),
        }
    }

    fn press_bar(&mut self, p: [f32; 2], mods: Mods) {
        let l = self.layout;
        if let (Some(f), Some(r)) = (l.rename, self.rename.as_ref())
            && contains(f.rect, p)
        {
            let at = self.field_offset(&r.field, f, p[0]);
            self.rename.as_mut().unwrap().field.set_cursor(at);
            self.focus = Focus::Rename;
            return;
        }
        if !self.find.open {
            return;
        }
        if contains(l.close_button, p) {
            self.close_find();
        } else if contains(l.case_button, p) {
            self.find.opts.case_sensitive = !self.find.opts.case_sensitive;
        } else if contains(l.word_button, p) {
            self.find.opts.whole_word = !self.find.opts.whole_word;
        } else if self.find.show_replace && contains(l.replace_button, p) {
            self.replace_one();
        } else if self.find.show_replace && contains(l.replace_all_button, p) {
            self.replace_all();
        } else if contains(l.find.rect, p) {
            let at = self.field_offset(&self.find.query, l.find, p[0]);
            if mods.shift {
                let a = self.find.query.primary().anchor;
                self.find.query.set_selection(a, at);
            } else {
                self.find.query.set_cursor(at);
            }
            self.focus = Focus::Find;
        } else if let Some(f) = l.replace.filter(|f| contains(f.rect, p)) {
            let at = self.field_offset(&self.find.with, f, p[0]);
            self.find.with.set_cursor(at);
            self.focus = Focus::Replace;
        }
    }

    pub fn motion(&mut self, p: [f32; 2]) {
        self.mouse = p;
        let Some(drag) = &self.drag else { return };
        let (unit, (a, b)) = (drag.unit, drag.anchor);
        let at = self.offset_at_point(p);
        let (x, y) = self.unit_range(unit, at);
        if x < a {
            self.editor.set_primary(b, x);
        } else {
            self.editor.set_primary(a, y.max(b));
        }
        self.reveal_cursor = true;
    }

    pub fn release(&mut self) {
        self.drag = None;
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// `lines` > 0 scrolls up; with `horizontal` it scrolls sideways.
    pub fn wheel(&mut self, lines: f32, horizontal: bool) {
        if horizontal {
            self.scroll[0] = (self.scroll[0] - lines * self.layout.advance * 6.0).max(0.0);
        } else {
            self.scroll[1] -= lines * 3.0;
        }
    }

    // ---- keyboard --------------------------------------------------------------

    /// Handles a key for the pane; false when it is not an editing key.
    pub fn key(&mut self, input: &KeyInput) -> bool {
        self.blink = Instant::now();
        match self.focus {
            Focus::Editor => self.editor_key(input),
            Focus::Find | Focus::Replace => self.find_key(input),
            Focus::Rename => self.rename_key(input),
        }
    }

    fn editor_key(&mut self, input: &KeyInput) -> bool {
        let m = input.mods;
        let named = input.named();
        if input.is(true, false, false, "f") {
            self.open_find(false);
        } else if input.is(true, false, false, "h") {
            self.open_find(true);
        } else if named == Some(NamedKey::F3) {
            self.find_next(!m.shift);
        } else if named == Some(NamedKey::F2) {
            self.start_rename();
        } else if input.is(false, true, true, "f") || input.is(true, true, false, "i") {
            self.format();
        } else if input.is(true, false, false, "d") {
            if self.editor.add_next_occurrence() {
                self.touch();
            }
        } else if input.is(true, true, false, "l") {
            let n = self.editor.select_all_occurrences();
            if n > 0 {
                self.notice = Some(format!("{n} occurrences selected"));
            }
        } else if m.ctrl && m.alt && matches!(named, Some(NamedKey::ArrowUp | NamedKey::ArrowDown)) {
            self.editor.add_cursor_vertical(named == Some(NamedKey::ArrowDown));
            self.touch();
        } else if m.ctrl && !m.alt && matches!(input.ch().as_deref(), Some("/" | "#" | "7")) && input.text.is_none_or(|t| t.chars().all(char::is_control)) {
            self.editor.toggle_line_comment();
            self.touch();
        } else if named == Some(NamedKey::Escape) {
            if self.find.open {
                self.close_find();
            } else if self.editor.collapse() {
                self.touch();
            } else {
                return false;
            }
        } else {
            let page = self.layout.visible.saturating_sub(1).max(1);
            if !edit_key(&mut self.editor, input, page, &mut self.line_clip) {
                return false;
            }
            self.touch();
        }
        true
    }

    fn find_key(&mut self, input: &KeyInput) -> bool {
        let m = input.mods;
        let replacing = self.focus == Focus::Replace;
        match input.named() {
            Some(NamedKey::Escape) => self.close_find(),
            Some(NamedKey::Enter) if m.ctrl && m.alt => self.replace_all(),
            Some(NamedKey::Enter) if m.alt && !replacing => {
                let matches = self.find.matches.clone();
                if !matches.is_empty() {
                    self.editor.set_selections(&matches);
                    self.focus = Focus::Editor;
                    self.notice = Some(format!("{} matches selected", matches.len()));
                    self.touch();
                }
            }
            Some(NamedKey::Enter) if replacing => self.replace_one(),
            Some(NamedKey::Enter) => self.find_next(!m.shift),
            Some(NamedKey::F3) => self.find_next(!m.shift),
            Some(NamedKey::Tab) if self.find.show_replace => {
                self.focus = if replacing { Focus::Find } else { Focus::Replace };
            }
            _ if input.is(false, false, true, "c") => self.find.opts.case_sensitive = !self.find.opts.case_sensitive,
            _ if input.is(false, false, true, "w") => self.find.opts.whole_word = !self.find.opts.whole_word,
            _ if input.is(true, false, false, "f") => self.open_find(false),
            _ if input.is(true, false, false, "h") => self.open_find(true),
            _ => {
                let field = if replacing { &mut self.find.with } else { &mut self.find.query };
                return edit_key(field, input, 1, &mut self.line_clip);
            }
        }
        true
    }

    fn rename_key(&mut self, input: &KeyInput) -> bool {
        match input.named() {
            Some(NamedKey::Escape) => {
                self.rename = None;
                self.focus = Focus::Editor;
            }
            Some(NamedKey::Enter) => self.apply_rename(),
            _ => {
                let Some(r) = &mut self.rename else { return false };
                return edit_key(&mut r.field, input, 1, &mut self.line_clip);
            }
        }
        true
    }

    // ---- find and replace --------------------------------------------------------

    fn open_find(&mut self, replace: bool) {
        self.rename = None;
        let p = self.editor.primary();
        let selected = self.editor.selected_text(p);
        if !selected.is_empty() && !selected.contains('\n') {
            let s = selected.to_owned();
            self.find.query.set_text(&s);
        }
        self.find.query.select_all();
        self.find.with.select_all();
        self.find.open = true;
        self.find.show_replace |= replace;
        self.find.origin = p.start();
        self.find.key = None;
        self.focus = if replace { Focus::Replace } else { Focus::Find };
        if replace && self.find.query.text().is_empty() {
            self.focus = Focus::Find;
        }
    }

    fn close_find(&mut self) {
        self.find.open = false;
        self.find.show_replace = false;
        self.focus = Focus::Editor;
    }

    /// Recomputes the matches when the text, query or options changed. A new
    /// query selects its first match from where the search started.
    fn refresh_matches(&mut self) {
        let key = (self.editor.version(), self.find.query.version(), self.find.opts);
        let Some(old) = self.find.key.replace(key) else {
            self.find.matches = find_matches(self.editor.text(), self.find.query.text(), &self.find.opts);
            return;
        };
        if old == key {
            return;
        }
        self.find.matches = find_matches(self.editor.text(), self.find.query.text(), &self.find.opts);
        if (old.1 != key.1 || old.2 != key.2) && self.find.open {
            let origin = self.find.origin;
            if let Some(&(a, b)) = self.find.matches.iter().find(|m| m.0 >= origin).or(self.find.matches.first()) {
                self.editor.set_selection(a, b);
                self.reveal_cursor = true;
            }
        }
    }

    fn find_next(&mut self, forward: bool) {
        if self.find.query.text().is_empty() {
            self.open_find(false);
            return;
        }
        self.refresh_matches();
        let m = &self.find.matches;
        if m.is_empty() {
            self.notice = Some(format!("No results for \"{}\"", self.find.query.text()));
            return;
        }
        let p = self.editor.primary();
        let next = if forward {
            m.iter().find(|r| r.0 >= p.end() && r.1 > p.start()).or(m.first())
        } else {
            m.iter().rev().find(|r| r.1 <= p.start() && r.0 < p.start()).or(m.last())
        };
        let &(a, b) = next.unwrap();
        self.editor.set_selection(a, b);
        self.find.origin = a;
        self.touch();
    }

    fn replace_one(&mut self) {
        self.refresh_matches();
        let p = self.editor.primary();
        let with = self.find.with.text().to_owned();
        if self.find.matches.contains(&(p.start(), p.end())) {
            self.editor.replace_ranges(&[(p.start(), p.end())], &with);
            let after = p.start() + with.len();
            self.editor.set_cursor(after);
            self.refresh_matches();
        }
        self.find_next(true);
    }

    fn replace_all(&mut self) {
        self.refresh_matches();
        let matches = self.find.matches.clone();
        if matches.is_empty() {
            self.notice = Some("Nothing to replace".into());
            return;
        }
        let with = self.find.with.text().to_owned();
        self.editor.replace_ranges(&matches, &with);
        self.notice = Some(format!("Replaced {} occurrence{}", matches.len(), if matches.len() == 1 { "" } else { "s" }));
        self.touch();
    }

    // ---- rename and format -------------------------------------------------------

    fn start_rename(&mut self) {
        let head = self.editor.primary().head;
        match source::symbol_at(self.editor.text(), head) {
            Some((name, _)) => {
                let mut field = Editor::single_line();
                field.set_text(&name);
                field.select_all();
                self.rename = Some(Rename { field, old: name });
                self.find.open = false;
                self.focus = Focus::Rename;
            }
            None => self.notice = Some("No symbol at the cursor to rename".into()),
        }
    }

    fn apply_rename(&mut self) {
        let Some(r) = self.rename.take() else { return };
        self.focus = Focus::Editor;
        let new = r.field.text().trim().to_owned();
        if new.is_empty() || new == r.old {
            return;
        }
        let occ = source::occurrences(self.editor.text(), &r.old);
        let spelled = source::spell_name(&new);
        self.editor.replace_ranges(&occ, &spelled);
        self.notice = Some(format!(
            "Renamed {} occurrence{} of {} to {spelled}",
            occ.len(),
            if occ.len() == 1 { "" } else { "s" },
            r.old
        ));
        self.touch();
    }

    pub fn format(&mut self) {
        let formatted = source::format(self.editor.text());
        if formatted == self.editor.text() {
            self.notice = Some("Already formatted".into());
        } else {
            self.editor.reformat(formatted);
            self.notice = Some("Document formatted".into());
            self.touch();
        }
    }

    // ---- drawing -----------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        font_size: f32,
        theme: &Theme,
        pane: Rect,
        dpi: f32,
        diagnostics: &[(usize, usize)],
    ) {
        if self.find.open {
            self.refresh_matches();
        }
        let size = font_size * dpi;
        let lh = text.line_metrics(fonts.mono, size).line_height.max(1.0).round();
        let adv = text.measure(fonts.mono, size, "0", None)[0].max(1.0);
        let sz = &theme.size.code;
        let pad = (sz.pad * dpi).round();
        let count = self.editor.line_count();

        // ---- bar (find / replace / rename) ----
        let field_h = (lh + sz.field_extra_height * dpi).round();
        let rows = if self.rename.is_some() {
            1
        } else if self.find.open {
            1 + usize::from(self.find.show_replace)
        } else {
            0
        };
        let bar_h = if rows > 0 { pad * 0.6 + rows as f32 * (field_h + pad * 0.6) } else { 0.0 };
        let mut layout = Layout { line_height: lh, advance: adv, ..Default::default() };
        // The card behind the pane provides the background.
        shapes.begin_layer(Transform::IDENTITY, None);
        if rows > 0 {
            let bar = [pane[0], pane[1], pane[2], pane[1] + bar_h];
            layout.bar = Some(bar);
            shapes.rect([bar[0], bar[3] - dpi, bar[2], bar[3]], theme.border.0, 0.0);
            self.draw_bar(shapes, text, fonts, size, theme, bar, field_h, pad, dpi, &mut layout);
        }

        // ---- code area ----
        let code = [pane[0], pane[1] + bar_h, pane[2], pane[3]];
        let digits = count.to_string().len().max(3);
        let gutter = (pad * 1.5 + digits as f32 * adv).round();
        layout.code = code;
        layout.text_x = code[0] + gutter + pad * 0.6;
        layout.origin_y = code[1] + pad * 0.5;
        let view_h = code[3] - layout.origin_y;
        layout.visible = ((view_h / lh).floor() as usize).max(1);
        let text_w = code[2] - layout.text_x - pad;
        self.layout = layout;

        // Scrolling.
        if let Some(line) = self.reveal_line.take() {
            let (first, visible) = (self.scroll[1], layout.visible as f32);
            if (line as f32) < first || line as f32 >= first + visible - 1.0 {
                self.scroll[1] = line as f32 - (visible / 3.0).floor();
            }
        }
        if self.drag.is_some() {
            let m = self.mouse;
            let step = if m[1] < code[1] {
                -((code[1] - m[1]) / lh).clamp(0.2, 3.0)
            } else if m[1] > code[3] {
                ((m[1] - code[3]) / lh).clamp(0.2, 3.0)
            } else {
                0.0
            };
            if step != 0.0 {
                self.scroll[1] += step * 0.5;
                self.scroll[1] = self.scroll[1].clamp(0.0, (count as f32 - 1.0).max(0.0));
                self.motion(m);
            }
        }
        if std::mem::take(&mut self.reveal_cursor) {
            let (line, col) = self.editor.line_col(self.editor.primary().head);
            let visible = layout.visible as f32;
            if (line as f32) < self.scroll[1] {
                self.scroll[1] = line as f32;
            } else if line as f32 > self.scroll[1] + visible - 1.0 {
                self.scroll[1] = line as f32 - visible + 1.0;
            }
            let x = col as f32 * adv;
            if x < self.scroll[0] + adv * 2.0 {
                self.scroll[0] = (x - adv * 4.0).max(0.0);
            } else if x > self.scroll[0] + text_w - adv * 2.0 {
                self.scroll[0] = x - text_w + adv * 6.0;
            }
        }
        let max_scroll = (count as f32 - layout.visible as f32 + 3.0).max(0.0);
        self.scroll[1] = self.scroll[1].clamp(0.0, max_scroll);
        self.scroll[0] = self.scroll[0].max(0.0);

        if self.highlights.0 != self.editor.version() {
            self.highlights = (self.editor.version(), source::highlight(self.editor.text()));
        }

        let ed = &self.editor;
        let first = self.scroll[1].floor() as usize;
        let y0 = layout.origin_y - self.scroll[1].fract() * lh;
        let last = (first + layout.visible + 2).min(count);
        let x0 = layout.text_x - self.scroll[0];
        let line_y = |l: usize| y0 + (l - first) as f32 * lh;
        let col_x = |off: usize| x0 + ed.col_of(off) as f32 * adv;
        let focused = self.active && self.focus == Focus::Editor;
        let cursor_line = ed.line_of(ed.primary().head);

        shapes.begin_layer(Transform::IDENTITY, Some(code));
        // Line backgrounds.
        for l in first..last {
            let y = line_y(l);
            if Some(l) == self.marked_line {
                shapes.rect([code[0], y, code[2], y + lh], theme.selection_row.0, 0.0);
                shapes.rect([code[0], y, code[0] + (theme.size.frame.marker_width * dpi).round(), y + lh], theme.accent.0, 0.0);
            } else if l == cursor_line && self.active {
                shapes.rect([code[0], y, code[2], y + lh], theme.current_line.0, 0.0);
            }
        }
        let text_clip = [layout.text_x - 2.0 * dpi, code[1], code[2], code[3]];
        shapes.begin_layer(Transform::IDENTITY, Some(text_clip));
        let range_start = ed.line_start(first);
        let range_end = ed.line_end(last.saturating_sub(1).max(first));
        // Find matches.
        if self.find.open {
            for &(a, b) in self.find.matches.iter().filter(|m| m.1 >= range_start && m.0 <= range_end) {
                self.rect_range(shapes, a, b, first, last, y0, x0, false, theme.find_match.0);
            }
        }
        // Selections.
        for s in ed.selections().iter().filter(|s| !s.is_empty() && s.end() >= range_start && s.start() <= range_end) {
            self.rect_range(shapes, s.start(), s.end(), first, last, y0, x0, true, theme.text_selection.0);
        }
        // Diagnostics: a zigzag under the reported range.
        let len = ed.text().len();
        for &(a, b) in diagnostics {
            let a = floor_boundary(ed.text(), a.min(len));
            let b = floor_boundary(ed.text(), b.min(len)).max(a);
            let (la, lb) = (ed.line_of(a), ed.line_of(b));
            for l in la.max(first)..=lb.min(last.saturating_sub(1)) {
                if l < first || l >= last {
                    continue;
                }
                let sa = if l == la { a } else { ed.line_start(l) };
                let sb = if l == lb { b } else { ed.line_end(l) };
                let (xa, mut xb) = (col_x(sa), col_x(sb));
                if xb - xa < adv {
                    xb = xa + adv;
                }
                let y = line_y(l) + lh - sz.squiggle * 1.25 * dpi;
                let step = sz.squiggle * dpi;
                let mut x = xa;
                let mut up = false;
                while x < xb {
                    let nx = (x + step).min(xb);
                    let (ya, yb) = if up { (y + step * 0.5, y - step * 0.5) } else { (y - step * 0.5, y + step * 0.5) };
                    shapes.line([x, ya], [nx, yb], theme.error.0, dpi.max(1.0), 0.0);
                    x = nx;
                    up = !up;
                }
            }
        }
        // Cursors.
        let blink_on = (self.blink.elapsed().as_millis() / BLINK.as_millis()) % 2 == 0 || self.drag.is_some();
        if blink_on || !focused {
            let w = (theme.size.frame.caret_width * dpi).round().max(1.0);
            let color = if focused { theme.cursor.0 } else { theme.cursor.alpha(0.35) };
            if focused || self.active {
                for s in ed.selections() {
                    let l = ed.line_of(s.head);
                    if l >= first && l < last {
                        let x = col_x(s.head).round();
                        let y = line_y(l);
                        shapes.rect([x, y, x + w, y + lh], color, 0.0);
                    }
                }
            }
        }
        shapes.begin_layer(Transform::IDENTITY, None);

        // Text.
        text.set_clip(Some(text_clip));
        let hl = &self.highlights.1;
        let mut hi = hl.partition_point(|h| h.1 <= range_start);
        for l in first..last {
            let (ls, le) = (ed.line_start(l), ed.line_end(l));
            let y = line_y(l);
            let mut pos = ls;
            let draw = |text: &mut TextRenderer, a: usize, b: usize, color: [f32; 4]| {
                if b > a {
                    let s = &ed.text()[a..b];
                    if !s.trim().is_empty() {
                        text.draw(TextStyle::new(fonts.mono, size, color), [col_x(a), y], s);
                    }
                }
            };
            while hi < hl.len() && hl[hi].0 < le {
                let (a, b, h) = hl[hi];
                let (a, b) = (a.max(ls), b.min(le));
                if a > pos {
                    draw(text, pos, a, theme.text.0);
                }
                draw(text, a.max(pos), b, highlight_color(theme, h));
                pos = pos.max(b);
                if hl[hi].1 > le {
                    break; // continues on the next line
                }
                hi += 1;
            }
            draw(text, pos, le, theme.text.0);
        }

        // Line numbers.
        text.set_clip(Some([code[0], code[1], layout.text_x - pad * 0.3, code[3]]));
        for l in first..last {
            let n = (l + 1).to_string();
            let w = n.len() as f32 * adv;
            let color = if l == cursor_line { theme.text.alpha(0.85) } else { theme.text_muted.alpha(0.6) };
            text.draw(TextStyle::new(fonts.mono, size, color), [code[0] + gutter - w, line_y(l)], &n);
        }
        text.set_clip(None);

        // Scrollbar.
        let total = count as f32 + 3.0;
        if total > layout.visible as f32 {
            let track = code[3] - code[1];
            let h = (track * layout.visible as f32 / total).max(sz.scrollbar_min_length * dpi);
            let y = code[1] + (track - h) * (self.scroll[1] / max_scroll.max(1.0));
            let (sw, x) = (sz.scrollbar_width * dpi, code[2] - (sz.scrollbar_width + sz.scrollbar_inset) * dpi);
            shapes.rect([x, y, x + sw, y + h], theme.text_muted.alpha(0.35), sw * 0.5);
        }
    }

    /// Fills the parts of `a..b` on the visible lines; `newline` extends a
    /// range that continues past a line end by a little to show the break.
    #[allow(clippy::too_many_arguments)]
    fn rect_range(&self, shapes: &mut ShapeRenderer, a: usize, b: usize, first: usize, last: usize, y0: f32, x0: f32, newline: bool, color: [f32; 4]) {
        let ed = &self.editor;
        let (lh, adv) = (self.layout.line_height, self.layout.advance);
        let (la, lb) = (ed.line_of(a), ed.line_of(b));
        for l in la.max(first)..=lb.min(last.saturating_sub(1)) {
            let sa = if l == la { a } else { ed.line_start(l) };
            let sb = if l == lb { b } else { ed.line_end(l) };
            let xa = x0 + ed.col_of(sa) as f32 * adv;
            let mut xb = x0 + ed.col_of(sb) as f32 * adv;
            if newline && l < lb {
                xb += adv * 0.6;
            }
            let y = y0 + (l - first) as f32 * lh;
            shapes.rect([xa, y, xb, y + lh], color, 0.0);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_bar(
        &self,
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        size: f32,
        theme: &Theme,
        bar: Rect,
        field_h: f32,
        pad: f32,
        dpi: f32,
        layout: &mut Layout,
    ) {
        let sz = &theme.size.code;
        let label_size = size * sz.label_scale;
        let adv = layout.advance;
        let mut y = bar[1] + pad * 0.6;
        let button = |shapes: &mut ShapeRenderer, text: &mut TextRenderer, x: f32, y: f32, label: &str, on: bool| -> Rect {
            let w = text.measure(fonts.body, label_size, label, None)[0] + pad;
            let r = [x, y, x + w, y + field_h];
            let fill = if on { theme.accent.alpha(0.35) } else { theme.bg.panel.0 };
            shapes.bordered_rect(r, fill, if on { theme.accent.0 } else { theme.border.0 }, sz.field_radius * dpi, dpi);
            let lh = text.line_metrics(fonts.body, label_size).line_height;
            text.draw(TextStyle::new(fonts.body, label_size, theme.text.0), [x + pad * 0.5, y + (field_h - lh) * 0.5], label);
            r
        };

        if let Some(r) = &self.rename {
            let label = format!("Rename {} to", r.old);
            let lw = text.measure(fonts.body, label_size, &label, None)[0];
            let llh = text.line_metrics(fonts.body, label_size).line_height;
            text.draw(TextStyle::new(fonts.body, label_size, theme.text_muted.0), [bar[0] + pad, y + (field_h - llh) * 0.5], &label);
            let fx = bar[0] + pad * 1.6 + lw;
            let rect = [fx, y, bar[2] - pad, y + field_h];
            let g = self.draw_field(shapes, text, fonts, size, theme, &r.field, rect, self.focus == Focus::Rename, adv, dpi, "");
            layout.rename = Some(g);
            return;
        }

        // Row 1: query, options, match count, close.
        let close_w = field_h;
        let close = [bar[2] - pad - close_w, y, bar[2] - pad, y + field_h];
        let mut right = close[0] - pad * 0.5;
        let count = {
            let n = self.find.matches.len();
            let p = self.editor.primary();
            match self.find.matches.iter().position(|&(a, b)| a == p.start() && b == p.end()) {
                _ if self.find.query.text().is_empty() => String::new(),
                Some(i) => format!("{} of {n}", i + 1),
                None if n == 0 => "No results".into(),
                None => format!("{n} found"),
            }
        };
        let cw = text.measure(fonts.body, label_size, "0000 of 0000", None)[0];
        let llh = text.line_metrics(fonts.body, label_size).line_height;
        let color = if self.find.matches.is_empty() && !self.find.query.text().is_empty() { theme.error.0 } else { theme.text_muted.0 };
        right -= cw;
        text.draw(TextStyle::new(fonts.body, label_size, color), [right, y + (field_h - llh) * 0.5], &count);
        right -= pad * 0.5;
        let ww = text.measure(fonts.body, label_size, "W", None)[0] + pad;
        let aw = text.measure(fonts.body, label_size, "Aa", None)[0] + pad;
        let word = button(shapes, text, right - ww, y, "W", self.find.opts.whole_word);
        right -= ww + pad * 0.3;
        let case = button(shapes, text, right - aw, y, "Aa", self.find.opts.case_sensitive);
        right -= aw + pad * 0.5;
        // Close "×" drawn as two lines.
        let c = [(close[0] + close[2]) * 0.5, (close[1] + close[3]) * 0.5];
        let k = field_h * 0.18;
        shapes.line([c[0] - k, c[1] - k], [c[0] + k, c[1] + k], theme.text_muted.0, 1.5 * dpi, 0.0);
        shapes.line([c[0] - k, c[1] + k], [c[0] + k, c[1] - k], theme.text_muted.0, 1.5 * dpi, 0.0);
        let rect = [bar[0] + pad, y, right, y + field_h];
        layout.find = self.draw_field(shapes, text, fonts, size, theme, &self.find.query, rect, self.focus == Focus::Find, adv, dpi, "Find");
        layout.case_button = case;
        layout.word_button = word;
        layout.close_button = close;

        // Row 2: replacement and its buttons.
        if self.find.show_replace {
            y += field_h + pad * 0.6;
            let allw = text.measure(fonts.body, label_size, "All", None)[0] + pad;
            let rw = text.measure(fonts.body, label_size, "Replace", None)[0] + pad;
            let all = button(shapes, text, close[2] - allw, y, "All", false);
            let one = button(shapes, text, all[0] - pad * 0.3 - rw, y, "Replace", false);
            let rect = [bar[0] + pad, y, one[0] - pad * 0.5, y + field_h];
            let g = self.draw_field(shapes, text, fonts, size, theme, &self.find.with, rect, self.focus == Focus::Replace, adv, dpi, "Replace");
            layout.replace = Some(g);
            layout.replace_button = one;
            layout.replace_all_button = all;
        }
    }

    /// A one-line text field; returns its geometry for mouse hits.
    #[allow(clippy::too_many_arguments)]
    fn draw_field(
        &self,
        shapes: &mut ShapeRenderer,
        text: &mut TextRenderer,
        fonts: Fonts,
        size: f32,
        theme: &Theme,
        ed: &Editor,
        rect: Rect,
        focused: bool,
        adv: f32,
        dpi: f32,
        placeholder: &str,
    ) -> FieldGeom {
        let border = if focused { theme.accent.0 } else { theme.border.0 };
        let sz = &theme.size.code;
        shapes.bordered_rect(rect, theme.bg.panel.0, border, sz.field_radius * dpi, dpi);
        let inner = sz.field_pad * dpi;
        let lh = text.line_metrics(fonts.mono, size).line_height;
        let y = rect[1] + ((rect[3] - rect[1]) - lh) * 0.5;
        let avail = rect[2] - rect[0] - inner * 2.0;
        let cursor_x = ed.col_of(ed.primary().head) as f32 * adv;
        let shift = (cursor_x - avail + adv).max(0.0);
        let x0 = rect[0] + inner - shift;
        for s in ed.selections().iter().filter(|s| !s.is_empty()) {
            let (a, b) = (ed.col_of(s.start()) as f32 * adv, ed.col_of(s.end()) as f32 * adv);
            shapes.rect([(x0 + a).max(rect[0]), y, (x0 + b).min(rect[2]), y + lh], theme.text_selection.0, 0.0);
        }
        let clip = [rect[0] + inner * 0.5, rect[1], rect[2] - inner * 0.5, rect[3]];
        text.set_clip(Some(clip));
        if ed.text().is_empty() {
            if !focused {
                text.draw(TextStyle::new(fonts.body, size * sz.label_scale, theme.text_muted.alpha(0.7)), [x0, y], placeholder);
            }
        } else {
            text.draw(TextStyle::new(fonts.mono, size, theme.text.0), [x0, y], ed.text());
        }
        text.set_clip(None);
        let blink_on = (self.blink.elapsed().as_millis() / BLINK.as_millis()) % 2 == 0;
        if focused && self.active && blink_on {
            let x = (x0 + cursor_x).round();
            shapes.rect([x, y, x + (theme.size.frame.caret_width * dpi).round().max(1.0), y + lh], theme.cursor.0, 0.0);
        }
        FieldGeom { rect, text_x: x0 }
    }
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn highlight_color(theme: &Theme, h: Highlight) -> [f32; 4] {
    let o = &theme.syntax;
    match h {
        Highlight::Keyword => o.keyword,
        Highlight::Definition => o.definition,
        Highlight::Type | Highlight::Number => o.package,
        Highlight::String => o.expression,
        Highlight::Comment => o.comment,
        Highlight::Name | Highlight::Symbol => theme.text,
    }
    .0
}

/// Copies (and remembers whole-line copies, to paste them as lines).
fn copy_to_clipboard((text, lines): (String, bool), line_clip: &mut Option<String>) {
    if !text.is_empty() {
        clipboard::set(&text);
        *line_clip = lines.then_some(text);
    }
}

fn paste(ed: &mut Editor, line_clip: &Option<String>) {
    let Some(clip) = clipboard::get() else { return };
    if clip.is_empty() {
        return;
    }
    let all_empty = ed.selections().iter().all(|s| s.is_empty());
    let n = ed.selections().len();
    if all_empty && line_clip.as_deref() == Some(clip.as_str()) {
        ed.insert_lines(&clip);
        return;
    }
    let parts: Vec<String> = clip.trim_end_matches('\n').split('\n').map(str::to_owned).collect();
    if n > 1 && parts.len() == n {
        ed.insert_each(&parts);
    } else {
        ed.insert(&clip);
    }
}

/// Keys every text area understands: movement, deletion, clipboard, undo and
/// typed text. Returns false for keys it does not use.
pub(crate) fn edit_key(ed: &mut Editor, input: &KeyInput, page: usize, line_clip: &mut Option<String>) -> bool {
    let Mods { ctrl, shift, alt } = input.mods;
    let motion = |m: Motion, ed: &mut Editor| ed.move_cursors(m, shift);
    match input.named() {
        Some(NamedKey::ArrowLeft) => motion(if ctrl { Motion::WordLeft } else { Motion::Left }, ed),
        Some(NamedKey::ArrowRight) => motion(if ctrl { Motion::WordRight } else { Motion::Right }, ed),
        Some(NamedKey::ArrowUp) if !alt => motion(Motion::Up, ed),
        Some(NamedKey::ArrowDown) if !alt => motion(Motion::Down, ed),
        Some(NamedKey::Home) => motion(if ctrl { Motion::DocStart } else { Motion::Home }, ed),
        Some(NamedKey::End) => motion(if ctrl { Motion::DocEnd } else { Motion::End }, ed),
        Some(NamedKey::PageUp) => motion(Motion::PageUp(page), ed),
        Some(NamedKey::PageDown) => motion(Motion::PageDown(page), ed),
        Some(NamedKey::Backspace) if ctrl => ed.delete_word(false),
        Some(NamedKey::Backspace) => ed.backspace(),
        Some(NamedKey::Delete) if shift => copy_to_clipboard(ed.cut(), line_clip),
        Some(NamedKey::Delete) if ctrl => ed.delete_word(true),
        Some(NamedKey::Delete) => ed.delete(),
        Some(NamedKey::Insert) if ctrl => copy_to_clipboard(ed.copy(), line_clip),
        Some(NamedKey::Insert) if shift => paste(ed, line_clip),
        Some(NamedKey::Enter) if !ctrl && !alt => {
            let before = ed.version();
            ed.newline();
            return ed.version() != before;
        }
        Some(NamedKey::Tab) if !ctrl && !alt => {
            let before = ed.version();
            if shift { ed.outdent() } else { ed.indent() }
            return ed.version() != before;
        }
        _ if ctrl && !alt => match input.ch().as_deref() {
            Some("a") => ed.select_all(),
            Some("c") => copy_to_clipboard(ed.copy(), line_clip),
            Some("x") => copy_to_clipboard(ed.cut(), line_clip),
            Some("v") => paste(ed, line_clip),
            Some("z") if shift => _ = ed.redo(),
            Some("z") => _ = ed.undo(),
            Some("y") => _ = ed.redo(),
            _ => return false,
        },
        // AltGr arrives as Ctrl+Alt and types text; plain Alt combinations do not.
        _ => {
            let Some(t) = input.text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control)) else { return false };
            if alt && !ctrl {
                return false;
            }
            for c in t.chars() {
                ed.type_char(c);
            }
        }
    }
    true
}
