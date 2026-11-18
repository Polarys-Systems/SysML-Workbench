//! Hover help for the model itself: what a name in the code (or a diagram
//! node, or one of its compartment lines) stands for. The name is resolved
//! against the edited file, the other project files and the standard
//! library, and described with its kind, where it is declared, its types and
//! their supertypes, multiplicity, direction, value, units, documentation,
//! owned and inherited features, and requirement status and model checks.
//!
//! The keyword reference ([`crate::docs`]) explains the language; this
//! explains the model.

use crate::docs;
use crate::document::{Document, Tone};
use crate::ibd::layers;
use crate::source::NamePath;
use sysml::units;
use sysml::{
    CommentKind, ConnectorKind, Direction, EntryId, Index, LineIndex, Member, MemberKind, Multiplicity, Name,
    QualifiedName, Span, Usage, UsageKind,
};

/// Owned features listed; the rest are counted.
const MAX_MEMBERS: usize = 8;
/// Names listed in one fact (units, users, inherited features, ...).
const MAX_LIST: usize = 6;
/// Supertypes followed from a type.
const MAX_CHAIN: usize = 10;

/// One `label  value` row of the popup.
pub struct Fact {
    pub label: &'static str,
    pub value: String,
    pub tone: Tone,
}

/// Everything the popup shows about one element.
pub struct ElementHelp {
    /// `<kg> kilogram`
    pub title: String,
    /// `attribute def · ISQ::MassValue · standard library`
    pub meta: String,
    /// The declaration on one line.
    pub signature: String,
    pub facts: Vec<Fact>,
    /// `doc` comments, as paragraphs.
    pub doc: Vec<String>,
    /// Where the doc text comes from, when not the element itself (its type).
    pub doc_from: Option<String>,
    /// Owned features, one per line (with a last `… n more` line).
    pub members: Vec<String>,
    /// The keyword reference entry for the element's kind.
    pub keyword: Option<usize>,
}

impl Fact {
    fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self { label, value: value.into(), tone: Tone::Value }
    }

    fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

// ---- finding the element -------------------------------------------------------

/// Help for the name at `offset` of `src` (the text `doc` was parsed from).
pub fn at_offset(doc: &Document, src: &str, offset: usize) -> Option<ElementHelp> {
    let path = crate::source::path_at(src, offset)?;
    let index = doc.index();
    let scope = doc.element_at(offset).and_then(|q| index.get(q));
    match declared_at(&index, path.span).or_else(|| resolve_path(&index, scope, &path)) {
        Some(id) => Some(describe(doc, &index, id, Some(src))),
        None => unit_help(&path),
    }
}

/// Help for the element with this qualified name (a diagram node).
pub fn for_qualified_name(doc: &Document, src: &str, qualified_name: &str) -> Option<ElementHelp> {
    let index = doc.index();
    let id = index.get(qualified_name)?;
    Some(describe(doc, &index, id, Some(src)))
}

/// Help for the owned feature of `owner` declared at `span` (a diagram
/// compartment line).
pub fn for_feature(doc: &Document, src: &str, owner: &str, span: Span) -> Option<ElementHelp> {
    let index = doc.index();
    let owner = index.get(owner)?;
    let id = index.entry(owner).children.iter().copied().find(|&c| index.entry(c).member.span == span)?;
    Some(describe(doc, &index, id, Some(src)))
}

/// The element of the edited file whose name is written at `span`.
fn declared_at(index: &Index, span: (usize, usize)) -> Option<EntryId> {
    let at = |n: Option<&Name>| n.is_some_and(|n| (n.span.start, n.span.end) == span);
    index.entries().iter().position(|e| {
        e.source == 0 && e.member.kind.identification().is_some_and(|i| at(i.name.as_ref()) || at(i.short_name.as_ref()))
    })
}

fn qualified(segments: &[String]) -> QualifiedName {
    let segments = segments.iter().map(|s| Name { text: s.clone(), span: Span::default() }).collect();
    QualifiedName { segments, span: Span::default() }
}

/// A named feature among the members of `id` and of its types and supertypes.
fn feature_of(index: &Index, id: EntryId, name: &str) -> Option<EntryId> {
    layers(index, &[id]).into_iter().find_map(|l| index.entry(l).children.iter().copied().find(|&c| index.entry(c).name == name))
}

