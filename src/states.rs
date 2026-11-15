//! State machine view: the states of one state definition or state usage,
//! nested in their owners, with `entry` / `do` / `exit` actions, the initial
//! and final pseudo-states and the transitions between them.
//!
//! States come from the element's own body and from its types and their
//! supertypes (`exhibit state flight : FlightStates` shows `FlightStates`).
//! A transition without `first` leaves the state declared just before it,
//! `entry; then s;` marks `s` as the initial state of its region, and
//! `then done` goes to the region's final state.
//!
//! Layout is automatic: the states of a region are ranked left to right from
//! the initial state along the transitions; parallel regions are stacked.
//! Transitions are drawn as curves, bent apart when two states have several.

use crate::diagram::{Metrics, TextRole};
use crate::ibd::layers;
use std::collections::{HashMap, VecDeque};
use sysml::{ActionRef, ConnectorKind, EntryId, Index, Member, MemberKind, Reference, StateActionKind, TriggerKind, Usage, UsageKind};

pub type Rect = [f32; 4];

/// Nesting levels expanded below the root.
const MAX_DEPTH: usize = 4;
const MAX_STATES: usize = 400;
const PAD: f32 = 16.0;
/// Between columns (at least) and between the states of a column.
const GAP: [f32; 2] = [90.0, 30.0];
const MIN_SIZE: [f32; 2] = [118.0, 44.0];
/// Diameter of the initial and final pseudo-states.
pub const PSEUDO: f32 = 18.0;
/// Bend between transitions joining the same two states.
const LANE: f32 = 30.0;
/// Longest label shown (characters).
const LABEL_MAX: usize = 56;
/// Segments a curved transition is drawn with.
const CURVE_STEPS: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateKind {
    /// The state definition or usage shown.
    Root,
    /// A state, or in an action flow an action.
    State,
    Initial,
    Final,
    // Action flows only.
    Fork,
    Join,
    Decision,
    Merge,
    /// `accept sig : Signal`
    Accept,
    /// `send Signal() to x`
    Send,
}

impl StateKind {
    /// Drawn as a fixed-size symbol, without a title.
    pub fn is_symbol(self) -> bool {
        matches!(self, StateKind::Initial | StateKind::Final | StateKind::Fork | StateKind::Join | StateKind::Decision | StateKind::Merge)
    }

    /// Size of a symbol, in diagram units.
    pub fn symbol_size(self) -> [f32; 2] {
        match self {
            StateKind::Fork | StateKind::Join => [8.0, 56.0],
            StateKind::Decision | StateKind::Merge => [28.0, 28.0],
            _ => [PSEUDO, PSEUDO],
        }
    }

    /// Round (its border is a circle for lines to end on).
    fn is_round(self) -> bool {
        matches!(self, StateKind::Initial | StateKind::Final | StateKind::Decision | StateKind::Merge)
    }
}

#[derive(Clone, Debug)]
pub struct State {
    pub name: String,
    /// `airborne`, `flight : FlightStates`
    pub title: String,
    pub keyword: String,
    pub kind: StateKind,
    /// Its states are concurrent regions (`state s parallel { }`).
    pub parallel: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub depth: usize,
    /// `entry / spinUp`, `do / monitor`
    pub actions: Vec<String>,
    /// The most specific declaration, to show it in the code (none for pseudo-states).
    pub qualified_name: Option<String>,
    /// The definition typing this state, to open its own view.
    pub type_name: Option<String>,
    /// Inner states exist but are below the depth limit.
    pub truncated: bool,
    /// Absolute, in diagram units (after [`StateMachine::layout`]).
    pub rect: Rect,
    /// Height of the title row (the whole state for a simple state).
    pub header: f32,
}

impl State {
    pub fn is_pseudo(&self) -> bool {
        self.kind.is_symbol()
    }

    pub fn center(&self) -> [f32; 2] {
        [(self.rect[0] + self.rect[2]) * 0.5, (self.rect[1] + self.rect[3]) * 0.5]
    }

