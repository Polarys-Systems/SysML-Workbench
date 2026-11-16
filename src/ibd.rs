//! Interconnection view: the parts of one element nested inside it, their
//! ports on the borders, and the connections, interfaces, bindings and flows
//! between them (the successor of the SysML v1 internal block diagram).
//!
//! The parts and ports of an element come from its own body and from its
//! types and their supertypes, so `part drone1 : Drone` shows what `Drone`
//! declares, with `:>> battery { ... }` redefinitions merged in. Connector
//! ends are feature chains (`battery.powerOut`) resolved against those paths.
//!
//! Layout is automatic: connected parts are ranked left to right in the
//! direction of their connections, ports sit on the side facing what they
//! connect to, and links are drawn as orthogonal elbows.

use crate::diagram::{Metrics, TextRole};
use std::collections::{HashMap, HashSet, VecDeque};
use sysml::{ConnectorKind, Direction, EntryId, Index, Member, MemberKind, Usage, UsageKind};

pub type Rect = [f32; 4];

/// Nesting levels expanded below the root.
const MAX_DEPTH: usize = 3;
const MAX_PARTS: usize = 400;
/// Declaring members and types gathered for one element.
const MAX_LAYERS: usize = 32;

/// Port square side, in diagram units.
pub const PORT: f32 = 12.0;
/// Distance between port centers along a side.
const PORT_STEP: f32 = 28.0;
const PAD: f32 = 16.0;
/// Between child columns, and between children in a column.
const GAP: [f32; 2] = [96.0, 34.0];
/// Straight piece of a link next to a port before it bends.
const STUB: f32 = 18.0;
/// Distance between parallel links.
const LANE: f32 = 14.0;
const MIN_SIZE: [f32; 2] = [150.0, 64.0];
/// Room between a port label and its port.
pub const LABEL_GAP: f32 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    fn normal(self) -> [f32; 2] {
        match self {
            Side::Left => [-1.0, 0.0],
            Side::Right => [1.0, 0.0],
            Side::Top => [0.0, -1.0],
            Side::Bottom => [0.0, 1.0],
        }
    }

    fn horizontal(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }
}

#[derive(Clone, Debug)]
pub struct Part {
    /// `battery : BatteryPack [4]`
    pub title: String,
    /// `part`, `item`, `part def` (for a root definition).
    pub keyword: String,
    /// A reference (`ref part`): drawn with a dashed border.
    pub is_ref: bool,
    /// Multiplicity above one: drawn as a stack.
    pub multiple: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub ports: Vec<usize>,
    pub depth: usize,
    /// The most specific declaration, to show it in the code.
    pub qualified_name: String,
    /// The definition typing this part, to open its own interconnection view.
    pub type_name: Option<String>,
    /// Inner parts exist but are below the depth limit.
    pub truncated: bool,
    /// Absolute, in diagram units (after [`Ibd::layout`]).
    pub rect: Rect,
    /// Height of the title row.
    pub header: f32,
}

