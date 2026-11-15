//! Model checks: findings beyond syntax and name errors, the kind of thing a
//! reviewer would point out. Each finding names the element it is about, so
//! the checks list can show it in the code.
//!
//! * ports of inner parts that nothing connects to,
//! * connections between ports of different definitions, or whose ends
//!   both send or both receive (one end is usually conjugated, `~T`),
//! * connector and transition ends that name nothing shown,
//! * requirements nothing satisfies or verifies, and verdicts that are
//!   violated or cannot be decided (with the value that is missing),
//! * actions nothing performs or allocates,
//! * definitions nothing uses.

use crate::allocations::Allocations;
use crate::ibd::{End, Ibd, LinkKind};
use crate::requirements::{ReqStatus, Requirements, walk};
use crate::states::StateMachine;
use std::collections::HashSet;
use sysml::{Direction, EntryId, Index, MemberKind, Model, UsageKind};

/// Interconnection and state views checked, at most (the best candidates).
const MAX_VIEWS: usize = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Info,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    PortTypes,
    PortDirections,
    UnconnectedPort,
    UnresolvedEnd,
    RequirementViolated,
    RequirementUndecided,
    RequirementNotSatisfied,
    RequirementNotVerified,
    Unallocated,
    Unused,
}

impl Rule {
    pub fn label(self) -> &'static str {
        match self {
            Rule::PortTypes => "Port types differ",
            Rule::PortDirections => "Port directions",
            Rule::UnconnectedPort => "Unconnected port",
            Rule::UnresolvedEnd => "End not found",
            Rule::RequirementViolated => "Violated",
            Rule::RequirementUndecided => "Undecided",
            Rule::RequirementNotSatisfied => "Not satisfied",
            Rule::RequirementNotVerified => "Not verified",
            Rule::Unallocated => "Unallocated",
            Rule::Unused => "Unused definition",
        }
    }

    fn severity(self) -> Severity {
        match self {
            Rule::PortTypes | Rule::PortDirections | Rule::UnresolvedEnd | Rule::RequirementViolated => Severity::Warning,
            _ => Severity::Info,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: Rule,
    pub severity: Severity,
    /// Qualified name of the element it is about.
    pub element: String,
    pub message: String,
    /// Where the element is declared: file (0: the edited one) and offset.
    pub source: usize,
    pub offset: usize,
    pub file: Option<String>,
}

pub struct Input<'a, 'm> {
    pub index: &'a Index<'m>,
    pub models: &'a [&'m Model],
    pub requirements: &'a Requirements,
    pub allocations: &'a Allocations,
    /// Project file of an entry (`None`: the library, not checked).
    pub source_of: &'a dyn Fn(EntryId) -> Option<usize>,
}

struct Out<'a, 'm> {
    index: &'a Index<'m>,
    findings: Vec<Finding>,
    seen: HashSet<(Rule, String, String)>,
}

impl Out<'_, '_> {
    fn add(&mut self, rule: Rule, element: &str, message: String) {
        if !self.seen.insert((rule, element.to_string(), message.clone())) {
            return;
        }
        let (source, offset) = match self.index.get(element) {
            Some(id) => (self.index.entry(id).source, self.index.entry(id).member.span.start),
            None => (0, 0),
        };
        self.findings.push(Finding { rule, severity: rule.severity(), element: element.to_string(), message, source, offset, file: None });
    }
}

fn short(q: &str) -> &str {
    q.rsplit("::").next().unwrap_or(q)
}

