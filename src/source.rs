//! SysML source helpers for the code pane: syntax highlighting, the
//! formatter and the symbol lookup behind "rename symbol". All work on the
//! token stream from [`sysml::lexer`], so strings and comments are never
//! mistaken for code.

use crate::editor::INDENT;
use sysml::lexer::{self, Token, TokenKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight {
    Keyword,
    /// Name declared by a definition or package.
    Definition,
    /// Name after `:`, `:>`, `:>>`, `specializes`, ... (a type or redefined feature).
    Type,
    Name,
    String,
    Number,
    Comment,
    Symbol,
}

/// Byte ranges with their highlight, sorted and non-overlapping. Text not
/// covered (whitespace, invalid characters) uses the default color.
pub fn highlight(src: &str) -> Vec<(usize, usize, Highlight)> {
    let mut diags = Vec::new();
    let tokens = lexer::lex(src, &mut diags);
    let mut out = Vec::with_capacity(tokens.len() + 16);
    let mut pos = 0;
    let mut prev: Option<TokenKind> = None;
    for t in &tokens {
        notes(src, pos, t.span.start, &mut out);
        pos = t.span.end;
        let h = match t.kind {
            TokenKind::Eof => break,
            TokenKind::Keyword(_) => Highlight::Keyword,
            TokenKind::Ident | TokenKind::UnrestrictedName => match prev {
                Some(TokenKind::Keyword("def" | "package" | "library")) => Highlight::Definition,
                Some(TokenKind::Symbol(":" | ":>" | ":>>" | "::>"))
                | Some(TokenKind::Keyword("specializes" | "subsets" | "redefines" | "references" | "typed" | "by")) => {
                    Highlight::Type
                }
                // `A::B` keeps the first segment's color for the rest of the name.
                Some(TokenKind::Symbol("::")) => match out.iter().rev().nth(1) {
                    Some(&(_, _, h @ (Highlight::Type | Highlight::Definition | Highlight::Name))) => h,
                    _ => Highlight::Name,
                },
                _ => Highlight::Name,
            },
            TokenKind::String => Highlight::String,
            TokenKind::Integer | TokenKind::Real => Highlight::Number,
            TokenKind::BlockComment => Highlight::Comment,
            TokenKind::Symbol(_) => Highlight::Symbol,
        };
        out.push((t.span.start, t.span.end, h));
        prev = Some(t.kind);
    }
    out
}

/// `//` and `//* */` notes in the gap `from..to` between tokens.
fn notes(src: &str, from: usize, to: usize, out: &mut Vec<(usize, usize, Highlight)>) {
    let gap = &src[from..to];
    let mut i = 0;
    while let Some(k) = gap[i..].find("//") {
        let start = i + k;
        let end = if gap[start + 2..].starts_with('*') {
            gap[start + 3..].find("*/").map_or(gap.len(), |e| start + 3 + e + 2)
        } else {
            gap[start..].find('\n').map_or(gap.len(), |e| start + e)
        };
        out.push((from + start, from + end, Highlight::Comment));
        i = end;
    }
}

// ---- symbols ---------------------------------------------------------------

fn tokens(src: &str) -> Vec<Token> {
    let mut diags = Vec::new();
    lexer::lex(src, &mut diags)
}

/// The name a name token stands for (quotes removed, escapes resolved).
fn name_of(src: &str, t: &Token) -> Option<String> {
    let raw = t.span.text(src);
    match t.kind {
        TokenKind::Ident => Some(raw.to_owned()),
        TokenKind::UnrestrictedName => {
            let inner = raw.strip_prefix('\'').unwrap_or(raw);
            Some(lexer::unescape(inner.strip_suffix('\'').unwrap_or(inner)))
        }
        _ => None,
    }
}

/// The name under (or just before) `offset` and its span.
pub fn symbol_at(src: &str, offset: usize) -> Option<(String, (usize, usize))> {
    let toks = tokens(src);
    let hit = |t: &&Token| matches!(t.kind, TokenKind::Ident | TokenKind::UnrestrictedName);
    let t = toks
        .iter()
        .filter(hit)
        .find(|t| t.span.start <= offset && offset < t.span.end)
        .or_else(|| toks.iter().filter(hit).find(|t| t.span.end == offset))?;
    Some((name_of(src, t)?, (t.span.start, t.span.end)))
}

/// The name written at `offset` with the names before it that it is
/// reached through: `Vehicles::Car` or `car.engine.mass`, up to and
/// including the name under `offset`. Each group of the result is a
/// qualified name (`A::B`); groups are joined by `.` in the source. Also
/// returns the hovered name's span and the token kind just before the path
/// (`:`, `:>>`, `[`, ...).
pub fn path_at(src: &str, offset: usize) -> Option<NamePath> {
    let toks = tokens(src);
    let is_name = |t: &Token| matches!(t.kind, TokenKind::Ident | TokenKind::UnrestrictedName);
    let at = toks.iter().position(|t| is_name(t) && t.span.start <= offset && offset < t.span.end)?;
    let mut groups: Vec<Vec<String>> = vec![vec![name_of(src, &toks[at])?]];
    let mut i = at;
    while i >= 2 && is_name(&toks[i - 2]) {
        match toks[i - 1].kind {
            TokenKind::Symbol("::") => groups[0].insert(0, name_of(src, &toks[i - 2])?),
            TokenKind::Symbol(".") => groups.insert(0, vec![name_of(src, &toks[i - 2])?]),
            _ => break,
        }
        i -= 2;
    }
    let before = i.checked_sub(1).map(|j| toks[j].kind);
    Some(NamePath { groups, span: (toks[at].span.start, toks[at].span.end), before })
}

/// See [`path_at`].
#[derive(Clone, Debug, PartialEq)]
pub struct NamePath {
    pub groups: Vec<Vec<String>>,
    pub span: (usize, usize),
    pub before: Option<TokenKind>,
}

/// Every token naming `name`, in either spelling (`Engine` or `'Engine'`).
pub fn occurrences(src: &str, name: &str) -> Vec<(usize, usize)> {
    tokens(src)
        .iter()
        .filter(|t| name_of(src, t).as_deref() == Some(name))
        .map(|t| (t.span.start, t.span.end))
        .collect()
}

/// `name` as source text: a basic name if possible, else a quoted name.
pub fn spell_name(name: &str) -> String {
    let mut chars = name.chars();
    let basic = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !lexer::is_keyword(name);
    if basic {
        name.to_owned()
    } else {
        let escaped = name.replace('\\', "\\\\").replace('\'', "\\'");
        format!("'{escaped}'")
    }
}

// ---- formatting --------------------------------------------------------------

/// Operators written with a space on both sides.
fn spaced(sym: &str) -> bool {
    matches!(sym, ":" | ":>" | ":>>" | "::>" | "=" | ":=" | "=>" | "==" | "!=" | "===" | "!==" | "<=" | ">=")
}

/// Spacing between two tokens on one line; `gap` is the original whitespace.
fn spacing(prev: TokenKind, next: TokenKind, gap: &str) -> &'static str {
    use TokenKind::Symbol as S;
    match (prev, next) {
        (_, S(";" | "," | ")" | "]" | "::" | ".")) => "",
        (S("(" | "[" | "::" | "." | "#" | "@"), _) => "",
        (S(","), _) => " ",
        (_, S("{")) => " ",
        (S(p), _) if spaced(p) => " ",
        (_, S(n)) if spaced(n) => " ",
        _ if gap.is_empty() => "",
        _ => " ",
    }
}