#[derive(Clone, Debug)]
pub struct Port {
    #[cfg_attr(not(test), allow(dead_code))]
    pub name: String,
    /// `powerOut : PowerPort`, `powerIn : ~PowerPort`
    pub label: String,
    pub owner: usize,
    /// After conjugation.
    pub direction: Option<Direction>,
    pub conjugated: bool,
    /// The port definition (qualified name), when it resolves.
    pub type_name: Option<String>,
    pub qualified_name: String,
    pub side: Side,
    pub center: [f32; 2],
    pub label_width: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum End {
    Part(usize),
    Port(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkKind {
    Connection,
    Interface,
    Binding,
    Flow,
}

#[derive(Clone, Debug)]
pub struct Link {
    pub kind: LinkKind,
    pub from: End,
    pub to: End,
    pub label: Option<String>,
    /// The routed polyline (after [`Ibd::layout`]).
    pub points: Vec<[f32; 2]>,
    /// Offset among links joining the same two ends (0 for a single link),
    /// so parallel links and their labels do not cover each other.
    pub lane: f32,
    /// Measured label width (after [`Ibd::layout`]).
    pub label_width: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Ibd {
    /// `parts[0]` is the root; owners come before what they own.
    pub parts: Vec<Part>,
    pub ports: Vec<Port>,
    pub links: Vec<Link>,
    /// Connector ends that name nothing shown (for the status bar).
    pub unresolved: usize,
}

/// Elements worth an interconnection view, best first: parts and part
/// definitions with inner parts, ports or connectors. Only `in_source`
/// entries (the file being edited) are offered.
pub fn candidates(index: &Index, in_source: impl Fn(EntryId) -> bool) -> Vec<String> {
    let mut scored: Vec<(usize, usize, String)> = Vec::new();
    for (id, e) in index.entries().iter().enumerate() {
        if !in_source(id) {
            continue;
        }
        let is_part = match &e.member.kind {
            MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::Part,
            MemberKind::Usage(u) => u.kind == UsageKind::Part && u.prefix.role.is_none() && u.connector.is_none(),
            _ => false,
        };
        if !is_part {
            continue;
        }
        let layers = layers(index, &[id]);
        let mut inner = 0;
        let mut links = 0;
        for &l in &layers {
            for m in index.entry(l).member.kind.body() {
                if let MemberKind::Usage(u) = &unwrap_then(m).kind {
                    if u.connector.as_ref().is_some_and(|c| c.ends.len() >= 2 && c.kind != ConnectorKind::Succession) {
                        links += 1;
                    } else if matches!(u.kind, UsageKind::Part | UsageKind::Item | UsageKind::Port) {
                        inner += 1;
                    }
                }
            }
        }
        if inner + links > 0 {
            scored.push((links, inner, e.qualified_name.clone()));
        }
    }
    // Most connections first, then most inner features; stable for equal scores.
    scored.sort_by_key(|s| std::cmp::Reverse((s.0, s.1)));
    scored.into_iter().map(|(_, _, q)| q).collect()
}

fn unwrap_then(m: &Member) -> &Member {
    match &m.kind {
        MemberKind::Then(inner) => unwrap_then(inner),
        _ => m,
    }
}

fn usage<'m>(index: &Index<'m>, id: EntryId) -> Option<&'m Usage> {
    match &index.entry(id).member.kind {
        MemberKind::Usage(u) => Some(u),
        _ => None,
    }
}

/// The entries describing an element: the given declarations, then their
/// types and the supertypes of those, most specific first.
pub(crate) fn layers(index: &Index, seeds: &[EntryId]) -> Vec<EntryId> {
    let mut out = Vec::new();
    let mut queue: VecDeque<EntryId> = seeds.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if out.contains(&id) || out.len() >= MAX_LAYERS {
            continue;
        }
        out.push(id);
        let e = index.entry(id);
        match &e.member.kind {
            MemberKind::Usage(u) => {
                queue.extend(u.typed_by.iter().filter_map(|t| index.resolve(e.parent, &t.name)));
            }
            MemberKind::Definition(d) => {
                queue.extend(d.specializes.iter().filter_map(|b| index.resolve(e.parent, b)));
            }
            _ => {}
        }
    }
    out
}

/// Named features over `layers`, grouped by name in order of first
/// appearance; each group lists its declarations most specific first.
fn features(index: &Index, layers: &[EntryId]) -> Vec<(String, Vec<EntryId>)> {
    let mut groups: Vec<(String, Vec<EntryId>)> = Vec::new();
    for &l in layers {
        for &c in &index.entry(l).children {
            let Some(u) = usage(index, c) else { continue };
            if u.connector.is_some() || u.prefix.role.is_some() || u.prefix.is_end {
                continue;
            }
            let name = index.entry(c).name;
            match groups.iter_mut().find(|(n, _)| n == name) {
                Some((_, ids)) => ids.push(c),
                None => groups.push((name.to_string(), vec![c])),
            }
        }
    }
    groups
}

/// The kind a group of declarations agrees on: a redefinition without a
/// keyword (`:>> battery { }`) takes the kind of what it redefines.
fn group_kind(index: &Index, group: &[EntryId]) -> UsageKind {
    group.iter().filter_map(|&id| usage(index, id)).map(|u| u.kind).find(|k| *k != UsageKind::Feature).unwrap_or(UsageKind::Feature)
}

struct Builder<'a, 'm> {
    index: &'a Index<'m>,
    ibd: Ibd,
    /// `battery.powerOut` → the port.
    paths: HashMap<String, End>,
    /// Links by full end paths, resolved once every part is known.
    pending: Vec<(LinkKind, String, String, Option<String>)>,
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() { name.to_string() } else { format!("{path}.{name}") }
}

impl Builder<'_, '_> {
    fn add_part(&mut self, parent: Option<usize>, path: &str, group: &[EntryId], depth: usize) -> usize {
        let index = self.index;
        let first = index.entry(group[0]);
        let usages: Vec<&Usage> = group.iter().filter_map(|&id| usage(index, id)).collect();
        let ty = usages.iter().find_map(|u| u.typed_by.first());
        let mult = usages.iter().find_map(|u| u.multiplicity.as_ref());
        let mut title = first.name.to_string();
        if let Some(t) = ty {
            title += &format!(" : {}{}", if t.conjugated { "~" } else { "" }, t.name.last().text);
        }
        if let Some(m) = mult {
            title += &format!(" {m}");
        }
        let multiple = mult.is_some_and(|m| match &m.upper.kind {
            sysml::ExprKind::Literal(sysml::Literal::Integer(n)) => *n > 1,
            sysml::ExprKind::Infinity => true,
            _ => false,
        });
        let keyword = match &first.member.kind {
            MemberKind::Definition(d) => format!("{} def", d.kind),
            _ => group_kind(index, group).keyword().to_string(),
        };
        let type_name = group.iter().find_map(|&id| {
            let e = index.entry(id);
            let u = usage(index, id)?;
            let t = u.typed_by.first()?;
            index.resolve(e.parent, &t.name).map(|t| index.entry(t).qualified_name.clone())
        });
        let id = self.ibd.parts.len();
        self.ibd.parts.push(Part {
            title,
            keyword,
            is_ref: usages.iter().any(|u| u.prefix.is_ref || u.kind == UsageKind::Ref),
            multiple,
            parent,
            children: Vec::new(),
            ports: Vec::new(),
            depth,
            qualified_name: first.qualified_name.clone(),
            type_name,
            truncated: false,
            rect: [0.0; 4],
            header: 0.0,
        });
        self.paths.insert(path.to_string(), End::Part(id));

        let layers = layers(index, group);
        for (name, fgroup) in features(index, &layers) {
            let child_path = join(path, &name);
            match group_kind(index, &fgroup) {
                UsageKind::Port => self.add_port(id, &child_path, &name, &fgroup),
                UsageKind::Part | UsageKind::Item => {
                    if depth >= MAX_DEPTH || self.ibd.parts.len() >= MAX_PARTS {
                        self.ibd.parts[id].truncated = true;
                    } else {
                        let c = self.add_part(Some(id), &child_path, &fgroup, depth + 1);
                        self.ibd.parts[id].children.push(c);
                    }
                }
                _ => {}
            }
        }

        // Connectors are often anonymous, so they are read from the bodies.
        let mut seen = HashSet::new();
        for &l in &layers {
            for m in index.entry(l).member.kind.body() {
                let m = unwrap_then(m);
                let MemberKind::Usage(u) = &m.kind else { continue };
                let Some(c) = &u.connector else { continue };
                if c.ends.len() < 2 || !seen.insert(m as *const Member) {
                    continue;
                }
                let kind = match c.kind {
                    ConnectorKind::Connect if u.kind == UsageKind::Interface => LinkKind::Interface,
                    ConnectorKind::Connect => LinkKind::Connection,
                    ConnectorKind::Bind => LinkKind::Binding,
                    ConnectorKind::Flow | ConnectorKind::Message => LinkKind::Flow,
                    ConnectorKind::Allocate | ConnectorKind::Succession => continue,
                };
                let name = u.id.display_name().map(|n| n.text.clone());
                let ty = u.typed_by.first().map(|t| t.name.last().text.clone());
                let label = match kind {
                    LinkKind::Flow => c.item_type.as_ref().map(|t| t.name.last().text.clone()).or(name),
                    _ => match (name, ty) {
                        (Some(n), Some(t)) => Some(format!("{n} : {t}")),
                        (n, t) => n.or(t),
                    },
                };
                let end = |i: usize| {
                    let segs: Vec<&str> = c.ends[i].target.chain.iter().flat_map(|q| q.segments.iter().map(|s| s.text.as_str())).collect();
                    join(path, &segs.join("."))
                };
                // `connect (a, b, c)`: the first end to each of the others.
                for i in 1..c.ends.len() {
                    self.pending.push((kind, end(0), end(i), label.clone()));
                }
            }
        }
        id
    }

