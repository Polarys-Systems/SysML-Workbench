//! A general-view diagram built from a SysML model: definitions and
//! package-level usages become nodes; specialization, typing, composition,
//! `satisfy`, dependencies, connections, flows and allocations become edges.
//!
//! Positions come from an automatic layered layout, or from a sidecar
//! `<model>.layout.toml` that stores nodes the user moved.

use crate::config::{DiagramConfig, EdgeVisibility};
use crate::document::Tone;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use sysml::eval::{Report, Status, Verdict};
use sysml::{ConnectorKind, EntryId, Index, Member, MemberKind, Model, Role, Span, UsageKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeStyle {
    Definition,
    Usage,
    Behavior,
    Requirement,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub qualified_name: String,
    /// `«part def»`
    pub keyword: String,
    pub name: String,
    /// Owned features listed in the node body.
    pub lines: Vec<String>,
    pub style: NodeStyle,
    /// Top-left corner in diagram units.
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// Measured widths of the kind label and the name.
    pub title_widths: [f32; 2],
    /// Positioned by the user; the automatic layout leaves it alone.
    pub pinned: bool,
    /// Source of the element (for edits from the diagram).
    pub span: Span,
    /// Source of each compartment line's feature, parallel to `lines`.
    pub line_spans: Vec<Span>,
    /// A definition (rather than a usage).
    pub is_def: bool,
    /// Usage kind (for a definition: the kind of its usages).
    pub kind: UsageKind,
    /// From the last measure, relative to `pos`: name top, name height,
    /// first compartment line top, compartment line height.
    pub rows: [f32; 4],
    /// Computed values and requirement verdicts, shown under the compartment lines.
    pub values: Vec<(String, Tone)>,
    /// For a requirement: the worst verdict of the `satisfy` relationships checking it.
    pub verdict: Option<Verdict>,
    /// Declared in another file of the project (its name): shown because
    /// this file refers to it, but not editable here.
    pub external: Option<String>,
}

impl Node {
    pub fn rect(&self) -> [f32; 4] {
        [self.pos[0], self.pos[1], self.pos[0] + self.size[0], self.pos[1] + self.size[1]]
    }

    /// The kind shown in the node's pill: `part def` for `«part def»`.
    pub fn kind_label(&self) -> &str {
        self.keyword.trim_start_matches('«').trim_end_matches('»')
    }

    pub fn center(&self) -> [f32; 2] {
        [self.pos[0] + self.size[0] * 0.5, self.pos[1] + self.size[1] * 0.5]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    /// `A :> B`: from the specific to the general definition.
    Specialization,
    /// `x : T`: from the usage to its definition.
    Typing,
    /// `part p : T` inside A: from the owner A to the part's type T.
    Composition,
    /// `satisfy R by x`: from x to R.
    Satisfy,
    Dependency,
    /// `connect a to b` (also interfaces and bindings).
    Connection,
    /// `flow from a to b`
    Flow,
    /// `allocate a to b`
    Allocation,
}

impl EdgeKind {
    pub fn visible(self, v: &EdgeVisibility) -> bool {
        match self {
            EdgeKind::Specialization => v.specialization,
            EdgeKind::Typing => v.typing,
            EdgeKind::Composition => v.composition,
            EdgeKind::Satisfy => v.satisfy,
            EdgeKind::Dependency => v.dependency,
            EdgeKind::Connection => v.connection,
            EdgeKind::Flow => v.flow,
            EdgeKind::Allocation => v.allocation,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub kind: EdgeKind,
    pub label: Option<String>,
    /// For `satisfy`: whether the subject meets the requirement.
    pub verdict: Option<Verdict>,
}

/// Text roles whose size the layout needs.
#[derive(Clone, Copy)]
pub enum TextRole {
    Keyword,
    Name,
    Compartment,
}

/// Text measurement supplied by the renderer, in diagram units.
pub trait Metrics {
    fn width(&mut self, role: TextRole, text: &str) -> f32;
    fn line_height(&mut self, role: TextRole) -> f32;
}

/// Padding inside the kind pill (horizontal, vertical) and the gap between the
/// name and the pill, in diagram units.
pub const PILL_PAD: [f32; 2] = [7.0, 2.0];
pub const PILL_GAP: f32 = 12.0;

/// Vertical structure of a node, shared by measuring and drawing: a header
/// row with the name on the left and the kind pill on the right, then the
/// compartment lines under a separator.
pub struct NodeLayout {
    /// Top and height of the kind pill; its text is centered in it.
    pub pill_y: f32,
    pub pill_h: f32,
    pub name_y: f32,
    /// y of the separator line, if the node has compartment lines.
    pub separator_y: Option<f32>,
    pub first_line_y: f32,
    pub line_height: f32,
    /// Top of the first value line (below the compartment lines).
    pub values_y: f32,
}

#[derive(Default)]
pub struct Diagram {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    by_name: HashMap<String, usize>,
}

impl Diagram {
    pub fn node_named(&self, qualified_name: &str) -> Option<usize> {
        self.by_name.get(qualified_name).copied()
    }

    /// The node for `qualified_name`, or for its closest owner that has one.
    pub fn node_for(&self, qualified_name: &str) -> Option<usize> {
        let mut name = qualified_name;
        loop {
            if let Some(n) = self.node_named(name) {
                return Some(n);
            }
            name = &name[..name.rfind("::")?];
        }
    }

    #[cfg(test)]
    pub fn build(model: &Model) -> Self {
        Self::build_in(&Index::build(model), model, &|_| None)
    }

    /// The diagram of `model`, entry source 0 of `index`. Definitions of other
    /// files that it specializes, is typed by or composes show as read-only
    /// nodes when `file_of` names their file (`None`: a library, left out).
    pub fn build_in(index: &Index, model: &Model, file_of: &dyn Fn(usize) -> Option<String>) -> Self {
        let mut d = Diagram::default();
        let mut node_of: Vec<Option<usize>> = vec![None; index.entries().len()];

        // Other files' definitions this file builds on.
        let mut external: Vec<(EntryId, String)> = Vec::new();
        for (id, entry) in index.entries().iter().enumerate() {
            if entry.source != 0 || node_kind(index, id).is_none() {
                continue;
            }
            let mut targets = Vec::new();
            match &entry.member.kind {
                MemberKind::Definition(def) => targets.extend(def.specializes.iter().filter_map(|b| index.resolve(entry.parent, b))),
                MemberKind::Usage(u) => targets.extend(u.typed_by.iter().filter_map(|t| index.resolve(entry.parent, &t.name))),
                _ => {}
            }
            for child in entry.member.kind.body() {
                if let MemberKind::Usage(u) = &child.kind {
                    targets.extend(u.typed_by.iter().filter_map(|t| index.resolve(Some(id), &t.name)));
                }
            }
            for t in targets {
                let source = index.entry(t).source;
                if source != 0 && !external.iter().any(|(e, _)| *e == t) && node_kind(index, t).is_some() && let Some(file) = file_of(source) {
                    external.push((t, file));
                }
            }
        }

        let local = index.entries().iter().enumerate().filter(|(_, e)| e.source == 0).map(|(id, _)| (id, None));
        let ghosts: Vec<(EntryId, Option<String>)> = external.into_iter().map(|(id, f)| (id, Some(f))).collect();
        for (id, file) in local.chain(ghosts) {
            let entry = index.entry(id);
            let Some((keyword, style)) = node_kind(index, id) else { continue };
            let (lines, line_spans): (Vec<String>, Vec<Span>) = if file.is_some() {
                (Vec::new(), Vec::new())
            } else {
                entry
                .member
                .kind
                .body()
                .iter()
                .filter_map(|m| match &m.kind {
                    MemberKind::Usage(u) if u.connector.is_none() && u.kind != UsageKind::Metadata => {
                        Some((m.kind.summary(), m.span))
                    }
                    _ => None,
                })
                .unzip()
            };
            let (is_def, kind) = match &entry.member.kind {
                MemberKind::Definition(def) => (true, def.kind.usage_kind()),
                MemberKind::Usage(u) => (false, u.kind),
                _ => (false, UsageKind::Feature),
            };
            node_of[id] = Some(d.nodes.len());
            d.by_name.insert(entry.qualified_name.clone(), d.nodes.len());
            d.nodes.push(Node {
                qualified_name: entry.qualified_name.clone(),
                keyword,
                name: entry.name.to_string(),
                lines,
                style,
                pos: [0.0, 0.0],
                size: [0.0, 0.0],
                title_widths: [0.0, 0.0],
                pinned: false,
                // Spans of other files mean nothing in this one.
                span: if file.is_some() { Span::default() } else { entry.member.span },
                line_spans,
                is_def,
                kind,
                rows: [0.0; 4],
                values: file.iter().map(|f| (format!("from {f}"), Tone::Muted)).collect(),
                verdict: None,
                external: file,
            });
        }

        // Closest node owning (or being) an entry.
        let owner_node = |mut id: EntryId| loop {
            if let Some(n) = node_of[id] {
                return Some(n);
            }
            id = index.entry(id).parent?;
        };

        let mut seen = HashSet::new();
        let mut add = |d: &mut Diagram, from: usize, to: usize, kind: EdgeKind, label: Option<String>| {
            if from != to && seen.insert((from, to, kind, label.clone())) {
                d.edges.push(Edge { from, to, kind, label, verdict: None });
            }
        };

        for (id, entry) in index.entries().iter().enumerate() {
            let Some(n) = node_of[id] else { continue };
            let scope = entry.parent;
            match &entry.member.kind {
                MemberKind::Definition(def) => {
                    for base in &def.specializes {
                        if let Some(t) = index.resolve(scope, base).and_then(|e| node_of[e]) {
                            add(&mut d, n, t, EdgeKind::Specialization, None);
                        }
                    }
                }
                MemberKind::Usage(u) => {
                    for ty in &u.typed_by {
                        if let Some(t) = index.resolve(scope, &ty.name).and_then(|e| node_of[e]) {
                            add(&mut d, n, t, EdgeKind::Typing, None);
                        }
                    }
                }
                _ => {}
            }
            // Owned parts, items, actions, ... typed by another node.
            for child in entry.member.kind.body() {
                let MemberKind::Usage(u) = &child.kind else { continue };
                let composite = matches!(
                    u.kind,
                    UsageKind::Part | UsageKind::Item | UsageKind::Action | UsageKind::State | UsageKind::Occurrence
                );
                if !composite || u.prefix.is_ref {
                    continue;
                }
                for ty in &u.typed_by {
                    if let Some(t) = index.resolve(Some(id), &ty.name).and_then(|e| node_of[e]) {
                        let mut label = child.kind.effective_name().unwrap_or_default().to_string();
                        if let Some(m) = &u.multiplicity {
                            label += &format!(" {m}");
                        }
                        add(&mut d, n, t, EdgeKind::Composition, Some(label));
                    }
                }
            }
        }

        // `satisfy` and `dependency` members can sit anywhere, named or not.
        let mut stack: Vec<(&[Member], Option<EntryId>)> = vec![(&model.members, None)];
        while let Some((members, scope)) = stack.pop() {
            for m in members {
                match &m.kind {
                    MemberKind::Usage(u) if u.prefix.role == Some(Role::Satisfy) => {
                        let req = u.references.first().and_then(|r| index.resolve_reference(scope, r));
                        let by = u.by.as_ref().and_then(|r| index.resolve_reference(scope, r));
                        if let (Some(req), Some(by)) = (req.and_then(owner_node), by.and_then(owner_node)) {
                            add(&mut d, by, req, EdgeKind::Satisfy, Some("«satisfy»".into()));
                        }
                    }
                    MemberKind::Usage(u) if u.connector.as_ref().is_some_and(|c| c.ends.len() >= 2) => {
                        let c = u.connector.as_ref().unwrap();
                        let kind = match c.kind {
                            ConnectorKind::Connect | ConnectorKind::Bind => Some(EdgeKind::Connection),
                            ConnectorKind::Flow | ConnectorKind::Message => Some(EdgeKind::Flow),
                            ConnectorKind::Allocate => Some(EdgeKind::Allocation),
                            // Successions order behavior; they are not structural edges.
                            ConnectorKind::Succession => None,
                        };
                        if let Some(kind) = kind {
                            let end = |i: usize| index.resolve_reference(scope, &c.ends[i].target).and_then(owner_node);
                            if let (Some(a), Some(b)) = (end(0), end(1)) {
                                let label = match kind {
                                    EdgeKind::Allocation => Some("«allocate»".to_string()),
                                    _ => u.id.display_name().map(|n| n.to_string()),
                                };
                                add(&mut d, a, b, kind, label);
                            }
                        }
                    }
                    MemberKind::Dependency(dep) => {
                        for c in &dep.clients {
                            for s in &dep.suppliers {
                                let c = index.resolve_reference(scope, c).and_then(owner_node);
                                let s = index.resolve_reference(scope, s).and_then(owner_node);
                                if let (Some(c), Some(s)) = (c, s) {
                                    add(&mut d, c, s, EdgeKind::Dependency, None);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let inner = index.id_of(m).or(scope);
                stack.push((m.kind.body(), inner));
            }
        }
        d
    }

    /// Shows computed values in instance nodes, and verdicts on requirements
    /// and their `satisfy` edges.
    pub fn apply_report(&mut self, report: &Report) {
        for inst in &report.instances {
            let Some(i) = self.node_named(&inst.qualified_name) else { continue };
            self.nodes[i].values = inst
                .values
                .iter()
                .map(|v| {
                    let tone = match v.status {
                        Status::Ok => Tone::Value,
                        Status::Unknown => Tone::Muted,
                        Status::Error => Tone::Bad,
                    };
                    (format!("{} = {}", v.name, v.text), tone)
                })
                .collect();
        }
        for c in &report.checks {
            let (Some(req), Some(by)) = (self.node_for(&c.requirement), self.node_for(&c.subject)) else { continue };
            let subject = c.subject.rsplit("::").next().unwrap_or(&c.subject);
            let (word, tone) = match c.verdict {
                Verdict::Satisfied => ("satisfied", Tone::Good),
                Verdict::Violated => ("VIOLATED", Tone::Bad),
                Verdict::Unknown => ("undecided", Tone::Muted),
                Verdict::Error => ("cannot check", Tone::Bad),
            };
            let node = &mut self.nodes[req];
            node.verdict = Some(node.verdict.map_or(c.verdict, |v| worse(v, c.verdict)));
            node.values.push((format!("{word} by {subject}"), tone));
            for e in self.edges.iter_mut().filter(|e| e.kind == EdgeKind::Satisfy && e.from == by && e.to == req) {
                e.verdict = Some(e.verdict.map_or(c.verdict, |v| worse(v, c.verdict)));
            }
        }
    }

    // ---- measuring and layout ----------------------------------------------

    /// The value lines a node shows.
    pub fn visible_values<'a>(node: &'a Node, cfg: &DiagramConfig) -> &'a [(String, Tone)] {
        if cfg.values { &node.values } else { &[] }
    }

    pub fn node_layout(node: &Node, cfg: &DiagramConfig, metrics: &mut dyn Metrics) -> NodeLayout {
        let pad = cfg.node_padding;
        let name_h = metrics.line_height(TextRole::Name);
        let pill_h = metrics.line_height(TextRole::Keyword) + 2.0 * PILL_PAD[1];
        let header_h = name_h.max(pill_h);
        let header_bottom = pad + header_h;
        let (lines, more) = Self::visible_lines(node, cfg);
        let line_rows = lines.len() + usize::from(more);
        let shown = line_rows > 0 || !Self::visible_values(node, cfg).is_empty();
        let first_line_y = if shown { header_bottom + pad * 1.3 } else { header_bottom };
        let line_height = metrics.line_height(TextRole::Compartment);
        NodeLayout {
            pill_y: pad + (header_h - pill_h) * 0.5,
            pill_h,
            name_y: pad + (header_h - name_h) * 0.5,
            separator_y: shown.then_some(header_bottom + pad * 0.7),
            first_line_y,
            line_height,
            values_y: first_line_y + line_rows as f32 * line_height + if line_rows > 0 { pad } else { 0.0 },
        }
    }

    /// The compartment lines a node shows, with a final "…" if some are cut.
    pub fn visible_lines<'a>(node: &'a Node, cfg: &DiagramConfig) -> (&'a [String], bool) {
        if !cfg.compartments {
            return (&[], false);
        }
        let n = node.lines.len().min(cfg.max_compartment_lines);
        (&node.lines[..n], node.lines.len() > n)
    }

    /// The compartment line of `node` at diagram point `world`, if any.
    pub fn line_at(&self, node: usize, world: [f32; 2], cfg: &DiagramConfig) -> Option<usize> {
        let n = self.nodes.get(node)?;
        let (lines, _) = Self::visible_lines(n, cfg);
        let y = world[1] - n.pos[1] - n.rows[2];
        if y < 0.0 || n.rows[3] <= 0.0 {
            return None;
        }
        let i = (y / n.rows[3]) as usize;
        (i < lines.len()).then_some(i)
    }


    /// Computes every node's size from its text.
    pub fn measure(&mut self, cfg: &DiagramConfig, metrics: &mut dyn Metrics) {
        for node in &mut self.nodes {
            let layout = Self::node_layout(node, cfg, metrics);
            let name_h = metrics.line_height(TextRole::Name);
            node.rows = [layout.name_y, name_h, layout.first_line_y, layout.line_height];
            node.title_widths =
                [metrics.width(TextRole::Keyword, node.kind_label()), metrics.width(TextRole::Name, &node.name)];
            let (lines, more) = Self::visible_lines(node, cfg);
            let values = Self::visible_values(node, cfg);
            // Header: name, gap, pill.
            let mut width = node.title_widths[1] + PILL_GAP + node.title_widths[0] + 2.0 * PILL_PAD[0];
            for line in lines.iter().chain(values.iter().map(|(v, _)| v)) {
                width = width.max(metrics.width(TextRole::Compartment, line));
            }
            let rows_end = if values.is_empty() {
                layout.first_line_y + (lines.len() + usize::from(more)) as f32 * layout.line_height
            } else {
                layout.values_y + values.len() as f32 * layout.line_height
            };
            let height = if layout.separator_y.is_some() {
                rows_end + cfg.node_padding * 0.8
            } else {
                layout.first_line_y + cfg.node_padding
            };
            let width = (width + 2.0 * cfg.node_padding).clamp(cfg.node_min_width, cfg.node_max_width.max(cfg.node_min_width));
            node.size = [width.round(), height.round()];
        }
    }

    /// Layered layout: general definitions and owners above specializations and
    /// parts. Pinned nodes keep their positions.
    pub fn auto_layout(&mut self, cfg: &DiagramConfig) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        // (above, below) pairs.
        let order: Vec<(usize, usize)> = self
            .edges
            .iter()
            .filter_map(|e| match e.kind {
                EdgeKind::Specialization | EdgeKind::Typing => Some((e.to, e.from)),
                EdgeKind::Composition => Some((e.from, e.to)),
                _ => None,
            })
            .collect();
        let mut layer = vec![0usize; n];
        // Longest path; the iteration bound also ends cycles.
        for _ in 0..n {
            let mut changed = false;
            for &(above, below) in &order {
                if layer[below] < layer[above] + 1 && layer[above] + 1 < n {
                    layer[below] = layer[above] + 1;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let max_layer = layer.iter().copied().max().unwrap_or(0);
        let mut rows: Vec<Vec<usize>> = Vec::new();
        let mut center_x = vec![0.0f32; n];
        for l in 0..=max_layer {
            let mut members: Vec<usize> = (0..n).filter(|&i| layer[i] == l).collect();
            if l > 0 {
                // Order by the average position of the nodes above.
                let key = |i: usize| {
                    let xs: Vec<f32> = order.iter().filter(|&&(_, b)| b == i).map(|&(a, _)| center_x[a]).collect();
                    if xs.is_empty() { f32::MAX } else { xs.iter().sum::<f32>() / xs.len() as f32 }
                };
                members.sort_by(|&a, &b| key(a).total_cmp(&key(b)));
            }
            for chunk in members.chunks(cfg.max_row.max(1)) {
                let width: f32 = chunk.iter().map(|&i| self.nodes[i].size[0]).sum::<f32>()
                    + cfg.gap[0] * (chunk.len() as f32 - 1.0);
                let mut x = -width * 0.5;
                for &i in chunk {
                    center_x[i] = x + self.nodes[i].size[0] * 0.5;
                    x += self.nodes[i].size[0] + cfg.gap[0];
                }
                rows.push(chunk.to_vec());
            }
        }

        let mut y = 0.0;
        for row in rows {
            let height = row.iter().map(|&i| self.nodes[i].size[1]).fold(0.0, f32::max);
            for i in row {
                let node = &mut self.nodes[i];
                if !node.pinned {
                    node.pos = [(center_x[i] - node.size[0] * 0.5).round(), y];
                }
            }
            y += height + cfg.gap[1];
        }
    }

    // ---- saved positions -----------------------------------------------------

    pub fn layout_path(model: &Path) -> PathBuf {
        let mut p = model.as_os_str().to_owned();
        p.push(".layout.toml");
        p.into()
    }

    /// Applies saved positions; returns how many nodes were pinned.
    pub fn load_positions(&mut self, path: &Path) -> usize {
        let Ok(text) = std::fs::read_to_string(path) else { return 0 };
        let Ok(file) = toml::from_str::<LayoutFile>(&text) else { return 0 };
        let mut count = 0;
        for (name, pos) in file.positions {
            if let Some(&i) = self.by_name.get(&name) {
                self.nodes[i].pos = pos;
                self.nodes[i].pinned = true;
                count += 1;
            }
        }
        count
    }

    /// Writes the pinned nodes' positions (or removes the file if there are none).
    pub fn save_positions(&self, path: &Path) -> std::io::Result<()> {
        let positions: BTreeMap<String, [f32; 2]> =
            self.nodes.iter().filter(|n| n.pinned).map(|n| (n.qualified_name.clone(), n.pos)).collect();
        if positions.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let text = toml::to_string(&LayoutFile { positions }).map_err(std::io::Error::other)?;
        std::fs::write(path, format!("# Node positions saved by EagleView. Delete to restore the automatic layout.\n{text}"))
    }

    /// Bounding box of all nodes.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        self.nodes.iter().map(Node::rect).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
    }
}

/// The verdict to show when several checks meet.
fn worse(a: Verdict, b: Verdict) -> Verdict {
    let rank = |v| match v {
        Verdict::Satisfied => 0,
        Verdict::Unknown => 1,
        Verdict::Error => 2,
        Verdict::Violated => 3,
    };
    if rank(b) > rank(a) { b } else { a }
}

#[derive(Serialize, Deserialize)]
struct LayoutFile {
    positions: BTreeMap<String, [f32; 2]>,
}

/// Whether an index entry becomes a node, with its keyword and style.
fn node_kind(index: &Index, id: EntryId) -> Option<(String, NodeStyle)> {
    let entry = index.entry(id);
    let style_of = |kind: UsageKind, def: bool| match kind {
        UsageKind::Action
        | UsageKind::State
        | UsageKind::Calc
        | UsageKind::Case
        | UsageKind::AnalysisCase
        | UsageKind::VerificationCase
        | UsageKind::UseCase => NodeStyle::Behavior,
        UsageKind::Requirement | UsageKind::Constraint | UsageKind::Concern | UsageKind::Viewpoint => {
            NodeStyle::Requirement
        }
        _ if def => NodeStyle::Definition,
        _ => NodeStyle::Usage,
    };
    match &entry.member.kind {
        MemberKind::Definition(d) => Some((format!("«{} def»", d.kind), style_of(d.kind.usage_kind(), true))),
        MemberKind::Usage(u) => {
            // Only usages owned directly by a package (or the file) get their own node;
            // nested features are listed inside their owner.
            let in_package =
                entry.parent.is_none_or(|p| matches!(index.entry(p).member.kind, MemberKind::Package(_)));
            let shown = u.kind.definition_kind().is_some()
                && !matches!(u.kind, UsageKind::Attribute | UsageKind::Enum | UsageKind::Metadata | UsageKind::Flow);
            (in_package && shown && u.prefix.role.is_none()).then(|| (format!("«{}»", u.kind), style_of(u.kind, false)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../sysml/tests/data/drone.sysml");

    fn edge(d: &Diagram, from: &str, to: &str, kind: EdgeKind) -> bool {
        let (Some(f), Some(t)) = (d.node_named(from), d.node_named(to)) else { return false };
        d.edges.iter().any(|e| e.from == f && e.to == t && e.kind == kind)
    }

    #[test]
    fn builds_nodes_and_edges() {
        let model = sysml::parse(SAMPLE).model;
        let d = Diagram::build(&model);
        let p = "DroneSystem::";
        for name in ["Drone", "Motor", "drone1", "MassLimit", "massReq", "Deliver"] {
            assert!(d.node_named(&format!("{p}{name}")).is_some(), "{name}");
        }
        // Nested features are compartment lines, not nodes.
        assert!(d.node_named("DroneSystem::Drone::battery").is_none());
        assert!(edge(&d, "DroneSystem::Motor", "DroneSystem::Component", EdgeKind::Specialization));
        assert!(edge(&d, "DroneSystem::drone1", "DroneSystem::Drone", EdgeKind::Typing));
        assert!(edge(&d, "DroneSystem::Drone", "DroneSystem::Motor", EdgeKind::Composition));
        assert!(edge(&d, "DroneSystem::drone1", "DroneSystem::massReq", EdgeKind::Satisfy));
        assert_eq!(d.node_for("DroneSystem::Drone::battery"), d.node_named("DroneSystem::Drone"));
    }

    struct Fixed;
    impl Metrics for Fixed {
        fn width(&mut self, _: TextRole, text: &str) -> f32 {
            text.len() as f32 * 7.0
        }
        fn line_height(&mut self, _: TextRole) -> f32 {
            16.0
        }
    }

    #[test]
    fn layout_puts_general_above_specific_and_round_trips() {
        let model = sysml::parse(SAMPLE).model;
        let mut d = Diagram::build(&model);
        let cfg = DiagramConfig::default();
        d.measure(&cfg, &mut Fixed);
        d.auto_layout(&cfg);
        let y = |n: &str| d.nodes[d.node_named(n).unwrap()].pos[1];
        assert!(y("DroneSystem::Component") < y("DroneSystem::Motor"));
        assert!(y("DroneSystem::Drone") < y("DroneSystem::Motor"));

        let path = std::env::temp_dir().join(format!("eagleview-test-{}.layout.toml", std::process::id()));
        let i = d.node_named("DroneSystem::Motor").unwrap();
        d.nodes[i].pos = [123.0, 456.0];
        d.nodes[i].pinned = true;
        d.save_positions(&path).unwrap();
        let mut again = Diagram::build(&model);
        assert_eq!(again.load_positions(&path), 1);
        assert_eq!(again.nodes[i].pos, [123.0, 456.0]);
        std::fs::remove_file(path).unwrap();
    }
}
