//! Text editing core for the code pane: a UTF-8 buffer with multiple
//! selections, undo/redo and the usual cursor movements and edits.
//!
//! Nothing here knows about fonts or pixels. Positions are byte offsets into
//! the text; columns are visual columns where a tab counts as [`INDENT`]
//! (the text renderer draws a tab as four spaces).

use std::time::{Duration, Instant};

/// Indentation unit and tab width, in columns.
pub const INDENT: usize = 4;
const UNDO_LIMIT: usize = 500;
/// Consecutive typing within this time is undone in one step.
const COALESCE: Duration = Duration::from_millis(1200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    /// Column that vertical movement tries to keep.
    goal: Option<usize>,
}

impl Selection {
    pub fn cursor(at: usize) -> Self {
        Self { anchor: at, head: at, goal: None }
    }
    pub fn new(anchor: usize, head: usize) -> Self {
        Self { anchor, head, goal: None }
    }
    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }
    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    DocStart,
    DocEnd,
    PageUp(usize),
    PageDown(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
    Other,
}

#[derive(Clone)]
struct Snapshot {
    text: String,
    sels: Vec<Selection>,
    primary: usize,
}

/// Replaces `start..end` of the current text with `text`.
#[derive(Clone, Debug)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Where an [`Edit`] landed in the new text.
#[derive(Clone, Copy)]
struct Applied {
    start: usize,
    end: usize,
    new_start: usize,
    new_len: usize,
}