    fn add_port(&mut self, owner: usize, path: &str, name: &str, group: &[EntryId]) {
        let index = self.index;
        let usages: Vec<&Usage> = group.iter().filter_map(|&id| usage(index, id)).collect();
        let ty = usages.iter().find_map(|u| u.typed_by.first());
        let conjugated = ty.is_some_and(|t| t.conjugated);
        let mut direction = usages.iter().find_map(|u| u.prefix.direction);
        if direction.is_none() {
            // From the directed features of the port definition.
            let dirs: Vec<Direction> = features(index, &layers(index, group))
                .iter()
                .filter_map(|(_, g)| g.iter().filter_map(|&id| usage(index, id)).find_map(|u| u.prefix.direction))
                .collect();
            direction = match () {
                _ if dirs.is_empty() => None,
                _ if dirs.iter().all(|d| *d == Direction::In) => Some(Direction::In),
                _ if dirs.iter().all(|d| *d == Direction::Out) => Some(Direction::Out),
                _ => Some(Direction::InOut),
            };
            if conjugated {
                direction = direction.map(|d| match d {
                    Direction::In => Direction::Out,
                    Direction::Out => Direction::In,
                    d => d,
                });
            }
        }
        let label = match ty {
            Some(t) => format!("{name} : {}{}", if t.conjugated { "~" } else { "" }, t.name.last().text),
            None => name.to_string(),
        };
        let id = self.ibd.ports.len();
        self.ibd.ports.push(Port {
            name: name.to_string(),
            label,
            owner,
            direction,
            conjugated,
            type_name: group.iter().find_map(|&id| {
                let e = index.entry(id);
                let t = usage(index, id)?.typed_by.first()?;
                index.resolve(e.parent, &t.name).map(|t| index.entry(t).qualified_name.clone())
            }),
            qualified_name: index.entry(group[0]).qualified_name.clone(),
            side: Side::Left,
            center: [0.0; 2],
            label_width: 0.0,
        });
        self.ibd.parts[owner].ports.push(id);
        self.paths.insert(path.to_string(), End::Port(id));
    }

