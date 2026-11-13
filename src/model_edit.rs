//! Model changes made from the diagram, expressed as text edits on the SysML
//! source. Every operation parses the current text, finds the elements by
//! qualified name and returns [`Edit`]s that the code editor applies as one
//! undo step, so the code stays the single source of truth.

use crate::editor::Edit;
use crate::source;
use sysml::lexer::{self, TokenKind};
use sysml::{DefKind, EntryId, Index, Member, MemberKind, Name, QualifiedName, Span, UsageKind};

/// Something the palette can create.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElementKind {
    Package,
    Def(DefKind),
    Usage(UsageKind),
}

impl ElementKind {
    /// Source keyword: `part def`, `part`, `package`.
    pub fn keyword(self) -> String {
        match self {
            ElementKind::Package => "package".into(),
            ElementKind::Def(k) => format!("{} def", k.keyword()),
            ElementKind::Usage(k) => k.keyword().into(),
        }
    }

    /// Menu label: `Part Definition`, `Part`.
    pub fn label(self) -> String {
        match self {
            ElementKind::Def(k) => format!("{} Definition", title_case(k.keyword())),
            _ => title_case(&self.keyword()),
        }
    }

    /// Base of generated names: `Part` for definitions, `part` for usages.
    fn base_name(self) -> String {
        let camel: String = self.keyword().replace(" def", "").split_whitespace().map(title_case).collect();
        match self {
            ElementKind::Usage(_) => lower_first(&camel),
            _ => camel,
        }
    }
}

/// A relationship drawn by dragging from one node to another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Relation {
    /// `def A :> B`
    Specialize,
    /// `usage a :> b`
    Subset,
    /// `usage a : B`
    Type,
    /// `def A { part b : B; }` (composition)
    Own(UsageKind),
    /// `def A { ref part b : B; }`
    Reference(UsageKind),
    /// `satisfy B by A;`
    Satisfy,
    /// `dependency from A to B;`
    Dependency,
    /// `connect A to B;`
    Connect,
    /// `flow from A to B;`
    Flow,
    /// `allocate A to B;`
    Allocate,
}

impl Relation {
    pub fn label(self) -> String {
        match self {
            Relation::Specialize => "Specialization  ( :> )".into(),
            Relation::Subset => "Subsetting  ( :> )".into(),
            Relation::Type => "Defined By  ( : )".into(),
            Relation::Own(k) => format!("Owned {}  (composition)", k.keyword()),
            Relation::Reference(k) => format!("Referenced {}  ( ref {} )", k.keyword(), k.keyword()),
            Relation::Satisfy => "Satisfy  ( satisfy … by … )".into(),
            Relation::Dependency => "Dependency".into(),
            Relation::Connect => "Connection  ( connect … to … )".into(),
            Relation::Flow => "Flow  ( flow from … to … )".into(),
            Relation::Allocate => "Allocation  ( allocate … to … )".into(),
        }
    }
}

/// What a node or feature is, for offering the relationships that make sense.
#[derive(Clone, Copy, Debug)]
pub struct ElementInfo {
    pub is_def: bool,
    pub kind: UsageKind,
}

/// Relationships the language allows from `a` to `b` (both diagram nodes).
pub fn relations_between(a: ElementInfo, b: ElementInfo) -> Vec<Relation> {
    let req = |i: ElementInfo| matches!(i.kind, UsageKind::Requirement | UsageKind::Concern);
    let mut out = Vec::new();
    match (a.is_def, b.is_def) {
        (true, true) => {
            out.push(Relation::Specialize);
            out.push(Relation::Own(b.kind));
            out.push(Relation::Reference(b.kind));
        }
        (false, true) => {
            out.push(Relation::Type);
            out.push(Relation::Own(b.kind));
        }
        (false, false) => {
            out.push(Relation::Connect);
            out.push(Relation::Flow);
            out.push(Relation::Subset);
        }
        (true, false) => {}
    }
    if req(b) {
        out.push(Relation::Satisfy);
    }
    out.push(Relation::Allocate);
    out.push(Relation::Dependency);
    // Attributes are values: they are not connected or allocated.
    if matches!(a.kind, UsageKind::Attribute | UsageKind::Enum) || matches!(b.kind, UsageKind::Attribute | UsageKind::Enum) {
        out.retain(|r| !matches!(r, Relation::Connect | Relation::Flow | Relation::Allocate));
    }
    out.dedup();
    out
}

