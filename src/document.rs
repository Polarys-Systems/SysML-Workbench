//! A SysML file: its source text, element positions, diagnostics and the
//! diagram graph. The code pane edits the text and hands it back for reparsing.

use crate::diagram::Diagram;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use sysml::eval::{Status, Verdict};
use sysml::{Category, Index, LineIndex, Member, Span};

/// A model element and where it is written.
pub struct Element {
    pub span: Span,
    /// Qualified name, if the element is named and addressable.
    pub qualified_name: Option<String>,
}

pub struct Diagnostic {
    pub span: Span,
    /// `line:col: message`
    pub text: String,
}

/// How an annotation reads: a plain value, something missing, good or bad news.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Value,
    Muted,
    Good,
    Bad,
}

/// Text shown after the end of a source line: a computed value or a
/// requirement verdict.
pub struct Annotation {
    /// Shown on the line holding this offset.
    pub offset: usize,
    pub text: String,
    pub tone: Tone,
}

pub struct Document {
    pub path: PathBuf,
    /// The text as last loaded or saved (`\n` line breaks).
    pub source: String,
    /// The file used `\r\n` line breaks; saving keeps them.
    crlf: bool,
    /// In source order (owners before their members).
    pub elements: Vec<Element>,
    pub diagnostics: Vec<Diagnostic>,
    /// Computed values and requirement verdicts, in source order.
    pub annotations: Vec<Annotation>,
    /// The containment tree in source order, one row per element
    /// (`tree[i]` describes `elements[i]`).
    pub tree: Vec<TreeRow>,
    /// Not saved anywhere yet (File > New, File > Close): saving asks for a path.
    pub untitled: bool,
    /// Error reading the file, if any.
    pub load_error: Option<String>,
    pub diagram: Diagram,
    /// Modification time when loaded or saved, to notice edits by other programs.
    pub modified: Option<SystemTime>,
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Document {
    pub fn load(path: PathBuf) -> Self {
        let modified = modified(&path);
        let mut doc = Document {
            path,
            source: String::new(),
            crlf: false,
            elements: Vec::new(),
            diagnostics: Vec::new(),
            annotations: Vec::new(),
            tree: Vec::new(),
            untitled: false,
            load_error: None,
            diagram: Diagram::default(),
            modified,
        };
        match std::fs::read_to_string(&doc.path) {
            Ok(src) => {
                doc.crlf = src.contains("\r\n");
                let src = src.replace("\r\n", "\n");
                doc.set_source(&src);
                doc.source = src;
            }
            Err(e) => doc.load_error = Some(format!("cannot read {}: {e}", doc.path.display())),
        }
        doc
    }

    /// Reparses `src` (the edited text); the file is not touched.
    pub fn set_source(&mut self, src: &str) {
        let result = sysml::parse(src);
        self.elements.clear();
        self.tree.clear();
        let mut path = Vec::new();
        collect(&result.model.members, &mut path, "", &mut self.elements, &mut self.tree);
        let report = sysml::eval::evaluate(&result.model, &Index::build(&result.model));
        let lines = LineIndex::new(src);
        let mut diagnostics: Vec<&sysml::Diagnostic> = result.diagnostics.iter().chain(&report.diagnostics).collect();
        diagnostics.sort_by_key(|d| d.span.start);
        self.diagnostics = diagnostics
            .into_iter()
            .map(|d| {
                let (line, col) = lines.line_col(d.span.start);
                Diagnostic { span: d.span, text: format!("{line}:{col}: {}", d.message) }
            })
            .collect();
        self.annotations = annotations(&report);
        self.diagram = Diagram::build(&result.model);
        self.diagram.apply_report(&report);
        if !self.untitled {
            self.diagram.load_positions(&Diagram::layout_path(&self.path));
        }
    }

    /// Writes `text` to the file, with the file's original line breaks.
    pub fn save(&mut self, text: &str) -> std::io::Result<()> {
        let out = if self.crlf { text.replace('\n', "\r\n") } else { text.to_owned() };
        std::fs::write(&self.path, out)?;
        self.source = text.to_owned();
        self.modified = modified(&self.path);
        self.load_error = None;
        Ok(())
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Has the file changed on disk since it was loaded or saved?
    pub fn changed_on_disk(&self) -> bool {
        !self.untitled && modified(&self.path) != self.modified
    }

    /// An empty model that is not saved anywhere yet.
    pub fn untitled() -> Self {
        let mut doc = Document::load(PathBuf::new());
        doc.path = PathBuf::from("Untitled.sysml");
        doc.untitled = true;
        doc.load_error = None;
        doc.modified = None;
        doc
    }

    /// Saves to a new path; later saves and the layout file use it.
    pub fn save_as(&mut self, path: PathBuf, text: &str) -> std::io::Result<()> {
        let old = std::mem::replace(&mut self.path, path);
        match self.save(text) {
            Ok(()) => {
                self.untitled = false;
                Ok(())
            }
            Err(e) => {
                self.path = old;
                Err(e)
            }
        }
    }

    /// Index of the innermost element (named or not) written around `offset`.
    pub fn row_at(&self, offset: usize) -> Option<usize> {
        self.elements.iter().rposition(|e| e.span.start <= offset && offset <= e.span.end)
    }

    /// Source offset where `qualified_name` is declared.
    pub fn offset_of(&self, qualified_name: &str) -> Option<usize> {
        self.elements.iter().find(|e| e.qualified_name.as_deref() == Some(qualified_name)).map(|e| e.span.start)
    }

    /// The innermost named element written around `offset`.
    pub fn element_at(&self, offset: usize) -> Option<&str> {
        // Owners come before their members, so the last match is the innermost.
        self.elements
            .iter()
            .filter(|e| e.span.start <= offset && offset <= e.span.end)
            .filter_map(|e| e.qualified_name.as_deref())
            .next_back()
    }
}

/// Flattens members, tracking qualified names like [`sysml::Index`] does, and
/// builds the matching tree rows.
fn collect<'a>(members: &'a [Member], path: &mut Vec<&'a str>, parent_key: &str, out: &mut Vec<Element>, tree: &mut Vec<TreeRow>) {
    for (i, m) in members.iter().enumerate() {
        let name = m.kind.effective_name();
        let qualified_name = name.filter(|_| path.len() == named_depth(path)).map(|n| {
            let mut q = path.join("::");
            if !q.is_empty() {
                q += "::";
            }
            q + n
        });
        let key = match name {
            Some(n) => format!("{parent_key}/{n}"),
            None => format!("{parent_key}/#{i}"),
        };
        let (keyword, label) = split_summary(m);
        let body = m.kind.body();
        tree.push(TreeRow {
            depth: path.len(),
            key: key.clone(),
            label,
            keyword,
            category: m.kind.category(),
            is_definition: matches!(m.kind, sysml::MemberKind::Definition(_)),
            has_children: !body.is_empty(),
        });
        out.push(Element { span: m.span, qualified_name });
        // Members of anonymous elements are not addressable (the empty segment marks that).
        path.push(name.unwrap_or(""));
        collect(body, path, &key, out, tree);
        path.pop();
    }
}

/// Values next to the expressions that compute them, verdicts next to `satisfy`.
fn annotations(report: &sysml::eval::Report) -> Vec<Annotation> {
    let notes = report.notes.iter().map(|n| {
        let (text, tone) = match n.status {
            Status::Ok => (format!("= {}", n.text), Tone::Value),
            Status::Unknown => (format!("= {}", n.text), Tone::Muted),
            Status::Error => (format!("! {}", n.text), Tone::Bad),
        };
        Annotation { offset: n.span.end, text, tone }
    });
    let checks = report.checks.iter().map(|c| {
        let (word, tone) = match c.verdict {
            Verdict::Satisfied => ("satisfied", Tone::Good),
            Verdict::Violated => ("VIOLATED", Tone::Bad),
            Verdict::Unknown => ("undecided", Tone::Muted),
            Verdict::Error => ("cannot check", Tone::Bad),
        };
        Annotation { offset: c.span.end, text: format!("{word}: {}", c.detail), tone }
    });
    let mut out: Vec<Annotation> = notes.chain(checks).collect();
    out.sort_by_key(|a| a.offset);
    out
}

/// `part engine : Engine [1]` → (`part`, `engine : Engine [1]`).
fn split_summary(m: &Member) -> (String, String) {
    let summary = m.kind.summary();
    let summary = summary.lines().next().unwrap_or("").trim().to_owned();
    let name = m.kind.name().map(|n| n.to_string());
    if let Some(at) = name.as_deref().and_then(|n| summary.find(n)).filter(|&at| at > 0) {
        return (summary[..at].trim().to_owned(), summary[at..].to_owned());
    }
    // Unnamed: the leading word is the keyword, the rest describes it.
    match summary.split_once([' ', ':']) {
        Some((k, rest)) if !rest.trim().is_empty() => (k.to_owned(), rest.trim_start_matches([':', ' ']).to_owned()),
        _ => (summary.clone(), String::new()),
    }
}

/// One element in the model tree.
pub struct TreeRow {
    /// Nesting depth (0 for top-level members).
    pub depth: usize,
    /// Stable identity across reparses: owner names joined by `/`, with
    /// `#index` for anonymous elements.
    pub key: String,
    /// Name and type, e.g. `engine : Engine [1]`.
    pub label: String,
    /// Kind keyword, e.g. `part def`.
    pub keyword: String,
    pub category: Category,
    pub is_definition: bool,
    pub has_children: bool,
}

/// Number of named segments in `path` (all of them, unless an anonymous element is on it).
fn named_depth(path: &[&str]) -> usize {
    path.iter().position(|s| s.is_empty()).unwrap_or(path.len())
}