    /// The element a path names, or the closest owner shown.
    fn lookup(&self, path: &str) -> Option<End> {
        let mut p = path;
        loop {
            if let Some(e) = self.paths.get(p) {
                return Some(*e);
            }
            p = &p[..p.rfind('.')?];
        }
    }
}

impl Ibd {
    /// The interconnection view of the element `root` (a part or part definition).
    pub fn build(index: &Index, root: EntryId) -> Ibd {
        let mut b = Builder { index, ibd: Ibd::default(), paths: HashMap::new(), pending: Vec::new() };
        b.add_part(None, "", &[root], 0);
        let mut seen = HashSet::new();
        for (kind, from, to, label) in std::mem::take(&mut b.pending) {
            match (b.lookup(&from), b.lookup(&to)) {
                (Some(f), Some(t)) if f != t => {
                    if seen.insert((kind, f, t, label.clone())) {
                        b.ibd.links.push(Link { kind, from: f, to: t, label, points: Vec::new(), lane: 0.0, label_width: 0.0 });
                    }
                }
                (Some(_), Some(_)) => {}
                _ => b.ibd.unresolved += 1,
            }
        }
        b.ibd
    }

    pub fn end_part(&self, end: End) -> usize {
        match end {
            End::Part(p) => p,
            End::Port(p) => self.ports[p].owner,
        }
    }

    fn is_ancestor(&self, ancestor: usize, mut part: usize) -> bool {
        loop {
            if part == ancestor {
                return true;
            }
            match self.parts[part].parent {
                Some(p) => part = p,
                None => return false,
            }
        }
    }

    /// The child of `owner` that contains `part`, if any.
    fn child_containing(&self, owner: usize, mut part: usize) -> Option<usize> {
        loop {
            let parent = self.parts[part].parent?;
            if parent == owner {
                return Some(part);
            }
            part = parent;
        }
    }

    /// The deepest part at a diagram point.
    pub fn part_at(&self, p: [f32; 2]) -> Option<usize> {
        self.parts.iter().rposition(|part| inside(part.rect, p))
    }

    pub fn port_at(&self, p: [f32; 2]) -> Option<usize> {
        let h = PORT * 0.5 + 2.0;
        self.ports.iter().position(|q| (p[0] - q.center[0]).abs() <= h && (p[1] - q.center[1]).abs() <= h)
    }

    pub fn bounds(&self) -> Option<Rect> {
        let r = self.parts.first()?.rect;
        let m = PORT;
        Some([r[0] - m, r[1] - m, r[2] + m, r[3] + m])
    }

    // ---- layout --------------------------------------------------------------------

