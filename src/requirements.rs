//! Requirements table and traceability matrix: every requirement with its
//! id, text, subject, what satisfies and verifies it and whether the
//! evaluated constraints hold; and the trace links as a matrix of
//! requirements against the elements that satisfy, verify or depend on them.

use std::collections::HashMap;
use sysml::eval::{Check, Verdict};
use sysml::{CommentKind, EntryId, Index, Member, MemberKind, Model, Role, UsageKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReqStatus {
    Satisfied,
    Violated,
    /// Satisfied by something, but the constraints cannot be decided.
    Undecided,
    /// The constraints could not be evaluated (an error).
    CannotCheck,
    /// Nothing satisfies it.
    Open,
    /// A requirement definition: its usages are what gets satisfied.
    Definition,
}

impl ReqStatus {
    pub fn label(self) -> &'static str {
        match self {
            ReqStatus::Satisfied => "satisfied",
            ReqStatus::Violated => "violated",
            ReqStatus::Undecided => "undecided",
            ReqStatus::CannotCheck => "cannot check",
            ReqStatus::Open => "not satisfied",
            ReqStatus::Definition => "definition",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Requirement {
    pub qualified_name: String,
    /// Short name, e.g. `R1` from `<'R1'>`; empty if none.
    pub id: String,
    pub name: String,
    pub is_def: bool,
    /// The definition of a requirement usage.
    pub ty: Option<String>,
    /// The `doc` comment (of the usage, else of its definition), on one line.
    pub text: String,
    /// `drone : Drone`
    pub subject: Option<String>,
    pub satisfied_by: Vec<String>,
    pub verified_by: Vec<String>,
    pub status: ReqStatus,
    /// The compared values of the worst check, e.g. `6.2 kg <= 5 kg`.
    pub detail: String,
    /// Requirements nested in other requirements are indented.
    pub depth: usize,
    /// The file it is written in (an index into the project's sources).
    pub source: usize,
    /// File name, when it is not the file being edited.
    pub file: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    Satisfy,
    Verify,
    /// A dependency or derivation between the element and the requirement.
    Dependency,
}

impl TraceKind {
    pub fn letter(self) -> &'static str {
        match self {
            TraceKind::Satisfy => "S",
            TraceKind::Verify => "V",
            TraceKind::Dependency => "D",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
    pub kind: TraceKind,
    pub verdict: Option<Verdict>,
}

#[derive(Clone, Debug)]
pub struct TraceColumn {
    pub qualified_name: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Requirements {
    pub rows: Vec<Requirement>,
    pub columns: Vec<TraceColumn>,
    pub cells: Vec<Cell>,
}

/// One `doc` paragraph on a single line, without the `*` margins of block comments.
pub fn one_line(text: &str) -> String {
    text.lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn short(q: &str) -> String {
    q.rsplit("::").next().unwrap_or(q).to_string()
}

fn is_requirement(index: &Index, id: EntryId) -> bool {
    match &index.entry(id).member.kind {
        MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::Requirement,
        MemberKind::Usage(u) => u.kind == UsageKind::Requirement && u.prefix.role.is_none() && u.connector.is_none(),
        _ => false,
    }
}

fn doc_of(member: &Member) -> Option<String> {
    member.kind.body().iter().find_map(|m| match &m.kind {
        MemberKind::Comment(c) if c.kind == CommentKind::Doc => Some(one_line(&c.text)).filter(|t| !t.is_empty()),
        _ => None,
    })
}

fn subject_of(member: &Member) -> Option<String> {
    member.kind.body().iter().find_map(|m| match &m.kind {
        MemberKind::Usage(u) if u.kind == UsageKind::Subject => {
            let name = m.kind.effective_name().unwrap_or("subject");
            Some(match u.typed_by.first() {
                Some(t) => format!("{name} : {}", t.name.last().text),
                None => name.to_string(),
            })
        }
        _ => None,
    })
}

/// The definitions behind a requirement, most specific first.
fn types_of(index: &Index, id: EntryId) -> Vec<EntryId> {
    let mut out = Vec::new();
    let mut queue = vec![id];
    while let Some(i) = queue.pop() {
        if out.contains(&i) || out.len() > 16 {
            continue;
        }
        out.push(i);
        let e = index.entry(i);
        match &e.member.kind {
            MemberKind::Usage(u) => queue.extend(u.typed_by.iter().filter_map(|t| index.resolve(e.parent, &t.name))),
            MemberKind::Definition(d) => queue.extend(d.specializes.iter().filter_map(|b| index.resolve(e.parent, b))),
            _ => {}
        }
    }
    out
}

fn rank(v: Verdict) -> u8 {
    match v {
        Verdict::Satisfied => 0,
        Verdict::Unknown => 1,
        Verdict::Error => 2,
        Verdict::Violated => 3,
    }
}

/// Visits every member of `models` with the entry whose body it is in.
pub(crate) fn walk<'m>(index: &Index<'m>, models: &[&'m Model], f: &mut impl FnMut(&'m Member, Option<EntryId>)) {
    for model in models {
        let mut stack: Vec<(&'m [Member], Option<EntryId>)> = vec![(&model.members, None)];
        while let Some((members, scope)) = stack.pop() {
            for m in members {
                f(m, scope);
                let inner = index.id_of(m).or(scope);
                stack.push((m.kind.body(), inner));
            }
        }
    }
}

/// A CSV field, quoted when it holds a comma, quote or line break.
pub(crate) fn csv_quote(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

impl Requirements {
    /// Requirements of the entries `source_of` maps to a project file
    /// (`None`: a library, left out), traced through `models`; verdicts come
    /// from the `satisfy` checks of the evaluator.
    pub fn build<'m>(index: &Index<'m>, models: &[&'m Model], checks: &[Check], source_of: impl Fn(EntryId) -> Option<usize>) -> Self {
        let mut out = Requirements::default();
        let mut row_of: HashMap<EntryId, usize> = HashMap::new();
        for (id, e) in index.entries().iter().enumerate() {
            let Some(source) = source_of(id) else { continue };
            if !is_requirement(index, id) {
                continue;
            }
            let types = types_of(index, id);
            let id_name = |i: EntryId| index.entry(i).member.kind.identification().and_then(|n| n.short_name.as_ref()).map(|n| n.text.clone());
            let mut depth = 0;
            let mut p = e.parent;
            while let Some(q) = p {
                if is_requirement(index, q) {
                    depth += 1;
                }
                p = index.entry(q).parent;
            }
            let is_def = matches!(e.member.kind, MemberKind::Definition(_));
            row_of.insert(id, out.rows.len());
            out.rows.push(Requirement {
                qualified_name: e.qualified_name.clone(),
                id: id_name(id).unwrap_or_default(),
                name: e.member.kind.name().map(|n| n.text.clone()).unwrap_or_else(|| e.name.to_string()),
                is_def,
                ty: types.get(1).map(|&t| short(&index.entry(t).qualified_name)),
                text: types.iter().find_map(|&t| doc_of(index.entry(t).member)).unwrap_or_default(),
                subject: types.iter().find_map(|&t| subject_of(index.entry(t).member)),
                satisfied_by: Vec::new(),
                verified_by: Vec::new(),
                status: if is_def { ReqStatus::Definition } else { ReqStatus::Open },
                detail: String::new(),
                depth,
                source,
                file: None,
            });
        }

        let mut col_of: HashMap<EntryId, usize> = HashMap::new();
        let mut links: Vec<(usize, EntryId, TraceKind)> = Vec::new();
        walk(index, models, &mut |m, scope| {
            match &m.kind {
                MemberKind::Usage(u) if matches!(u.prefix.role, Some(Role::Satisfy | Role::Verify)) => {
                    let Some(req) = u.references.first().and_then(|r| index.resolve_reference(scope, r)) else { return };
                    let Some(&row) = row_of.get(&req) else { return };
                    if u.prefix.role == Some(Role::Satisfy) {
                        // `satisfy R;` inside a part: the part satisfies it.
                        let by = match &u.by {
                            Some(r) => index.resolve_reference(scope, r),
                            None => scope,
                        };
                        if let Some(by) = by {
                            links.push((row, by, TraceKind::Satisfy));
                        }
                    } else {
                        // `verify R;` in a verification case's objective: the case verifies it.
                        let mut v = scope;
                        while let Some(i) = v {
                            let case = match &index.entry(i).member.kind {
                                MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::VerificationCase,
                                MemberKind::Usage(u) => u.kind == UsageKind::VerificationCase,
                                _ => false,
                            };
                            if case {
                                break;
                            }
                            v = index.entry(i).parent;
                        }
                        if let Some(v) = v.or(scope) {
                            links.push((row, v, TraceKind::Verify));
                        }
                    }
                }
                MemberKind::Dependency(d) => {
                    for c in &d.clients {
                        for s in &d.suppliers {
                            let (Some(c), Some(s)) = (index.resolve_reference(scope, c), index.resolve_reference(scope, s)) else { continue };
                            // The requirement is the row; a requirement derived from another is the client.
                            match (row_of.get(&c), row_of.get(&s)) {
                                (Some(&row), _) => links.push((row, s, TraceKind::Dependency)),
                                (None, Some(&row)) => links.push((row, c, TraceKind::Dependency)),
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        });

        let verdicts: HashMap<(&str, &str), Verdict> = {
            let mut v: HashMap<(&str, &str), Verdict> = HashMap::new();
            for c in checks {
                let e = v.entry((c.requirement.as_str(), c.subject.as_str())).or_insert(c.verdict);
                if rank(c.verdict) > rank(*e) {
                    *e = c.verdict;
                }
            }
            v
        };
        for (row, other, kind) in links {
            let col = *col_of.entry(other).or_insert_with(|| {
                let q = index.entry(other).qualified_name.clone();
                out.columns.push(TraceColumn { name: short(&q), qualified_name: q });
                out.columns.len() - 1
            });
            let other_name = short(&index.entry(other).qualified_name);
            let req = &mut out.rows[row];
            let verdict = (kind == TraceKind::Satisfy)
                .then(|| verdicts.get(&(req.qualified_name.as_str(), index.entry(other).qualified_name.as_str())).copied())
                .flatten();
            match kind {
                TraceKind::Satisfy if !req.satisfied_by.contains(&other_name) => req.satisfied_by.push(other_name),
                TraceKind::Verify if !req.verified_by.contains(&other_name) => req.verified_by.push(other_name),
                _ => {}
            }
            if !out.cells.iter().any(|c| c.row == row && c.col == col && c.kind == kind) {
                out.cells.push(Cell { row, col, kind, verdict });
            }
        }

        // Status: the worst verdict of the checks on each requirement.
        for req in &mut out.rows {
            let worst = checks.iter().filter(|c| c.requirement == req.qualified_name).max_by_key(|c| rank(c.verdict));
            match worst {
                Some(c) => {
                    req.status = match c.verdict {
                        Verdict::Satisfied => ReqStatus::Satisfied,
                        Verdict::Violated => ReqStatus::Violated,
                        Verdict::Unknown => ReqStatus::Undecided,
                        Verdict::Error => ReqStatus::CannotCheck,
                    };
                    req.detail = c.detail.clone();
                }
                None if !req.is_def && !req.satisfied_by.is_empty() => req.status = ReqStatus::Undecided,
                None => {}
            }
        }
        out
    }

    pub fn cell(&self, row: usize, col: usize) -> impl Iterator<Item = &Cell> {
        self.cells.iter().filter(move |c| c.row == row && c.col == col)
    }

    /// A requirement usage nothing satisfies, verifies or derives from.
    pub fn untraced(&self, row: usize) -> bool {
        !self.rows[row].is_def && !self.cells.iter().any(|c| c.row == row)
    }

    /// The table as CSV (RFC 4180 quoting), for spreadsheets and requirement tools.
    pub fn to_csv(&self) -> String {
        let q = |s: &String| csv_quote(s);
        let mut out = String::from("Id,Name,Kind,Definition,Text,Subject,Satisfied by,Verified by,Status,Detail,Qualified name\n");
        for r in &self.rows {
            let fields = [
                r.id.clone(),
                r.name.clone(),
                if r.is_def { "requirement def".into() } else { "requirement".into() },
                r.ty.clone().unwrap_or_default(),
                r.text.clone(),
                r.subject.clone().unwrap_or_default(),
                r.satisfied_by.join("; "),
                r.verified_by.join("; "),
                r.status.label().to_string(),
                r.detail.clone(),
                r.qualified_name.clone(),
            ];
            out += &fields.iter().map(q).collect::<Vec<_>>().join(",");
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../sysml/tests/data/drone.sysml");

    fn build(src: &str) -> Requirements {
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let report = sysml::eval::evaluate(&model, &index);
        Requirements::build(&index, &[&model], &report.checks, |_| Some(0))
    }

    #[test]
    fn sample_requirements_with_status_and_trace() {
        let reqs = build(SAMPLE);
        let def = reqs.rows.iter().find(|r| r.name == "MassLimit").unwrap();
        assert!(def.is_def);
        assert_eq!(def.id, "R1");
        assert_eq!(def.text, "The drone shall weigh at most 5 kg.");
        assert_eq!(def.subject.as_deref(), Some("drone : Drone"));
        let usage = reqs.rows.iter().find(|r| r.name == "massReq").unwrap();
        assert_eq!(usage.ty.as_deref(), Some("MassLimit"));
        assert_eq!(usage.text, def.text, "the usage takes the definition's text");
        assert_eq!(usage.satisfied_by, ["drone1"]);
        assert_ne!(usage.status, ReqStatus::Open);
        // massReq × drone1 is a satisfy cell; MassLimit has a dependency from `weight`?
        let col = reqs.columns.iter().position(|c| c.name == "drone1").unwrap();
        let row = reqs.rows.iter().position(|r| r.name == "massReq").unwrap();
        assert_eq!(reqs.cell(row, col).next().unwrap().kind, TraceKind::Satisfy);
        let csv = reqs.to_csv();
        assert!(csv.lines().count() == reqs.rows.len() + 1);
        assert!(csv.contains("R1,MassLimit,requirement def"), "{csv}");
    }

    #[test]
    fn verification_and_open_requirements() {
        let src = "package P {
            part def Car { attribute mass = 900; }
            part car : Car;
            requirement def <'R2'> Light { doc /* Light enough. */ subject c : Car; require constraint { c.mass < 1000 } }
            requirement light : Light;
            requirement fast { doc /* Fast,
                               * really fast. */ }
            satisfy light by car;
            verification def WeighIt {
                subject c : Car;
                objective { verify light; }
            }
            dependency from fast to light;
        }";
        let reqs = build(src);
        let light = reqs.rows.iter().find(|r| r.name == "light").unwrap();
        assert_eq!(light.status, ReqStatus::Satisfied, "{}", light.detail);
        assert_eq!(light.verified_by, ["WeighIt"]);
        let fast = reqs.rows.iter().position(|r| r.name == "fast").unwrap();
        assert_eq!(reqs.rows[fast].text, "Fast, really fast.");
        assert_eq!(reqs.rows[fast].status, ReqStatus::Open);
        // `fast` depends on `light`: traced, though not satisfied.
        assert!(!reqs.untraced(fast));
        assert!(reqs.cells.iter().any(|c| c.row == fast && c.kind == TraceKind::Dependency));
    }
}