/// A name as written in `scope`: owned, imported or (for a plain name)
/// inherited by `scope` or one of its owners from their supertypes.
fn lookup(index: &Index, scope: Option<EntryId>, segments: &[String]) -> Option<EntryId> {
    let found = index.resolve(scope, &qualified(segments));
    // An anonymous redefinition (`:>> mass = 2 [kg];`) stands for the feature it redefines.
    if let Some(id) = found
        && is_anonymous_redefinition(index, id)
        && let Some(p) = index.entry(id).parent
        && let Some(original) = inherited(index, p, &segments[0])
    {
        return Some(original);
    }
    if found.is_some() || segments.len() != 1 {
        return found;
    }
    let mut at = scope;
    while let Some(s) = at {
        if let Some(f) = inherited(index, s, &segments[0]) {
            return Some(f);
        }
        at = index.entry(s).parent;
    }
    None
}

/// A feature `name` of a supertype (or type) of `owner`, not of `owner` itself.
fn inherited(index: &Index, owner: EntryId, name: &str) -> Option<EntryId> {
    layers(index, &[owner])
        .into_iter()
        .skip(1)
        .find_map(|l| index.entry(l).children.iter().copied().find(|&c| index.entry(c).name == name && !is_anonymous_redefinition(index, c)))
}

fn is_anonymous_redefinition(index: &Index, id: EntryId) -> bool {
    matches!(&index.entry(id).member.kind, MemberKind::Usage(u) if u.id.display_name().is_none() && !u.redefines.is_empty())
}

fn resolve_path(index: &Index, scope: Option<EntryId>, path: &NamePath) -> Option<EntryId> {
    let (first, rest) = path.groups.split_first()?;
    let mut at = lookup(index, scope, first).or_else(|| {
        // Unit symbols in `[kg]` usually are not imported.
        (first.len() == 1 && path.groups.len() == 1).then(|| index.resolve(None, &qualified(&["SI".into(), first[0].clone()]))).flatten()
    })?;
    for group in rest {
        for seg in group {
            at = feature_of(index, at, seg)?;
        }
    }
    Some(at)
}

/// A unit symbol the library does not declare but the value checker knows
/// (`kW`, `mAh`, `rpm`).
fn unit_help(path: &NamePath) -> Option<ElementHelp> {
    let [group] = path.groups.as_slice() else { return None };
    let [symbol] = group.as_slice() else { return None };
    let unit = units::lookup(symbol)?;
    let mut facts = vec![Fact::new("Dimension", dimension(unit.dim))];
    facts.push(Fact::new("In SI units", format!("1 {symbol} = {} {}", units::format_number(unit.scale), base_unit(unit.dim))));
    Some(ElementHelp {
        title: symbol.clone(),
        meta: "unit symbol · known to the value checker".into(),
        signature: format!("value [{symbol}]"),
        facts,
        doc: vec![
            "A unit written in a quantity value such as `5 [kW]`. EagleView converts values to SI base units when it evaluates expressions and checks that both sides of a comparison or sum have the same dimension.".replace('`', ""),
        ],
        doc_from: None,
        members: Vec::new(),
        keyword: docs::reference().lookup("units"),
    })
}

fn dimension(dim: units::Dim) -> String {
    if dim.is_none() { "dimensionless".into() } else { dim.to_string() }
}

fn base_unit(dim: units::Dim) -> String {
    if dim.is_none() { "(dimensionless)".into() } else { dim.to_string() }
}

// ---- describing it ---------------------------------------------------------------

