//! Tokenizer for the SysML v2 textual notation.
//!
//! `//` line notes and `//* ... */` block notes are skipped. Regular comments
//! `/* ... */` are kept as [`TokenKind::BlockComment`] tokens, because SysML treats
//! them as model elements (`doc`, `comment`).

use crate::Diagnostic;
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// A basic name such as `Vehicle`.
    Ident,
    /// A quoted name such as `'Vehicle Model'` (span includes the quotes).
    UnrestrictedName,
    /// A reserved word, e.g. `part`.
    Keyword(&'static str),
    Integer,
    Real,
    /// A string literal (span includes the quotes).
    String,
    /// `/* ... */` (span includes the delimiters).
    BlockComment,
    /// Punctuation or operator, e.g. `:>>`.
    Symbol(&'static str),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// SysML v2 reserved words, sorted for binary search.
pub const KEYWORDS: &[&str] = &[
    "about", "abstract", "accept", "action", "actor", "after", "alias", "all", "allocate",
    "allocation", "analysis", "and", "as", "assert", "assign", "assume", "at", "attribute", "bind",
    "binding", "by", "calc", "case", "comment", "concern", "connect", "connection", "constant",
    "constraint", "crosses", "decide", "def", "default", "defined", "dependency", "derived", "do",
    "doc", "else", "end", "entry", "enum", "event", "exhibit", "exit", "expose", "false", "filter",
    "first", "flow", "for", "fork", "frame", "from", "hastype", "if", "implies", "import", "in",
    "include", "individual", "inout", "interface", "istype", "item", "join", "language", "library",
    "locale", "loop", "merge", "message", "meta", "metadata", "new", "nonunique", "not", "null",
    "objective", "occurrence", "of", "or", "ordered", "out", "package", "parallel", "part",
    "perform", "port", "private", "protected", "public", "readonly", "redefines", "ref",
    "references", "render", "rendering", "rep", "require", "requirement", "return", "satisfy",
    "send", "snapshot", "specializes", "stakeholder", "standard", "state", "subject", "subsets",
    "succession", "terminate", "then", "timeslice", "to", "transition", "true", "typed", "until",
    "use", "variant", "variation", "verification", "verify", "via", "view", "viewpoint", "when",
    "while", "xor",
];

/// Punctuation and operators, longest first so the first match is the longest.
const SYMBOLS: &[&str] = &[
    "::>", ":>>", "===", "!==", "::", ":>", ":=", "==", "!=", "<=", ">=", "->", "**", "=>", "??",
    "..", "{", "}", "(", ")", "[", "]", ";", ",", ":", "=", ".", "*", "+", "-", "/", "%", "^", "<",
    ">", "@", "#", "~", "?", "&", "|", "!", "$",
];

/// Is `text` a reserved word?
pub fn is_keyword(text: &str) -> bool {
    KEYWORDS.binary_search(&text).is_ok()
}

/// Splits `src` into tokens. The result always ends with [`TokenKind::Eof`].
/// Problems are reported to `diags`; lexing always continues.
pub fn lex(src: &str, diags: &mut Vec<Diagnostic>) -> Vec<Token> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0;
    let digit_at = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);

    while i < len {
        let c = bytes[i];
        let start = i;

        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Notes: `// ...` and `//* ... */`.
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            if bytes.get(i + 2) == Some(&b'*') {
                match find(src, i + 3, "*/") {
                    Some(end) => i = end + 2,
                    None => {
                        diags.push(Diagnostic::error("unterminated note", Span::new(start, len)));
                        i = len;
                    }
                }
            } else {
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            continue;
        }

        let kind = if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = match find(src, i + 2, "*/") {
                Some(end) => end + 2,
                None => {
                    diags.push(Diagnostic::error("unterminated comment", Span::new(start, len)));
                    len
                }
            };
            TokenKind::BlockComment
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            match KEYWORDS.binary_search(&&src[start..i]) {
                Ok(k) => TokenKind::Keyword(KEYWORDS[k]),
                Err(_) => TokenKind::Ident,
            }
        } else if c.is_ascii_digit() {
            let mut kind = TokenKind::Integer;
            while digit_at(i) {
                i += 1;
            }
            if bytes.get(i) == Some(&b'.') && digit_at(i + 1) {
                i += 1;
                while digit_at(i) {
                    i += 1;
                }
                kind = TokenKind::Real;
            }
            if matches!(bytes.get(i), Some(b'e' | b'E')) {
                let mut j = i + 1;
                if matches!(bytes.get(j), Some(b'+' | b'-')) {
                    j += 1;
                }
                if digit_at(j) {
                    i = j;
                    while digit_at(i) {
                        i += 1;
                    }
                    kind = TokenKind::Real;
                }
            }
            kind
        } else if c == b'\'' || c == b'"' {
            i += 1;
            let mut closed = false;
            while i < len {
                match bytes[i] {
                    b'\\' => i += 2,
                    b if b == c => {
                        i += 1;
                        closed = true;
                        break;
                    }
                    _ => i += 1,
                }
            }
            i = i.min(len);
            if !closed {
                let what = if c == b'"' { "string" } else { "quoted name" };
                diags.push(Diagnostic::error(format!("unterminated {what}"), Span::new(start, len)));
            }
            if c == b'"' { TokenKind::String } else { TokenKind::UnrestrictedName }
        } else if let Some(sym) = SYMBOLS.iter().find(|s| src[i..].starts_with(**s)) {
            i += sym.len();
            TokenKind::Symbol(sym)
        } else {
            let ch = src[i..].chars().next().unwrap_or('\u{fffd}');
            i += ch.len_utf8();
            diags.push(Diagnostic::error(format!("unexpected character '{ch}'"), Span::new(start, i)));
            continue;
        };
        tokens.push(Token { kind, span: Span::new(start, i) });
    }
    tokens.push(Token { kind: TokenKind::Eof, span: Span::new(len, len) });
    tokens
}