/// Maps an offset of the old text to the new one. Offsets inside a replaced
/// range stay inside the replacement; an insertion point keeps text inserted
/// exactly there on its right.
fn map_offset(applied: &[Applied], pos: usize) -> usize {
    let mut delta = 0isize;
    for a in applied {
        if pos < a.start || (pos == a.start && a.start == a.end) {
            break;
        }
        if pos < a.end {
            return a.new_start + (pos - a.start).min(a.new_len);
        }
        delta = (a.new_start + a.new_len) as isize - a.end as isize;
    }
    (pos as isize + delta) as usize
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

pub fn is_word_char(c: char) -> bool {
    class(c) == CharClass::Word
}

pub struct Editor {
    text: String,
    line_starts: Vec<usize>,
    /// Sorted by position, never overlapping.
    sels: Vec<Selection>,
    primary: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<(EditKind, Instant)>,
    version: u64,
    saved: String,
    /// Text fields (find, replace, rename): newlines become spaces.
    single_line: bool,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        let mut e = Self {
            text: String::new(),
            line_starts: vec![0],
            sels: vec![Selection::cursor(0)],
            primary: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            version: 0,
            saved: String::new(),
            single_line: false,
        };
        e.set_text(text);
        e.mark_saved();
        e
    }

    pub fn single_line() -> Self {
        let mut e = Self::new("");
        e.single_line = true;
        e
    }

    // ---- content ---------------------------------------------------------------

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Increases with every change of the text (including undo).
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Replaces the whole text (e.g. a reload) and forgets the undo history.
    /// Cursors keep their line and column where possible.
    pub fn set_text(&mut self, text: &str) {
        let keep: Vec<(usize, usize)> = self.sels.iter().map(|s| self.line_col(s.head)).collect();
        self.text = if self.single_line { text.replace(['\r', '\n'], " ") } else { text.replace("\r\n", "\n") };
        self.rebuild_lines();
        self.sels = keep
            .into_iter()
            .map(|(l, c)| {
                let l = l.min(self.line_count() - 1);
                Selection::cursor(self.offset_at(l, c))
            })
            .collect();
        self.primary = self.primary.min(self.sels.len() - 1);
        self.normalize();
        self.undo.clear();
        self.redo.clear();
        self.last_edit = None;
        self.version += 1;
    }

    pub fn mark_saved(&mut self) {
        self.saved.clone_from(&self.text);
    }

    pub fn is_modified(&self) -> bool {
        self.text != self.saved
    }

    fn rebuild_lines(&mut self) {
        self.line_starts.clear();
        self.line_starts.push(0);
        self.line_starts.extend(self.text.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i + 1));
    }

    // ---- lines and columns -----------------------------------------------------

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    pub fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts[line]
    }

    /// End of the line's text, before its `\n`.
    pub fn line_end(&self, line: usize) -> usize {
        self.line_starts.get(line + 1).map_or(self.text.len(), |&s| s - 1)
    }

    pub fn line_text(&self, line: usize) -> &str {
        &self.text[self.line_start(line)..self.line_end(line)]
    }

    /// Visual column of `offset` within its line.
    pub fn col_of(&self, offset: usize) -> usize {
        let start = self.line_start(self.line_of(offset));
        visual_width(&self.text[start..offset])
    }

    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        (self.line_of(offset), self.col_of(offset))
    }

    /// Offset of visual column `col` in `line` (clamped to the line end; a
    /// column inside a tab maps to the nearer side).
    pub fn offset_at(&self, line: usize, col: usize) -> usize {
        let start = self.line_start(line);
        let mut acc = 0;
        for (i, c) in self.line_text(line).char_indices() {
            let w = if c == '\t' { INDENT } else { 1 };
            if acc + w > col {
                return start + if col - acc > w / 2 { i + c.len_utf8() } else { i };
            }
            acc += w;
        }
        self.line_end(line)
    }

    fn prev_char(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map_or(0, |(j, _)| j)
    }

    fn next_char(&self, i: usize) -> usize {
        i + self.text[i..].chars().next().map_or(0, char::len_utf8)
    }

    fn char_at(&self, i: usize) -> Option<char> {
        self.text[i..].chars().next()
    }

    fn char_before(&self, i: usize) -> Option<char> {
        self.text[..i].chars().next_back()
    }

    fn word_left(&self, mut i: usize) -> usize {
        while let Some(c) = self.char_before(i).filter(|c| class(*c) == CharClass::Space) {
            i -= c.len_utf8();
        }
        if let Some(k) = self.char_before(i).map(class) {
            while let Some(c) = self.char_before(i).filter(|c| class(*c) == k) {
                i -= c.len_utf8();
            }
        }
        i
    }

    fn word_right(&self, mut i: usize) -> usize {
        while let Some(c) = self.char_at(i).filter(|c| class(*c) == CharClass::Space) {
            i += c.len_utf8();
        }
        if let Some(k) = self.char_at(i).map(class) {
            while let Some(c) = self.char_at(i).filter(|c| class(*c) == k) {
                i += c.len_utf8();
            }
        }
        i
    }

    /// The word (or run of punctuation / whitespace) around `offset`.
    pub fn word_at(&self, offset: usize) -> (usize, usize) {
        let k = match (self.char_before(offset).map(class), self.char_at(offset).map(class)) {
            (_, Some(CharClass::Word)) => CharClass::Word,
            (Some(CharClass::Word), _) => CharClass::Word,
            (_, Some(k)) => k,
            (Some(k), None) => k,
            (None, None) => return (offset, offset),
        };
        let (mut a, mut b) = (offset, offset);
        while let Some(c) = self.char_before(a).filter(|c| class(*c) == k && *c != '\n') {
            a -= c.len_utf8();
        }
        while let Some(c) = self.char_at(b).filter(|c| class(*c) == k && *c != '\n') {
            b += c.len_utf8();
        }
        (a, b)
    }

    /// Byte range of `line` including its line break.
    pub fn line_range(&self, line: usize) -> (usize, usize) {
        (self.line_start(line), self.line_starts.get(line + 1).copied().unwrap_or(self.text.len()))
    }

    fn first_non_space(&self, line: usize) -> usize {
        let t = self.line_text(line);
        self.line_start(line) + (t.len() - t.trim_start_matches([' ', '\t']).len())
    }

    // ---- selections -----------------------------------------------------------

    pub fn selections(&self) -> &[Selection] {
        &self.sels
    }

    pub fn primary(&self) -> Selection {
        self.sels[self.primary]
    }

    pub fn selected_text(&self, s: Selection) -> &str {
        &self.text[s.start()..s.end()]
    }

    /// Sorts the selections and merges the ones that overlap.
    fn normalize(&mut self) {
        let len = self.text.len();
        let primary = self.primary;
        let mut sels: Vec<(Selection, bool)> = std::mem::take(&mut self.sels)
            .into_iter()
            .enumerate()
            .map(|(i, mut s)| {
                s.anchor = s.anchor.min(len);
                s.head = s.head.min(len);
                (s, i == primary)
            })
            .collect();
        sels.sort_by_key(|(s, _)| (s.start(), s.end()));
        let mut out: Vec<(Selection, bool)> = Vec::with_capacity(sels.len());
        for (s, p) in sels {
            if let Some((last, lp)) = out.last_mut() {
                let touching = s.start() == last.end() && (s.is_empty() || last.is_empty());
                if s.start() < last.end() || s.start() == last.start() || touching {
                    let (a, b) = (last.start(), last.end().max(s.end()));
                    let goal = last.goal;
                    *last = if last.head < last.anchor { Selection::new(b, a) } else { Selection::new(a, b) };
                    last.goal = goal;
                    *lp |= p;
                    continue;
                }
            }
            out.push((s, p));
        }
        self.primary = out.iter().position(|(_, p)| *p).unwrap_or(out.len() - 1);
        self.sels = out.into_iter().map(|(s, _)| s).collect();
    }

    fn end_typing(&mut self) {
        self.last_edit = None;
    }

    pub fn set_cursor(&mut self, at: usize) {
        self.set_selection(at, at);
    }

    pub fn set_selection(&mut self, anchor: usize, head: usize) {
        self.sels = vec![Selection::new(anchor, head)];
        self.primary = 0;
        self.normalize();
        self.end_typing();
    }

    /// Sets the primary selection's range, keeping the others.
    pub fn set_primary(&mut self, anchor: usize, head: usize) {
        self.sels[self.primary] = Selection::new(anchor, head);
        self.normalize();
        self.end_typing();
    }

    /// Adds a selection and makes it primary.
    pub fn add_selection(&mut self, anchor: usize, head: usize) {
        self.sels.push(Selection::new(anchor, head));
        self.primary = self.sels.len() - 1;
        self.normalize();
        self.end_typing();
    }

    /// Replaces all selections; the last one becomes primary.
    pub fn set_selections(&mut self, ranges: &[(usize, usize)]) {
        if ranges.is_empty() {
            return;
        }
        self.sels = ranges.iter().map(|&(a, h)| Selection::new(a, h)).collect();
        self.primary = self.sels.len() - 1;
        self.normalize();
        self.end_typing();
    }

    pub fn select_all(&mut self) {
        self.set_selection(0, self.text.len());
    }

    /// Escape: drops the extra cursors, or else the selection.
    pub fn collapse(&mut self) -> bool {
        if self.sels.len() > 1 {
            let p = self.primary();
            self.sels = vec![p];
            self.primary = 0;
            true
        } else if !self.primary().is_empty() {
            let h = self.primary().head;
            self.set_cursor(h);
            true
        } else {
            false
        }
    }

    pub fn move_cursors(&mut self, motion: Motion, extend: bool) {
        let vertical = matches!(motion, Motion::Up | Motion::Down | Motion::PageUp(_) | Motion::PageDown(_));
        let sels = std::mem::take(&mut self.sels);
        self.sels = sels
            .iter()
            .map(|s| {
                if !extend && !s.is_empty() && matches!(motion, Motion::Left | Motion::Right) {
                    return Selection::cursor(if motion == Motion::Left { s.start() } else { s.end() });
                }
                let goal = if vertical { Some(s.goal.unwrap_or_else(|| self.col_of(s.head))) } else { None };
                let head = self.target(s.head, motion, goal.unwrap_or(0));
                let anchor = if extend { s.anchor } else { head };
                Selection { anchor, head, goal }
            })
            .collect();
        self.normalize();
        self.end_typing();
    }

    fn target(&self, head: usize, motion: Motion, goal: usize) -> usize {
        let line = self.line_of(head);
        let last = self.line_count() - 1;
        let vertical = |l: isize| {
            if l < 0 {
                0
            } else if l as usize > last {
                self.text.len()
            } else {
                self.offset_at(l as usize, goal)
            }
        };
        match motion {
            Motion::Left => self.prev_char(head),
            Motion::Right => self.next_char(head),
            Motion::Up => vertical(line as isize - 1),
            Motion::Down => vertical(line as isize + 1),
            Motion::PageUp(n) => vertical((line as isize - n as isize).max(0)),
            Motion::PageDown(n) => vertical((line + n).min(last) as isize),
            Motion::WordLeft => self.word_left(head),
            Motion::WordRight => self.word_right(head),
            Motion::Home => {
                let first = self.first_non_space(line);
                if head == first { self.line_start(line) } else { first }
            }
            Motion::End => self.line_end(line),
            Motion::DocStart => 0,
            Motion::DocEnd => self.text.len(),
        }
    }

    /// Ctrl+Alt+Up/Down: a new cursor on the line above the first / below the last cursor.
    pub fn add_cursor_vertical(&mut self, down: bool) {
        let s = if down { *self.sels.last().unwrap() } else { self.sels[0] };
        let line = self.line_of(s.head);
        let goal = s.goal.unwrap_or_else(|| self.col_of(s.head));
        let target = if down { line + 1 } else { line.wrapping_sub(1) };
        if target >= self.line_count() {
            return;
        }
        let at = self.offset_at(target, goal);
        self.sels.push(Selection { anchor: at, head: at, goal: Some(goal) });
        self.primary = self.sels.len() - 1;
        self.normalize();
        self.end_typing();
    }

    /// Ctrl+D: selects the word under an empty primary cursor, else adds the
    /// next occurrence of the primary selection. Returns false when there is none.
    pub fn add_next_occurrence(&mut self) -> bool {
        let p = self.primary();
        if p.is_empty() {
            let (a, b) = self.word_at(p.head);
            if a == b {
                return false;
            }
            self.set_primary(a, b);
            return true;
        }
        let needle = self.selected_text(p).to_owned();
        let after = self.sels.last().unwrap().end();
        let found = find_matches(&self.text, &needle, &FindOptions { case_sensitive: true, whole_word: false })
            .into_iter()
            .filter(|&(a, b)| !self.sels.iter().any(|s| s.start() == a && s.end() == b))
            .min_by_key(|&(a, _)| if a >= after { a - after } else { a + self.text.len() });
        match found {
            Some((a, b)) => {
                self.add_selection(a, b);
                true
            }
            None => false,
        }
    }

    /// Ctrl+Shift+L: selects every occurrence of the primary selection (or word).
    pub fn select_all_occurrences(&mut self) -> usize {
        let p = self.primary();
        let (needle, whole) = if p.is_empty() {
            let (a, b) = self.word_at(p.head);
            (self.text[a..b].to_owned(), true)
        } else {
            (self.selected_text(p).to_owned(), false)
        };
        if needle.trim().is_empty() {
            return 0;
        }
        let m = find_matches(&self.text, &needle, &FindOptions { case_sensitive: true, whole_word: whole });
        let n = m.len();
        // Keep the primary where it was.
        let primary_start = if p.is_empty() { self.word_at(p.head).0 } else { p.start() };
        let had_primary = m.iter().any(|&(a, _)| a == primary_start);
        let mut ranges: Vec<(usize, usize)> = m.into_iter().filter(|&(a, _)| a != primary_start).collect();
        if had_primary {
            ranges.push((primary_start, primary_start + needle.len()));
        }
        self.set_selections(&ranges);
        n
    }

    // ---- editing ---------------------------------------------------------------

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), sels: self.sels.clone(), primary: self.primary }
    }

    fn checkpoint(&mut self, kind: EditKind) {
        let now = Instant::now();
        let coalesce = kind != EditKind::Other
            && matches!(self.last_edit, Some((k, t)) if k == kind && now.duration_since(t) < COALESCE);
        if !coalesce {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = Some((kind, now));
    }

    /// Applies edits in one pass (sorted, overlaps clipped away).
    fn splice(&mut self, mut edits: Vec<Edit>) -> Vec<Applied> {
        edits.sort_by_key(|e| (e.start, e.end));
        let mut out = String::with_capacity(self.text.len() + edits.iter().map(|e| e.text.len()).sum::<usize>());
        let mut applied = Vec::with_capacity(edits.len());
        let mut pos = 0;
        for e in edits {
            let start = e.start.max(pos).min(self.text.len());
            let end = e.end.max(start).min(self.text.len());
            out.push_str(&self.text[pos..start]);
            let new_start = out.len();
            out.push_str(&e.text);
            applied.push(Applied { start, end, new_start, new_len: e.text.len() });
            pos = end;
        }
        out.push_str(&self.text[pos..]);
        self.text = out;
        self.rebuild_lines();
        self.version += 1;
        applied
    }

    /// One edit per selection; `f` returns the range to replace, the new text
    /// and optionally the new selection relative to the start of the new text
    /// (default: a cursor after it).
    fn edit_each(&mut self, kind: EditKind, mut f: impl FnMut(&Self, Selection) -> (Edit, Option<(usize, usize)>)) {
        let results: Vec<_> = self.sels.iter().map(|&s| f(self, s)).collect();
        if results.iter().all(|(e, _)| e.start == e.end && e.text.is_empty()) {
            return;
        }
        self.checkpoint(kind);
        let rel: Vec<_> = results.iter().map(|(e, r)| r.unwrap_or((e.text.len(), e.text.len()))).collect();
        let applied = self.splice(results.into_iter().map(|(e, _)| e).collect());
        // `splice` sorted the edits; the selections were already in order.
        self.sels = applied.iter().zip(rel).map(|(a, (x, y))| Selection::new(a.new_start + x, a.new_start + y)).collect();
        self.normalize();
    }

    /// Applies edits not tied to the selections; selections follow the text.
    pub fn apply_edits(&mut self, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        self.checkpoint(EditKind::Other);
        let applied = self.splice(edits);
        for s in &mut self.sels {
            s.anchor = map_offset(&applied, s.anchor);
            s.head = map_offset(&applied, s.head);
            s.goal = None;
        }
        self.normalize();
        self.end_typing();
    }

    /// Replaces every range with `text` in one undo step.
    pub fn replace_ranges(&mut self, ranges: &[(usize, usize)], text: &str) {
        self.apply_edits(ranges.iter().map(|&(start, end)| Edit { start, end, text: text.to_owned() }).collect());
    }

    /// Replaces the whole text with a version that differs only in whitespace
    /// (formatting); cursors keep their position relative to the non-blank text.
    pub fn reformat(&mut self, new: String) {
        if new == self.text {
            return;
        }
        self.checkpoint(EditKind::Other);
        let old = std::mem::replace(&mut self.text, new);
        let map = |pos: usize| -> usize {
            let n = old[..pos].chars().filter(|c| !c.is_whitespace()).count();
            let mut seen = 0;
            for (i, c) in self.text.char_indices() {
                if !c.is_whitespace() {
                    if seen == n {
                        // Keep a cursor that sat after a token on the token.
                        return if pos > 0 && old[..pos].ends_with(|c: char| !c.is_whitespace()) {
                            self.text[..i].trim_end().len()
                        } else {
                            i
                        };
                    }
                    seen += 1;
                }
            }
            self.text.trim_end().len()
        };
        self.sels = self.sels.iter().map(|s| Selection::new(map(s.anchor), map(s.head))).collect();
        self.rebuild_lines();
        self.version += 1;
        self.normalize();
        self.end_typing();
    }

    fn clean(&self, s: &str) -> String {
        if self.single_line { s.replace(['\r', '\n'], " ") } else { s.replace("\r\n", "\n").replace('\r', "\n") }
    }

    /// Replaces each selection with `s`.
    pub fn insert(&mut self, s: &str) {
        let s = self.clean(s);
        let kind = if s.chars().count() == 1 && s != "\n" { EditKind::Insert } else { EditKind::Other };
        self.edit_each(kind, |_, sel| (Edit { start: sel.start(), end: sel.end(), text: s.clone() }, None));
    }

    /// Pastes one piece per selection (when the clipboard has one line per cursor).
    pub fn insert_each(&mut self, parts: &[String]) {
        if parts.len() != self.sels.len() {
            return;
        }
        let mut parts = parts.iter();
        self.edit_each(EditKind::Other, |e, sel| {
            (Edit { start: sel.start(), end: sel.end(), text: e.clean(parts.next().unwrap()) }, None)
        });
    }

    /// Pastes whole lines above each cursor's line (text copied without a selection).
    pub fn insert_lines(&mut self, s: &str) {
        let s = self.clean(s);
        self.edit_each(EditKind::Other, |e, sel| {
            let start = e.line_start(e.line_of(sel.head));
            let rel = sel.head - start + s.len();
            (Edit { start, end: start, text: s.clone() }, Some((rel, rel)))
        });
    }

    /// Typing a character; `}` on an otherwise blank line dedents it.
    pub fn type_char(&mut self, c: char) {
        if c == '}' && !self.single_line {
            self.edit_each(EditKind::Insert, |e, sel| {
                let line = e.line_of(sel.start());
                let ls = e.line_start(line);
                let prefix = &e.text[ls..sel.start()];
                if sel.is_empty() && !prefix.is_empty() && prefix.chars().all(|c| c == ' ' || c == '\t') {
                    let col = visual_width(prefix);
                    let indent = " ".repeat((col - 1) / INDENT * INDENT);
                    (Edit { start: ls, end: sel.end(), text: indent + "}" }, None)
                } else {
                    (Edit { start: sel.start(), end: sel.end(), text: "}".into() }, None)
                }
            });
        } else {
            self.insert(c.encode_utf8(&mut [0; 4]));
        }
    }

    /// Enter: keeps the indentation, indents after `{` / `(` and splits `{}`.
    pub fn newline(&mut self) {
        if self.single_line {
            return;
        }
        self.edit_each(EditKind::Other, |e, sel| {
            let line = e.line_of(sel.start());
            let ls = e.line_start(line);
            let indent_end = e.first_non_space(line).min(sel.start());
            let indent = &e.text[ls..indent_end];
            let before = e.text[ls..sel.start()].trim_end();
            let after = &e.text[sel.end()..e.line_end(e.line_of(sel.end()))];
            // Drop the blanks before the cursor so no trailing whitespace is left.
            let start = ls + before.len().max(indent_end - ls);
            if before.ends_with(['{', '(', '[']) {
                let inner = format!("{indent}{}", " ".repeat(INDENT));
                let closes = after.trim_start().starts_with(['}', ')', ']']);
                let text = if closes { format!("\n{inner}\n{indent}") } else { format!("\n{inner}") };
                let after_ws = after.len() - after.trim_start().len();
                let rel = 1 + inner.len();
                (Edit { start, end: sel.end() + if closes { after_ws } else { 0 }, text }, Some((rel, rel)))
            } else {
                (Edit { start, end: sel.end(), text: format!("\n{indent}") }, None)
            }
        });
    }

    pub fn backspace(&mut self) {
        self.edit_each(EditKind::Delete, |e, sel| {
            if !sel.is_empty() || sel.head == 0 {
                return (Edit { start: sel.start(), end: sel.end(), text: String::new() }, None);
            }
            let ls = e.line_start(e.line_of(sel.head));
            let prefix = &e.text[ls..sel.head];
            let start = if !prefix.is_empty() && prefix.bytes().all(|b| b == b' ') {
                ls + (prefix.len() - 1) / INDENT * INDENT
            } else {
                e.prev_char(sel.head)
            };
            (Edit { start, end: sel.head, text: String::new() }, None)
        });
    }

    pub fn delete(&mut self) {
        self.edit_each(EditKind::Delete, |e, sel| {
            let end = if sel.is_empty() { e.next_char(sel.head) } else { sel.end() };
            (Edit { start: sel.start(), end, text: String::new() }, None)
        });
    }

    pub fn delete_word(&mut self, forward: bool) {
        self.edit_each(EditKind::Delete, |e, sel| {
            let (start, end) = match (sel.is_empty(), forward) {
                (false, _) => (sel.start(), sel.end()),
                (true, true) => (sel.head, e.word_right(sel.head)),
                (true, false) => (e.word_left(sel.head), sel.head),
            };
            (Edit { start, end, text: String::new() }, None)
        });
    }

    /// Lines touched by the selections (a selection ending at column 0 does not
    /// include that line), deduplicated and sorted.
    fn selected_lines(&self) -> Vec<usize> {
        let mut lines = Vec::new();
        for s in &self.sels {
            let first = self.line_of(s.start());
            let mut last = self.line_of(s.end());
            if last > first && self.line_start(last) == s.end() {
                last -= 1;
            }
            lines.extend(first..=last);
        }
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// Tab: indents the selected lines, or inserts spaces up to the next tab stop.
    pub fn indent(&mut self) {
        if self.single_line {
            return;
        }
        let multi = self.sels.iter().any(|s| self.line_of(s.start()) != self.line_of(s.end()));
        if multi {
            let edits = self
                .selected_lines()
                .into_iter()
                .filter(|&l| !self.line_text(l).trim().is_empty())
                .map(|l| Edit { start: self.line_start(l), end: self.line_start(l), text: " ".repeat(INDENT) })
                .collect();
            self.shift_lines(edits);
        } else {
            self.edit_each(EditKind::Insert, |e, sel| {
                let n = INDENT - e.col_of(sel.start()) % INDENT;
                (Edit { start: sel.start(), end: sel.end(), text: " ".repeat(n) }, None)
            });
        }
    }

    /// Shift+Tab: removes one level of indentation from the selected lines.
    pub fn outdent(&mut self) {
        if self.single_line {
            return;
        }
        let edits = self
            .selected_lines()
            .into_iter()
            .filter_map(|l| {
                let t = self.line_text(l);
                let n = if t.starts_with('\t') { 1 } else { t.bytes().take(INDENT).take_while(|&b| b == b' ').count() };
                (n > 0).then(|| Edit { start: self.line_start(l), end: self.line_start(l) + n, text: String::new() })
            })
            .collect();
        self.shift_lines(edits);
    }

    /// Indentation edits at line starts; a selection that began at a line start
    /// keeps covering the whole line.
    fn shift_lines(&mut self, edits: Vec<Edit>) {
        if edits.is_empty() {
            return;
        }
        let at_line_start: Vec<(bool, bool)> = self
            .sels
            .iter()
            .map(|s| (self.line_start(self.line_of(s.anchor)) == s.anchor, self.line_start(self.line_of(s.head)) == s.head))
            .collect();
        let starts: Vec<usize> = self.sels.iter().map(|s| self.line_of(s.anchor)).zip(self.sels.iter().map(|s| self.line_of(s.head))).flat_map(|(a, b)| [a, b]).collect();
        self.apply_edits(edits);
        for (i, s) in self.sels.iter_mut().enumerate() {
            if at_line_start[i].0 {
                s.anchor = self.line_starts[starts[2 * i]];
            }
            if at_line_start[i].1 {
                s.head = self.line_starts[starts[2 * i + 1]];
            }
        }
    }

    /// Ctrl+/: comments or uncomments the selected lines with `//`.
    pub fn toggle_line_comment(&mut self) {
        if self.single_line {
            return;
        }
        let lines: Vec<usize> = self.selected_lines().into_iter().filter(|&l| !self.line_text(l).trim().is_empty()).collect();
        if lines.is_empty() {
            return;
        }
        let commented = lines.iter().all(|&l| self.line_text(l).trim_start().starts_with("//"));
        let min_indent = lines.iter().map(|&l| self.first_non_space(l) - self.line_start(l)).min().unwrap_or(0);
        let edits = lines
            .iter()
            .map(|&l| {
                let first = self.first_non_space(l);
                if commented {
                    let rest = &self.text[first + 2..];
                    let n = 2 + usize::from(rest.starts_with(' '));
                    Edit { start: first, end: first + n, text: String::new() }
                } else {
                    let at = self.line_start(l) + min_indent;
                    Edit { start: at, end: at, text: "// ".into() }
                }
            })
            .collect();
        self.apply_edits(edits);
    }

    /// Text to copy: the selections joined by newlines, or the cursors' whole
    /// lines when nothing is selected (`true` in the second field).
    pub fn copy(&self) -> (String, bool) {
        if self.sels.iter().all(Selection::is_empty) {
            let mut lines: Vec<usize> = self.sels.iter().map(|s| self.line_of(s.head)).collect();
            lines.dedup();
            let mut out = String::new();
            for l in lines {
                let (a, b) = self.line_range(l);
                out.push_str(&self.text[a..b]);
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            (out, true)
        } else {
            let parts: Vec<&str> = self.sels.iter().filter(|s| !s.is_empty()).map(|&s| self.selected_text(s)).collect();
            (parts.join("\n"), false)
        }
    }

    /// Copies, then deletes what was copied.
    pub fn cut(&mut self) -> (String, bool) {
        let copied = self.copy();
        if copied.1 {
            let mut lines: Vec<usize> = self.sels.iter().map(|s| self.line_of(s.head)).collect();
            lines.dedup();
            let edits = lines
                .into_iter()
                .map(|l| {
                    let (mut a, b) = self.line_range(l);
                    // The last line has no line break of its own: take the previous one.
                    if b == self.text.len() && !self.text[a..b].ends_with('\n') && a > 0 {
                        a -= 1;
                    }
                    Edit { start: a, end: b, text: String::new() }
                })
                .collect();
            self.apply_edits(edits);
        } else {
            self.edit_each(EditKind::Other, |_, s| (Edit { start: s.start(), end: s.end(), text: String::new() }, None));
        }
        copied
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        self.restore(s);
        true
    }

    fn restore(&mut self, s: Snapshot) {
        self.text = s.text;
        self.sels = s.sels;
        self.primary = s.primary;
        self.rebuild_lines();
        self.version += 1;
        self.normalize();
        self.end_typing();
    }
}

/// Width of `s` in visual columns (tab = [`INDENT`]).
pub fn visual_width(s: &str) -> usize {
    s.chars().map(|c| if c == '\t' { INDENT } else { 1 }).sum()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
}

/// Non-overlapping occurrences of `needle` in `text`, as byte ranges.
pub fn find_matches(text: &str, needle: &str, opts: &FindOptions) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let needle_chars: Vec<char> = needle.chars().map(|c| if opts.case_sensitive { c } else { fold(c) }).collect();
    let mut i = 0;
    while i < text.len() {
        let mut chars = text[i..].char_indices();
        let mut end = None;
        for (k, &n) in needle_chars.iter().enumerate() {
            match chars.next() {
                Some((j, c)) if (if opts.case_sensitive { c } else { fold(c) }) == n => {
                    if k + 1 == needle_chars.len() {
                        end = Some(i + j + c.len_utf8());
                    }
                }
                _ => break,
            }
        }
        if let Some(end) = end {
            let word_ok = !opts.whole_word
                || (!text[..i].chars().next_back().is_some_and(is_word_char)
                    && !text[end..].chars().next().is_some_and(is_word_char));
            if word_ok {
                out.push((i, end));
                i = end;
                continue;
            }
        }
        i += text[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(text: &str, at: usize) -> Editor {
        let mut e = Editor::new(text);
        e.set_cursor(at);
        e
    }

    #[test]
    fn typing_and_undo() {
        let mut e = ed("ab", 1);
        e.insert("x");
        e.insert("y");
        assert_eq!(e.text(), "axyb");
        assert!(e.undo());
        assert_eq!(e.text(), "ab");
        assert_eq!(e.primary().head, 1);
        assert!(e.redo());
        assert_eq!(e.text(), "axyb");
    }

    #[test]
    fn multi_cursor_insert_and_backspace() {
        let mut e = Editor::new("a\nb\nc");
        e.set_cursor(0);
        e.add_cursor_vertical(true);
        e.add_cursor_vertical(true);
        assert_eq!(e.selections().len(), 3);
        e.insert("- ");
        assert_eq!(e.text(), "- a\n- b\n- c");
        e.backspace();
        assert_eq!(e.text(), "-a\n-b\n-c");
    }

    #[test]
    fn newline_indents_after_brace() {
        let mut e = ed("part def A {}", 12);
        e.newline();
        assert_eq!(e.text(), "part def A {\n    \n}");
        assert_eq!(e.primary().head, 17);
        e.type_char('x');
        e.newline();
        e.type_char('}');
        assert_eq!(e.text(), "part def A {\n    x\n}\n}");
    }

    #[test]
    fn backspace_removes_indent_unit() {
        let mut e = ed("        x", 8);
        e.backspace();
        assert_eq!(e.text(), "    x");
    }

    #[test]
    fn indent_and_outdent_lines() {
        let mut e = Editor::new("a\nb\n");
        e.set_selection(0, 3);
        e.indent();
        assert_eq!(e.text(), "    a\n    b\n");
        assert_eq!((e.primary().start(), e.primary().end()), (0, 11));
        e.outdent();
        assert_eq!(e.text(), "a\nb\n");
    }

    #[test]
    fn copy_cut_lines() {
        let mut e = ed("one\ntwo\nthree", 5);
        assert_eq!(e.copy(), ("two\n".to_string(), true));
        e.cut();
        assert_eq!(e.text(), "one\nthree");
        e.set_cursor(6);
        e.cut();
        assert_eq!(e.text(), "one");
    }

    #[test]
    fn next_occurrence_and_find() {
        let mut e = ed("foo bar foo foobar", 1);
        assert!(e.add_next_occurrence());
        assert_eq!(e.selected_text(e.primary()), "foo");
        assert!(e.add_next_occurrence());
        assert!(e.add_next_occurrence());
        assert_eq!(e.selections().len(), 3);
        let m = find_matches(e.text(), "FOO", &FindOptions { case_sensitive: false, whole_word: true });
        assert_eq!(m, vec![(0, 3), (8, 11)]);
    }

    #[test]
    fn vertical_motion_keeps_goal() {
        let mut e = ed("abcdef\nab\nabcdef", 5);
        e.move_cursors(Motion::Down, false);
        assert_eq!(e.primary().head, 9);
        e.move_cursors(Motion::Down, false);
        assert_eq!(e.primary().head, 15);
    }

    #[test]
    fn word_motion() {
        let mut e = ed("part engine : Engine;", 0);
        e.move_cursors(Motion::WordRight, false);
        assert_eq!(e.primary().head, 4);
        e.move_cursors(Motion::WordRight, true);
        assert_eq!(e.selected_text(e.primary()), " engine");
        e.delete_word(false);
        assert_eq!(e.text(), "part : Engine;");
    }

    #[test]
    fn toggle_comment() {
        let mut e = Editor::new("  a\n  b");
        e.select_all();
        e.toggle_line_comment();
        assert_eq!(e.text(), "  // a\n  // b");
        e.toggle_line_comment();
        assert_eq!(e.text(), "  a\n  b");
    }

    #[test]
    fn reformat_keeps_cursor_on_token() {
        let mut e = ed("a   b", 5);
        e.reformat("a b".into());
        assert_eq!(e.primary().head, 3);
    }
}
