//! Action flow view: the steps of one action definition or action usage and
//! the order they run in. Built as a [`StateMachine`] graph (actions are its
//! states) so it shares the layout, routing and drawing of the state view.
//!
//! * Steps: sub-actions (`action a;`, `perform action p : P;`), `accept` and
//!   `send` actions, and the control nodes `fork`, `join`, `decide`, `merge`.
//! * Control flow: `first a then b;`, `then b;` after a step (from the step
//!   before it), `first start;` and `then done;` (the initial and final
//!   nodes), and the `if g then a;` / `else b;` branches of a decision.
//! * Object flow: `flow [of T] from a.x to b.y;`, drawn between the steps
//!   owning the two ends.
//!
//! Steps written in place with steps of their own (`action fly { ... }`) are
//! shown nested; steps typed by a definition (`action fly : Fly;`) are not
//! expanded: double-click them to open the definition's own flow.

use crate::ibd::layers;
use crate::states::{State, StateKind, StateMachine, Transition, clip, path_of, transition_label};
use std::collections::HashMap;
use sysml::{ConnectorKind, EntryId, Index, Member, MemberKind, TriggerKind, UsageKind};

/// Nesting levels expanded below the root.
const MAX_DEPTH: usize = 3;
const MAX_NODES: usize = 400;
/// Parameters listed on a step.
const MAX_PARAMS: usize = 6;

fn unwrap_then(m: &Member) -> &Member {
    match &m.kind {
        MemberKind::Then(inner) => unwrap_then(inner),
        _ => m,
    }
}

/// What a body member is in the flow, if it is a step.
fn step_kind(m: &Member) -> Option<StateKind> {
    match &m.kind {
        MemberKind::Usage(u) if u.connector.is_none() => match u.kind {
            UsageKind::Action => Some(StateKind::State),
            UsageKind::Fork => Some(StateKind::Fork),
            UsageKind::Join => Some(StateKind::Join),
            UsageKind::Decide => Some(StateKind::Decision),
            UsageKind::Merge => Some(StateKind::Merge),
            _ => None,
        },
        MemberKind::Accept(_) => Some(StateKind::Accept),
        MemberKind::Send(_) => Some(StateKind::Send),
        _ => None,
    }
}

fn is_succession(m: &Member) -> bool {
    matches!(&m.kind, MemberKind::Usage(u) if u.connector.as_ref().is_some_and(|c| c.kind == ConnectorKind::Succession))
}

fn is_action_element(m: &Member) -> bool {
    match &m.kind {
        MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::Action,
        MemberKind::Usage(u) => u.kind == UsageKind::Action && u.connector.is_none(),
        _ => false,
    }
}

/// Steps and successions over the bodies of `layers`.
fn flow_count(index: &Index, layers: &[EntryId]) -> usize {
    layers
        .iter()
        .flat_map(|&l| index.entry(l).member.kind.body())
        .filter(|m| {
            let m = unwrap_then(m);
            step_kind(m).is_some() || is_succession(m)
        })
        .count()
}

/// Elements worth an action flow view, most steps first: action
/// definitions, and action usages not nested in another action (such as
/// `perform action` in a part). Only `in_source` entries are offered.
pub fn candidates(index: &Index, in_source: impl Fn(EntryId) -> bool) -> Vec<String> {
    let mut scored: Vec<(usize, bool, String)> = Vec::new();
    for (id, e) in index.entries().iter().enumerate() {
        if !in_source(id) || !is_action_element(e.member) {
            continue;
        }
        if matches!(e.member.kind, MemberKind::Usage(_)) && e.parent.is_some_and(|p| is_action_element(index.entry(p).member)) {
            continue;
        }
        let n = flow_count(index, &layers(index, &[id]));
        if n > 0 {
            scored.push((n, matches!(e.member.kind, MemberKind::Definition(_)), e.qualified_name.clone()));
        }
    }
    scored.sort_by_key(|s| std::cmp::Reverse((s.0, s.1)));
    scored.into_iter().map(|(_, _, q)| q).collect()
}

/// A flow end before resolution: a step already known, or a path to look up.
#[derive(Clone)]
enum End {
    Node(usize),
    Path(Vec<String>),
}

struct Pending {
    scope: usize,
    from: End,
    to: End,
    label: Option<String>,
    qualified_name: Option<String>,
    offset: Option<usize>,
    flow: bool,
}

