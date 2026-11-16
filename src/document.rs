//! A SysML file: its source text, element positions, diagnostics and the
//! diagram graph. The code pane edits the text and hands it back for reparsing.

use crate::diagram::Diagram;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use sysml::eval::{Status, Verdict};
use crate::allocations::Allocations;
use crate::checks::Finding;
use crate::requirements::Requirements;
use sysml::{Category, EntryId, Index, LineIndex, Member, MemberKind, Model, Severity, Span};

/// Project files are looked for this many folders below the model's folder.
const PROJECT_DEPTH: usize = 3;
const MAX_PROJECT_FILES: usize = 200;
/// Folders never searched for project files.
const SKIPPED_DIRS: [&str; 4] = ["target", "node_modules", "dist", "build"];

/// Another `.sysml` file of the project: read only, its names resolve in
/// the edited file and its requirements show in the requirement views.
pub struct ProjectFile {
    pub path: PathBuf,
    pub model: Model,
    pub modified: Option<SystemTime>,
    /// The file has syntax errors (what could be read is still used).
    pub has_errors: bool,
}

impl ProjectFile {
    fn load(path: PathBuf) -> Option<Self> {
        let src = std::fs::read_to_string(&path).ok()?;
        let result = sysml::parse(&src.replace("\r\n", "\n"));
        Some(ProjectFile { modified: modified(&path), has_errors: result.has_errors(), model: result.model, path })
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// The `.sysml` files in `model`'s folder and the folders below it, except `model`.
pub fn project_paths(model: &Path) -> Vec<PathBuf> {
    let dir = match model.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let me = std::fs::canonicalize(model).ok();
    let mut out = Vec::new();
    let mut stack = vec![(dir, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if depth < PROJECT_DEPTH && !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if path.extension().is_some_and(|x| x.eq_ignore_ascii_case("sysml"))
                && std::fs::canonicalize(&path).ok() != me
            {
                out.push(path);
                if out.len() >= MAX_PROJECT_FILES {
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

/// One namespace over the edited model, the other project files and the standard library.
fn index_over<'a>(model: &'a Model, project: &'a [ProjectFile]) -> Index<'a> {
    let mut models: Vec<&Model> = vec![model];
    models.extend(project.iter().map(|f| &f.model));
    models.extend(sysml::library::files().iter().map(|f| &f.model));
    Index::build_many(&models)
}

/// Type names (`: T`, `:> T` on definitions) of `model` that resolve nowhere.
fn unresolved_types(model: &Model, index: &Index) -> Vec<sysml::Diagnostic> {
    let mut out = Vec::new();
    let mut stack: Vec<(&[Member], Option<EntryId>)> = vec![(&model.members, None)];
    while let Some((members, scope)) = stack.pop() {
        for m in members {
            let names: Vec<&sysml::QualifiedName> = match &m.kind {
                MemberKind::Definition(d) => d.specializes.iter().collect(),
                MemberKind::Usage(u) => u.typed_by.iter().map(|t| &t.name).collect(),
                _ => Vec::new(),
            };
            for n in names {
                if index.resolve(scope, n).is_none() {
                    out.push(sysml::Diagnostic {
                        severity: Severity::Warning,
                        message: format!("`{n}` is not found in this file, the other files of its folder or the standard library"),
                        span: n.span,
                    });
                }
            }
            stack.push((m.kind.body(), index.id_of(m).or(scope)));
        }
    }
    out
}

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
    /// The model last parsed, for views built on demand.
    pub model: Model,
    /// Incremented by every reparse, so views know when to rebuild.
    pub generation: u64,
    pub requirements: Requirements,
    pub allocations: Allocations,
    /// Model checks over the whole project, warnings first.
    pub checks: Vec<Finding>,
    /// The other `.sysml` files of the folder (and below), in path order;
    /// entry source `i + 1` of [`Self::index`] is `project[i]`.
    pub project: Vec<ProjectFile>,
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
            model: Model::default(),
            generation: 0,
            requirements: Requirements::default(),
            allocations: Allocations::default(),
            checks: Vec::new(),
            project: Vec::new(),
            modified,
        };
        if !doc.path.as_os_str().is_empty() {
            doc.project = project_paths(&doc.path).into_iter().filter_map(ProjectFile::load).collect();
        }
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
        let index = index_over(&result.model, &self.project);
        let report = sysml::eval::evaluate_source(&result.model, &index, 0);
        let unresolved = unresolved_types(&result.model, &index);
        let lines = LineIndex::new(src);
        let mut diagnostics: Vec<&sysml::Diagnostic> = result.diagnostics.iter().chain(&report.diagnostics).chain(&unresolved).collect();
        diagnostics.sort_by_key(|d| d.span.start);
        self.diagnostics = diagnostics
            .into_iter()
            .map(|d| {
                let (line, col) = lines.line_col(d.span.start);
                Diagnostic { span: d.span, text: format!("{line}:{col}: {}", d.message) }
            })
            .collect();
        self.annotations = annotations(&report);
        let names: Vec<String> = self.project.iter().map(ProjectFile::file_name).collect();
        let file_of = |source: usize| source.checked_sub(1).and_then(|i| names.get(i).cloned());
        self.diagram = Diagram::build_in(&index, &result.model, &file_of);
        self.diagram.apply_report(&report);
        if !self.untitled {
            self.diagram.load_positions(&Diagram::layout_path(&self.path));
        }
        // Requirements of the whole project, checked where their `satisfy` is written.
        let mut checks = report.checks.clone();
        let mut models: Vec<&Model> = vec![&result.model];
        for (i, f) in self.project.iter().enumerate() {
            checks.extend(sysml::eval::evaluate_source(&f.model, &index, i + 1).checks);
            models.push(&f.model);
        }
        let project_len = self.project.len();
        let mut requirements = Requirements::build(&index, &models, &checks, |id| {
            let s = index.entry(id).source;
            (s <= project_len).then_some(s)
        });
        for r in &mut requirements.rows {
            r.file = file_of(r.source);
        }
        let source_of = |id: EntryId| {
            let s = index.entry(id).source;
            (s <= project_len).then_some(s)
        };
        let mut allocations = Allocations::build(&index, &models, source_of);
        for a in allocations.rows.iter_mut().chain(allocations.columns.iter_mut()) {
            a.file = file_of(a.source);
        }
        let mut checks = crate::checks::run(&crate::checks::Input { index: &index, models: &models, requirements: &requirements, allocations: &allocations, source_of: &source_of });
        for c in &mut checks {
            c.file = file_of(c.source);
        }
        drop(index);
        self.requirements = requirements;
        self.allocations = allocations;
        self.checks = checks;
        self.model = result.model;
        self.generation += 1;
    }

    /// Name lookup over the model last parsed, the other project files and
    /// the standard library ([`Entry::source`](sysml::Entry): 0 is this file,
    /// `i + 1` is `project[i]`, the rest the library).
    pub fn index(&self) -> Index<'_> {
        index_over(&self.model, &self.project)
    }

    /// Whether entry `id` of [`Self::index`] is written in this file.
    pub fn is_local(&self, index: &Index, id: EntryId) -> bool {
        index.entry(id).source == 0
    }

    /// The file of an entry source of [`Self::index`], if it is a project file.
    pub fn source_path(&self, source: usize) -> Option<&Path> {
        match source {
            0 => Some(&self.path),
            s => self.project.get(s - 1).map(|f| f.path.as_path()),
        }
    }

    /// Picks up project files that were added, removed or changed on disk.
    /// True if anything changed (the edited text should then be reparsed).
    pub fn refresh_project(&mut self) -> bool {
        if self.untitled || self.path.as_os_str().is_empty() {
            return false;
        }
        let paths = project_paths(&self.path);
        let same_list = paths.len() == self.project.len() && paths.iter().zip(&self.project).all(|(p, f)| *p == f.path);
        if same_list && self.project.iter().all(|f| modified(&f.path) == f.modified) {
            return false;
        }
        let mut old: Vec<ProjectFile> = std::mem::take(&mut self.project);
        self.project = paths
            .into_iter()
            .filter_map(|p| match old.iter().position(|f| f.path == p && modified(&p) == f.modified) {
                Some(i) => Some(old.swap_remove(i)),
                None => ProjectFile::load(p),
            })
            .collect();
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ibd::Ibd;

    /// A folder with `files`, removed when dropped.
    struct TempProject(PathBuf);

    impl TempProject {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("eagleview-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for (path, text) in files {
                let p = dir.join(path);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
            TempProject(dir)
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const PARTS: &str = "package Parts {
        port def FuelPort { in item fuel; }
        part def Engine { port intake : FuelPort; attribute mass = 120 [kg]; }
        requirement def <'R9'> Light { doc /* Light enough. */ subject c; }
    }";
    const CAR: &str = "package Car {
        private import Parts::*;
        private import ISQ::*;
        part def Car { part engine : Engine; port tank : ~FuelPort; connect tank to engine.intake; attribute top : SpeedValue; }
        part car : Car;
        requirement light : Light;
        satisfy light by car;
        part def Broken { attribute x : NoSuchType; }
    }";

    #[test]
    fn project_files_resolve_and_trace_across_files() {
        let p = TempProject::new("project", &[("car.sysml", CAR), ("lib/parts.sysml", PARTS), ("target/ignored.sysml", "package X {")]);
        let doc = Document::load(p.0.join("car.sysml"));
        assert_eq!(doc.project.len(), 1, "target/ is skipped");
        assert_eq!(doc.project[0].file_name(), "parts.sysml");
        // Only the deliberately unknown type is reported; Engine, FuelPort and SpeedValue resolve.
        let warnings: Vec<&str> = doc.diagnostics.iter().map(|d| d.text.as_str()).collect();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("`NoSuchType` is not found"), "{warnings:?}");
        // Engine shows as a read-only node from the other file.
        let engine = doc.diagram.node_named("Parts::Engine").expect("external node");
        assert_eq!(doc.diagram.nodes[engine].external.as_deref(), Some("parts.sysml"));
        assert!(doc.diagram.nodes.iter().filter(|n| n.external.is_some()).all(|n| n.qualified_name.starts_with("Parts::")));
        // Requirements of both files, the definition's text across files.
        let light = doc.requirements.rows.iter().find(|r| r.name == "light").unwrap();
        assert_eq!(light.text, "Light enough.");
        assert_eq!(light.satisfied_by, ["car"]);
        let def = doc.requirements.rows.iter().find(|r| r.name == "Light").unwrap();
        assert_eq!((def.id.as_str(), def.file.as_deref()), ("R9", Some("parts.sysml")));
        // The interconnection view looks into the other file's definition.
        let index = doc.index();
        let ibd = Ibd::build(&index, index.get("Car::Car").unwrap());
        assert!(ibd.parts.iter().any(|p| p.title == "engine : Engine"));
        assert_eq!(ibd.links.len(), 1, "tank to engine.intake");
        assert!(crate::ibd::candidates(&index, |id| doc.is_local(&index, id)).iter().all(|q| q.starts_with("Car::")));
    }

    #[test]
    fn project_changes_are_picked_up() {
        let p = TempProject::new("refresh", &[("a.sysml", "package A { part def X; }")]);
        let mut doc = Document::load(p.0.join("a.sysml"));
        assert!(!doc.refresh_project());
        std::fs::write(p.0.join("b.sysml"), "package B { part def Y; }").unwrap();
        assert!(doc.refresh_project());
        assert_eq!(doc.project.len(), 1);
        assert!(doc.index().get("B::Y").is_some());
    }
}
