//! The allocation matrix: behavior (actions, states) against the structure
//! that carries it out (parts). Cells come from `allocate a to b` (on its own
//! or in an `allocation` usage), and from `perform` and `exhibit` inside a
//! part, the usual way to say a part carries out an action or a state machine.
//!
//! Rows are every action definition and package-level action of the project,
//! plus whatever is allocated; one that nothing allocates is flagged.

use crate::requirements::walk;
use std::collections::HashMap;
use crate::ibd::layers;
use sysml::{ConnectorKind, EntryId, Index, MemberKind, Model, Reference, Role, UsageKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocKind {
    Allocate,
    Perform,
    Exhibit,
}

impl AllocKind {
    pub fn letter(self) -> char {
        match self {
            AllocKind::Allocate => 'A',
            AllocKind::Perform => 'P',
            AllocKind::Exhibit => 'E',
        }
    }
}

#[derive(Clone, Debug)]
pub struct Axis {
    pub qualified_name: String,
    pub name: String,
    /// `action def`, `part`, ...
    pub keyword: String,
    /// The project file declaring it, when not the edited one (for labels).
    pub file: Option<String>,
    pub source: usize,
    id: EntryId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocCell {
    pub row: usize,
    pub col: usize,
    pub kind: AllocKind,
}

#[derive(Clone, Debug, Default)]
pub struct Allocations {
    pub rows: Vec<Axis>,
    pub columns: Vec<Axis>,
    pub cells: Vec<AllocCell>,
    /// `allocate` ends that name nothing found.
    pub unresolved: usize,
}

fn keyword(index: &Index, id: EntryId) -> String {
    match &index.entry(id).member.kind {
        MemberKind::Definition(d) => format!("{} def", d.kind),
        MemberKind::Usage(u) => u.kind.keyword().to_string(),
        _ => "element".to_string(),
    }
}

/// A feature chain (`drone.battery`), each step looked up in the element
/// before it and in its types (`battery` is declared by `drone`'s `Drone`).
pub(crate) fn resolve_chain(index: &Index, scope: Option<EntryId>, r: &Reference) -> Option<EntryId> {
    let (first, rest) = r.chain.split_first()?;
    let mut at = index.resolve(scope, first)?;
    for qn in rest {
        for seg in &qn.segments {
            at = layers(index, &[at]).into_iter().find_map(|l| index.entry(l).children.iter().copied().find(|&c| index.entry(c).name == seg.text))?;
        }
    }
    Some(at)
}

fn is_structure(index: &Index, id: EntryId) -> bool {
    let kind = match &index.entry(id).member.kind {
        MemberKind::Definition(d) => d.kind.usage_kind(),
        MemberKind::Usage(u) => u.kind,
        _ => return false,
    };
    matches!(kind, UsageKind::Part | UsageKind::Item | UsageKind::Occurrence)
}

impl Allocations {
    /// Over the entries `source_of` maps to a project file (`None`: a library).
    pub fn build<'m>(index: &Index<'m>, models: &[&'m Model], source_of: impl Fn(EntryId) -> Option<usize>) -> Self {
        let mut out = Allocations::default();
        let mut row_of: HashMap<EntryId, usize> = HashMap::new();
        let mut col_of: HashMap<EntryId, usize> = HashMap::new();
        let axis = |id: EntryId, source: usize| {
            let e = index.entry(id);
            Axis { qualified_name: e.qualified_name.clone(), name: e.name.to_string(), keyword: keyword(index, id), file: None, source, id }
        };

        // Functions that should be allocated somewhere.
        for (id, e) in index.entries().iter().enumerate() {
            let Some(source) = source_of(id) else { continue };
            let function = match &e.member.kind {
                MemberKind::Definition(d) => d.kind.usage_kind() == UsageKind::Action,
                MemberKind::Usage(u) => {
                    u.kind == UsageKind::Action
                        && u.prefix.role.is_none()
                        && e.parent.is_none_or(|p| matches!(index.entry(p).member.kind, MemberKind::Package(_)))
                }
                _ => false,
            };
            if function {
                row_of.insert(id, out.rows.len());
                out.rows.push(axis(id, source));
            }
        }

        let mut links: Vec<(EntryId, EntryId, AllocKind)> = Vec::new();
        let mut unresolved = 0;
        walk(index, models, &mut |m, scope| {
            let MemberKind::Usage(u) = &m.kind else { return };
            if let Some(c) = u.connector.as_ref().filter(|c| c.kind == ConnectorKind::Allocate && c.ends.len() >= 2) {
                match (resolve_chain(index, scope, &c.ends[0].target), resolve_chain(index, scope, &c.ends[1].target)) {
                    (Some(a), Some(b)) => links.push((a, b, AllocKind::Allocate)),
                    _ => unresolved += 1,
                }
                return;
            }
            let kind = match u.prefix.role {
                Some(Role::Perform) => AllocKind::Perform,
                Some(Role::Exhibit) => AllocKind::Exhibit,
                _ => return,
            };
            let Some(owner) = scope.filter(|&s| is_structure(index, s)) else { return };
            // `perform x;` names what it performs; `perform action a : A;` is typed by it.
            let target = u
                .references
                .first()
                .and_then(|r| resolve_chain(index, scope, r))
                .or_else(|| u.typed_by.first().and_then(|t| index.resolve(scope, &t.name)))
                .or_else(|| index.id_of(m));
            if let Some(t) = target {
                links.push((t, owner, kind));
            }
        });
        out.unresolved = unresolved;

        for (from, to, kind) in links {
            // Only what the project declares; the library is not shown.
            let (Some(fs), Some(ts)) = (source_of(from), source_of(to)) else { continue };
            let row = *row_of.entry(from).or_insert_with(|| {
                out.rows.push(axis(from, fs));
                out.rows.len() - 1
            });
            let col = *col_of.entry(to).or_insert_with(|| {
                out.columns.push(axis(to, ts));
                out.columns.len() - 1
            });
            let cell = AllocCell { row, col, kind };
            if !out.cells.contains(&cell) {
                out.cells.push(cell);
            }
        }
        out.sort();
        out
    }

    /// Rows and columns in declaration order.
    fn sort(&mut self) {
        let order = |axes: &mut Vec<Axis>| {
            let mut idx: Vec<usize> = (0..axes.len()).collect();
            idx.sort_by_key(|&i| axes[i].id);
            let mut new_of = vec![0; axes.len()];
            for (new, &old) in idx.iter().enumerate() {
                new_of[old] = new;
            }
            let old = std::mem::take(axes);
            let mut slots: Vec<Option<Axis>> = old.into_iter().map(Some).collect();
            *axes = idx.iter().map(|&i| slots[i].take().expect("each index once")).collect();
            new_of
        };
        let rows = order(&mut self.rows);
        let cols = order(&mut self.columns);
        for c in &mut self.cells {
            c.row = rows[c.row];
            c.col = cols[c.col];
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> impl Iterator<Item = &AllocCell> {
        self.cells.iter().filter(move |c| c.row == row && c.col == col)
    }

    /// One line per row: the row, then the letters of each column's cells.
    pub fn to_csv(&self) -> String {
        let q = crate::requirements::csv_quote;
        let mut out = String::from("Element,Kind,File");
        for c in &self.columns {
            out += &format!(",{}", q(&c.qualified_name));
        }
        out.push('\n');
        for (r, row) in self.rows.iter().enumerate() {
            out += &format!("{},{},{}", q(&row.qualified_name), q(&row.keyword), q(row.file.as_deref().unwrap_or("")));
            for c in 0..self.columns.len() {
                let letters: String = self.cell(r, c).map(|x| x.kind.letter()).collect();
                out += &format!(",{letters}");
            }
            out.push('\n');
        }
        out
    }

    /// Nothing allocates the function of row `row`.
    pub fn unallocated(&self, row: usize) -> bool {
        !self.cells.iter().any(|c| c.row == row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_perform_and_exhibit() {
        let src = "package P {
            action def Deliver;
            action def Charge;
            action def Navigate;
            state def Modes;
            part def Drone {
                part battery;
                perform action deliver : Deliver;
                exhibit state modes : Modes;
            }
            part drone : Drone;
            allocate Charge to drone.battery;
            allocation a : Alloc allocate Navigate to drone;
            allocate Missing to drone;
        }";
        let model = sysml::parse(src).model;
        let index = Index::build(&model);
        let a = Allocations::build(&index, &[&model], |_| Some(0));
        let rows: Vec<&str> = a.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(rows, ["Deliver", "Charge", "Navigate", "Modes"]);
        let cols: Vec<&str> = a.columns.iter().map(|c| c.qualified_name.as_str()).collect();
        assert_eq!(cols, ["P::Drone", "P::Drone::battery", "P::drone"]);
        let at = |r: &str, c: &str| {
            let r = a.rows.iter().position(|x| x.name == r).unwrap();
            let c = a.columns.iter().position(|x| x.qualified_name == c).unwrap();
            a.cell(r, c).map(|c| c.kind).collect::<Vec<_>>()
        };
        assert_eq!(at("Deliver", "P::Drone"), [AllocKind::Perform]);
        assert_eq!(at("Modes", "P::Drone"), [AllocKind::Exhibit]);
        assert_eq!(at("Charge", "P::Drone::battery"), [AllocKind::Allocate]);
        assert_eq!(at("Navigate", "P::drone"), [AllocKind::Allocate]);
        assert_eq!(a.unresolved, 1);
        assert!(!a.unallocated(0));
    }
}