pub fn run(input: &Input) -> Vec<Finding> {
    let index = input.index;
    let source_of = input.source_of;
    let in_project = |id: EntryId| source_of(id).is_some();
    let mut out = Out { index, findings: Vec::new(), seen: HashSet::new() };

    // ---- connections, from the interconnection view of each part ----
    // A part definition and the parts typed by it show the same ports: a port
    // is unconnected only if no view connects it.
    let mut ports: Vec<(String, String, bool)> = Vec::new();
    for root in crate::ibd::candidates(index, in_project).iter().take(MAX_VIEWS) {
        let Some(id) = index.get(root) else { continue };
        let ibd = Ibd::build(index, id);
        check_ibd(&mut out, &ibd, root, &mut ports);
    }
    for (port, message, linked) in ports {
        if !linked {
            out.add(Rule::UnconnectedPort, &port, message);
        }
    }

    // ---- state machines ----
    for root in crate::states::candidates(index, in_project).iter().take(MAX_VIEWS) {
        let Some(id) = index.get(root) else { continue };
        let sm = StateMachine::build(index, id);
        if sm.unresolved > 0 {
            let n = sm.unresolved;
            out.add(Rule::UnresolvedEnd, root, format!("{n} transition {} name no state of {}", if n == 1 { "end" } else { "ends" }, short(root)));
        }
    }

    for root in crate::actions::candidates(index, in_project).iter().take(MAX_VIEWS) {
        let Some(id) = index.get(root) else { continue };
        let flow = crate::actions::build(index, id);
        if flow.unresolved > 0 {
            let n = flow.unresolved;
            out.add(Rule::UnresolvedEnd, root, format!("{n} succession or flow {} name no step of {}", if n == 1 { "end" } else { "ends" }, short(root)));
        }
    }

    // ---- requirements ----
    for r in input.requirements.rows.iter().filter(|r| !r.is_def) {
        match r.status {
            ReqStatus::Violated | ReqStatus::CannotCheck => {
                let detail = if r.detail.is_empty() { String::new() } else { format!(": {}", r.detail) };
                out.add(Rule::RequirementViolated, &r.qualified_name, format!("{} is not met{detail}", r.name));
            }
            ReqStatus::Undecided => {
                let detail = if r.detail.is_empty() { "a value it needs is missing".to_string() } else { r.detail.clone() };
                out.add(Rule::RequirementUndecided, &r.qualified_name, format!("{} cannot be decided: {detail}", r.name));
            }
            _ => {}
        }
        if r.satisfied_by.is_empty() {
            out.add(Rule::RequirementNotSatisfied, &r.qualified_name, format!("nothing satisfies {} (`satisfy {} by …;`)", r.name, r.name));
        }
        if r.verified_by.is_empty() {
            out.add(Rule::RequirementNotVerified, &r.qualified_name, format!("no verification case verifies {}", r.name));
        }
    }

    // ---- allocation ----
    let alloc = input.allocations;
    for (i, row) in alloc.rows.iter().enumerate() {
        if alloc.unallocated(i) && row.keyword == "action def" {
            out.add(Rule::Unallocated, &row.qualified_name, format!("no part performs {} or has it allocated", row.name));
        }
    }

    // ---- definitions nothing uses ----
    let mut used: HashSet<EntryId> = HashSet::new();
    walk(index, input.models, &mut |m, scope| {
        let names: Vec<&sysml::QualifiedName> = match &m.kind {
            MemberKind::Usage(u) => u
                .typed_by
                .iter()
                .map(|t| &t.name)
                .chain(u.connector.as_ref().and_then(|c| c.item_type.as_ref()).map(|t| &t.name))
                .chain(u.references.iter().flat_map(|r| r.chain.first()))
                .collect(),
            MemberKind::Definition(d) => d.specializes.iter().collect(),
            _ => Vec::new(),
        };
        // Names in a body resolve from the element itself.
        let at = index.id_of(m).map(|id| index.entry(id).parent).unwrap_or(scope);
        for n in names.into_iter().chain(&m.metadata) {
            if let Some(t) = index.resolve(at, n) {
                used.insert(t);
            }
        }
    });
    for (id, e) in index.entries().iter().enumerate() {
        if !in_project(id) || used.contains(&id) {
            continue;
        }
        let MemberKind::Definition(d) = &e.member.kind else { continue };
        let checked = matches!(d.kind.usage_kind(), UsageKind::Part | UsageKind::Port | UsageKind::Item | UsageKind::Interface | UsageKind::Connection | UsageKind::State);
        if checked {
            out.add(Rule::Unused, &e.qualified_name, format!("{} def {} is not used: nothing is typed by or specializes it", d.kind, e.name));
        }
    }

    let mut findings = out.findings;
    findings.sort_by_key(|f| (f.severity, f.rule, f.source, f.offset));
    findings
}

pub fn to_csv(findings: &[Finding]) -> String {
    let q = crate::requirements::csv_quote;
    let mut out = String::from("Severity,Check,Element,Message,File\n");
    for f in findings {
        let fields = [f.severity.label(), f.rule.label(), &f.element, &f.message, f.file.as_deref().unwrap_or("")];
        out += &fields.iter().map(|s| q(s)).collect::<Vec<_>>().join(",");
        out.push('\n');
    }
    out
}