fn is_open(k: TokenKind) -> bool {
    matches!(k, TokenKind::Symbol("{" | "(" | "["))
}

fn is_close(k: TokenKind) -> bool {
    matches!(k, TokenKind::Symbol("}" | ")" | "]"))
}

/// Text that spans lines and must keep its inner lines: comments and notes
/// (whose inner lines are re-indented with their first line) and strings
/// (kept exactly).
struct Block {
    start: usize,
    end: usize,
    shift: bool,
}

/// Re-indents by bracket depth, normalizes spacing between tokens, trims
/// trailing whitespace and collapses runs of blank lines. Only whitespace
/// changes; text inside strings and comments is kept.
pub fn format(src: &str) -> String {
    let src = src.replace("\r\n", "\n");
    let all = tokens(&src);
    let toks = &all[..all.len() - 1]; // without Eof

    let mut blocks: Vec<Block> = Vec::new();
    let mut gap_start = 0;
    for t in &all {
        let mut found = Vec::new();
        notes(&src, gap_start, t.span.start, &mut found);
        blocks.extend(found.into_iter().map(|(s, e, _)| Block { start: s, end: e, shift: true }));
        if t.kind != TokenKind::Eof {
            match t.kind {
                TokenKind::BlockComment => blocks.push(Block { start: t.span.start, end: t.span.end, shift: true }),
                TokenKind::String | TokenKind::UnrestrictedName => {
                    blocks.push(Block { start: t.span.start, end: t.span.end, shift: false })
                }
                _ => {}
            }
        }
        gap_start = t.span.end;
    }
    blocks.retain(|b| src[b.start..b.end].contains('\n'));
    blocks.sort_by_key(|b| b.start);

    let mut line_starts = vec![0];
    line_starts.extend(src.bytes().enumerate().filter(|&(_, b)| b == b'\n').map(|(i, _)| i + 1));

    let mut out_lines: Vec<(String, bool)> = Vec::with_capacity(line_starts.len());
    let mut depth: usize = 0;
    let mut ti = 0; // next token
    let mut bi = 0; // block that may contain the current line
    // Indentation change applied to the inner lines of the current block.
    let mut shift: isize = 0;
    // Last token so far: a statement that has not ended continues on the next line.
    let mut last: Option<TokenKind> = None;

    for (l, &ls) in line_starts.iter().enumerate() {
        let le = line_starts.get(l + 1).map_or(src.len(), |&n| n - 1);
        let line = &src[ls..le];
        while bi < blocks.len() && blocks[bi].end < ls {
            bi += 1;
        }
        let inside = blocks.get(bi).filter(|b| b.start < ls && ls <= b.end);
        // Tokens starting on this line (earlier lines consumed the ones before).
        let first_tok = ti;
        while ti < toks.len() && toks[ti].span.start < le {
            ti += 1;
        }
        let line_toks = &toks[first_tok..ti];
        let depth_before = depth;
        let continues = last.is_some_and(|k| !matches!(k, TokenKind::Symbol(";" | "}") | TokenKind::BlockComment) && !is_open(k));
        if let Some(t) = line_toks.last() {
            last = Some(t.kind);
        }
        for t in line_toks {
            if is_open(t.kind) {
                depth += 1;
            } else if is_close(t.kind) {
                depth = depth.saturating_sub(1);
            }
        }

        if let Some(b) = inside {
            // Inner line of a comment, note or string.
            let text = if b.shift {
                let indent = line.len() - line.trim_start_matches(' ').len();
                let new = (indent as isize + shift).max(0) as usize;
                format!("{}{}", " ".repeat(new), &line[indent..]).trim_end().to_owned()
            } else {
                line.to_owned()
            };
            out_lines.push((text, true));
            continue;
        }

        let content = line.trim_start_matches([' ', '\t']);
        let content_start = le - content.len();
        if content.trim().is_empty() {
            out_lines.push((String::new(), false));
            continue;
        }
        let level = if line_toks.first().is_some_and(|t| is_close(t.kind) && t.span.start == content_start) {
            depth_before.saturating_sub(1)
        } else {
            depth_before + usize::from(continues)
        };
        let indent = level * INDENT;
        let old_indent = visual(&line[..content_start - ls]);

        // Rebuild the line from its tokens and the gaps between them.
        let mut text = " ".repeat(indent);
        let mut pos = content_start;
        let mut prev: Option<TokenKind> = None;
        for t in line_toks {
            let gap = &src[pos..t.span.start];
            if gap.trim().is_empty() {
                if let Some(p) = prev {
                    text.push_str(spacing(p, t.kind, gap));
                }
            } else {
                // A `//* */` note between tokens: keep it with single spaces around.
                if prev.is_some() {
                    text.push(' ');
                }
                text.push_str(gap.trim());
                text.push(' ');
            }
            let end = t.span.end.min(le);
            text.push_str(&src[t.span.start..end]);
            pos = end;
            prev = Some(t.kind);
        }
        let tail = &src[pos..le];
        if !tail.trim().is_empty() {
            let note = tail.trim_start();
            if prev.is_some() {
                // Keep a trailing note's alignment, but at least one space.
                let ws = &tail[..tail.len() - note.len()];
                text.push_str(if ws.is_empty() { " " } else { ws });
            }
            text.push_str(note);
        }
        // A block starting on this line shifts its inner lines along.
        if let Some(b) = blocks.get(bi).filter(|b| b.start >= ls && b.start <= le) {
            shift = if b.shift { indent as isize - old_indent as isize } else { 0 };
        }
        let keep_tail = blocks.get(bi).is_some_and(|b| b.start >= ls && b.start <= le && !b.shift);
        out_lines.push((if keep_tail { text } else { text.trim_end().to_owned() }, false));
    }

    // Collapse blank runs; drop leading and trailing blank lines.
    let mut out = String::with_capacity(src.len());
    let mut blank_run = 0;
    let mut started = false;
    for (line, verbatim) in &out_lines {
        if line.is_empty() && !verbatim {
            blank_run += 1;
            continue;
        }
        if started && blank_run > 0 {
            out.push('\n');
        }
        blank_run = 0;
        started = true;
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn visual(s: &str) -> usize {
    crate::editor::visual_width(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_indentation_and_spacing() {
        let src = "package P {\npart def A{\n  attribute m:Real ;   // mass\n\n\n   part e :Engine[1] ;\n}\n}\n\n";
        let expected = "package P {\n    part def A {\n        attribute m : Real;   // mass\n\n        part e : Engine[1];\n    }\n}\n";
        assert_eq!(format(src), expected);
        assert_eq!(format(expected), expected);
    }

    #[test]
    fn keeps_comment_inner_lines() {
        let src = "package P {\n/*\n * doc\n */\npart x;\n}\n";
        let expected = "package P {\n    /*\n     * doc\n     */\n    part x;\n}\n";
        assert_eq!(format(src), expected);
    }

    #[test]
    fn format_changes_only_whitespace() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/sysml/tests/data/drone.sysml")).unwrap();
        let out = format(&src);
        let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(strip(&src), strip(&out));
        assert_eq!(format(&out), out, "formatting is stable");
        let messages = |s: &str| sysml::parse(s).diagnostics.into_iter().map(|d| d.message).collect::<Vec<_>>();
        assert_eq!(messages(&out), messages(&src));
    }

    #[test]
    fn rename_lookup() {
        let src = "part def Engine; part e : Engine; part f : 'Engine'; doc /* Engine */";
        let (name, _) = symbol_at(src, 10).unwrap();
        assert_eq!(name, "Engine");
        assert_eq!(occurrences(src, &name).len(), 3);
        assert_eq!(spell_name("Big Engine"), "'Big Engine'");
        assert_eq!(spell_name("part"), "'part'");
        assert_eq!(spell_name("Motor"), "Motor");
    }

    #[test]
    fn highlights_declarations_and_notes() {
        let src = "part def A :> B; // note";
        let h = highlight(src);
        let kinds: Vec<_> = h.iter().map(|&(_, _, k)| k).collect();
        use Highlight::*;
        assert_eq!(kinds, [Keyword, Keyword, Definition, Symbol, Type, Symbol, Comment]);
    }
}
