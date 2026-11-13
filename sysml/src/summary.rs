//! One-line, human-readable summaries of members, for outlines and tree views.

use crate::ast::*;
use std::fmt::Display;

/// Broad grouping of members, e.g. for coloring a tree view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Package,
    Import,
    Comment,
    Definition,
    Usage,
    /// Connectors, successions, dependencies, `satisfy`, ...
    Relationship,
    /// Transitions, state actions, accept/send.
    Behavior,
    Expression,
}

fn name(id: &Identification) -> String {
    id.display_name().map(|n| n.to_string()).unwrap_or_default()
}

fn list<T: Display>(prefix: &str, items: &[T]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let joined: Vec<_> = items.iter().map(T::to_string).collect();
    format!(" {prefix} {}", joined.join(", "))
}

impl MemberKind {
    pub fn category(&self) -> Category {
        match self {
            MemberKind::Package(_) => Category::Package,
            MemberKind::Import(_) | MemberKind::Alias(_) => Category::Import,
            MemberKind::Comment(_) => Category::Comment,
            MemberKind::Definition(_) => Category::Definition,
            MemberKind::Usage(u) if u.connector.is_some() || u.id.display_name().is_none() && !u.references.is_empty() => {
                Category::Relationship
            }
            MemberKind::Usage(_) => Category::Usage,
            MemberKind::Dependency(_) => Category::Relationship,
            MemberKind::Transition(_) | MemberKind::StateAction(_) | MemberKind::Accept(_) | MemberKind::Send(_) => {
                Category::Behavior
            }
            MemberKind::Then(m) => m.kind.category(),
            MemberKind::Filter(_) | MemberKind::Expression(_) => Category::Expression,
        }
    }

    /// A one-line description such as `part engine : Engine [1]`.
    pub fn summary(&self) -> String {
        match self {
            MemberKind::Package(p) => format!("package {}", name(&p.id)),
            MemberKind::Import(i) => format!(
                "{} {}{}{}",
                if i.is_expose { "expose" } else { "import" },
                i.target,
                if i.is_namespace { "::*" } else { "" },
                if i.is_recursive { "::**" } else { "" }
            ),
            MemberKind::Alias(a) => format!("alias {} for {}", name(&a.id), a.target),
            MemberKind::Comment(c) => {
                let kind = match c.kind {
                    CommentKind::Doc => "doc",
                    CommentKind::Comment => "comment",
                    CommentKind::Rep => "rep",
                };
                format!("{kind}{}: {}", list("about", &c.about), c.text.replace('\n', " "))
            }
            MemberKind::Dependency(d) => {
                format!("dependency{}{}", list("from", &d.clients), list("to", &d.suppliers))
            }
            MemberKind::Definition(d) => {
                let abs = if d.is_abstract { "abstract " } else { "" };
                format!("{abs}{} def {}{}", d.kind, name(&d.id), list(":>", &d.specializes))
            }
            MemberKind::Usage(u) => usage_summary(u),
            MemberKind::Transition(t) => {
                let mut s = if t.is_else { "else".to_string() } else { "transition".to_string() };
                if let Some(n) = t.id.display_name() {
                    s += &format!(" {n}");
                }
                if let Some(src) = &t.source {
                    s += &format!(" first {src}");
                }
                if let Some(trigger) = &t.trigger {
                    s += &format!(" accept {}", trigger_summary(trigger));
                }
                if let Some(g) = &t.guard {
                    s += &format!(" if {g}");
                }
                if let Some(target) = &t.target {
                    s += &format!("{} {target}", if t.is_else { "" } else { " then" });
                }
                s
            }
            MemberKind::StateAction(a) => {
                let kind = match a.kind {
                    StateActionKind::Entry => "entry",
                    StateActionKind::Do => "do",
                    StateActionKind::Exit => "exit",
                };
                match &a.action {
                    None => kind.to_string(),
                    Some(ActionRef::Ref(r)) => format!("{kind} {r}"),
                    Some(ActionRef::Inline(m)) => format!("{kind} {}", m.kind.summary()),
                }
            }
            MemberKind::Accept(a) => format!("accept {}", trigger_summary(&a.trigger)),
            MemberKind::Send(s) => {
                let mut out = format!("send {}", s.payload);
                if let Some(via) = &s.via {
                    out += &format!(" via {via}");
                }
                if let Some(to) = &s.to {
                    out += &format!(" to {to}");
                }
                out
            }
            MemberKind::Then(m) => format!("then {}", m.kind.summary()),
            MemberKind::Filter(e) => format!("filter {e}"),
            MemberKind::Expression(e) => e.to_string(),
        }
    }
}