fn find(src: &str, from: usize, pat: &str) -> Option<usize> {
    src.get(from..)?.find(pat).map(|p| p + from)
}

/// Removes the surrounding quote characters of a string or quoted name, if present.
pub(crate) fn strip_delimiters(raw: &str, quote: char) -> &str {
    let inner = raw.strip_prefix(quote).unwrap_or(raw);
    inner.strip_suffix(quote).unwrap_or(inner)
}

/// Resolves backslash escapes in string literals and quoted names.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('n') => '\n',
            Some('t') => '\t',
            Some('r') => '\r',
            Some('b') => '\u{8}',
            Some('f') => '\u{c}',
            Some(other) => other,
            None => '\\',
        });
    }
    out
}

/// The text of a `/* ... */` comment: delimiters and leading `*` decorations
/// removed, surrounding blank lines dropped.
pub fn comment_text(raw: &str) -> String {
    let inner = raw.strip_prefix("/*").unwrap_or(raw);
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    let lines: Vec<&str> = inner
        .lines()
        .map(|line| {
            let t = line.trim();
            t.strip_prefix('*').map(|r| r.strip_prefix(' ').unwrap_or(r)).unwrap_or(t).trim_end()
        })
        .collect();
    let first = lines.iter().position(|l| !l.is_empty());
    let last = lines.iter().rposition(|l| !l.is_empty());
    match (first, last) {
        (Some(f), Some(l)) => lines[f..=l].join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let mut d = Vec::new();
        let t = lex(src, &mut d);
        assert!(d.is_empty(), "{d:?}");
        t.into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn keywords_sorted() {
        assert!(KEYWORDS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn symbols_longest_match() {
        use TokenKind::*;
        assert_eq!(
            kinds(":>> :> ::> :: [0..*]"),
            [
                Symbol(":>>"),
                Symbol(":>"),
                Symbol("::>"),
                Symbol("::"),
                Symbol("["),
                Integer,
                Symbol(".."),
                Symbol("*"),
                Symbol("]"),
                Eof
            ]
        );
    }

    #[test]
    fn numbers_names_comments() {
        use TokenKind::*;
        assert_eq!(
            kinds("part 'my part' 1.5e3 2 // note\n /* doc */ //* block note */ \"s\\\"\""),
            [Keyword("part"), UnrestrictedName, Real, Integer, BlockComment, String, Eof]
        );
    }

    #[test]
    fn comment_cleanup() {
        assert_eq!(comment_text("/*\n * Line one\n * Line two\n */"), "Line one\nLine two");
        assert_eq!(comment_text("/* single */"), "single");
    }

    #[test]
    fn reports_errors() {
        let mut d = Vec::new();
        lex("part § x 'open", &mut d);
        assert_eq!(d.len(), 2);
    }
}