/// `part def`, `attribute`, `package`, ...
fn kind_keyword(m: &Member) -> String {
    match &m.kind {
        MemberKind::Package(p) if p.is_library => format!("{}library package", if p.is_standard { "standard " } else { "" }),
        MemberKind::Package(_) => "package".into(),
        MemberKind::Alias(_) => "alias".into(),
        MemberKind::Comment(c) => match c.kind {
            CommentKind::Doc => "doc",
            CommentKind::Comment => "comment",
            CommentKind::Rep => "rep",
        }
        .into(),
        MemberKind::Dependency(_) => "dependency".into(),
        MemberKind::Definition(d) => format!("{} def", d.kind),
        MemberKind::Usage(u) => match (u.prefix.role, u.kind) {
            (Some(r), _) if u.id.display_name().is_none() => r.keyword().into(),
            (_, UsageKind::Feature) if u.prefix.is_ref => "ref".into(),
            (_, UsageKind::Feature) => "feature".into(),
            (_, k) => k.keyword().into(),
        },
        MemberKind::Transition(_) => "transition".into(),
        MemberKind::Then(m) => kind_keyword(m),
        _ => "element".into(),
    }
}

/// `part def Engine` → `part def`, for describing a resolved type in a fact.
fn kind_of(index: &Index, id: EntryId) -> String {
    kind_keyword(index.entry(id).member)
}

/// The supertypes of `id`, nearest first, following the first base of each
/// (definitions) or the first type (usages).
fn supertypes(index: &Index, id: EntryId) -> Vec<EntryId> {
    let mut out = Vec::new();
    let mut at = id;
    while out.len() < MAX_CHAIN {
        let e = index.entry(at);
        let next = match &e.member.kind {
            MemberKind::Definition(d) => d.specializes.iter().find_map(|b| index.resolve(e.parent, b)),
            _ => None,
        };
        match next {
            Some(n) if n != id && !out.contains(&n) => {
                out.push(n);
                at = n;
            }
            _ => break,
        }
    }
    out
}

fn chain_text(index: &Index, ids: &[EntryId]) -> String {
    ids.iter().map(|&i| index.entry(i).name).collect::<Vec<_>>().join(" → ")
}

fn short_list(items: Vec<String>) -> String {
    let n = items.len();
    let mut out: Vec<String> = items.into_iter().take(MAX_LIST).collect();
    if n > MAX_LIST {
        out.push(format!("… {} more", n - MAX_LIST));
    }
    out.join(", ")
}

/// `[0..1]` → `[0..1] (optional)`.
fn multiplicity_text(m: &Multiplicity) -> String {
    let text = m.to_string();
    let lower = m.lower.as_ref().map(ToString::to_string);
    let upper = m.upper.to_string();
    let meaning = match (lower.as_deref(), upper.as_str()) {
        (Some("0"), "1") => "optional, at most one",
        (Some("0"), "*") | (None, "*") => "any number",
        (Some("1"), "*") => "at least one",
        (None, "1") | (Some("1"), "1") => "exactly one",
        (None, "0") => "none",
        (None, _) => "exactly this many",
        (Some(_), "*") => "at least the lower bound",
        _ => "between the bounds",
    };
    format!("{text}  {meaning}")
}

/// Text of the `doc` comments owned by `id`, as paragraphs.
fn doc_text(index: &Index, id: EntryId) -> Vec<String> {
    let docs = index.entry(id).member.kind.body().iter().filter_map(|m| match &m.kind {
        MemberKind::Comment(c) if c.kind == CommentKind::Doc => Some(c.text.as_str()),
        _ => None,
    });
    let mut out = Vec::new();
    for text in docs {
        let lines: Vec<&str> = text.lines().map(|l| l.trim().trim_start_matches('*').trim()).collect();
        for p in lines.split(|l| l.is_empty()) {
            let p = p.join(" ");
            if !p.is_empty() {
                out.push(p);
            }
        }
    }
    out
}

/// `kilogram (kg)` for a unit entry.
fn unit_label(index: &Index, id: EntryId) -> String {
    let e = index.entry(id);
    match e.short_name {
        Some(s) => format!("{s} ({})", e.name),
        None => e.name.to_string(),
    }
}

/// The type name a usage is typed by (its last segment), if any.
fn type_name(u: &Usage) -> Option<&str> {
    u.typed_by.first().map(|t| t.name.last().text.as_str())
}

/// Library units (`kg`, `g`, ...) declared with type `unit_def` (`MassUnit`).
fn units_of(index: &Index, unit_def: &str) -> Vec<EntryId> {
    index
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(&e.member.kind, MemberKind::Usage(u) if type_name(u) == Some(unit_def)) && e.qualified_name.starts_with("SI::"))
        .map(|(i, _)| i)
        .collect()
}