struct Builder<'a, 'm> {
    index: &'a Index<'m>,
    sm: StateMachine,
    pending: Vec<Pending>,
}

fn param_line(m: &Member) -> Option<String> {
    let MemberKind::Usage(u) = &m.kind else { return None };
    let dir = match u.prefix.direction? {
        sysml::Direction::In => "in",
        sysml::Direction::Out => "out",
        sysml::Direction::InOut => "inout",
    };
    let name = m.kind.effective_name()?;
    Some(match u.typed_by.first() {
        Some(t) => format!("{dir} {name} : {}", t.name.last().text),
        None => format!("{dir} {name}"),
    })
}

impl Builder<'_, '_> {
    fn push(&mut self, parent: Option<usize>, kind: StateKind, name: String, title: String) -> usize {
        let id = self.sm.states.len();
        let depth = parent.map_or(0, |p| self.sm.states[p].depth + 1);
        self.sm.states.push(State {
            name,
            title,
            keyword: String::new(),
            kind,
            parallel: false,
            parent,
            children: Vec::new(),
            depth,
            actions: Vec::new(),
            qualified_name: None,
            type_name: None,
            truncated: false,
            rect: [0.0; 4],
            header: 0.0,
        });
        if let Some(p) = parent {
            self.sm.states[p].children.push(id);
        }
        id
    }

    /// A named step (or the root) declared by `group`, with its own steps inside.
    fn add_step(&mut self, parent: Option<usize>, group: &[EntryId], kind: StateKind) -> usize {
        let index = self.index;
        let first = index.entry(group[0]);
        let ty = group.iter().find_map(|&id| match &index.entry(id).member.kind {
            MemberKind::Usage(u) => u.typed_by.first(),
            _ => None,
        });
        let title = if kind.is_symbol() {
            String::new()
        } else {
            match ty {
                Some(t) => format!("{} : {}", first.name, t.name.last().text),
                None => first.name.to_string(),
            }
        };
        let me = self.push(parent, kind, first.name.to_string(), title);
        let st = &mut self.sm.states[me];
        st.qualified_name = Some(first.qualified_name.clone());
        st.keyword = match &first.member.kind {
            MemberKind::Definition(d) => format!("{} def", d.kind),
            MemberKind::Usage(u) => match u.prefix.role {
                Some(r) => format!("{} {}", r.keyword(), u.kind.keyword()),
                None => u.kind.keyword().to_string(),
            },
            _ => "action".to_string(),
        };
        st.type_name = group.iter().find_map(|&id| {
            let e = index.entry(id);
            let MemberKind::Usage(u) = &e.member.kind else { return None };
            let t = u.typed_by.first()?;
            index.resolve(e.parent, &t.name).map(|t| index.entry(t).qualified_name.clone())
        });
        if kind != StateKind::State {
            return me;
        }

        // Parameters, from the step and its types.
        let all = layers(index, group);
        let mut params: Vec<String> = Vec::new();
        for &l in &all {
            for m in index.entry(l).member.kind.body() {
                if let Some(p) = param_line(m)
                    && !params.iter().any(|q| q.split(' ').nth(1) == p.split(' ').nth(1))
                {
                    params.push(p);
                }
            }
        }
        if params.len() > MAX_PARAMS {
            params.truncate(MAX_PARAMS - 1);
            params.push("…".to_string());
        }
        self.sm.states[me].actions = params;

        // The root shows the steps of its definitions too; a nested step only
        // its own (its definition has a view of its own).
        let steps: Vec<EntryId> = if parent.is_none() { all } else { group.to_vec() };
        self.add_steps(me, &steps);
        me
    }

