//! The SysML v2 quick reference (`docs/sysml-reference.toml`), embedded in the
//! binary, and the lookups from keywords, symbols and diagram kinds to its
//! entries that the hover help uses.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

const SOURCE: &str = include_str!("../docs/sysml-reference.toml");

/// Prefix keywords that only qualify the keyword after them: a compartment
/// line `in attribute x` is documented by `attribute`, not by `in`.
const MODIFIERS: &[&str] = &[
    "abstract", "constant", "derived", "end", "in", "individual", "inout", "nonunique", "ordered", "out", "private",
    "protected", "public", "readonly", "ref", "variation",
];

#[derive(Deserialize)]
pub struct Category {
    pub id: String,
    pub title: String,
}

#[derive(Deserialize)]
pub struct Entry {
    pub names: Vec<String>,
    pub category: String,
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub syntax: Vec<String>,
    #[serde(default)]
    details: String,
    #[serde(default)]
    pub example: Option<String>,
    #[serde(default)]
    pub see: Vec<String>,
    #[serde(default)]
    pub spec: Option<String>,
}

impl Entry {
    /// The details as paragraphs: hard line breaks inside a paragraph become
    /// spaces and the `code` backticks are dropped.
    pub fn paragraphs(&self) -> Vec<String> {
        self.details
            .split("\n\n")
            .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" ").replace('`', ""))
            .filter(|p| !p.is_empty())
            .collect()
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(default, rename = "category")]
    categories: Vec<Category>,
    #[serde(default, rename = "entry")]
    entries: Vec<Entry>,
}

pub struct Reference {
    categories: Vec<Category>,
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
}

/// The embedded reference, parsed on first use (empty if it does not parse,
/// which the tests rule out).
pub fn reference() -> &'static Reference {
    static REFERENCE: OnceLock<Reference> = OnceLock::new();
    REFERENCE.get_or_init(|| {
        Reference::parse(SOURCE).unwrap_or_else(|e| {
            eprintln!("sysml-reference.toml: {e}");
            Reference { categories: Vec::new(), entries: Vec::new(), by_name: HashMap::new() }
        })
    })
}

impl Reference {
    fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut by_name = HashMap::new();
        for (i, e) in file.entries.iter().enumerate() {
            for name in &e.names {
                by_name.entry(name.clone()).or_insert(i);
            }
        }
        Ok(Self { categories: file.categories, entries: file.entries, by_name })
    }

    pub fn entry(&self, i: usize) -> &Entry {
        &self.entries[i]
    }

    pub fn category_title<'a>(&'a self, id: &'a str) -> &'a str {
        self.categories.iter().find(|c| c.id == id).map_or(id, |c| c.title.as_str())
    }

    pub fn lookup(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// The entry for a reserved word.
    pub fn for_keyword(&self, keyword: &str) -> Option<usize> {
        self.lookup(keyword)
    }

    /// The entry for a punctuation token; brackets are documented as pairs.
    pub fn for_symbol(&self, symbol: &str) -> Option<usize> {
        match symbol {
            "[" | "]" => self.lookup("[ ]"),
            "{" | "}" => self.lookup("{ }"),
            _ => self.lookup(symbol),
        }
    }

    /// The entry for a diagram node's kind label (`part def`, `use case`,
    /// `analysis case def`, ...): the whole label, then without `def`, then
    /// its first documented word.
    pub fn for_kind(&self, label: &str) -> Option<usize> {
        let label = label.trim();
        let bare = label.strip_suffix(" def").unwrap_or(label);
        self.lookup(label)
            .or_else(|| self.lookup(bare))
            .or_else(|| bare.split_whitespace().find_map(|w| self.lookup(w)))
            .or_else(|| (bare != label).then(|| self.lookup("def")).flatten())
    }

    /// The entry for a diagram compartment line (`in attribute mass : Real`):
    /// the first documented keyword that is not a mere prefix, else the first
    /// documented keyword among the leading ones.
    pub fn for_line(&self, line: &str) -> Option<usize> {
        let leading: Vec<&str> = line.split_whitespace().take_while(|w| sysml::lexer::is_keyword(w)).collect();
        leading
            .iter()
            .filter(|w| !MODIFIERS.contains(w))
            .find_map(|w| self.lookup(w))
            .or_else(|| leading.iter().find_map(|w| self.lookup(w)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_covers_every_keyword() {
        let r = Reference::parse(SOURCE).expect("sysml-reference.toml parses");
        assert!(!r.entries.is_empty());
        for kw in sysml::lexer::KEYWORDS {
            assert!(r.lookup(kw).is_some(), "no entry for keyword `{kw}`");
        }
        for e in &r.entries {
            assert!(r.categories.iter().any(|c| c.id == e.category), "unknown category {}", e.category);
        }
    }

    #[test]
    fn kinds_and_lines() {
        let r = Reference::parse(SOURCE).unwrap();
        let first = |i: Option<usize>| i.map(|i| r.entry(i).names[0].as_str());
        assert_eq!(first(r.for_kind("part def")), Some("part"));
        assert_eq!(first(r.for_kind("requirement")), Some("requirement"));
        assert_eq!(first(r.for_kind("use case def")), Some("use"));
        assert_eq!(first(r.for_kind("analysis case")), Some("analysis"));
        assert_eq!(first(r.for_line("in attribute mass : Real")), Some("attribute"));
        assert_eq!(first(r.for_line("ref part owner")), Some("part"));
        assert_eq!(first(r.for_line("in x : Real")), Some("in"));
        assert_eq!(first(r.for_symbol("]")), Some("[ ]"));
        assert_eq!(first(r.for_symbol(":>")), Some("specializes"));
        assert!(r.for_symbol(",").is_none());
    }

    #[test]
    fn paragraphs_are_joined() {
        let r = Reference::parse(SOURCE).unwrap();
        let p = r.entry(r.lookup("part").unwrap()).paragraphs();
        assert!(!p.is_empty());
        assert!(p.iter().all(|p| !p.contains('\n') && !p.contains('`')));
    }
}