    /// Sizes and places every part, then the ports and the link routes.
    pub fn layout(&mut self, metrics: &mut dyn Metrics) {
        if self.parts.is_empty() {
            return;
        }
        let title_h = metrics.line_height(TextRole::Name);
        let label_h = metrics.line_height(TextRole::Compartment);
        for port in &mut self.ports {
            port.label_width = metrics.width(TextRole::Compartment, &port.label);
        }
        for link in &mut self.links {
            link.label_width = link.label.as_deref().map_or(0.0, |l| metrics.width(TextRole::Keyword, l));
        }
        // Starting sides from the directions: in on the left, out on the right.
        for i in 0..self.parts.len() {
            let mut counts = [0usize; 2];
            for &p in &self.parts[i].ports.clone() {
                let side = match self.ports[p].direction {
                    Some(Direction::In) => Side::Left,
                    Some(Direction::Out) => Side::Right,
                    _ if counts[0] <= counts[1] => Side::Left,
                    _ => Side::Right,
                };
                counts[usize::from(side == Side::Right)] += 1;
                self.ports[p].side = side;
            }
        }

        // Sizes bottom-up (owners come first, so walk backwards); child
        // offsets are relative to their owner's top-left corner.
        let n = self.parts.len();
        let mut size = vec![[0.0f32; 2]; n];
        let mut offset = vec![[0.0f32; 2]; n];
        for i in (0..n).rev() {
            let header = (PAD * 0.6 + title_h + PAD * 0.5).round();
            self.parts[i].header = header;
            let title_w = metrics.width(TextRole::Name, &self.parts[i].title)
                + metrics.width(TextRole::Keyword, &self.parts[i].keyword)
                + PAD * 1.5;
            let side_w = |side: Side, ibd: &Ibd| {
                ibd.parts[i].ports.iter().filter(|&&p| ibd.ports[p].side == side).map(|&p| ibd.ports[p].label_width).fold(0.0, f32::max)
            };
            let (lw, rw) = (side_w(Side::Left, self), side_w(Side::Right, self));
            let margin = |w: f32| if w > 0.0 { w + PORT * 0.5 + LABEL_GAP + PAD * 0.5 } else { PAD };
            let ports = self.parts[i].ports.len();
            let on_left = self.parts[i].ports.iter().filter(|&&p| self.ports[p].side == Side::Left).count();
            let per_side = on_left.max(ports - on_left);
            let ports_h = header + per_side as f32 * PORT_STEP + PAD * 0.5;
            let more_h = if self.parts[i].truncated { label_h + PAD * 0.5 } else { 0.0 };
            let children = self.parts[i].children.clone();
            let (inner_w, inner_h) = if children.is_empty() {
                (lw + rw + if lw + rw > 0.0 { PORT + 2.0 * LABEL_GAP + PAD } else { 0.0 }, 0.0)
            } else {
                let cols = self.columns(i, &children);
                // Room for the widest label of the links between these children.
                let label_w = self
                    .links
                    .iter()
                    .filter(|l| self.child_containing(i, self.end_part(l.from)).is_some() && self.child_containing(i, self.end_part(l.to)).is_some())
                    .map(|l| l.label_width)
                    .fold(0.0, f32::max);
                let col_gap = GAP[0].max(label_w + 2.0 * STUB + 24.0).min(GAP[0] * 3.5);
                let mut x = 0.0f32;
                let mut col_heights = Vec::new();
                for col in &cols {
                    let h = col.iter().map(|&c| size[c][1]).sum::<f32>() + GAP[1] * (col.len() as f32 - 1.0);
                    col_heights.push(h);
                }
                let inner_h = col_heights.iter().copied().fold(0.0, f32::max);
                for (col, h) in cols.iter().zip(&col_heights) {
                    let w = col.iter().map(|&c| size[c][0]).fold(0.0, f32::max);
                    let mut y = (inner_h - h) * 0.5;
                    for &c in col {
                        offset[c] = [x + (w - size[c][0]) * 0.5, y];
                        y += size[c][1] + GAP[1];
                    }
                    x += w + col_gap;
                }
                (x - col_gap, inner_h)
            };
            let (left, right) = if children.is_empty() { (PAD, PAD) } else { (margin(lw), margin(rw)) };
            let w = (left + inner_w + right).max(title_w + 2.0 * PAD).max(MIN_SIZE[0]).round();
            let h = (header + inner_h + if children.is_empty() { 0.0 } else { PAD } + more_h + PAD * 0.5).max(ports_h).max(MIN_SIZE[1]).round();
            // Center the children horizontally in the room the title may add.
            let extra = (w - left - inner_w - right).max(0.0) * 0.5;
            for &c in &children {
                offset[c][0] += left + extra;
                offset[c][1] += header;
            }
            size[i] = [w, h];
        }
        // Absolute rects top-down.
        for i in 0..n {
            let origin = match self.parts[i].parent {
                Some(p) => [self.parts[p].rect[0] + offset[i][0], self.parts[p].rect[1] + offset[i][1]],
                None => [0.0, 0.0],
            };
            self.parts[i].rect = [origin[0], origin[1], origin[0] + size[i][0], origin[1] + size[i][1]];
        }
        self.place_ports();
        self.route();
    }