/// For a quantity value type (`MassValue`) in ISQ: its units.
fn quantity_facts(index: &Index, def: EntryId, facts: &mut Vec<Fact>) {
    let e = index.entry(def);
    if !e.qualified_name.starts_with("ISQ::") {
        return;
    }
    if let Some(quantity) = e.name.strip_suffix("Value") {
        let units = units_of(index, &format!("{quantity}Unit"));
        if let Some(dim) = units.iter().find_map(|&u| unit_of(index, u)).map(|u| u.dim) {
            facts.push(Fact::new("Dimension", dimension(dim)));
        }
        if !units.is_empty() {
            facts.push(Fact::new("Units", short_list(units.iter().map(|&u| unit_label(index, u)).collect())));
        }
    } else if let Some(quantity) = e.name.strip_suffix("Unit") {
        facts.push(Fact::new("Measures", format!("{quantity}Value")));
        let units = units_of(index, e.name);
        if !units.is_empty() {
            facts.push(Fact::new("SI units", short_list(units.iter().map(|&u| unit_label(index, u)).collect())));
        }
    }
}

/// What the value checker knows about a library unit (by its symbol, else its name).
fn unit_of(index: &Index, id: EntryId) -> Option<units::Unit> {
    let e = index.entry(id);
    e.short_name.and_then(units::lookup).or_else(|| units::lookup(e.name))
}

/// `ElectricPotentialDifference` → `electric potential difference`.
fn words(camel: &str) -> String {
    let mut out = String::new();
    for (i, c) in camel.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// The documentation of `id`: its `doc` comments, or for the ISQ quantity
/// types (which have none) a description made from the name.
fn documentation(index: &Index, id: EntryId) -> Vec<String> {
    let own = doc_text(index, id);
    let e = index.entry(id);
    if !own.is_empty() || !e.qualified_name.starts_with("ISQ::") {
        return own;
    }
    let text = if let Some(q) = e.name.strip_suffix("Value") {
        format!("A value of the quantity {} (ISO 80000): a number together with a unit of that quantity.", words(q))
    } else if let Some(q) = e.name.strip_suffix("Unit") {
        format!("The type of the units of {} (ISO 80000); SI declares its units with this type.", words(q))
    } else if matches!(e.member.kind, MemberKind::Usage(_)) {
        format!("The general {} feature: attributes of this quantity can subset it.", words(e.name))
    } else {
        return own;
    };
    vec![text]
}

/// For a unit (`attribute <kg> kilogram : MassUnit` in SI): what it measures and its scale.
fn unit_facts(index: &Index, id: EntryId, u: &Usage, facts: &mut Vec<Fact>) {
    let e = index.entry(id);
    if !e.qualified_name.starts_with("SI::") {
        return;
    }
    let Some(quantity) = type_name(u).and_then(|t| t.strip_suffix("Unit")) else { return };
    facts.push(Fact::new("Unit of", format!("{quantity}Value")));
    let symbol = e.short_name.unwrap_or(e.name);
    if let Some(unit) = unit_of(index, id) {
        facts.push(Fact::new("Dimension", dimension(unit.dim)));
        if unit.scale != 1.0 {
            facts.push(Fact::new("In SI units", format!("1 {symbol} = {} {}", units::format_number(unit.scale), base_unit(unit.dim))));
        }
    }
}

/// Usages typed by `def` and definitions specializing it, in the project files.
fn users(index: &Index, def: EntryId, project_len: usize) -> (Vec<String>, Vec<String>) {
    let mut typed = Vec::new();
    let mut specialized = Vec::new();
    for (i, e) in index.entries().iter().enumerate() {
        if e.source > project_len || i == def {
            continue;
        }
        match &e.member.kind {
            MemberKind::Usage(u) if u.typed_by.iter().any(|t| index.resolve(e.parent, &t.name) == Some(def)) => {
                let owner = e.parent.map(|p| format!(" in {}", index.entry(p).name)).unwrap_or_default();
                typed.push(format!("{}{owner}", e.name));
            }
            MemberKind::Definition(d) if d.specializes.iter().any(|b| index.resolve(e.parent, b) == Some(def)) => {
                specialized.push(e.name.to_string());
            }
            _ => {}
        }
    }
    (typed, specialized)
}

/// Named features `id` gets from its types and supertypes, with where from.
fn inherited_features(index: &Index, id: EntryId) -> Vec<String> {
    let own: Vec<&str> = index.entry(id).children.iter().map(|&c| index.entry(c).name).collect();
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for l in layers(index, &[id]).into_iter().skip(1) {
        for &c in &index.entry(l).children {
            let ce = index.entry(c);
            let feature = matches!(&ce.member.kind, MemberKind::Usage(u) if u.connector.is_none());
            if feature && !own.contains(&ce.name) && !seen.contains(&ce.name) {
                seen.push(ce.name);
                out.push(format!("{} ({})", ce.name, index.entry(l).name));
            }
        }
    }
    out
}

/// One-line summaries of the owned members worth listing.
fn member_lines(m: &Member) -> Vec<String> {
    let listed: Vec<String> = m
        .kind
        .body()
        .iter()
        .filter(|c| !matches!(c.kind, MemberKind::Comment(_) | MemberKind::Import(_) | MemberKind::Expression(_)))
        .map(|c| c.kind.summary().lines().next().unwrap_or("").trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let n = listed.len();
    let mut out: Vec<String> = listed.into_iter().take(MAX_MEMBERS).collect();
    if n > MAX_MEMBERS {
        out.push(format!("… {} more", n - MAX_MEMBERS));
    }
    out
}

fn direction_text(d: Direction) -> &'static str {
    match d {
        Direction::In => "in  flows into its owner",
        Direction::Out => "out  flows out of its owner",
        Direction::InOut => "inout  flows both ways",
    }
}

fn modifiers(u: &Usage) -> Vec<&'static str> {
    let p = &u.prefix;
    let flags = [
        (p.is_abstract, "abstract"),
        (p.is_ref, "ref"),
        (p.is_derived, "derived"),
        (p.is_readonly, "readonly"),
        (p.is_constant, "constant"),
        (p.is_end, "end"),
        (p.is_variation, "variation"),
        (p.is_variant, "variant"),
        (p.is_individual, "individual"),
        (p.is_event, "event"),
        (u.is_ordered, "ordered"),
        (u.is_nonunique, "nonunique"),
        (u.is_parallel, "parallel"),
    ];
    flags.iter().filter(|f| f.0).map(|f| f.1).collect()
}