/// Usage kinds offered by "Add Feature", with the definition kind of their types.
pub const FEATURE_KINDS: [UsageKind; 10] = [
    UsageKind::Attribute,
    UsageKind::Part,
    UsageKind::Port,
    UsageKind::Item,
    UsageKind::Action,
    UsageKind::State,
    UsageKind::Requirement,
    UsageKind::Constraint,
    UsageKind::Occurrence,
    UsageKind::Calc,
];

/// Value types of the standard library, offered for attributes.
pub const SCALAR_TYPES: [&str; 6] = ["Real", "Integer", "Natural", "Boolean", "String", "Complex"];

// ---- lookups -------------------------------------------------------------------

fn find(index: &Index, qualified_name: &str) -> Option<EntryId> {
    index.get(qualified_name)
}

fn qn(text: &str) -> QualifiedName {
    let segments = text.split("::").map(|s| Name { text: s.to_owned(), span: Span::default() }).collect();
    QualifiedName { segments, span: Span::default() }
}

/// The shortest name for `target` that resolves to it from `scope`.
fn name_from(index: &Index, scope: Option<EntryId>, target: EntryId) -> String {
    let full = &index.entry(target).qualified_name;
    let parts: Vec<&str> = full.split("::").collect();
    for k in 1..=parts.len() {
        let candidate = parts[parts.len() - k..].join("::");
        if index.resolve(scope, &qn(&candidate)) == Some(target) {
            return spell(&candidate);
        }
    }
    spell(full)
}

/// How to write a type that is not in the model (a library type such as
/// `ScalarValues::Real`): its short name when the file imports its package.
fn library_name(model: &sysml::Model, qualified: &str) -> String {
    let Some((package, last)) = qualified.rsplit_once("::") else { return spell(qualified) };
    let mut imported = false;
    model.walk(&mut |m, _| {
        if let MemberKind::Import(i) = &m.kind {
            imported |= i.is_namespace && i.target.to_string() == package;
        }
    });
    if imported { spell(last) } else { spell(qualified) }
}

/// A qualified name as source text (each segment quoted if needed).
fn spell(qualified: &str) -> String {
    qualified.split("::").map(source::spell_name).collect::<Vec<_>>().join("::")
}

/// The nearest package containing (or being) `id`, if any.
fn package_of(index: &Index, mut id: Option<EntryId>) -> Option<EntryId> {
    while let Some(i) = id {
        if matches!(index.entry(i).member.kind, MemberKind::Package(_)) {
            return Some(i);
        }
        id = index.entry(i).parent;
    }
    None
}

/// Where new top-level elements go: the package around `near` (a qualified
/// name), else the first package of the file; `None` means the file itself.
pub fn default_container(src: &str, near: Option<&str>) -> Option<String> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let from_near = near.and_then(|n| find(&index, n)).and_then(|id| package_of(&index, Some(id)));
    let first = || index.roots().iter().copied().find(|&r| matches!(index.entry(r).member.kind, MemberKind::Package(_)));
    from_near.or_else(first).map(|id| index.entry(id).qualified_name.clone())
}

/// `base`, or `base2`, `base3`, ... — the first that does not occur in `src`.
pub fn unique_name(src: &str, base: &str) -> String {
    let taken = |n: &str| !source::occurrences(src, n).is_empty();
    if !taken(base) {
        return base.to_owned();
    }
    (2..).map(|i| format!("{base}{i}")).find(|n| !taken(n)).unwrap()
}

/// Definitions usable as types of a `kind` usage: `(qualified name, shown name)`.
pub fn types_for(src: &str, kind: Option<UsageKind>) -> Vec<(String, String)> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let mut out: Vec<(String, String)> = index
        .entries()
        .iter()
        .filter_map(|e| match &e.member.kind {
            MemberKind::Definition(d) => {
                let fits = match kind {
                    None => true,
                    Some(UsageKind::Attribute) => matches!(d.kind, DefKind::Attribute | DefKind::Enum),
                    Some(k) => d.kind.usage_kind() == k,
                };
                fits.then(|| (e.qualified_name.clone(), e.name.to_string()))
            }
            _ => None,
        })
        .collect();
    if matches!(kind, Some(UsageKind::Attribute)) {
        out.extend(SCALAR_TYPES.iter().map(|t| (format!("ScalarValues::{t}"), t.to_string())));
    }
    out
}