    /// Has inner states drawn inside it.
    pub fn is_composite(&self) -> bool {
        !self.children.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Transition {
    pub from: usize,
    pub to: usize,
    /// `Launch [ready] / send Ack()`
    pub label: Option<String>,
    pub qualified_name: Option<String>,
    /// Start of the declaration in the edited file, for transitions without a name.
    pub offset: Option<usize>,
    /// The routed polyline (after [`StateMachine::layout`]).
    pub points: Vec<[f32; 2]>,
    pub label_rect: Option<Rect>,
    /// An object flow (`flow from a.x to b.y`) rather than a control flow.
    pub flow: bool,
    label_size: [f32; 2],
}

impl Transition {
    pub fn new(from: usize, to: usize, label: Option<String>, qualified_name: Option<String>, offset: Option<usize>, flow: bool) -> Self {
        Transition { from, to, label, qualified_name, offset, points: Vec::new(), label_rect: None, flow, label_size: [0.0; 2] }
    }
}

#[derive(Clone, Debug, Default)]
pub struct StateMachine {
    /// `states[0]` is the root; owners come before what they own.
    pub states: Vec<State>,
    pub transitions: Vec<Transition>,
    /// Transition ends that name no state shown (for the status bar).
    pub unresolved: usize,
}

/// What can be selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    State(usize),
    Transition(usize),
}

fn unwrap_then(m: &Member) -> &Member {
    match &m.kind {
        MemberKind::Then(inner) => unwrap_then(inner),
        _ => m,
    }
}

fn state_usage(m: &Member) -> Option<&Usage> {
    match &m.kind {
        MemberKind::Usage(u) if u.kind == UsageKind::State && u.connector.is_none() => Some(u),
        _ => None,
    }
}

fn is_state_element(m: &Member) -> bool {
    match &m.kind {
        MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::State,
        MemberKind::Usage(u) => u.kind == UsageKind::State,
        _ => false,
    }
}

/// States and transitions over the bodies of `layers`.
fn behavior_count(index: &Index, layers: &[EntryId]) -> usize {
    layers
        .iter()
        .flat_map(|&l| index.entry(l).member.kind.body())
        .filter(|m| {
            let m = unwrap_then(m);
            state_usage(m).is_some() || matches!(m.kind, MemberKind::Transition(_))
        })
        .count()
}

/// Elements worth a state machine view, most states and transitions first:
/// state definitions, and state usages not nested in another state (such as
/// `exhibit state` in a part). Only `in_source` entries are offered.
pub fn candidates(index: &Index, in_source: impl Fn(EntryId) -> bool) -> Vec<String> {
    let mut scored: Vec<(usize, bool, String)> = Vec::new();
    for (id, e) in index.entries().iter().enumerate() {
        if !in_source(id) {
            continue;
        }
        let top = match &e.member.kind {
            MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::State,
            MemberKind::Usage(u) => {
                u.kind == UsageKind::State && u.connector.is_none() && !e.parent.is_some_and(|p| is_state_element(index.entry(p).member))
            }
            _ => false,
        };
        if !top {
            continue;
        }
        let n = behavior_count(index, &layers(index, &[id]));
        if n > 0 {
            scored.push((n, matches!(e.member.kind, MemberKind::Definition(_)), e.qualified_name.clone()));
        }
    }
    // Most states and transitions first; a definition before the usages showing it.
    scored.sort_by_key(|s| std::cmp::Reverse((s.0, s.1)));
    scored.into_iter().map(|(_, _, q)| q).collect()
}

pub(crate) fn path_of(r: &Reference) -> Vec<String> {
    r.chain.iter().flat_map(|q| q.segments.iter().map(|s| s.text.clone())).collect()
}

pub(crate) fn clip(mut s: String) -> String {
    if s.chars().count() > LABEL_MAX {
        s = s.chars().take(LABEL_MAX - 1).collect::<String>() + "…";
    }
    s
}

pub(crate) fn action_text(a: &ActionRef) -> String {
    match a {
        ActionRef::Ref(r) => r.to_string(),
        ActionRef::Inline(m) => match &m.kind {
            MemberKind::Send(s) => format!("send {}", s.payload),
            MemberKind::Usage(u) => match (u.id.display_name(), u.typed_by.first()) {
                (Some(n), _) => n.text.clone(),
                (None, Some(t)) => format!(": {}", t.name.last().text),
                _ => "action".to_string(),
            },
            MemberKind::Accept(_) => "accept".to_string(),
            _ => "action".to_string(),
        },
    }
}

/// `Launch [ready] / send Ack()`
pub(crate) fn transition_label(t: &sysml::Transition) -> Option<String> {
    let mut parts = Vec::new();
    if t.is_else {
        parts.push("else".to_string());
    }
    if let Some(trigger) = &t.trigger {
        parts.push(match &trigger.kind {
            TriggerKind::Signal { name, ty } => match (ty, name) {
                (Some(ty), _) => ty.name.last().text.clone(),
                (None, Some(n)) => n.text.clone(),
                _ => "accept".to_string(),
            },
            TriggerKind::When(e) => format!("when {e}"),
            TriggerKind::At(e) => format!("at {e}"),
            TriggerKind::After(e) => format!("after {e}"),
        });
    }
    if let Some(g) = &t.guard {
        parts.push(format!("[{g}]"));
    }
    if let Some(e) = &t.effect {
        parts.push(format!("/ {}", action_text(e)));
    }
    if parts.is_empty() {
        return t.id.display_name().map(|n| n.text.clone());
    }
    Some(clip(parts.join(" ")))
}

enum From {
    State(usize),
    Path(Vec<String>),
}

struct Pending {
    scope: usize,
    from: From,
    to: Vec<String>,
    label: Option<String>,
    qualified_name: Option<String>,
    offset: Option<usize>,
}

struct Builder<'a, 'm> {
    index: &'a Index<'m>,
    sm: StateMachine,
    pending: Vec<Pending>,
}

impl Builder<'_, '_> {
    fn add_state(&mut self, parent: Option<usize>, group: &[EntryId], depth: usize) -> usize {
        let index = self.index;
        let first = index.entry(group[0]);
        let usages: Vec<&Usage> = group
            .iter()
            .filter_map(|&id| match &index.entry(id).member.kind {
                MemberKind::Usage(u) => Some(u),
                _ => None,
            })
            .collect();
        let ty = usages.iter().find_map(|u| u.typed_by.first());
        let mut title = first.name.to_string();
        if let Some(t) = ty {
            title += &format!(" : {}", t.name.last().text);
        }
        let keyword = match &first.member.kind {
            MemberKind::Definition(d) => format!("{} def", d.kind),
            MemberKind::Usage(u) => match u.prefix.role {
                Some(r) => format!("{} state", r.keyword()),
                None => "state".to_string(),
            },
            _ => "state".to_string(),
        };
        let type_name = group.iter().find_map(|&id| {
            let e = index.entry(id);
            let MemberKind::Usage(u) = &e.member.kind else { return None };
            let t = u.typed_by.first()?;
            index.resolve(e.parent, &t.name).map(|t| index.entry(t).qualified_name.clone())
        });
        let layers = layers(index, group);
        let parallel = layers.iter().any(|&l| match &index.entry(l).member.kind {
            MemberKind::Definition(d) => d.is_parallel,
            MemberKind::Usage(u) => u.is_parallel,
            _ => false,
        });
        let me = self.sm.states.len();
        self.sm.states.push(State {
            name: first.name.to_string(),
            title,
            keyword,
            kind: if parent.is_none() { StateKind::Root } else { StateKind::State },
            parallel,
            parent,
            children: Vec::new(),
            depth,
            actions: Vec::new(),
            qualified_name: Some(first.qualified_name.clone()),
            type_name,
            truncated: false,
            rect: [0.0; 4],
            header: 0.0,
        });
        if let Some(p) = parent {
            self.sm.states[p].children.push(me);
        }

        // Inner states, by name over every layer (redefinitions merge).
        let mut groups: Vec<(String, Vec<EntryId>)> = Vec::new();
        for &l in &layers {
            for outer in index.entry(l).member.kind.body() {
                // The index knows `then state s;` by the `then` member.
                let m = unwrap_then(outer);
                if state_usage(m).is_none() {
                    continue;
                }
                let (Some(name), Some(id)) = (m.kind.effective_name(), index.id_of(m).or(index.id_of(outer))) else { continue };
                match groups.iter_mut().find(|(n, _)| n == name) {
                    Some((_, ids)) => ids.push(id),
                    None => groups.push((name.to_string(), vec![id])),
                }
            }
        }
        if depth >= MAX_DEPTH {
            self.sm.states[me].truncated = !groups.is_empty();
            return me;
        }
        let mut by_name = HashMap::new();
        for (name, ids) in &groups {
            if self.sm.states.len() >= MAX_STATES {
                self.sm.states[me].truncated = true;
                break;
            }
            let child = self.add_state(Some(me), ids, depth + 1);
            by_name.insert(name.clone(), child);
        }

        // Actions and transitions, in declaration order.
        let mut seen_actions = Vec::new();
        for &l in &layers {
            let local = index.entry(l).source == 0;
            let mut prev: Option<usize> = None;
            let mut initial_next = false;
            for member in index.entry(l).member.kind.body() {
                let is_then = matches!(member.kind, MemberKind::Then(_));
                let m = unwrap_then(member);
                match &m.kind {
                    MemberKind::StateAction(a) => match (&a.kind, &a.action) {
                        (StateActionKind::Entry, None) => initial_next = true,
                        (kind, action) => {
                            let kw = match kind {
                                StateActionKind::Entry => "entry",
                                StateActionKind::Do => "do",
                                StateActionKind::Exit => "exit",
                            };
                            if !seen_actions.contains(&kw) {
                                seen_actions.push(kw);
                                let what = action.as_ref().map_or_else(|| "—".to_string(), action_text);
                                self.sm.states[me].actions.push(clip(format!("{kw} / {what}")));
                            }
                        }
                    },
                    MemberKind::Usage(_) if state_usage(m).is_some() => {
                        let here = m.kind.effective_name().and_then(|n| by_name.get(n)).copied();
                        // `then state s;`: reached from the state before it.
                        if is_then && let (Some(p), Some(h)) = (prev, here) {
                            self.pending.push(Pending {
                                scope: me,
                                from: From::State(p),
                                to: vec![self.sm.states[h].name.clone()],
                                label: None,
                                qualified_name: None,
                                offset: None,
                            });
                        }
                        prev = here.or(prev);
                    }
                    MemberKind::Usage(u) if u.kind == UsageKind::Succession => {
                        let Some(c) = u.connector.as_ref().filter(|c| c.kind == ConnectorKind::Succession) else { continue };
                        let (from, to) = if c.implicit_source {
                            let from = if initial_next { Some(self.pseudo(me, StateKind::Initial)) } else { prev };
                            let Some(from) = from else { continue };
                            let Some(end) = c.ends.last() else { continue };
                            (From::State(from), path_of(&end.target))
                        } else if c.ends.len() >= 2 {
                            (From::Path(path_of(&c.ends[0].target)), path_of(&c.ends[1].target))
                        } else {
                            continue;
                        };
                        initial_next = false;
                        self.pending.push(Pending { scope: me, from, to, label: None, qualified_name: None, offset: local.then_some(m.span.start) });
                    }
                    MemberKind::Transition(t) => {
                        let from = match (&t.source, prev) {
                            (Some(s), _) => From::Path(path_of(s)),
                            (None, Some(p)) => From::State(p),
                            (None, None) => continue,
                        };
                        let Some(target) = &t.target else { continue };
                        let qualified_name = t.id.display_name().and(index.id_of(m)).map(|id| index.entry(id).qualified_name.clone());
                        self.pending.push(Pending {
                            scope: me,
                            from,
                            to: path_of(target),
                            label: transition_label(t),
                            qualified_name,
                            offset: local.then_some(m.span.start),
                        });
                    }
                    _ => {}
                }
            }
        }
        me
    }