fn trigger_summary(t: &Trigger) -> String {
    let mut s = match &t.kind {
        TriggerKind::Signal { name: Some(n), ty: Some(ty) } => format!("{n} : {ty}"),
        TriggerKind::Signal { ty: Some(ty), .. } => ty.to_string(),
        TriggerKind::Signal { .. } => String::new(),
        TriggerKind::When(e) => format!("when {e}"),
        TriggerKind::At(e) => format!("at {e}"),
        TriggerKind::After(e) => format!("after {e}"),
    };
    if let Some(via) = &t.via {
        s += &format!(" via {via}");
    }
    s
}

fn usage_summary(u: &Usage) -> String {
    // `then x;`
    if let Some(Connector { implicit_source: true, ends, .. }) = &u.connector
        && let [end] = ends.as_slice()
    {
        return format!("then {}", end.target);
    }
    let p = &u.prefix;
    let mut s = String::new();
    let mut word = |w: &str| {
        s += w;
        s += " ";
    };
    match p.direction {
        Some(Direction::In) => word("in"),
        Some(Direction::Out) => word("out"),
        Some(Direction::InOut) => word("inout"),
        None => {}
    }
    if p.is_abstract {
        word("abstract");
    }
    if p.is_end {
        word("end");
    }
    if p.is_ref {
        word("ref");
    }
    if let Some(role) = p.role {
        word(role.keyword());
    }
    let named = u.id.display_name().is_some();
    // Leave out keywords the source didn't spell: `x : T;`, `:>> x`, `perform x;`,
    // `bind a = b;`, `first a then b;`.
    let implied = match u.kind {
        UsageKind::Feature => named || !u.redefines.is_empty(),
        UsageKind::Binding | UsageKind::Connection | UsageKind::Allocation | UsageKind::Succession => {
            !named && u.connector.is_some()
        }
        _ => false,
    } || (!named && p.role.is_some() && !u.references.is_empty());
    if !implied {
        s += u.kind.keyword();
    }
    let mut s = s.trim_end().to_string();
    if let Some(n) = u.id.display_name() {
        s += &format!(" {n}");
    }
    s += &list(":", &u.typed_by);
    s += &list(":>", &u.subsets);
    s += &list(":>>", &u.redefines);
    if p.role.is_some() && !named {
        // `perform drive;`, `satisfy R by v;`: the reference is the subject of the role.
        let refs: Vec<_> = u.references.iter().map(Reference::to_string).collect();
        s += &format!(" {}", refs.join(", "));
    } else {
        s += &list("::>", &u.references);
    }
    if let Some(m) = &u.multiplicity {
        s += &format!(" {m}");
    }
    if let Some(v) = &u.value {
        let op = match v.kind {
            ValueKind::Bound => "=",
            ValueKind::Initial => ":=",
        };
        let default = if v.is_default { "default " } else { "" };
        s += &format!(" {default}{op} {}", v.expr);
    }
    if let Some(c) = &u.connector {
        let ends: Vec<_> = c.ends.iter().map(|e| e.target.to_string()).collect();
        let of = c.item_type.as_ref().map(|t| format!(" of {t}")).unwrap_or_default();
        s += &match c.kind {
            ConnectorKind::Connect if ends.len() == 2 => format!(" connect {} to {}", ends[0], ends[1]),
            ConnectorKind::Connect => format!(" connect ({})", ends.join(", ")),
            ConnectorKind::Bind => format!(" bind {}", ends.join(" = ")),
            ConnectorKind::Allocate => format!(" allocate {}", ends.join(" to ")),
            ConnectorKind::Flow | ConnectorKind::Message if ends.is_empty() => of,
            ConnectorKind::Flow | ConnectorKind::Message => format!("{of} from {}", ends.join(" to ")),
            ConnectorKind::Succession => format!(" first {}", ends.join(" then ")),
        };
    }
    if !u.about.is_empty() {
        s += &list("about", &u.about);
    }
    if let Some(by) = &u.by {
        s += &format!(" by {by}");
    }
    s.trim().to_string()
}