    /// Children of `owner` in columns: ranked left to right along the links
    /// between them; unconnected children fill a grid.
    fn columns(&self, owner: usize, children: &[usize]) -> Vec<Vec<usize>> {
        let pos = |c: usize| children.iter().position(|&x| x == c);
        let mut order: Vec<(usize, usize)> = Vec::new();
        for l in &self.links {
            let a = self.child_containing(owner, self.end_part(l.from)).and_then(pos);
            let b = self.child_containing(owner, self.end_part(l.to)).and_then(pos);
            if let (Some(a), Some(b)) = (a, b)
                && a != b
                && !order.contains(&(b, a))
            {
                order.push((a, b));
            }
        }
        let n = children.len();
        let connected: Vec<bool> = (0..n).map(|i| order.iter().any(|&(a, b)| a == i || b == i)).collect();
        let mut rank = vec![0usize; n];
        for _ in 0..n {
            let mut changed = false;
            for &(a, b) in &order {
                if rank[b] < rank[a] + 1 && rank[a] + 1 < n {
                    rank[b] = rank[a] + 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let ranks = rank.iter().zip(&connected).filter(|(_, c)| **c).map(|(r, _)| r + 1).max().unwrap_or(0);
        let mut cols: Vec<Vec<usize>> = vec![Vec::new(); ranks];
        let loose: Vec<usize> = (0..n).filter(|&i| !connected[i]).collect();
        for i in (0..n).filter(|&i| connected[i]) {
            cols[rank[i]].push(children[i]);
        }
        // Unconnected children: a grid of roughly square shape after the ranked columns.
        if !loose.is_empty() {
            let per_col = (loose.len() as f32).sqrt().ceil() as usize;
            for chunk in loose.chunks(per_col.max(1)) {
                cols.push(chunk.iter().map(|&i| children[i]).collect());
            }
        }
        cols.retain(|c| !c.is_empty());
        cols
    }

    fn center_of(&self, end: End) -> [f32; 2] {
        match end {
            End::Port(p) if self.ports[p].center != [0.0, 0.0] => self.ports[p].center,
            _ => center(self.parts[self.end_part(end)].rect),
        }
    }

    /// Puts each connected port on the side facing what it connects to, and
    /// spreads the ports along their sides.
    fn place_ports(&mut self) {
        let capacity = |r: Rect, header: f32, side: Side| {
            let len = if side.horizontal() { r[3] - r[1] - header } else { r[2] - r[0] - 2.0 * PAD };
            ((len - PAD * 0.5) / PORT_STEP).floor().max(1.0) as usize
        };
        // Owners first, so inner ports can aim at the outer ports already placed.
        let mut order: Vec<usize> = (0..self.ports.len()).collect();
        order.sort_by_key(|&p| self.parts[self.ports[p].owner].depth);
        let mut targets: Vec<Option<[f32; 2]>> = vec![None; self.ports.len()];
        for &p in &order {
            let owner = self.ports[p].owner;
            let r = self.parts[owner].rect;
            let others: Vec<[f32; 2]> = self
                .links
                .iter()
                .filter_map(|l| match (l.from, l.to) {
                    (End::Port(a), b) if a == p => Some(b),
                    (a, End::Port(b)) if b == p => Some(a),
                    _ => None,
                })
                .map(|e| self.center_of(e))
                .collect();
            if others.is_empty() {
                continue;
            }
            let t = [others.iter().map(|o| o[0]).sum::<f32>() / others.len() as f32, others.iter().map(|o| o[1]).sum::<f32>() / others.len() as f32];
            targets[p] = Some(t);
            let side = if inside(r, t) {
                // Something inside: the nearest side to it.
                let d = [(Side::Left, t[0] - r[0]), (Side::Right, r[2] - t[0]), (Side::Top, t[1] - r[1]), (Side::Bottom, r[3] - t[1])];
                d.into_iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap().0
            } else {
                let c = center(r);
                let (dx, dy) = ((t[0] - c[0]) / (r[2] - r[0]).max(1.0), (t[1] - c[1]) / (r[3] - r[1]).max(1.0));
                if dx.abs() >= dy.abs() {
                    if dx < 0.0 { Side::Left } else { Side::Right }
                } else if dy < 0.0 {
                    Side::Top
                } else {
                    Side::Bottom
                }
            };
            let used = self.parts[owner].ports.iter().filter(|&&q| q != p && self.ports[q].side == side).count();
            if used < capacity(r, self.parts[owner].header, side) {
                self.ports[p].side = side;
            }
            self.spread(owner, &targets);
        }
        for i in 0..self.parts.len() {
            self.spread(i, &targets);
        }
    }

    /// Spreads the ports of `part` evenly along each side, ordered by what they aim at.
    fn spread(&mut self, part: usize, targets: &[Option<[f32; 2]>]) {
        let r = self.parts[part].rect;
        let header = self.parts[part].header;
        for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
            let mut on: Vec<usize> = self.parts[part].ports.iter().copied().filter(|&p| self.ports[p].side == side).collect();
            let axis = usize::from(side.horizontal());
            on.sort_by(|&a, &b| {
                let ka = targets[a].map_or(f32::MAX, |t| t[axis]);
                let kb = targets[b].map_or(f32::MAX, |t| t[axis]);
                ka.total_cmp(&kb)
            });
            let k = on.len() as f32;
            for (j, &p) in on.iter().enumerate() {
                let f = (j as f32 + 1.0) / (k + 1.0);
                self.ports[p].center = match side {
                    Side::Left => [r[0], r[1] + header + (r[3] - r[1] - header) * f],
                    Side::Right => [r[2], r[1] + header + (r[3] - r[1] - header) * f],
                    Side::Top => [r[0] + (r[2] - r[0]) * f, r[1]],
                    Side::Bottom => [r[0] + (r[2] - r[0]) * f, r[3]],
                };
            }
        }
    }

    /// Where a link leaves an end, and the direction it leaves in.
    fn anchor(&self, end: End, other: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        match end {
            End::Port(p) => {
                let port = &self.ports[p];
                let mut n = port.side.normal();
                // A boundary port linked to something inside its owner leaves inwards.
                if inside(self.parts[port.owner].rect, other) {
                    n = [-n[0], -n[1]];
                }
                (port.center, n)
            }
            End::Part(i) => {
                let r = self.parts[i].rect;
                let c = center(r);
                if inside(r, other) {
                    // An owner linked to something inside it: from the top edge down.
                    return ([other[0].clamp(r[0] + PAD, r[2] - PAD), r[1] + self.parts[i].header], [0.0, 1.0]);
                }
                let (dx, dy) = ((other[0] - c[0]) / (r[2] - r[0]).max(1.0), (other[1] - c[1]) / (r[3] - r[1]).max(1.0));
                if dx.abs() >= dy.abs() {
                    let s = dx.signum();
                    ([if s < 0.0 { r[0] } else { r[2] }, c[1]], [s, 0.0])
                } else {
                    let s = dy.signum();
                    ([c[0], if s < 0.0 { r[1] } else { r[3] }], [0.0, s])
                }
            }
        }
    }

    fn route(&mut self) {
        // Lanes: links between the same ends spread around the middle.
        let pair = |l: &Link| if l.from <= l.to { (l.from, l.to) } else { (l.to, l.from) };
        for i in 0..self.links.len() {
            let same: Vec<usize> = (0..self.links.len()).filter(|&j| pair(&self.links[j]) == pair(&self.links[i])).collect();
            let k = same.iter().position(|&j| j == i).unwrap_or(0) as f32;
            self.links[i].lane = k - (same.len() as f32 - 1.0) * 0.5;
        }
        for i in 0..self.links.len() {
            let (from, to) = (self.links[i].from, self.links[i].to);
            let shift = self.links[i].lane * LANE;
            let (a, na) = self.anchor(from, self.center_of(to));
            let (b, nb) = self.anchor(to, self.center_of(from));
            let p = [a[0] + na[0] * STUB, a[1] + na[1] * STUB];
            let q = [b[0] + nb[0] * STUB, b[1] + nb[1] * STUB];
            let (ha, hb) = (na[1] == 0.0, nb[1] == 0.0);
            let middle: Vec<[f32; 2]> = match (ha, hb) {
                (true, true) => {
                    let mx = (p[0] + q[0]) * 0.5 + shift;
                    vec![[mx, p[1]], [mx, q[1]]]
                }
                (false, false) => {
                    let my = (p[1] + q[1]) * 0.5 + shift;
                    vec![[p[0], my], [q[0], my]]
                }
                (true, false) => vec![[q[0], p[1]]],
                (false, true) => vec![[p[0], q[1]]],
            };
            let mut pts = vec![a, p];
            pts.extend(middle);
            pts.push(q);
            pts.push(b);
            pts.dedup_by(|x, y| (x[0] - y[0]).abs() < 0.01 && (x[1] - y[1]).abs() < 0.01);
            self.links[i].points = pts;
        }
    }

    /// Whether `end` is shown inside the part `part` (or is it).
    pub fn touches(&self, end: End, part: usize) -> bool {
        self.is_ancestor(part, self.end_part(end))
    }
}

fn center(r: Rect) -> [f32; 2] {
    [(r[0] + r[2]) * 0.5, (r[1] + r[3]) * 0.5]
}

fn inside(r: Rect, p: [f32; 2]) -> bool {
    p[0] > r[0] && p[0] < r[2] && p[1] > r[1] && p[1] < r[3]
}

/// Midpoint along a polyline, and the direction of the segment there.
pub fn midpoint(points: &[[f32; 2]]) -> Option<([f32; 2], [f32; 2])> {
    point_along(points, 0.5)
}

/// The point a fraction `f` of the way along a polyline, and the direction there.
pub fn point_along(points: &[[f32; 2]], f: f32) -> Option<([f32; 2], [f32; 2])> {
    let seg_len = |a: [f32; 2], b: [f32; 2]| ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let total: f32 = points.windows(2).map(|w| seg_len(w[0], w[1])).sum();
    let mut left = total * f;
    for w in points.windows(2) {
        let l = seg_len(w[0], w[1]);
        if l > 0.0 && left <= l {
            let f = left / l;
            let d = [(w[1][0] - w[0][0]) / l, (w[1][1] - w[0][1]) / l];
            return Some(([w[0][0] + (w[1][0] - w[0][0]) * f, w[0][1] + (w[1][1] - w[0][1]) * f], d));
        }
        left -= l;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../sysml/tests/data/drone.sysml");

    struct Fixed;
    impl Metrics for Fixed {
        fn width(&mut self, _: TextRole, text: &str) -> f32 {
            text.len() as f32 * 7.0
        }
        fn line_height(&mut self, _: TextRole) -> f32 {
            16.0
        }
    }

    fn ibd_of(src: &str, root: &str) -> Ibd {
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let mut ibd = Ibd::build(&index, index.get(root).unwrap());
        ibd.layout(&mut Fixed);
        ibd
    }

    fn port<'a>(ibd: &'a Ibd, name: &str) -> &'a Port {
        ibd.ports.iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn instance_shows_the_parts_and_ports_of_its_type() {
        let ibd = ibd_of(SAMPLE, "DroneSystem::drone1");
        let titles: Vec<&str> = ibd.parts.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles[0], "drone1 : Drone");
        assert!(titles.contains(&"battery : BatteryPack"), "{titles:?}");
        assert!(titles.contains(&"motors : Motor [4]"), "{titles:?}");
        assert!(ibd.parts.iter().any(|p| p.title.starts_with("cargo") && p.is_ref));
        // The redefinition `:>> battery` keeps the type from `Drone`.
        assert_eq!(port(&ibd, "powerOut").direction, Some(Direction::Out));
        assert_eq!(port(&ibd, "powerIn").direction, Some(Direction::In), "conjugated out is in");
        assert!(port(&ibd, "powerIn").conjugated);
    }

    #[test]
    fn connections_join_ports_and_route_between_them() {
        let ibd = ibd_of(SAMPLE, "DroneSystem::Drone");
        let kinds: Vec<LinkKind> = ibd.links.iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&LinkKind::Interface), "{kinds:?}");
        assert!(kinds.contains(&LinkKind::Flow), "{kinds:?}");
        assert!(kinds.contains(&LinkKind::Binding), "{kinds:?}");
        let out = ibd.ports.iter().position(|p| p.name == "powerOut").unwrap();
        let inn = ibd.ports.iter().position(|p| p.name == "powerIn").unwrap();
        let flow = ibd.links.iter().find(|l| l.kind == LinkKind::Flow).unwrap();
        assert_eq!((flow.from, flow.to), (End::Port(out), End::Port(inn)));
        assert_eq!(flow.label.as_deref(), Some("PowerValue"));
        // Battery feeds the motors: left of them, ports facing each other.
        let (b, m) = (ibd.ports[out].owner, ibd.ports[inn].owner);
        assert!(ibd.parts[b].rect[2] < ibd.parts[m].rect[0]);
        assert_eq!(ibd.ports[out].side, Side::Right);
        assert_eq!(ibd.ports[inn].side, Side::Left);
        // Routes start and end on the ports, with right angles only.
        let pts = &flow.points;
        assert_eq!(pts.first(), Some(&ibd.ports[out].center));
        assert_eq!(pts.last(), Some(&ibd.ports[inn].center));
        assert!(pts.windows(2).all(|w| w[0][0] == w[1][0] || w[0][1] == w[1][1]), "{pts:?}");
        // Children sit inside their owner.
        for p in &ibd.parts {
            if let Some(o) = p.parent {
                let (r, q) = (p.rect, ibd.parts[o].rect);
                assert!(r[0] >= q[0] && r[1] >= q[1] && r[2] <= q[2] && r[3] <= q[3], "{} outside", p.title);
            }
        }
    }

    #[test]
    fn nested_parts_and_boundary_ports() {
        let src = "package P {
            port def DataPort { in item d; }
            part def Sensor { port out1 : ~DataPort; }
            part def Board { part cpu { port in1 : DataPort; } }
            part def Sys {
                port ext : DataPort;
                part s : Sensor;
                part b : Board;
                connect s.out1 to b.cpu.in1;
                connect ext to s;
                connect nowhere.x to s;
            }
        }";
        let ibd = ibd_of(src, "P::Sys");
        let cpu = ibd.parts.iter().position(|p| p.title == "cpu").unwrap();
        assert_eq!(ibd.parts[ibd.parts[cpu].parent.unwrap()].title, "b : Board");
        let link = &ibd.links[0];
        assert_eq!(ibd.end_part(link.to), cpu);
        // `nowhere` names nothing: that connector is counted, not drawn.
        assert_eq!(ibd.unresolved, 1);
        assert_eq!(ibd.links.len(), 2);
        let ext = port(&ibd, "ext");
        assert_eq!(ext.owner, 0);
        assert_eq!(ext.direction, Some(Direction::In));
        assert_eq!(candidates(&Index::build(&sysml::parse(src).model), |_| true)[0], "P::Sys");
    }
}