/// Whether `qualified_name` is a definition, and its usage kind.
pub fn info_of(src: &str, qualified_name: &str) -> Option<ElementInfo> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    match &index.entry(find(&index, qualified_name)?).member.kind {
        MemberKind::Definition(d) => Some(ElementInfo { is_def: true, kind: d.kind.usage_kind() }),
        MemberKind::Usage(u) => Some(ElementInfo { is_def: false, kind: u.kind }),
        _ => None,
    }
}

/// The palette: groups of element kinds the diagram can create.
pub fn palette() -> [(&'static str, Vec<ElementKind>); 3] {
    use ElementKind::{Def, Usage};
    [
        (
            "Structure",
            vec![
                Def(DefKind::Part),
                Usage(UsageKind::Part),
                Def(DefKind::Item),
                Usage(UsageKind::Item),
                Def(DefKind::Port),
                Usage(UsageKind::Port),
                Def(DefKind::Attribute),
                Def(DefKind::Enum),
                Def(DefKind::Interface),
                Def(DefKind::Connection),
                Def(DefKind::Allocation),
                Def(DefKind::Occurrence),
                Usage(UsageKind::Occurrence),
            ],
        ),
        (
            "Behavior",
            vec![
                Def(DefKind::Action),
                Usage(UsageKind::Action),
                Def(DefKind::State),
                Usage(UsageKind::State),
                Def(DefKind::Calc),
                Usage(UsageKind::Calc),
                Def(DefKind::UseCase),
                Usage(UsageKind::UseCase),
                Def(DefKind::AnalysisCase),
                Def(DefKind::VerificationCase),
            ],
        ),
        (
            "Requirements",
            vec![
                Def(DefKind::Requirement),
                Usage(UsageKind::Requirement),
                Def(DefKind::Constraint),
                Usage(UsageKind::Constraint),
                Def(DefKind::Concern),
                Def(DefKind::Viewpoint),
                Def(DefKind::View),
                Def(DefKind::Metadata),
            ],
        ),
    ]
}

// ---- text helpers --------------------------------------------------------------

fn line_start(src: &str, at: usize) -> usize {
    src[..at].rfind('\n').map_or(0, |i| i + 1)
}

fn indent_at(src: &str, at: usize) -> String {
    let ls = line_start(src, at);
    src[ls..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

/// An edit that adds the member text `text` at the end of `owner`'s body,
/// turning a `;` declaration into one with a body if needed.
fn insert_into(src: &str, owner: Option<&Member>, text: &str) -> Edit {
    let Some(owner) = owner else {
        let sep = if src.is_empty() || src.ends_with("\n\n") {
            ""
        } else if src.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        return Edit { start: src.len(), end: src.len(), text: format!("{sep}{text}\n") };
    };
    let end = owner.span.end.min(src.len());
    let base = indent_at(src, owner.span.start);
    let inner = format!("{base}    ");
    let head = src[..end].trim_end();
    if head.ends_with('}') {
        let close = head.len() - 1;
        let ls = line_start(src, close);
        if src[ls..close].trim().is_empty() {
            return Edit { start: ls, end: ls, text: format!("{inner}{text}\n") };
        }
        let before = src[..close].trim_end_matches([' ', '\t']).len();
        return Edit { start: before, end: close, text: format!("\n{inner}{text}\n{base}") };
    }
    if head.ends_with(';') {
        let semi = head.len() - 1;
        return Edit { start: semi, end: semi + 1, text: format!(" {{\n{inner}{text}\n{base}}}") };
    }
    // Unusual: no body delimiter. Put the member after the declaration.
    Edit { start: end, end, text: format!("\n{base}{text}") }
}

/// Removes `span`, together with its line(s) when nothing else is on them.
pub fn delete(src: &str, span: Span) -> Edit {
    let (mut start, mut end) = (span.start.min(src.len()), span.end.min(src.len()));
    let ls = line_start(src, start);
    let le = src[end..].find('\n').map_or(src.len(), |i| end + i);
    if src[ls..start].trim().is_empty() && src[end..le].trim().is_empty() {
        start = ls;
        end = (le + 1).min(src.len());
    }
    Edit { start, end, text: String::new() }
}

/// Renames every occurrence of `old` (like F2 in the editor).
pub fn rename(src: &str, old: &str, new: &str) -> Vec<Edit> {
    let spelled = source::spell_name(new.trim());
    if new.trim().is_empty() || new.trim() == old {
        return Vec::new();
    }
    source::occurrences(src, old).into_iter().map(|(start, end)| Edit { start, end, text: spelled.clone() }).collect()
}

/// The declaration part of a member: from its start to its body or `;`.
pub fn header_range(src: &str, span: Span) -> (usize, usize) {
    let mut diags = Vec::new();
    let tokens = lexer::lex(&src[..span.end.min(src.len())], &mut diags);
    let mut depth = 0i32;
    for t in tokens.iter().filter(|t| t.span.start >= span.start) {
        match t.kind {
            TokenKind::Symbol("(" | "[") => depth += 1,
            TokenKind::Symbol(")" | "]") => depth -= 1,
            TokenKind::Symbol("{" | ";") if depth <= 0 => return (span.start, src[..t.span.start].trim_end().len()),
            _ => {}
        }
    }
    (span.start, span.end)
}

// ---- operations ----------------------------------------------------------------

/// A model change: the edits and the qualified name of the element it created.
pub struct Change {
    pub edits: Vec<Edit>,
    pub created: Option<String>,
}

fn qualified(container: Option<&str>, name: &str) -> String {
    match container {
        Some(c) => format!("{c}::{name}"),
        None => name.to_owned(),
    }
}

/// Adds a new `kind` element named after its kind to `container` (a package or
/// definition, by qualified name; `None` for the file itself).
pub fn create(src: &str, container: Option<&str>, kind: ElementKind, ty: Option<&str>) -> Option<Change> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let owner_id = match container {
        Some(c) => Some(find(&index, c)?),
        None => None,
    };
    let name = unique_name(src, &format!("{}1", kind.base_name()));
    let typing = ty
        .map(|t| find(&index, t).map(|id| name_from(&index, owner_id, id)).unwrap_or_else(|| library_name(&model, t)))
        .map(|t| format!(" : {t}"))
        .unwrap_or_default();
    let text = match kind {
        ElementKind::Package => format!("package {name} {{\n}}"),
        _ => format!("{} {name}{typing};", kind.keyword()),
    };
    let owner = owner_id.map(|id| index.entry(id).member);
    let edit = insert_into(src, owner, &reindent(&text, owner, src));
    Some(Change { edits: vec![edit], created: Some(qualified(container, &name)) })
}

/// Multi-line member text indented for its owner's body (the first line is
/// placed by [`insert_into`]).
fn reindent(text: &str, owner: Option<&Member>, src: &str) -> String {
    let inner = owner.map(|o| format!("{}    ", indent_at(src, o.span.start))).unwrap_or_default();
    text.lines().enumerate().map(|(i, l)| if i == 0 { l.to_owned() } else { format!("{inner}{l}") }).collect::<Vec<_>>().join("\n")
}

/// Adds a feature `[ref] kind name [: Type];` to `owner`.
pub fn add_feature(src: &str, owner: &str, kind: UsageKind, is_ref: bool, name: Option<&str>, ty: Option<&str>) -> Option<Change> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let owner_id = find(&index, owner)?;
    let base = match (name, ty) {
        (Some(n), _) => n.to_owned(),
        (None, Some(t)) => lower_first(t.rsplit("::").next().unwrap_or(t)),
        (None, None) => format!("{}1", ElementKind::Usage(kind).base_name()),
    };
    let name = unique_name(src, &base);
    let typing = ty
        .map(|t| find(&index, t).map(|id| name_from(&index, Some(owner_id), id)).unwrap_or_else(|| library_name(&model, t)))
        .map(|t| format!(" : {t}"))
        .unwrap_or_default();
    let prefix = if is_ref { "ref " } else { "" };
    let text = format!("{prefix}{} {}{typing};", kind.keyword(), source::spell_name(&name));
    let edit = insert_into(src, Some(index.entry(owner_id).member), &text);
    Some(Change { edits: vec![edit], created: Some(format!("{owner}::{name}")) })
}

/// Types the usage `usage` by `ty` (replacing its first type, if any).
pub fn set_type(src: &str, usage: &str, ty: &str) -> Option<Change> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let id = find(&index, usage)?;
    let entry = index.entry(id);
    let MemberKind::Usage(u) = &entry.member.kind else { return None };
    let ty_name = find(&index, ty).map(|t| name_from(&index, entry.parent, t)).unwrap_or_else(|| library_name(&model, ty));
    let edit = match u.typed_by.first() {
        Some(t) => Edit { start: t.name.span.start, end: t.name.span.end, text: ty_name },
        None => {
            let at = u.id.name.as_ref().or(u.id.short_name.as_ref())?.span.end;
            Edit { start: at, end: at, text: format!(" : {ty_name}") }
        }
    };
    Some(Change { edits: vec![edit], created: None })
}