    /// The initial or final pseudo-state of region `owner`, created on first use.
    fn pseudo(&mut self, owner: usize, kind: StateKind) -> usize {
        if let Some(&c) = self.sm.states[owner].children.iter().find(|&&c| self.sm.states[c].kind == kind) {
            return c;
        }
        let id = self.sm.states.len();
        let depth = self.sm.states[owner].depth + 1;
        self.sm.states.push(State {
            name: String::new(),
            title: String::new(),
            keyword: String::new(),
            kind,
            parallel: false,
            parent: Some(owner),
            children: Vec::new(),
            depth,
            actions: Vec::new(),
            qualified_name: None,
            type_name: None,
            truncated: false,
            rect: [0.0; 4],
            header: 0.0,
        });
        self.sm.states[owner].children.push(id);
        id
    }

    /// The state `path` names as seen from region `scope`: its own states
    /// first, then those of the regions around it.
    fn resolve(&mut self, scope: usize, path: &[String]) -> Option<usize> {
        if path.len() == 1 && path[0] == "done" {
            return Some(self.pseudo(scope, StateKind::Final));
        }
        let mut at = Some(scope);
        while let Some(s) = at {
            if let Some(found) = self.descend(s, path) {
                return Some(found);
            }
            at = self.sm.states[s].parent;
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

impl StateMachine {
    pub fn build(index: &Index, root: EntryId) -> StateMachine {
        let mut b = Builder { index, sm: StateMachine::default(), pending: Vec::new() };
        b.add_state(None, &[root], 0);
        for p in std::mem::take(&mut b.pending) {
            let from = match p.from {
                From::State(s) => Some(s),
                From::Path(path) => b.resolve(p.scope, &path),
            };
            let to = b.resolve(p.scope, &p.to);
            match (from, to) {
                (Some(from), Some(to)) => b.sm.transitions.push(Transition::new(from, to, p.label, p.qualified_name, p.offset, false)),
                _ => b.sm.unresolved += 1,
            }
        }
        // The same transition declared in a state and in its definition.
        let mut seen = Vec::new();
        b.sm.transitions.retain(|t| {
            let key = (t.from, t.to, t.label.clone(), t.qualified_name.clone());
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });
        b.sm
    }

    pub fn state_at(&self, p: [f32; 2]) -> Option<usize> {
        let inside = |r: Rect| p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3];
        (0..self.states.len()).filter(|&i| inside(self.states[i].rect)).max_by_key(|&i| self.states[i].depth)
    }

    /// The transition whose label or line is at `p` (`tolerance` in diagram units).
    pub fn transition_at(&self, p: [f32; 2], tolerance: f32) -> Option<usize> {
        let inside = |r: Rect| p[0] >= r[0] && p[0] <= r[2] && p[1] >= r[1] && p[1] <= r[3];
        if let Some(i) = self.transitions.iter().position(|t| t.label_rect.is_some_and(inside)) {
            return Some(i);
        }
        self.transitions
            .iter()
            .enumerate()
            .map(|(i, t)| (i, t.points.windows(2).map(|w| segment_distance(p, w[0], w[1])).fold(f32::INFINITY, f32::min)))
            .filter(|&(_, d)| d <= tolerance)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    pub fn hit(&self, p: [f32; 2], tolerance: f32) -> Option<Hit> {
        self.transition_at(p, tolerance).map(Hit::Transition).or_else(|| self.state_at(p).map(Hit::State))
    }

    pub fn bounds(&self) -> Option<Rect> {
        let root = self.states.first()?;
        let mut b = root.rect;
        for r in self.transitions.iter().filter_map(|t| t.label_rect).chain(self.transitions.iter().flat_map(|t| t.points.iter().map(|p| [p[0], p[1], p[0], p[1]]))) {
            b = [b[0].min(r[0]), b[1].min(r[1]), b[2].max(r[2]), b[3].max(r[3])];
        }
        Some(b)
    }

    /// Whether `state` is `of` or nested in it.
    pub fn is_within(&self, mut state: usize, of: usize) -> bool {
        loop {
            if state == of {
                return true;
            }
            match self.states[state].parent {
                Some(p) => state = p,
                None => return false,
            }
        }
    }

    /// The child of `owner` that holds `state` (`state` itself if it is one).
    fn child_holding(&self, owner: usize, mut state: usize) -> Option<usize> {
        loop {
            let parent = self.states[state].parent?;
            if parent == owner {
                return Some(state);
            }
            state = parent;
        }
    }

    pub fn layout(&mut self, m: &mut dyn Metrics) {
        if self.states.is_empty() {
            return;
        }
        let title_h = m.line_height(TextRole::Name);
        let line_h = m.line_height(TextRole::Compartment);
        let label_h = m.line_height(TextRole::Keyword);
        for t in &mut self.transitions {
            t.label_size = [t.label.as_deref().map_or(0.0, |l| m.width(TextRole::Keyword, l)), label_h];
        }
        let mut sizes = vec![[0.0f32; 2]; self.states.len()];
        let mut offsets = vec![[0.0f32; 2]; self.states.len()];
        self.measure(0, m, title_h, line_h, &mut sizes, &mut offsets);
        self.place(0, [0.0, 0.0], &sizes, &offsets);
        self.route(label_h);
    }

    /// Sizes `i` and positions its children relative to it.
    fn measure(&mut self, i: usize, m: &mut dyn Metrics, title_h: f32, line_h: f32, sizes: &mut [[f32; 2]], offsets: &mut [[f32; 2]]) {
        if self.states[i].is_pseudo() {
            sizes[i] = self.states[i].kind.symbol_size();
            return;
        }
        let children = self.states[i].children.clone();
        for &c in &children {
            self.measure(c, m, title_h, line_h, sizes, offsets);
        }
        let s = &self.states[i];
        let mut title_w = m.width(TextRole::Name, &s.title);
        if s.kind == StateKind::Root || s.is_composite() {
            title_w += m.width(TextRole::Keyword, &format!("«{}»", s.keyword)) + 24.0;
        }
        let actions_w = s.actions.iter().map(|a| m.width(TextRole::Compartment, a)).fold(0.0, f32::max);
        let actions_h = if s.actions.is_empty() { 0.0 } else { s.actions.len() as f32 * line_h + 12.0 };
        let truncated_h = if s.truncated { line_h + 8.0 } else { 0.0 };
        let header = title_h + 14.0;
        let mut w = (title_w + 2.0 * PAD).max(actions_w + 2.0 * PAD).max(MIN_SIZE[0]);
        if children.is_empty() {
            let h = if s.actions.is_empty() && !s.truncated { MIN_SIZE[1].max(title_h + 20.0) } else { header + actions_h + truncated_h + 4.0 };
            self.states[i].header = if s.actions.is_empty() && !s.truncated { h } else { header };
            sizes[i] = [w, h];
            return;
        }

        // Children: ranked columns, or stacked regions for a parallel state.
        let top = header + actions_h + PAD;
        let mut placed: Vec<(usize, [f32; 2])> = Vec::new();
        let content = if s.parallel {
            let width = children.iter().map(|&c| sizes[c][0]).fold(0.0, f32::max);
            let mut y = 0.0;
            for &c in &children {
                sizes[c][0] = width;
                placed.push((c, [0.0, y]));
                y += sizes[c][1] + GAP[1] * 0.8;
            }
            [width, y - GAP[1] * 0.8]
        } else {
            let ranks = self.ranks(i, &children);
            let columns = ranks.iter().copied().max().map_or(0, |r| r + 1);
            // Each gap fits the labels of the transitions crossing it between neighbor columns.
            let mut gaps = vec![GAP[0]; columns];
            for t in &self.transitions {
                let (Some(a), Some(b)) = (self.child_holding(i, t.from), self.child_holding(i, t.to)) else { continue };
                let (Some(ka), Some(kb)) = (children.iter().position(|&c| c == a), children.iter().position(|&c| c == b)) else { continue };
                let (ra, rb) = (ranks[ka].min(ranks[kb]), ranks[ka].max(ranks[kb]));
                if rb == ra + 1 {
                    gaps[ra] = gaps[ra].max((t.label_size[0] + 40.0).min(GAP[0] * 6.0));
                }
            }
            let mut x = 0.0;
            let mut gap = 0.0;
            let col_h: Vec<f32> = (0..columns)
                .map(|r| {
                    let hs: Vec<f32> = children.iter().zip(&ranks).filter(|&(_, &k)| k == r).map(|(&c, _)| sizes[c][1]).collect();
                    hs.iter().sum::<f32>() + GAP[1] * hs.len().saturating_sub(1) as f32
                })
                .collect();
            let height = col_h.iter().copied().fold(0.0, f32::max);
            for (r, &ch) in col_h.iter().enumerate() {
                let members: Vec<usize> = children.iter().zip(&ranks).filter(|&(_, &k)| k == r).map(|(&c, _)| c).collect();
                let cw = members.iter().map(|&c| sizes[c][0]).fold(0.0, f32::max);
                let mut y = (height - ch) * 0.5;
                for c in members {
                    placed.push((c, [x + (cw - sizes[c][0]) * 0.5, y]));
                    y += sizes[c][1] + GAP[1];
                }
                gap = gaps[r];
                x += cw + gap;
            }
            [(x - gap).max(0.0), height]
        };
        w = w.max(content[0] + 2.0 * PAD);
        let dx = PAD + (w - 2.0 * PAD - content[0]) * 0.5;
        for (c, o) in placed {
            offsets[c] = [dx + o[0], top + o[1]];
        }
        self.states[i].header = header;
        sizes[i] = [w, top + content[1] + PAD + truncated_h];
    }

    /// Column of each child of `owner`: breadth first from the initial state
    /// along the transitions between them; the final state comes last.
    fn ranks(&self, owner: usize, children: &[usize]) -> Vec<usize> {
        let pos = |s: usize| children.iter().position(|&c| c == s);
        let mut edges: Vec<(usize, usize)> = Vec::new();
        for t in &self.transitions {
            // Object flows say what is passed, not what runs first.
            if let (Some(a), Some(b)) = (self.child_holding(owner, t.from), self.child_holding(owner, t.to))
                && a != b
                && !t.flow
                && let (Some(a), Some(b)) = (pos(a), pos(b))
            {
                edges.push((a, b));
            }
        }
        let n = children.len();
        let is = |k: usize, kind: StateKind| self.states[children[k]].kind == kind;
        let has_initial = (0..n).any(|k| is(k, StateKind::Initial));
        let seeds: Vec<usize> = (0..n).filter(|&k| is(k, StateKind::Initial)).chain((0..n).filter(|&k| !is(k, StateKind::Initial))).collect();

        // Edges that loop back (to a node still being visited, depth first from
        // the initial state) do not push their target to the right.
        let mut state = vec![0u8; n]; // 0 new, 1 on the stack, 2 done
        let mut back = vec![false; edges.len()];
        for &seed in &seeds {
            if state[seed] != 0 {
                continue;
            }
            let mut stack: Vec<(usize, usize)> = vec![(seed, 0)];
            state[seed] = 1;
            while let Some(&mut (a, ref mut next)) = stack.last_mut() {
                if let Some(e) = (*next..edges.len()).find(|&e| edges[e].0 == a) {
                    *next = e + 1;
                    let b = edges[e].1;
                    match state[b] {
                        0 => {
                            state[b] = 1;
                            stack.push((b, 0));
                        }
                        1 => back[e] = true,
                        _ => {}
                    }
                } else {
                    state[a] = 2;
                    stack.pop();
                }
            }
        }

        // Longest path over the other edges: a node sits right of everything leading to it.
        let mut rank = vec![0usize; n];
        let mut incoming = vec![0usize; n];
        for (e, &(_, b)) in edges.iter().enumerate() {
            if !back[e] {
                incoming[b] += 1;
            }
        }
        // A node nothing leads to (a decision only reached by name, say) does not
        // push what it leads to rightwards when those are reached anyway.
        let source = |k: usize| has_initial && incoming[k] == 0 && !is(k, StateKind::Initial);
        let sources: Vec<usize> = (0..n).filter(|&k| source(k)).collect();
        for (e, &(a, b)) in edges.iter().enumerate() {
            if !back[e] && sources.contains(&a) && incoming[b] > 1 {
                back[e] = true;
                incoming[b] -= 1;
            }
        }
        // Unreached nodes join the first column after the initial state.
        for k in 0..n {
            if incoming[k] == 0 && has_initial && !is(k, StateKind::Initial) {
                rank[k] = 1;
            }
        }
        let mut queue: VecDeque<usize> = (0..n).filter(|&k| incoming[k] == 0).collect();
        while let Some(a) = queue.pop_front() {
            for (e, &(x, y)) in edges.iter().enumerate() {
                if x != a || back[e] {
                    continue;
                }
                rank[y] = rank[y].max(rank[a] + 1);
                incoming[y] -= 1;
                if incoming[y] == 0 {
                    queue.push_back(y);
                }
            }
        }
        // Such a node sits just before the first of them.
        for &s in &sources {
            if let Some(first) = edges.iter().filter(|e| e.0 == s).map(|e| rank[e.1]).min() {
                rank[s] = rank[s].max(first.saturating_sub(1)).max(1);
            }
        }
        let last = (0..n).filter(|&k| !is(k, StateKind::Final)).map(|k| rank[k]).max().map_or(0, |r| r + 1);
        (0..n).map(|k| if is(k, StateKind::Final) { last } else { rank[k] }).collect()
    }

    fn place(&mut self, i: usize, at: [f32; 2], sizes: &[[f32; 2]], offsets: &[[f32; 2]]) {
        let [w, h] = sizes[i];
        self.states[i].rect = [at[0], at[1], at[0] + w, at[1] + h];
        for c in self.states[i].children.clone() {
            self.place(c, [at[0] + offsets[c][0], at[1] + offsets[c][1]], sizes, offsets);
        }
    }

    /// Where a line from inside state `s` towards `target` leaves it.
    fn exit_point(&self, s: usize, target: [f32; 2]) -> [f32; 2] {
        let st = &self.states[s];
        let c = st.center();
        let d = [target[0] - c[0], target[1] - c[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
        if len < 1e-3 {
            return c;
        }
        if st.kind.is_round() {
            let r = (st.rect[2] - st.rect[0]) * 0.5;
            return [c[0] + d[0] / len * r, c[1] + d[1] / len * r];
        }
        let hw = (st.rect[2] - st.rect[0]) * 0.5;
        let hh = (st.rect[3] - st.rect[1]) * 0.5;
        // On the border in the direction of `target`, even when `target` lies inside
        // (a composite state holds the midpoint of a transition to its neighbor).
        let t = (hw / d[0].abs().max(1e-6)).min(hh / d[1].abs().max(1e-6));
        [c[0] + d[0] * t, c[1] + d[1] * t]
    }

    /// States a line between `from` and `to` must not cross: all but those two,
    /// what holds them and what they hold.
    fn obstacles(&self, from: usize, to: usize) -> Vec<Rect> {
        (1..self.states.len())
            .filter(|&s| ![from, to].iter().any(|&e| self.is_within(e, s) || self.is_within(s, e)))
            .map(|s| self.states[s].rect)
            .collect()
    }

    /// For a straight line from `p0` to `p2` that would cross another state:
    /// a route below (or above) everything in the way, with its label point.
    fn detour(&self, from: usize, to: usize, p0: [f32; 2], p2: [f32; 2]) -> Option<(Vec<[f32; 2]>, [f32; 2])> {
        let obstacles = self.obstacles(from, to);
        let inside = |p: [f32; 2], r: &Rect| p[0] > r[0] - 2.0 && p[0] < r[2] + 2.0 && p[1] > r[1] - 2.0 && p[1] < r[3] + 2.0;
        let steps = 40;
        let crossed: Vec<&Rect> = obstacles
            .iter()
            .filter(|r| (1..steps).any(|i| {
                let t = i as f32 / steps as f32;
                inside([p0[0] + (p2[0] - p0[0]) * t, p0[1] + (p2[1] - p0[1]) * t], r)
            }))
            .collect();
        if crossed.is_empty() {
            return None;
        }
        let (a, b) = (self.states[from].rect, self.states[to].rect);
        let (x0, x1) = ((a[0] + a[2]) * 0.5, (b[0] + b[2]) * 0.5);
        // Everything between the two ends, horizontally.
        let span = |r: &&Rect| r[2] > x0.min(x1) && r[0] < x0.max(x1);
        let between: Vec<&Rect> = obstacles.iter().filter(span).collect();
        let below = between.iter().map(|r| r[3]).fold(a[3].max(b[3]), f32::max) + 14.0;
        let above = between.iter().map(|r| r[1]).fold(a[1].min(b[1]), f32::min) - 14.0;
        let mid_y = (p0[1] + p2[1]) * 0.5;
        let down = (below - mid_y).abs() <= (mid_y - above).abs();
        let (y, ya, yb) = if down { (below, a[3], b[3]) } else { (above, a[1], b[1]) };
        let points = vec![[x0, ya], [x0, y], [x1, y], [x1, yb]];
        Some((points, [(x0 + x1) * 0.5, y]))
    }

    fn route(&mut self, label_h: f32) {
        // Transitions joining the same two states, in either direction, bend apart.
        let mut groups: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
        for (k, t) in self.transitions.iter().enumerate() {
            if t.from != t.to {
                groups.entry((t.from.min(t.to), t.from.max(t.to))).or_default().push(k);
            }
        }
        let mut loops: HashMap<usize, usize> = HashMap::new();
        for k in 0..self.transitions.len() {
            let (from, to) = (self.transitions[k].from, self.transitions[k].to);
            let [lw, lh] = self.transitions[k].label_size;
            let (points, mid) = if from == to {
                let n = loops.entry(from).or_insert(0);
                *n += 1;
                let r = self.states[from].rect;
                let lift = 26.0 + 16.0 * (*n - 1) as f32;
                let (a, b) = ([r[2] - 46.0, r[1]], [r[2] - 14.0, r[1]]);
                let pts = cubic(a, [a[0], a[1] - lift], [b[0], b[1] - lift], b);
                let mid = [(a[0] + b[0]) * 0.5, r[1] - lift * 0.75 - lh * 0.5 - 2.0];
                (pts, mid)
            } else {
                let group = &groups[&(from.min(to), from.max(to))];
                let lane = group.iter().position(|&g| g == k).unwrap_or(0) as f32 - (group.len() - 1) as f32 * 0.5;
                let (lo, hi) = (from.min(to), from.max(to));
                let (a, b) = (self.states[lo].center(), self.states[hi].center());
                let d = [b[0] - a[0], b[1] - a[1]];
                let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-3);
                let n = [-d[1] / len, d[0] / len];
                let off = lane * LANE;
                let want = [(a[0] + b[0]) * 0.5 + n[0] * off, (a[1] + b[1]) * 0.5 + n[1] * off];
                let p0 = self.exit_point(from, want);
                let p2 = self.exit_point(to, want);
                if off.abs() < 0.5 {
                    match self.detour(from, to, p0, p2) {
                        Some(route) => route,
                        None => (vec![p0, p2], [(p0[0] + p2[0]) * 0.5, (p0[1] + p2[1]) * 0.5]),
                    }
                } else {
                    let m = [(p0[0] + p2[0]) * 0.5 + n[0] * off, (p0[1] + p2[1]) * 0.5 + n[1] * off];
                    let q = [(4.0 * m[0] - p0[0] - p2[0]) * 0.5, (4.0 * m[1] - p0[1] - p2[1]) * 0.5];
                    (quadratic(p0, q, p2), m)
                }
            };
            let t = &mut self.transitions[k];
            t.points = points;
            t.label_rect = t.label.as_ref().map(|_| [mid[0] - lw * 0.5 - 5.0, mid[1] - label_h * 0.5 - 2.0, mid[0] + lw * 0.5 + 5.0, mid[1] + label_h * 0.5 + 2.0]);
        }
    }
}

fn quadratic(a: [f32; 2], q: [f32; 2], b: [f32; 2]) -> Vec<[f32; 2]> {
    (0..=CURVE_STEPS)
        .map(|i| {
            let t = i as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            [u * u * a[0] + 2.0 * u * t * q[0] + t * t * b[0], u * u * a[1] + 2.0 * u * t * q[1] + t * t * b[1]]
        })
        .collect()
}

fn cubic(a: [f32; 2], c1: [f32; 2], c2: [f32; 2], b: [f32; 2]) -> Vec<[f32; 2]> {
    (0..=CURVE_STEPS)
        .map(|i| {
            let t = i as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            [k0 * a[0] + k1 * c1[0] + k2 * c2[0] + k3 * b[0], k0 * a[1] + k1 * c1[1] + k2 * c2[1] + k3 * b[1]]
        })
        .collect()
}

fn segment_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 < 1e-6 { 0.0 } else { (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0) };
    let c = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed;
    impl Metrics for Fixed {
        fn width(&mut self, _: TextRole, s: &str) -> f32 {
            s.chars().count() as f32 * 7.0
        }
        fn line_height(&mut self, _: TextRole) -> f32 {
            16.0
        }
    }

    fn build(src: &str, root: &str) -> StateMachine {
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let mut sm = StateMachine::build(&index, index.get(root).unwrap());
        sm.layout(&mut Fixed);
        sm
    }

    fn name(sm: &StateMachine, i: usize) -> String {
        let s = &sm.states[i];
        match s.kind {
            StateKind::Initial => "(initial)".into(),
            StateKind::Final => "(final)".into(),
            _ => s.name.clone(),
        }
    }

    fn edges(sm: &StateMachine) -> Vec<(String, String, Option<String>)> {
        sm.transitions.iter().map(|t| (name(sm, t.from), name(sm, t.to), t.label.clone())).collect()
    }

    #[test]
    fn sample_flight_states() {
        let sm = build(include_str!("../sysml/tests/data/drone.sysml"), "DroneSystem::FlightStates");
        let e = edges(&sm);
        let has = |a: &str, b: &str, l: Option<&str>| e.contains(&(a.into(), b.into(), l.map(Into::into)));
        assert!(has("(initial)", "grounded", None), "{e:?}");
        assert!(has("grounded", "airborne", Some("Launch [drone1.mode == FlightMode::idle] / send Ack()")), "{e:?}");
        // No `first`: leaves the state declared before it.
        assert!(has("airborne", "grounded", Some("Land")), "{e:?}");
        assert!(e.iter().any(|(a, b, l)| a == "airborne" && b == "grounded" && l.as_deref().is_some_and(|l| l.starts_with("after 30"))), "{e:?}");
        assert_eq!(sm.unresolved, 0);
        let airborne = sm.states.iter().find(|s| s.name == "airborne").unwrap();
        assert_eq!(airborne.actions, ["entry / spinUp", "do / monitor", "exit / action"]);
        // The initial state is left of the state it starts in, which is left of the next.
        let x = |n: &str| sm.states[(0..sm.states.len()).find(|&i| name(&sm, i) == n).unwrap()].rect[0];
        assert!(x("(initial)") < x("grounded") && x("grounded") < x("airborne"));
        // Nothing overlaps among siblings.
        let kids = &sm.states[0].children;
        for (i, &a) in kids.iter().enumerate() {
            for &b in &kids[i + 1..] {
                let (ra, rb) = (sm.states[a].rect, sm.states[b].rect);
                assert!(ra[2] <= rb[0] || rb[2] <= ra[0] || ra[3] <= rb[1] || rb[3] <= ra[1], "{} overlaps {}", name(&sm, a), name(&sm, b));
            }
        }
        // The three transitions between grounded and airborne bend apart, labels clear of each other.
        let between: Vec<Rect> = sm.transitions.iter().filter(|t| name(&sm, t.from) != "(initial)").filter_map(|t| t.label_rect).collect();
        assert_eq!(between.len(), 3);
        for (i, a) in between.iter().enumerate() {
            for b in &between[i + 1..] {
                assert!(a[2] <= b[0] || b[2] <= a[0] || a[3] <= b[1] || b[3] <= a[1], "labels overlap: {a:?} {b:?}");
            }
        }
    }

    #[test]
    fn nested_parallel_and_done() {
        let src = "package P {
            state def Ops {
                entry; then off;
                state off;
                state on parallel {
                    state motor { entry; then idle; state idle; state running; transition first idle accept Go then running; }
                    state light { entry; then dark; state dark; transition first dark then done; }
                }
                transition first off accept Start then on;
                transition first on.motor.running accept Stop then off;
                transition first off accept Missing then nowhere;
            }
            part def Robot { exhibit state ops : Ops; }
        }";
        let sm = build(src, "P::Ops");
        let e = edges(&sm);
        assert!(e.contains(&("off".into(), "on".into(), Some("Start".into()))));
        assert!(e.contains(&("running".into(), "off".into(), Some("Stop".into()))), "dotted source path: {e:?}");
        assert!(e.contains(&("idle".into(), "running".into(), Some("Go".into()))));
        assert!(e.contains(&("dark".into(), "(final)".into(), None)));
        assert_eq!(sm.unresolved, 1, "`nowhere` is not a state");
        let on = sm.states.iter().find(|s| s.name == "on").unwrap();
        assert!(on.parallel);
        // Regions stack vertically.
        let (m, l) = (sm.states.iter().find(|s| s.name == "motor").unwrap(), sm.states.iter().find(|s| s.name == "light").unwrap());
        assert!(m.rect[3] <= l.rect[1]);
        // The exhibited state shows the definition's states.
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let ex = StateMachine::build(&index, index.get("P::Robot::ops").unwrap());
        assert_eq!(ex.states[0].title, "ops : Ops");
        assert!(ex.states.iter().any(|s| s.name == "running"));
        let c = candidates(&index, |_| true);
        assert_eq!(c, ["P::Ops", "P::Robot::ops"]);
    }
}
