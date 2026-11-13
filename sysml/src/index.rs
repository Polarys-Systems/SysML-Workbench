//! Qualified-name index and simple name resolution over a parsed [`Model`].
//!
//! Resolution follows the lexical scopes (innermost first), `import`s
//! (`A::B`, `A::*`, `A::**`, `A::*::**`) and `alias`es. It does not look at
//! inherited features (members of supertypes), which needs full type analysis.

use crate::ast::*;
use std::collections::HashMap;

/// Guards against import cycles.
const MAX_IMPORT_DEPTH: usize = 16;

/// A named member, with its position in the namespace tree.
#[derive(Clone, Debug)]
pub struct Entry<'m> {
    /// `Package::Part::feature`
    pub qualified_name: String,
    pub name: &'m str,
    pub member: &'m Member,
    pub parent: Option<EntryId>,
    pub children: Vec<EntryId>,
}

/// Position of an [`Entry`] in [`Index::entries`].
pub type EntryId = usize;

/// All named members of a model, by qualified name.
#[derive(Clone, Debug, Default)]
pub struct Index<'m> {
    entries: Vec<Entry<'m>>,
    by_qualified_name: HashMap<String, EntryId>,
    roots: Vec<EntryId>,
    root_imports: Vec<&'m Import>,
    /// Imports owned by each entry, parallel to `entries`.
    imports: Vec<Vec<&'m Import>>,
}

impl<'m> Index<'m> {
    pub fn build(model: &'m Model) -> Self {
        let mut index = Index::default();
        index.add_members(&model.members, None);
        index
    }

    fn add_members(&mut self, members: &'m [Member], parent: Option<EntryId>) {
        for member in members {
            if let MemberKind::Import(import) = &member.kind {
                match parent {
                    Some(p) => self.imports[p].push(import),
                    None => self.root_imports.push(import),
                }
                continue;
            }
            // Members of anonymous elements are not addressable by name.
            let Some(name) = member.kind.effective_name() else { continue };
            let qualified_name = match parent {
                Some(p) => format!("{}::{name}", self.entries[p].qualified_name),
                None => name.to_string(),
            };
            let id = self.entries.len();
            self.by_qualified_name.entry(qualified_name.clone()).or_insert(id);
            self.entries.push(Entry { qualified_name, name, member, parent, children: Vec::new() });
            self.imports.push(Vec::new());
            match parent {
                Some(p) => self.entries[p].children.push(id),
                None => self.roots.push(id),
            }
            self.add_members(member.kind.body(), Some(id));
        }
    }

    pub fn entries(&self) -> &[Entry<'m>] {
        &self.entries
    }

    pub fn entry(&self, id: EntryId) -> &Entry<'m> {
        &self.entries[id]
    }

    /// Top-level named members.
    pub fn roots(&self) -> &[EntryId] {
        &self.roots
    }

    /// Looks up an exact qualified name such as `"Vehicles::Vehicle::mass"`.
    pub fn get(&self, qualified_name: &str) -> Option<EntryId> {
        self.by_qualified_name.get(qualified_name).copied()
    }

    /// Resolves `name` as written inside `scope` (`None` = top level).
    pub fn resolve(&self, scope: Option<EntryId>, name: &QualifiedName) -> Option<EntryId> {
        self.resolve_at(scope, name, true, 0)
    }

    /// Resolves the first element of a feature chain (`a.b.c` → `a`), then each
    /// following element as a member of the previous one.
    pub fn resolve_reference(&self, scope: Option<EntryId>, r: &Reference) -> Option<EntryId> {
        let (first, rest) = r.chain.split_first()?;
        let mut found = self.resolve(scope, first)?;
        for qn in rest {
            for seg in &qn.segments {
                found = self.member_named(Some(found), &seg.text, true, 0)?;
            }
        }
        Some(found)
    }

    fn resolve_at(
        &self,
        scope: Option<EntryId>,
        name: &QualifiedName,
        use_imports: bool,
        depth: usize,
    ) -> Option<EntryId> {
        if depth > MAX_IMPORT_DEPTH {
            return None;
        }
        let (first, rest) = name.segments.split_first()?;
        let mut scope = scope;
        let mut found = loop {
            if let Some(found) = self.member_named(scope, &first.text, use_imports, depth) {
                break found;
            }
            // Walk outward; `None` (the top level) is the last scope tried.
            scope = self.entries[scope?].parent;
        };
        for seg in rest {
            found = self.member_named(Some(found), &seg.text, use_imports, depth)?;
        }
        Some(found)
    }

    /// A member of `scope` named `name`: owned directly, or imported.
    fn member_named(
        &self,
        scope: Option<EntryId>,
        name: &str,
        use_imports: bool,
        depth: usize,
    ) -> Option<EntryId> {
        let children = match scope {
            Some(s) => &self.entries[s].children,
            None => &self.roots,
        };
        if let Some(&c) = children.iter().find(|&&c| self.entries[c].name == name) {
            return Some(self.follow_alias(c, depth));
        }
        if !use_imports || depth > MAX_IMPORT_DEPTH {
            return None;
        }
        let imports = match scope {
            Some(s) => &self.imports[s],
            None => &self.root_imports,
        };
        for import in imports {
            // Import targets are resolved lexically, without imports, to avoid cycles.
            let Some(target) = self.resolve_at(scope, &import.target, false, depth + 1) else {
                continue;
            };
            let target = self.follow_alias(target, depth);
            let hit = if import.is_namespace {
                self.member_named(Some(target), name, true, depth + 1)
            } else {
                (self.entries[target].name == name).then_some(target)
            };
            let hit = hit.or_else(|| {
                import.is_recursive.then(|| self.descendant_named(target, name)).flatten()
            });
            if hit.is_some() {
                return hit;
            }
        }
        None
    }

    fn descendant_named(&self, root: EntryId, name: &str) -> Option<EntryId> {
        let mut stack = self.entries[root].children.clone();
        while let Some(id) = stack.pop() {
            if self.entries[id].name == name {
                return Some(id);
            }
            stack.extend(&self.entries[id].children);
        }
        None
    }

    fn follow_alias(&self, id: EntryId, depth: usize) -> EntryId {
        if let MemberKind::Alias(alias) = &self.entries[id].member.kind
            && let Some(target) =
                self.resolve_at(self.entries[id].parent, &alias.target, true, depth + 1)
        {
            return target;
        }
        id
    }
}