/// Adds `base` to a definition's `:>` list, or to a usage's subsetted features.
pub fn specialize(src: &str, element: &str, base: &str) -> Option<Change> {
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let id = find(&index, element)?;
    let entry = index.entry(id);
    let base_name = find(&index, base).map(|b| name_from(&index, entry.parent, b)).unwrap_or_else(|| spell(base));
    let (last, name_end) = match &entry.member.kind {
        MemberKind::Definition(d) => (d.specializes.last().map(|q| q.span.end), d.id.display_name().map(|n| n.span.end)),
        MemberKind::Usage(u) => {
            let after = [u.typed_by.last().map(|t| t.span.end), u.multiplicity.as_ref().map(|m| m.span.end)]
                .into_iter()
                .flatten()
                .max();
            (u.subsets.last().map(|r| r.span.end), after.or(u.id.display_name().map(|n| n.span.end)))
        }
        _ => return None,
    };
    let edit = match last {
        Some(at) => Edit { start: at, end: at, text: format!(", {base_name}") },
        None => {
            let at = name_end?;
            Edit { start: at, end: at, text: format!(" :> {base_name}") }
        }
    };
    Some(Change { edits: vec![edit], created: None })
}

/// Draws relationship `rel` from `from` to `to` (qualified names).
pub fn relate(src: &str, rel: Relation, from: &str, to: &str) -> Option<Change> {
    match rel {
        Relation::Specialize | Relation::Subset => return specialize(src, from, to),
        Relation::Type => return set_type(src, from, to),
        Relation::Own(k) => return add_feature(src, from, k, false, None, Some(to)),
        Relation::Reference(k) => return add_feature(src, from, k, true, None, Some(to)),
        _ => {}
    }
    let model = sysml::parse(src).model;
    let index = Index::build(&model);
    let (a, b) = (find(&index, from)?, find(&index, to)?);
    // The statement goes into the package around the source element.
    let scope = package_of(&index, index.entry(a).parent);
    let (na, nb) = (name_from(&index, scope, a), name_from(&index, scope, b));
    let text = match rel {
        Relation::Satisfy => format!("satisfy {nb} by {na};"),
        Relation::Dependency => format!("dependency from {na} to {nb};"),
        Relation::Connect => format!("connect {na} to {nb};"),
        Relation::Flow => format!("flow from {na} to {nb};"),
        Relation::Allocate => format!("allocate {na} to {nb};"),
        _ => unreachable!(),
    };
    let edit = insert_into(src, scope.map(|s| index.entry(s).member), &text);
    Some(Change { edits: vec![edit], created: None })
}