/// Connection findings of one interconnection view; the ports of its inner
/// parts go to `ports` (declaration, message, connected anywhere yet).
fn check_ibd(out: &mut Out, ibd: &Ibd, root: &str, ports: &mut Vec<(String, String, bool)>) {
    if ibd.unresolved > 0 {
        let n = ibd.unresolved;
        out.add(Rule::UnresolvedEnd, root, format!("{n} connector {} in {} name nothing found", if n == 1 { "end" } else { "ends" }, short(root)));
    }
    for link in &ibd.links {
        if link.kind == LinkKind::Binding {
            continue;
        }
        let (End::Port(a), End::Port(b)) = (link.from, link.to) else { continue };
        let (pa, pb) = (&ibd.ports[a], &ibd.ports[b]);
        let what = format!("{} → {}", pa.name, pb.name);
        if let (Some(ta), Some(tb)) = (&pa.type_name, &pb.type_name)
            && ta != tb
        {
            out.add(Rule::PortTypes, &pa.qualified_name, format!("{what}: {} and {} are different port definitions", short(ta), short(tb)));
        }
        match (pa.direction, pb.direction) {
            (Some(Direction::Out), Some(Direction::Out)) | (Some(Direction::In), Some(Direction::In)) => {
                let dir = if pa.direction == Some(Direction::Out) { "send" } else { "receive" };
                let ty = pb.type_name.as_deref().map_or("T", short);
                out.add(Rule::PortDirections, &pa.qualified_name, format!("{what}: both ends {dir}; type one end `~{ty}`"));
            }
            _ => {}
        }
    }
    // Ports of inner parts (the root's own ports connect outwards).
    let mut linked: HashSet<usize> = HashSet::new();
    for link in &ibd.links {
        for end in [link.from, link.to] {
            if let End::Port(p) = end {
                linked.insert(p);
            }
        }
    }
    for (i, port) in ibd.ports.iter().enumerate() {
        if port.owner == 0 {
            continue;
        }
        let connected = linked.contains(&i);
        match ports.iter_mut().find(|p| p.0 == port.qualified_name) {
            Some(p) => p.2 |= connected,
            None => {
                let owner = &ibd.parts[port.owner];
                let owner_name = owner.title.split(" : ").next().unwrap_or(&owner.title);
                ports.push((port.qualified_name.clone(), format!("{} of {owner_name} is not connected to anything", port.name), connected));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(src: &str) -> Vec<Finding> {
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let reqs = Requirements::build(&index, &[&model], &sysml::eval::evaluate(&model, &index).checks, |_| Some(0));
        let alloc = Allocations::build(&index, &[&model], |_| Some(0));
        let source_of = |_| Some(0);
        run(&Input { index: &index, models: &[&model], requirements: &reqs, allocations: &alloc, source_of: &source_of })
    }

    fn has(f: &[Finding], rule: Rule, element: &str) -> bool {
        f.iter().any(|x| x.rule == rule && x.element == element)
    }

    #[test]
    fn finds_port_and_connection_problems() {
        let f = findings(
            "package P {
                port def Power { out item p; }
                port def Data { out item d; }
                part def A { port out1 : Power; port spare : Power; }
                part def B { port in1 : ~Power; port in2 : Power; port in3 : Data; }
                part def Sys {
                    part a : A;
                    part b : B;
                    connect a.out1 to b.in1;
                    connect a.out1 to b.in2;
                    connect a.out1 to b.in3;
                    connect a.out1 to b.nothing.deeper;
                }
                part def Lonely;
                part sys : Sys;
            }",
        );
        assert!(has(&f, Rule::UnconnectedPort, "P::A::spare"), "{f:#?}");
        assert!(!has(&f, Rule::UnconnectedPort, "P::A::out1"));
        assert!(f.iter().any(|x| x.rule == Rule::PortDirections && x.message.contains("out1 → in2")), "{f:#?}");
        assert!(!f.iter().any(|x| x.rule == Rule::PortDirections && x.message.contains("out1 → in1")));
        assert!(f.iter().any(|x| x.rule == Rule::PortTypes && x.message.contains("out1 → in3")), "{f:#?}");
        assert!(has(&f, Rule::Unused, "P::Lonely"));
        assert!(!has(&f, Rule::Unused, "P::Power"));
        assert_eq!(f[0].severity, Severity::Warning, "warnings first");
        // `Sys` and `sys : Sys` show the same connections: each finding once.
        assert_eq!(f.iter().filter(|x| x.rule == Rule::PortDirections).count(), 2, "out1 → in2 and out1 → in3: {f:#?}");
        assert_eq!(f.iter().filter(|x| x.rule == Rule::UnconnectedPort).count(), 1, "{f:#?}");
    }

    #[test]
    fn sample_model_findings() {
        let f = findings(include_str!("../sysml/tests/data/drone.sysml"));
        // The undecided verdict names the value that is missing.
        let undecided = f.iter().find(|x| x.rule == Rule::RequirementUndecided).expect("massReq is undecided");
        assert_eq!(undecided.element, "DroneSystem::massReq");
        assert!(undecided.message.contains("drone1.motors.mass has no value"), "{}", undecided.message);
        assert!(has(&f, Rule::RequirementNotVerified, "DroneSystem::massReq"));
        assert!(!has(&f, Rule::RequirementNotSatisfied, "DroneSystem::massReq"));
        // Deliver is performed by the drone; the power link is consistent.
        assert!(!has(&f, Rule::Unallocated, "DroneSystem::Deliver"));
        assert!(!f.iter().any(|x| matches!(x.rule, Rule::PortTypes | Rule::PortDirections)), "{f:#?}");
        assert!(!has(&f, Rule::Unused, "DroneSystem::Motor"));
    }
}