/// `Engine  part def · Parts::Engine`, or `Engine  not found`.
fn resolved_text(index: &Index, scope: Option<EntryId>, name: &QualifiedName, prefix: &str) -> Fact {
    match index.resolve(scope, name) {
        Some(t) => Fact::new("", format!("{prefix}{name}  {} · {}", kind_of(index, t), index.entry(t).qualified_name)),
        None => Fact::new("", format!("{prefix}{name}  not found")).tone(Tone::Bad),
    }
}

/// Everything about entry `id`; `src` is the edited text (for line numbers).
pub fn describe(doc: &Document, index: &Index, id: EntryId, src: Option<&str>) -> ElementHelp {
    let e = index.entry(id);
    let m = e.member;
    let keyword = kind_keyword(m);
    let project_len = doc.project.len();
    let location = match e.source {
        0 => match src {
            Some(s) => format!("line {}", LineIndex::new(s).line_col(m.span.start).0),
            None => "this file".into(),
        },
        s if s <= project_len => doc.project[s - 1].file_name(),
        _ => "standard library".into(),
    };
    let title = match e.short_name {
        Some(s) => format!("<{s}> {}", e.name),
        None => e.name.to_string(),
    };
    let mut facts: Vec<Fact> = Vec::new();
    let mut doc_paragraphs = documentation(index, id);
    let mut doc_from = None;

    match &m.kind {
        MemberKind::Definition(d) => {
            for (i, b) in d.specializes.iter().enumerate() {
                let f = resolved_text(index, e.parent, b, "");
                facts.push(Fact { label: if i == 0 { "Specializes" } else { "" }, ..f });
            }
            let chain = supertypes(index, id);
            if chain.len() > 1 {
                facts.push(Fact::new("Supertypes", chain_text(index, &chain)));
            }
            if d.specializes.is_empty()
                && let Some(base) = docs::reference().for_kind(&keyword).and_then(|r| docs::reference().entry(r).library_def.as_deref())
                && base != e.qualified_name
            {
                facts.push(Fact::new("Implicitly", format!("specializes {base}")).tone(Tone::Muted));
            }
            let flags: Vec<&str> =
                [(d.is_abstract, "abstract"), (d.is_variation, "variation"), (d.is_individual, "individual"), (d.is_parallel, "parallel")]
                    .iter()
                    .filter(|f| f.0)
                    .map(|f| f.1)
                    .collect();
            if !flags.is_empty() {
                facts.push(Fact::new("Modifiers", flags.join(", ")));
            }
            quantity_facts(index, id, &mut facts);
            let inherited = inherited_features(index, id);
            if !inherited.is_empty() {
                facts.push(Fact::new("Inherits", short_list(inherited)));
            }
            let (typed, specialized) = users(index, id, project_len);
            if !typed.is_empty() {
                facts.push(Fact::new("Used by", short_list(typed)));
            }
            if !specialized.is_empty() {
                facts.push(Fact::new("Specialized by", short_list(specialized)));
            }
        }
        MemberKind::Usage(u) => {
            for (i, t) in u.typed_by.iter().enumerate() {
                let prefix = if t.conjugated { "~" } else { "" };
                let mut f = resolved_text(index, e.parent, &t.name, prefix);
                if t.conjugated {
                    f.value += "  (conjugated: directions reversed)";
                }
                facts.push(Fact { label: if i == 0 { "Type" } else { "" }, ..f });
            }
            let ty = u.typed_by.first().and_then(|t| index.resolve(e.parent, &t.name));
            if let Some(ty) = ty {
                let chain = supertypes(index, ty);
                if !chain.is_empty() {
                    facts.push(Fact::new("Supertypes", chain_text(index, &chain)));
                }
                quantity_facts(index, ty, &mut facts);
                if doc_paragraphs.is_empty() {
                    doc_paragraphs = documentation(index, ty);
                    if !doc_paragraphs.is_empty() {
                        doc_from = Some(index.entry(ty).name.to_string());
                    }
                }
            }
            if u.typed_by.is_empty() && u.connector.is_none() && u.prefix.role.is_none() && u.references.is_empty() {
                let base = docs::reference().for_kind(&keyword).and_then(|r| docs::reference().entry(r).library_usage.as_deref());
                if let Some(base) = base.filter(|b| *b != e.qualified_name) {
                    facts.push(Fact::new("Implicitly", format!("subsets {base}")).tone(Tone::Muted));
                }
            }
            unit_facts(index, id, u, &mut facts);
            if let Some(mult) = &u.multiplicity {
                facts.push(Fact::new("Multiplicity", multiplicity_text(mult)));
            }
            if let Some(d) = u.prefix.direction {
                facts.push(Fact::new("Direction", direction_text(d)));
            }
            let flags = modifiers(u);
            if !flags.is_empty() {
                facts.push(Fact::new("Modifiers", flags.join(", ")));
            }
            let refs = |rs: &[sysml::Reference]| short_list(rs.iter().map(ToString::to_string).collect());
            if !u.subsets.is_empty() {
                facts.push(Fact::new("Subsets", refs(&u.subsets)));
            }
            if !u.redefines.is_empty() {
                let mut text = refs(&u.redefines);
                if let Some(p) = e.parent
                    && let Some(orig) = inherited(index, p, &u.redefines[0].last().text)
                {
                    text += &format!("  from {}", index.entry(orig).parent.map_or("", |o| index.entry(o).name));
                    if let MemberKind::Usage(ou) = &index.entry(orig).member.kind
                        && u.typed_by.is_empty()
                    {
                        for t in &ou.typed_by {
                            facts.push(Fact::new("Type", format!("{t}  (inherited)")));
                        }
                    }
                }
                facts.push(Fact::new("Redefines", text));
            }
            if let Some(role) = u.prefix.role
                && !u.references.is_empty()
            {
                facts.push(Fact::new("Role", format!("{} {}", role.keyword(), refs(&u.references))));
            } else if !u.references.is_empty() {
                facts.push(Fact::new("References", refs(&u.references)));
            }
            if let Some(by) = &u.by {
                facts.push(Fact::new("By", by.to_string()));
            }
            if let Some(c) = &u.connector {
                let ends: Vec<String> = c.ends.iter().map(|e| e.target.to_string()).collect();
                let (label, sep) = match c.kind {
                    ConnectorKind::Bind => ("Binds", " = "),
                    ConnectorKind::Succession => ("Order", " then "),
                    ConnectorKind::Allocate => ("Allocates", " to "),
                    _ => ("Ends", " → "),
                };
                if !ends.is_empty() {
                    facts.push(Fact::new(label, ends.join(sep)));
                }
                if let Some(t) = &c.item_type {
                    facts.push(Fact::new("Carries", t.to_string()));
                }
            }
            if let Some(v) = &u.value
                && e.source == 0
            {
                let computed = doc.annotations.iter().find(|a| a.offset >= v.span.start && a.offset <= v.span.end);
                if let Some(a) = computed {
                    facts.push(Fact::new("Evaluates to", a.text.trim_start_matches("= ").to_owned()).tone(a.tone));
                }
            }
        }
        MemberKind::Alias(a) => {
            facts.push(Fact { label: "Alias for", ..resolved_text(index, e.parent, &a.target, "") });
        }
        MemberKind::Package(_) => {
            let mut defs = 0;
            let mut usages = 0;
            let mut packages = 0;
            for &c in &e.children {
                match index.entry(c).member.kind {
                    MemberKind::Definition(_) => defs += 1,
                    MemberKind::Usage(_) => usages += 1,
                    MemberKind::Package(_) => packages += 1,
                    _ => {}
                }
            }
            let mut parts = Vec::new();
            for (n, what) in [(packages, "package"), (defs, "definition"), (usages, "usage")] {
                if n > 0 {
                    parts.push(format!("{n} {what}{}", if n == 1 { "" } else { "s" }));
                }
            }
            if !parts.is_empty() {
                facts.push(Fact::new("Contains", parts.join(", ")));
            }
        }
        _ => {}
    }

    // Requirement status, from the requirements table.
    if let Some(r) = doc.requirements.rows.iter().find(|r| r.qualified_name == e.qualified_name) {
        if !r.id.is_empty() && e.short_name.is_none() {
            facts.push(Fact::new("Id", r.id.clone()));
        }
        if let Some(s) = &r.subject {
            facts.push(Fact::new("Subject", s.clone()));
        }
        if !r.is_def {
            use crate::requirements::ReqStatus as S;
            let tone = match r.status {
                S::Satisfied => Tone::Good,
                S::Violated | S::CannotCheck => Tone::Bad,
                _ => Tone::Muted,
            };
            let detail = if r.detail.is_empty() { String::new() } else { format!("  {}", r.detail) };
            facts.push(Fact::new("Status", format!("{}{detail}", r.status.label())).tone(tone));
        }
        if !r.satisfied_by.is_empty() {
            facts.push(Fact::new("Satisfied by", short_list(r.satisfied_by.clone())));
        }
        if !r.verified_by.is_empty() {
            facts.push(Fact::new("Verified by", short_list(r.verified_by.clone())));
        }
    }
    // Model checks about it.
    for f in doc.checks.iter().filter(|f| f.element == e.qualified_name).take(3) {
        facts.push(Fact::new("Check", f.message.clone()).tone(Tone::Bad));
    }

    let r = docs::reference();
    ElementHelp {
        meta: format!("{keyword} · {} · {location}", e.qualified_name),
        signature: m.kind.summary().lines().next().unwrap_or("").trim().to_owned(),
        title,
        facts,
        doc: doc_paragraphs,
        doc_from,
        members: member_lines(m),
        keyword: r.for_kind(&keyword).or_else(|| r.for_line(&keyword)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SRC: &str = "package P {
    private import ISQ::*;
    private import ScalarValues::*;
    part def Vehicle {
        doc /* Anything that moves people. */
        attribute mass : MassValue [1];
        in port fuel : ~FuelPort;
    }
    port def FuelPort;
    part def Car :> Vehicle {
        :>> mass = 1200 [kg];
        attribute wheels : Integer = 2 * 2;
    }
    part car : Car;
}";

    fn doc() -> Document {
        let mut d = Document::load(PathBuf::new());
        d.load_error = None;
        d.set_source(SRC);
        d
    }

    fn at(d: &Document, needle: &str, nth: usize) -> ElementHelp {
        let start = SRC.match_indices(needle).nth(nth).unwrap().0;
        let offset = start + needle.find(|c: char| c.is_alphanumeric()).unwrap();
        at_offset(d, SRC, offset).unwrap_or_else(|| panic!("no help for {needle}"))
    }

    fn fact<'a>(h: &'a ElementHelp, label: &str) -> Option<&'a str> {
        h.facts.iter().find(|f| f.label == label).map(|f| f.value.as_str())
    }

    #[test]
    fn declarations_and_references() {
        let d = doc();
        let car = at(&d, "Car :>", 0);
        assert!(car.meta.starts_with("part def · P::Car · line 10"), "{}", car.meta);
        assert!(fact(&car, "Specializes").unwrap().starts_with("Vehicle  part def · P::Vehicle"));
        assert!(fact(&car, "Inherits").unwrap().contains("fuel (Vehicle)"));
        assert_eq!(fact(&car, "Used by"), Some("car in P"));
        // `part car : Car` - the type reference, from the usage.
        let typed = at(&d, ": Car", 0);
        assert_eq!(typed.title, "Car");
        let car_usage = at(&d, "car :", 0);
        assert!(fact(&car_usage, "Type").unwrap().starts_with("Car  part def"));
        assert_eq!(car_usage.doc_from, None, "Car has no doc of its own");
        let vehicle = at(&d, "Vehicle {", 0);
        assert_eq!(vehicle.doc, ["Anything that moves people."]);
        assert!(fact(&vehicle, "Implicitly").unwrap().contains("Parts::Part"));
        assert!(vehicle.members.iter().any(|m| m.starts_with("attribute mass")));
        assert_eq!(vehicle.keyword.map(|k| docs::reference().entry(k).names[0].as_str()), Some("part"));
    }

    #[test]
    fn features_types_and_units() {
        let d = doc();
        let mass = at(&d, "mass :", 0);
        assert!(fact(&mass, "Type").unwrap().starts_with("MassValue  attribute def · ISQ::MassValue"));
        assert!(fact(&mass, "Supertypes").unwrap().starts_with("ScalarQuantityValue → "));
        assert!(fact(&mass, "Units").unwrap().contains("kg (kilogram)"));
        assert_eq!(fact(&mass, "Dimension"), Some("kg"));
        assert_eq!(mass.doc_from.as_deref(), Some("MassValue"));
        assert!(mass.doc[0].starts_with("A value of the quantity mass"), "{:?}", mass.doc);
        assert!(fact(&mass, "Multiplicity").unwrap().contains("exactly one"));
        let fuel = at(&d, "fuel", 0);
        assert!(fact(&fuel, "Direction").unwrap().starts_with("in"));
        assert!(fact(&fuel, "Type").unwrap().contains("conjugated"));
        // The anonymous redefinition `:>> mass` stands for Vehicle::mass.
        let redefined = at(&d, ":>> mass", 0);
        assert!(redefined.meta.contains("P::Vehicle::mass"), "{}", redefined.meta);
        let kg = at(&d, "kg]", 0);
        assert_eq!(kg.title, "<kg> kilogram");
        assert_eq!(fact(&kg, "Unit of"), Some("MassValue"));
        let int = at(&d, "Integer", 0);
        assert!(int.doc[0].starts_with("A whole number"));
        assert!(fact(&int, "Supertypes").unwrap().starts_with("Rational → Real → Complex"));
        let wheels = at(&d, "wheels", 0);
        assert_eq!(fact(&wheels, "Evaluates to"), Some("4"));
    }

    #[test]
    fn unknown_units() {
        let src = "package P { attribute p = 5 [kW]; }";
        let mut d = Document::load(PathBuf::new());
        d.set_source(src);
        let h = at_offset(&d, src, src.find("kW").unwrap()).unwrap();
        assert_eq!(fact(&h, "In SI units"), Some("1 kW = 1000 W"));
    }
}