// ---- names -----------------------------------------------------------------------

fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_lowercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Applies edits to a string (for tests and previews).
#[cfg(test)]
fn apply(src: &str, edits: &[Edit]) -> String {
    let mut edits = edits.to_vec();
    edits.sort_by_key(|e| std::cmp::Reverse(e.start));
    let mut s = src.to_owned();
    for e in edits {
        s.replace_range(e.start..e.end, &e.text);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "package P {\n    part def Engine;\n    part def Car {\n        attribute mass : Real;\n    }\n    part car1;\n    requirement def Speed;\n}\n";

    fn ok(src: &str, change: Option<Change>) -> String {
        let out = apply(src, &change.expect("change").edits);
        let r = sysml::parse(&out);
        assert!(r.diagnostics.is_empty(), "{out}\n{:?}", r.diagnostics);
        out
    }

    #[test]
    fn creates_elements_in_packages_and_files() {
        let out = ok(SRC, create(SRC, Some("P"), ElementKind::Def(DefKind::Port), None));
        assert!(out.contains("    port def Port1;\n}\n"), "{out}");
        let out = ok("", create("", None, ElementKind::Usage(UsageKind::Part), None));
        assert_eq!(out, "part part1;\n");
        let c = create(SRC, Some("P"), ElementKind::Usage(UsageKind::Part), Some("P::Car")).unwrap();
        assert_eq!(c.created.as_deref(), Some("P::part1"));
        assert!(ok(SRC, Some(c)).contains("part part1 : Car;"));
        assert_eq!(default_container(SRC, Some("P::Car::mass")).as_deref(), Some("P"));
    }

    #[test]
    fn adds_features_opening_bodies() {
        let out = ok(SRC, add_feature(SRC, "P::Engine", UsageKind::Attribute, false, None, Some("ScalarValues::Real")));
        assert!(out.contains("part def Engine {\n        attribute real : ScalarValues::Real;\n    }"), "{out}");
        let out = ok(SRC, add_feature(SRC, "P::Car", UsageKind::Part, false, None, Some("P::Engine")));
        assert!(out.contains("attribute mass : Real;\n        part engine : Engine;\n    }"), "{out}");
        let one = "part def A { }";
        let out = ok(one, add_feature(one, "A", UsageKind::Port, false, Some("p"), None));
        assert_eq!(out, "part def A {\n    port p;\n}");
    }

    #[test]
    fn relationships() {
        let out = ok(SRC, relate(SRC, Relation::Specialize, "P::Car", "P::Engine"));
        assert!(out.contains("part def Car :> Engine {"), "{out}");
        let out = ok(&out, relate(&out, Relation::Specialize, "P::Car", "P::Speed"));
        assert!(out.contains("part def Car :> Engine, Speed {"), "{out}");
        let out = ok(SRC, relate(SRC, Relation::Type, "P::car1", "P::Car"));
        assert!(out.contains("part car1 : Car;"), "{out}");
        let out = ok(&out, relate(&out, Relation::Type, "P::car1", "P::Engine"));
        assert!(out.contains("part car1 : Engine;"), "{out}");
        let out = ok(SRC, relate(SRC, Relation::Satisfy, "P::car1", "P::Speed"));
        assert!(out.contains("    satisfy Speed by car1;\n}"), "{out}");
        for rel in [Relation::Dependency, Relation::Connect, Relation::Flow, Relation::Allocate] {
            ok(SRC, relate(SRC, rel, "P::car1", "P::Engine"));
        }
        let out = ok(SRC, relate(SRC, Relation::Reference(UsageKind::Part), "P::Car", "P::Engine"));
        assert!(out.contains("ref part engine : Engine;"), "{out}");
    }

    #[test]
    fn deletes_renames_and_headers() {
        let car = sysml::parse(SRC).model;
        let index = Index::build(&car);
        let span = index.entry(index.get("P::Car::mass").unwrap()).member.span;
        let out = apply(SRC, &[delete(SRC, span)]);
        assert!(out.contains("part def Car {\n    }"), "{out}");
        let out = apply(SRC, &rename(SRC, "Car", "Vehicle Model"));
        assert!(out.contains("part def 'Vehicle Model'"), "{out}");
        let span = index.entry(index.get("P::Car").unwrap()).member.span;
        let (s, e) = header_range(SRC, span);
        assert_eq!(&SRC[s..e], "part def Car");
        let span = index.entry(index.get("P::Engine").unwrap()).member.span;
        let (s, e) = header_range(SRC, span);
        assert_eq!(&SRC[s..e], "part def Engine");
    }

    #[test]
    fn every_palette_kind_parses_and_features_can_be_added() {
        for (_, kinds) in palette() {
            for kind in kinds {
                let out = ok(SRC, create(SRC, Some("P"), kind, None));
                let name = format!("P::{}1", kind.base_name());
                let d = crate::diagram::Diagram::build(&sysml::parse(&out).model);
                assert!(d.node_named(&name).is_some(), "{kind:?} is not a diagram node:\n{out}");
                if let ElementKind::Def(_) = kind {
                    for f in FEATURE_KINDS {
                        ok(&out, add_feature(&out, &name, f, false, None, None));
                    }
                }
            }
        }
    }

    #[test]
    fn library_types_use_short_names_when_imported() {
        let src = "package P {\n    private import ScalarValues::*;\n    part def A;\n}\n";
        let out = ok(src, add_feature(src, "P::A", UsageKind::Attribute, false, Some("x"), Some("ScalarValues::Real")));
        assert!(out.contains("attribute x : Real;"), "{out}");
    }

    #[test]
    fn offers_sensible_relations() {
        let def = |k| ElementInfo { is_def: true, kind: k };
        let usage = |k| ElementInfo { is_def: false, kind: k };
        let r = relations_between(def(UsageKind::Part), def(UsageKind::Requirement));
        assert!(r.contains(&Relation::Specialize) && r.contains(&Relation::Satisfy));
        let r = relations_between(usage(UsageKind::Part), usage(UsageKind::Part));
        assert!(r.contains(&Relation::Connect) && !r.contains(&Relation::Specialize));
        assert!(types_for(SRC, Some(UsageKind::Attribute)).iter().any(|(_, n)| n == "Real"));
    }
}
