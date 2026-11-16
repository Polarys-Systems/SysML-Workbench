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
    /// The short name, when the element also has a regular name (`<kg> kilogram`).
    pub short_name: Option<&'m str>,
    /// Which of the models given to [`Index::build_many`] declares it (0 for [`Index::build`]).
    pub source: usize,
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
    /// Entry of each named member, including the later declarations of a
    /// package split over several files.
    by_member: HashMap<*const Member, EntryId>,
}

impl<'m> Index<'m> {
    pub fn build(model: &'m Model) -> Self {
        Self::build_many(&[model])
    }

    /// One namespace over several files: top-level names of every model are
    /// visible to the others, and a package declared in more than one file
    /// (`package P` in a.sysml and in b.sysml) holds the members of all of them.
    pub fn build_many(models: &[&'m Model]) -> Self {
        let mut index = Index::default();
        for (source, model) in models.iter().enumerate() {
            index.add_members(&model.members, None, source);
        }
        index
    }

    fn add_members(&mut self, members: &'m [Member], parent: Option<EntryId>, source: usize) {
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
            // A package already declared (in another file): add to it.
            if let MemberKind::Package(_) = &member.kind
                && let Some(&existing) = self.by_qualified_name.get(&qualified_name)
                && matches!(self.entries[existing].member.kind, MemberKind::Package(_))
            {
                self.by_member.insert(member as *const Member, existing);
                self.add_members(member.kind.body(), Some(existing), source);
                continue;
            }
            let id = self.entries.len();
            self.by_member.insert(member as *const Member, id);
            self.by_qualified_name.entry(qualified_name.clone()).or_insert(id);
            let short_name = member
                .kind
                .identification()
                .filter(|i| i.name.is_some())
                .and_then(|i| i.short_name.as_ref())
                .map(|n| n.text.as_str());
            self.entries.push(Entry { qualified_name, name, member, parent, children: Vec::new(), short_name, source });
            self.imports.push(Vec::new());
            match parent {
                Some(p) => self.entries[p].children.push(id),
                None => self.roots.push(id),
            }
            self.add_members(member.kind.body(), Some(id), source);
        }
    }

    /// The entry a member declares (for a package split over files: the merged one).
    pub fn id_of(&self, member: &Member) -> Option<EntryId> {
        self.by_member.get(&(member as *const Member)).copied()
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
        let named = |c: &EntryId| self.entries[*c].name == name || self.entries[*c].short_name == Some(name);
        if let Some(&c) = children.iter().find(|c| named(c)) {
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
                named(&target).then_some(target)
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
            if self.entries[id].name == name || self.entries[id].short_name == Some(name) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_split_over_files_merge() {
        let a = crate::parse("package P { part def A; } package Q { part def C; }").model;
        let b = crate::parse("package P { part def B :> A; part q : Q::C; }").model;
        let index = Index::build_many(&[&a, &b]);
        let p = index.get("P").unwrap();
        assert_eq!(index.entry(p).children.len(), 3, "A, B and q in one package");
        let b_id = index.get("P::B").unwrap();
        assert_eq!(index.entry(b_id).source, 1);
        // Names in the second file resolve through the merged package.
        let MemberKind::Definition(def) = &index.entry(b_id).member.kind else { panic!() };
        assert_eq!(index.resolve(Some(p), &def.specializes[0]), index.get("P::A"));
        assert_eq!(index.id_of(&b.members[0]), Some(p), "the second declaration maps to the merged package");
    }
}