    fn add_steps(&mut self, me: usize, layers: &[EntryId]) {
        let index = self.index;
        let depth = self.sm.states[me].depth;
        // Named steps, by name over every layer (redefinitions merge).
        let mut groups: Vec<(String, StateKind, Vec<EntryId>)> = Vec::new();
        for &l in layers {
            for outer in index.entry(l).member.kind.body() {
                // The index knows `then action a;` by the `then` member.
                let m = unwrap_then(outer);
                let Some(kind) = step_kind(m) else { continue };
                let (Some(name), Some(id)) = (m.kind.effective_name(), index.id_of(m).or(index.id_of(outer))) else { continue };
                match groups.iter_mut().find(|(n, _, _)| n == name) {
                    Some((_, _, ids)) => ids.push(id),
                    None => groups.push((name.to_string(), kind, vec![id])),
                }
            }
        }
        if depth >= MAX_DEPTH && groups.iter().any(|g| g.1 == StateKind::State) {
            self.sm.states[me].truncated = true;
            return;
        }
        let mut by_name: HashMap<String, usize> = HashMap::new();
        for (name, kind, ids) in &groups {
            if self.sm.states.len() >= MAX_NODES {
                self.sm.states[me].truncated = true;
                break;
            }
            let child = self.add_step(Some(me), ids, *kind);
            by_name.insert(name.clone(), child);
        }

        // Control and object flow, in declaration order.
        for &l in layers {
            let local = index.entry(l).source == 0;
            let mut prev: Option<End> = None;
            for member in index.entry(l).member.kind.body() {
                let is_then = matches!(member.kind, MemberKind::Then(_));
                let m = unwrap_then(member);
                let offset = local.then_some(m.span.start);
                let qualified_name = || m.kind.name().and(index.id_of(m).or(index.id_of(member))).map(|id| index.entry(id).qualified_name.clone());
                if let Some(kind) = step_kind(m) {
                    let node = match m.kind.effective_name().and_then(|n| by_name.get(n)) {
                        Some(&n) => n,
                        None => {
                            let title = match &m.kind {
                                MemberKind::Accept(a) => match &a.trigger.kind {
                                    TriggerKind::Signal { ty: Some(t), .. } => format!("accept {}", t.name.last().text),
                                    TriggerKind::Signal { name: Some(n), .. } => format!("accept {}", n.text),
                                    TriggerKind::When(e) => format!("accept when {e}"),
                                    TriggerKind::At(e) => format!("accept at {e}"),
                                    TriggerKind::After(e) => format!("accept after {e}"),
                                    _ => "accept".to_string(),
                                },
                                MemberKind::Send(s) => format!("send {}", s.payload),
                                _ => "action".to_string(),
                            };
                            let n = self.push(Some(me), kind, String::new(), clip(title));
                            self.sm.states[n].keyword = match kind {
                                StateKind::Accept => "accept".into(),
                                StateKind::Send => "send".into(),
                                _ => "action".into(),
                            };
                            n
                        }
                    };
                    if is_then && let Some(p) = prev.clone() {
                        self.pending.push(Pending { scope: me, from: p, to: End::Node(node), label: None, qualified_name: None, offset, flow: false });
                    }
                    prev = Some(End::Node(node));
                    continue;
                }
                match &m.kind {
                    MemberKind::Usage(u) => {
                        let Some(c) = &u.connector else { continue };
                        match c.kind {
                            ConnectorKind::Succession => {
                                if c.implicit_source {
                                    let Some(end) = c.ends.last() else { continue };
                                    let to = End::Path(path_of(&end.target));
                                    if let Some(p) = prev.clone() {
                                        self.pending.push(Pending { scope: me, from: p, to: to.clone(), label: None, qualified_name: qualified_name(), offset, flow: false });
                                    }
                                    // Each `then` after a fork starts from the fork.
                                    let fork = matches!(prev, Some(End::Node(n)) if self.sm.states[n].kind == StateKind::Fork);
                                    if !fork {
                                        prev = Some(to);
                                    }
                                } else if c.ends.len() == 1 {
                                    // `first start;`: the next `then` starts here.
                                    prev = Some(End::Path(path_of(&c.ends[0].target)));
                                } else if c.ends.len() >= 2 {
                                    let to = End::Path(path_of(&c.ends[1].target));
                                    self.pending.push(Pending {
                                        scope: me,
                                        from: End::Path(path_of(&c.ends[0].target)),
                                        to: to.clone(),
                                        label: None,
                                        qualified_name: qualified_name(),
                                        offset,
                                        flow: false,
                                    });
                                    prev = Some(to);
                                }
                            }
                            ConnectorKind::Flow | ConnectorKind::Message if c.ends.len() >= 2 => {
                                let from = path_of(&c.ends[0].target);
                                let label = match (&c.item_type, &u.id.display_name()) {
                                    (Some(t), _) => Some(t.name.last().text.clone()),
                                    (None, Some(n)) => Some(n.text.clone()),
                                    // `a.x`: the parameter carried.
                                    (None, None) => (from.len() > 1).then(|| from[from.len() - 1].clone()),
                                };
                                self.pending.push(Pending {
                                    scope: me,
                                    from: End::Path(from),
                                    to: End::Path(path_of(&c.ends[1].target)),
                                    label,
                                    qualified_name: qualified_name(),
                                    offset,
                                    flow: true,
                                });
                            }
                            _ => {}
                        }
                    }
                    // `if g then a;`, `else b;` after a decision, `accept S then a;`.
                    MemberKind::Transition(t) => {
                        let from = match (&t.source, prev.clone()) {
                            (Some(s), _) => End::Path(path_of(s)),
                            (None, Some(p)) => p,
                            (None, None) => continue,
                        };
                        let Some(target) = &t.target else { continue };
                        self.pending.push(Pending {
                            scope: me,
                            from,
                            to: End::Path(path_of(target)),
                            label: transition_label(t),
                            qualified_name: qualified_name(),
                            offset,
                            flow: false,
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    /// The initial or final node of `owner`, created on first use.
    fn symbol(&mut self, owner: usize, kind: StateKind) -> usize {
        if let Some(&c) = self.sm.states[owner].children.iter().find(|&&c| self.sm.states[c].kind == kind && self.sm.states[c].name.is_empty()) {
            return c;
        }
        self.push(Some(owner), kind, String::new(), String::new())
    }

    /// The step a path names from `scope`: `start` and `done`, its own steps,
    /// then those around it. `a.x` (a parameter of `a`) names `a`.
    fn resolve(&mut self, scope: usize, end: &End) -> Option<usize> {
        let path = match end {
            End::Node(n) => return Some(*n),
            End::Path(p) => p,
        };
        match path.as_slice() {
            [one] if one == "start" => return Some(self.symbol(scope, StateKind::Initial)),
            [one] if one == "done" => return Some(self.symbol(scope, StateKind::Final)),
            _ => {}
        }
        for len in (1..=path.len()).rev() {
            let mut at = Some(scope);
            while let Some(s) = at {
                if let Some(found) = self.descend(s, &path[..len]) {
                    return Some(found);
                }
                at = self.sm.states[s].parent;
            }
        }
        None
    }

    fn descend(&self, from: usize, path: &[String]) -> Option<usize> {
        let mut at = from;
        for seg in path {
            at = *self.sm.states[at].children.iter().find(|&&c| !self.sm.states[c].name.is_empty() && &self.sm.states[c].name == seg)?;
        }
        (at != from).then_some(at)
    }
}

/// The action flow of `root` (an action definition or usage).
pub fn build(index: &Index, root: EntryId) -> StateMachine {
    let mut b = Builder { index, sm: StateMachine::default(), pending: Vec::new() };
    let me = b.add_step(None, &[root], StateKind::State);
    b.sm.states[me].kind = StateKind::Root;
    let mut seen = Vec::new();
    for p in std::mem::take(&mut b.pending) {
        match (b.resolve(p.scope, &p.from), b.resolve(p.scope, &p.to)) {
            (Some(from), Some(to)) if from != to || !p.flow => {
                let key = (from, to, p.label.clone(), p.flow);
                if !seen.contains(&key) {
                    seen.push(key);
                    b.sm.transitions.push(Transition::new(from, to, p.label, p.qualified_name, p.offset, p.flow));
                }
            }
            (Some(_), Some(_)) => {}
            _ => b.sm.unresolved += 1,
        }
    }
    b.sm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagram::{Metrics, TextRole};

    struct Fixed;
    impl Metrics for Fixed {
        fn width(&mut self, _: TextRole, s: &str) -> f32 {
            s.chars().count() as f32 * 7.0
        }
        fn line_height(&mut self, _: TextRole) -> f32 {
            16.0
        }
    }

    fn label(sm: &StateMachine, i: usize) -> String {
        let s = &sm.states[i];
        match s.kind {
            StateKind::Initial => "(start)".into(),
            StateKind::Final => "(done)".into(),
            StateKind::Decision => format!("(decide{})", s.name),
            StateKind::Fork => format!("(fork {})", s.name),
            StateKind::Join => format!("(join {})", s.name),
            StateKind::Merge => format!("(merge {})", s.name),
            StateKind::Accept | StateKind::Send => s.title.clone(),
            _ => s.name.clone(),
        }
    }

    fn edges(sm: &StateMachine) -> Vec<String> {
        sm.transitions
            .iter()
            .map(|t| {
                let arrow = if t.flow { "=>" } else { "->" };
                match &t.label {
                    Some(l) => format!("{} {arrow} {} [{l}]", label(sm, t.from), label(sm, t.to)),
                    None => format!("{} {arrow} {}", label(sm, t.from), label(sm, t.to)),
                }
            })
            .collect()
    }

    fn flow(src: &str, root: &str) -> StateMachine {
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let mut sm = build(&index, index.get(root).unwrap());
        sm.layout(&mut Fixed);
        sm
    }

    #[test]
    fn sample_deliver() {
        let sm = flow(include_str!("../sysml/tests/data/drone.sysml"), "DroneSystem::Deliver");
        let e = edges(&sm);
        for want in [
            "(start) -> takeOff",
            "takeOff -> fly",
            "fly -> land",
            "(decide) -> land [[success]]",
            "(decide) -> takeOff [else]",
        ] {
            assert!(e.iter().any(|x| x == want), "missing {want}: {e:#?}");
        }
        assert_eq!(e.iter().filter(|x| x.as_str() == "fly -> land").count(), 1, "`then land` and `first fly then land` are one edge");
        assert_eq!(sm.unresolved, 0);
        assert_eq!(sm.states[0].actions, ["in payload : Payload", "out success : Boolean"]);
        let fly = sm.states.iter().find(|s| s.name == "fly").unwrap();
        assert_eq!(fly.actions, ["in target : Real"]);
        // Left to right in the order they run.
        let x = |n: &str| sm.states.iter().find(|s| s.name == n).unwrap().rect[0];
        assert!(x("takeOff") < x("fly") && x("fly") < x("land"));
        // The part performing it offers the same flow.
        let model = sysml::parse(include_str!("../sysml/tests/data/drone.sysml")).model;
        let index = Index::build(&model);
        let c = candidates(&index, |_| true);
        assert_eq!(c.first().map(String::as_str), Some("DroneSystem::Deliver"));
        assert!(c.iter().any(|q| q == "DroneSystem::Drone::deliver"));
    }

    #[test]
    fn control_nodes_object_flows_and_nesting() {
        let src = "package P {
            action def Pipe {
                in item raw : Ore;
                first start;
                then fork f;
                    then grind;
                    then heat;
                action grind { out item dust : Dust; }
                action heat : Heat { in item dust : Dust; }
                flow of Dust from grind.dust to heat.dust;
                join j;
                first grind then j;
                first heat then j;
                then merge m;
                then accept Ready;
                then send Report() to log;
                then action pack { first start; then box; action box; then done; }
                then done;
                first heat then nowhere;
            }
        }";
        let sm = flow(src, "P::Pipe");
        let e = edges(&sm);
        for want in [
            "(start) -> (fork f)",
            "(fork f) -> grind",
            "(fork f) -> heat",
            "grind => heat [Dust]",
            "grind -> (join j)",
            "heat -> (join j)",
            "(join j) -> (merge m)",
            "(merge m) -> accept Ready",
            "accept Ready -> send Report()",
            "send Report() -> pack",
            "pack -> (done)",
            "box -> (done)",
        ] {
            assert!(e.iter().any(|x| x == want), "missing {want}: {e:#?}");
        }
        assert_eq!(sm.unresolved, 1, "`nowhere` is not a step");
        let pack = sm.states.iter().position(|s| s.name == "pack").unwrap();
        let inner: Vec<String> = sm.states[pack].children.iter().map(|&c| label(&sm, c)).collect();
        assert!(inner.contains(&"box".to_string()) && inner.contains(&"(start)".to_string()), "{inner:?}");
        let heat = sm.states.iter().find(|s| s.name == "heat").unwrap();
        assert_eq!(heat.type_name.as_deref(), None, "Heat is not declared, so it does not resolve");
        assert!(heat.children.is_empty(), "typed steps are not expanded");
        let fork = sm.states.iter().find(|s| s.kind == StateKind::Fork).unwrap();
        assert!(fork.rect[3] - fork.rect[1] > fork.rect[2] - fork.rect[0], "a fork is a vertical bar");
    }
}
