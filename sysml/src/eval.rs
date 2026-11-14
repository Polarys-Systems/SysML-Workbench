//! Evaluates attribute values (with units) and checks requirements.
//!
//! A feature is looked up in an [`Instance`]: the members that declare it,
//! most specific first. For `drone1.battery` those are the `:>> battery`
//! redefinition inside `drone1`, then `part battery : BatteryPack` in `Drone`,
//! then the definitions `BatteryPack` and `Component`. The first of them with a
//! value wins, and the names in that value are looked up in the same instance,
//! so `totalMass = battery.mass + ...` written in `Drone` sees `drone1`'s battery.
//!
//! [`evaluate`] runs over a whole model and reports
//!
//! * the value of each attribute written with an expression ([`Note`]),
//! * the attribute values of each package-level part, item, ... ([`InstanceValues`]),
//! * whether each `satisfy R by x` holds ([`Check`]),
//! * unit and type errors as diagnostics.
//!
//! ```
//! let src = "package P {
//!     part def Box { attribute w = 2 [kg]; attribute h = 500 [g]; attribute total = w + h; }
//!     part box : Box;
//! }";
//! let model = sysml::parse(src).model;
//! let index = sysml::Index::build(&model);
//! let ev = sysml::eval::Evaluator::new(&index);
//! assert_eq!(ev.value_of("P::box", &["total"]).unwrap().to_string(), "2.5 kg");
//! ```

use crate::ast::*;
use crate::index::{EntryId, Index};
use crate::units::{self, Dim, Unit};
use crate::{Diagnostic, Severity, Span};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

/// Backstop for deep (non-circular) chains of values and calls.
const MAX_DEPTH: usize = 200;
/// Most layers (declaring members and their types) one instance gathers.
const MAX_LAYERS: usize = 64;
/// Most copies a fixed multiplicity (`part wheels : Wheel [4]`) expands to.
const MAX_COUNT: u64 = 10_000;

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Value<'m> {
    /// In SI base units; `unit` is the unit it was written in, used to show it.
    Number { value: f64, dim: Dim, unit: Option<Rc<Unit>> },
    Bool(bool),
    String(String),
    /// An enumeration literal, by qualified name.
    Enum(String),
    /// Several values; `()` (null) is the empty sequence.
    Seq(Vec<Value<'m>>),
    /// A part, item, port, ...
    Instance(Rc<Instance<'m>>),
}

fn plain(value: f64) -> Value<'static> {
    Value::Number { value, dim: Dim::NONE, unit: None }
}

impl fmt::Display for Value<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Number { value, unit: Some(u), .. } => write!(f, "{} {}", units::format_number(value / u.scale), u.name),
            Value::Number { value, dim, .. } if dim.is_none() => f.write_str(&units::format_number(*value)),
            Value::Number { value, dim, .. } => write!(f, "{} {dim}", units::format_number(*value)),
            Value::Bool(b) => write!(f, "{b}"),
            Value::String(s) => write!(f, "{s:?}"),
            // `Pkg::Mode::idle` → `Mode::idle`
            Value::Enum(q) => {
                let parts: Vec<&str> = q.rsplitn(3, "::").collect();
                match parts.as_slice() {
                    [lit, ty, ..] => write!(f, "{ty}::{lit}"),
                    _ => f.write_str(q),
                }
            }
            Value::Seq(items) => {
                f.write_str("(")?;
                for (i, v) in items.iter().take(5).enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                if items.len() > 5 {
                    write!(f, ", … {} values", items.len())?;
                }
                f.write_str(")")
            }
            Value::Instance(i) => f.write_str(&i.path),
        }
    }
}

/// `the part drone1`, `4 values`, ... for messages.
fn describe(v: &Value) -> String {
    match v {
        Value::Number { .. } => format!("the number {v}"),
        Value::Bool(_) => format!("`{v}`"),
        Value::String(_) => format!("the string {v}"),
        Value::Enum(_) => format!("`{v}`"),
        Value::Seq(items) if items.is_empty() => "no value".into(),
        Value::Seq(items) => format!("{} values", items.len()),
        Value::Instance(i) => format!("`{}`", i.path),
    }
}

/// Why a value could not be worked out.
#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    /// The model does not say enough, e.g. a feature has no value yet.
    Unknown(String),
    /// The model is inconsistent, e.g. `kg + W`; also reported as a diagnostic.
    Error(String),
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Unknown(s) | Problem::Error(s) => f.write_str(s),
        }
    }
}

type R<'m> = Result<Value<'m>, Problem>;

fn unknown<T>(msg: impl Into<String>) -> Result<T, Problem> {
    Err(Problem::Unknown(msg.into()))
}

// ---------------------------------------------------------------------------
// Instances
// ---------------------------------------------------------------------------

/// A member contributing features to an instance, and the namespace its body's
/// names resolve in.
#[derive(Clone, Copy, Debug)]
struct Layer<'m> {
    member: &'m Member,
    ns: Option<EntryId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Count {
    One,
    Exactly(u64),
    /// `[1..*]`, `[2..4]`: not fixed.
    Open,
}

/// A part, item, requirement, ... as seen from a given owner.
#[derive(Clone, Debug)]
pub struct Instance<'m> {
    /// Most specific first.
    layers: Vec<Layer<'m>>,
    parent: Option<Rc<Instance<'m>>>,
    /// Feature values given from outside: a requirement's subject, call arguments.
    bound: Vec<(String, Value<'m>)>,
    /// `drone1.battery`, for messages.
    pub path: String,
    /// A package: its usages are named without a prefix.
    is_namespace: bool,
    count: Count,
}

impl Instance<'_> {
    fn child_path(&self, name: &str) -> String {
        if self.is_namespace || self.path.is_empty() { name.to_string() } else { format!("{}.{name}", self.path) }
    }
}

fn unwrap_then(m: &Member) -> &Member {
    match &m.kind {
        MemberKind::Then(inner) => unwrap_then(inner),
        _ => m,
    }
}

fn int_literal(e: &Expr) -> Option<u64> {
    match e.kind {
        ExprKind::Literal(Literal::Integer(n)) => Some(n),
        _ => None,
    }
}

/// How many of a feature there are, from the first multiplicity declared.
fn count_of(members: &[(&Member, Option<EntryId>)]) -> Count {
    for (m, _) in members {
        if let MemberKind::Usage(Usage { multiplicity: Some(mult), .. }) = &m.kind {
            let upper = int_literal(&mult.upper);
            let lower = match &mult.lower {
                Some(l) => int_literal(l),
                None => upper,
            };
            return match (lower, upper) {
                (Some(1), Some(1)) | (Some(0), Some(1)) => Count::One,
                (Some(a), Some(b)) if a == b && a <= MAX_COUNT => Count::Exactly(a),
                _ => Count::Open,
            };
        }
    }
    Count::One
}

/// The expression giving a calc's or constraint's result: the last bare
/// expression in its body, or the value of its `return` feature.
fn result_expr(member: &Member) -> Option<&Expr> {
    let mut result = None;
    for m in member.kind.body() {
        match &unwrap_then(m).kind {
            MemberKind::Expression(e) => result = Some(e),
            MemberKind::Usage(u) if u.kind == UsageKind::Return && result.is_none() => {
                result = u.value.as_ref().map(|v| &v.expr);
            }
            _ => {}
        }
    }
    result
}

fn is_comparison(op: BinaryOp) -> bool {
    matches!(op, BinaryOp::Lt | BinaryOp::Gt | BinaryOp::Le | BinaryOp::Ge | BinaryOp::Eq | BinaryOp::Ne)
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

pub struct Evaluator<'a, 'm> {
    index: &'a Index<'m>,
    entry_of: HashMap<*const Member, EntryId>,
    statics: RefCell<HashMap<EntryId, Rc<Instance<'m>>>>,
    /// Values being worked out (value member, instance path), to catch cycles.
    active: RefCell<HashSet<(usize, String)>>,
    depth: Cell<usize>,
    calls: Cell<usize>,
    diagnostics: RefCell<Vec<Diagnostic>>,
}

impl<'a, 'm> Evaluator<'a, 'm> {
    pub fn new(index: &'a Index<'m>) -> Self {
        let entry_of = index.entries().iter().enumerate().map(|(id, e)| (e.member as *const Member, id)).collect();
        Evaluator {
            index,
            entry_of,
            statics: RefCell::default(),
            active: RefCell::default(),
            depth: Cell::new(0),
            calls: Cell::new(0),
            diagnostics: RefCell::default(),
        }
    }

    /// Problems found so far.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.diagnostics.borrow().clone()
    }

    /// The value of `path` (a feature chain) in the element named
    /// `qualified_name`, e.g. `("Pkg::drone1", &["battery", "mass"])`.
    pub fn value_of(&self, qualified_name: &str, path: &[&str]) -> R<'m> {
        let Some(id) = self.index.get(qualified_name) else { return unknown(format!("`{qualified_name}` is not found")) };
        let mut v = Value::Instance(self.static_inst(id));
        for name in path {
            v = self.member_of(v, name, Span::default())?;
        }
        Ok(v)
    }

    fn error(&self, msg: impl Into<String>, span: Span) -> Problem {
        self.report(Severity::Error, msg, span)
    }

    fn report(&self, severity: Severity, msg: impl Into<String>, span: Span) -> Problem {
        let message = msg.into();
        let d = Diagnostic { severity, message: message.clone(), span };
        let mut ds = self.diagnostics.borrow_mut();
        if !ds.contains(&d) {
            ds.push(d);
        }
        Problem::Error(message)
    }

    // ---- instances -----------------------------------------------------------

    /// `member` and the definitions it is typed by or specializes, transitively.
    fn push_layers(&self, member: &'m Member, owner_ns: Option<EntryId>, out: &mut Vec<Layer<'m>>) {
        let member = unwrap_then(member);
        if out.len() >= MAX_LAYERS || out.iter().any(|l| std::ptr::eq(l.member, member)) {
            return;
        }
        let ns = self.entry_of.get(&(member as *const Member)).copied().or(owner_ns);
        out.push(Layer { member, ns });
        let types: Vec<&QualifiedName> = match &member.kind {
            MemberKind::Definition(d) => d.specializes.iter().collect(),
            MemberKind::Usage(u) => u.typed_by.iter().map(|t| &t.name).collect(),
            _ => Vec::new(),
        };
        for ty in types {
            if let Some(t) = self.index.resolve(owner_ns, ty) {
                let entry = self.index.entry(t);
                self.push_layers(entry.member, entry.parent, out);
            }
        }
    }

    /// The usages named `name` in the bodies of `inst`'s layers.
    fn members_named(&self, inst: &Instance<'m>, name: &str) -> Vec<(&'m Member, Option<EntryId>)> {
        let mut out = Vec::new();
        for layer in &inst.layers {
            for m in layer.member.kind.body() {
                let m = unwrap_then(m);
                if let MemberKind::Usage(u) = &m.kind
                    && u.connector.is_none()
                    && u.prefix.role.is_none()
                    && m.kind.effective_name() == Some(name)
                {
                    out.push((m, layer.ns));
                }
            }
        }
        out
    }

    fn make_child(&self, parent: &Rc<Instance<'m>>, name: &str, members: &[(&'m Member, Option<EntryId>)]) -> Instance<'m> {
        let mut layers = Vec::new();
        for &(m, ns) in members {
            self.push_layers(m, ns, &mut layers);
        }
        Instance {
            layers,
            parent: Some(parent.clone()),
            bound: Vec::new(),
            path: parent.child_path(name),
            is_namespace: false,
            count: count_of(members),
        }
    }

    /// The element `id` as seen from its owners (for a nested feature, merged
    /// with what its owner's types say about it).
    fn static_inst(&self, id: EntryId) -> Rc<Instance<'m>> {
        let cached = self.statics.borrow().get(&id).cloned();
        if let Some(inst) = cached {
            return inst;
        }
        let entry = self.index.entry(id);
        let parent = entry.parent.map(|p| self.static_inst(p));
        let members = match (&entry.member.kind, &parent) {
            (MemberKind::Usage(_), Some(p)) => self.members_named(p, entry.name),
            _ => Vec::new(),
        };
        let inst = match &parent {
            Some(p) if !members.is_empty() => self.make_child(p, entry.name, &members),
            _ => Instance {
                layers: {
                    let mut l = Vec::new();
                    self.push_layers(entry.member, entry.parent, &mut l);
                    l
                },
                path: parent.as_ref().map(|p| p.child_path(entry.name)).unwrap_or_else(|| entry.name.to_string()),
                parent,
                bound: Vec::new(),
                is_namespace: matches!(entry.member.kind, MemberKind::Package(_)),
                count: Count::One,
            },
        };
        let inst = Rc::new(inst);
        self.statics.borrow_mut().insert(id, inst.clone());
        inst
    }

    /// Whether the feature holds plain values (attributes) rather than parts, items, ...
    fn is_value_like(&self, members: &[(&'m Member, Option<EntryId>)]) -> bool {
        let mut layers = Vec::new();
        for &(m, ns) in members {
            self.push_layers(m, ns, &mut layers);
        }
        for l in &layers {
            match &l.member.kind {
                MemberKind::Usage(u) => match u.kind {
                    UsageKind::Attribute | UsageKind::Enum => return true,
                    UsageKind::Feature | UsageKind::Ref | UsageKind::Subject | UsageKind::Return => {}
                    _ => return false,
                },
                MemberKind::Definition(d) => return matches!(d.kind, DefKind::Attribute | DefKind::Enum),
                _ => {}
            }
        }
        // Untyped, or typed by a library type such as `Real`.
        true
    }

    /// Feature `name` of `inst`; `None` if `inst` has no such feature.
    fn feature(&self, inst: &Rc<Instance<'m>>, name: &str, span: Span) -> Option<R<'m>> {
        if let Some((_, v)) = inst.bound.iter().find(|(n, _)| n == name) {
            return Some(Ok(v.clone()));
        }
        let members = self.members_named(inst, name);
        if members.is_empty() {
            return None;
        }
        if let Some(&(m, ns)) = members.iter().find(|(m, _)| matches!(&m.kind, MemberKind::Usage(Usage { value: Some(_), .. }))) {
            return Some(self.member_value(inst, name, m, ns, span));
        }
        let path = inst.child_path(name);
        if self.is_value_like(&members) {
            return Some(unknown(format!("{path} has no value")));
        }
        let child = Rc::new(self.make_child(inst, name, &members));
        Some(match child.count {
            Count::One => Ok(Value::Instance(child)),
            Count::Exactly(n) => Ok(Value::Seq(vec![Value::Instance(child); n as usize])),
            Count::Open => unknown(format!("the number of {path} is not fixed")),
        })
    }

    /// The value written on `m` (a feature of `inst` named `name`).
    fn member_value(&self, inst: &Rc<Instance<'m>>, name: &str, m: &'m Member, ns: Option<EntryId>, span: Span) -> R<'m> {
        let MemberKind::Usage(Usage { value: Some(v), .. }) = &m.kind else { return unknown(format!("{name} has no value")) };
        let key = (m as *const Member as usize, inst.path.clone());
        if !self.active.borrow_mut().insert(key.clone()) {
            let at = if span == Span::default() { v.expr.span } else { span };
            return Err(self.error(format!("{} depends on itself", inst.child_path(name)), at));
        }
        let r = self.nested(v.expr.span, || self.eval(&v.expr, inst, ns));
        self.active.borrow_mut().remove(&key);
        r
    }

    fn nested(&self, span: Span, f: impl FnOnce() -> R<'m>) -> R<'m> {
        let d = self.depth.get();
        if d >= MAX_DEPTH {
            return Err(self.error("values nest too deeply here", span));
        }
        self.depth.set(d + 1);
        let r = f();
        self.depth.set(d);
        r
    }

    // ---- expressions ---------------------------------------------------------

    fn eval(&self, e: &'m Expr, inst: &Rc<Instance<'m>>, ns: Option<EntryId>) -> R<'m> {
        match &e.kind {
            ExprKind::Literal(l) => Ok(match l {
                Literal::Integer(n) => plain(*n as f64),
                Literal::Real(v) => plain(*v),
                Literal::String(s) => Value::String(s.clone()),
                Literal::Bool(b) => Value::Bool(*b),
                Literal::Null => Value::Seq(Vec::new()),
            }),
            ExprKind::Name(qn) => self.name(qn, inst, ns, e.span),
            ExprKind::Unary { op, operand } => {
                let v = self.eval(operand, inst, ns)?;
                match op {
                    UnaryOp::Plus => self.number(&v, operand.span).map(|_| v),
                    UnaryOp::Minus => {
                        let (value, dim, unit) = self.number(&v, operand.span)?;
                        Ok(Value::Number { value: -value, dim, unit })
                    }
                    UnaryOp::Not => Ok(Value::Bool(!self.boolean(&v, operand.span)?)),
                    UnaryOp::BitNot => unknown("`~` is not evaluated"),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, inst, ns, e.span),
            ExprKind::Conditional { cond, then, otherwise } => {
                let c = self.eval(cond, inst, ns)?;
                if self.boolean(&c, cond.span)? { self.eval(then, inst, ns) } else { self.eval(otherwise, inst, ns) }
            }
            ExprKind::Member { base, member } => {
                let b = self.eval(base, inst, ns)?;
                self.member_of(b, &member.text, member.span)
            }
            ExprKind::Arrow { base, func, args } => {
                let b = self.eval(base, inst, ns)?;
                let mut argv = Vec::new();
                for a in args {
                    argv.push(self.eval(&a.value, inst, ns)?);
                }
                self.arrow(b, &func.last().text, argv, e.span)
            }
            ExprKind::Call { callee, args } => self.call(callee, args, inst, ns, e.span),
            ExprKind::Index { base, index } => {
                let b = self.eval(base, inst, ns)?;
                let i = self.eval(index, inst, ns)?;
                let (i, _, _) = self.number(&i, index.span)?;
                let items = match b {
                    Value::Seq(items) => items,
                    v => vec![v],
                };
                let at = i as usize;
                if i.fract() != 0.0 || at == 0 || at > items.len() {
                    return Err(self.error(format!("index {} is outside 1..{}", units::format_number(i), items.len()), index.span));
                }
                Ok(items[at - 1].clone())
            }
            ExprKind::Quantity { value, unit } => {
                let v = self.eval(value, inst, ns)?;
                let u = Rc::new(self.unit(unit, ns)?);
                self.with_unit(v, &u, unit.span)
            }
            ExprKind::Sequence(items) => {
                let mut out = Vec::new();
                for item in items {
                    match self.eval(item, inst, ns)? {
                        Value::Seq(v) => out.extend(v),
                        v => out.push(v),
                    }
                }
                Ok(Value::Seq(out))
            }
            ExprKind::Infinity | ExprKind::Classification { .. } => unknown(format!("`{e}` is not evaluated")),
        }
    }

    /// A name: a feature of the instance or one of its owners, else whatever
    /// the name means where it is written.
    fn name(&self, qn: &QualifiedName, inst: &Rc<Instance<'m>>, ns: Option<EntryId>, span: Span) -> R<'m> {
        if let [single] = qn.segments.as_slice() {
            let mut cur = Some(inst.clone());
            while let Some(i) = cur {
                if let Some(r) = self.feature(&i, &single.text, span) {
                    return r;
                }
                cur = i.parent.clone();
            }
        }
        match self.index.resolve(ns, qn) {
            Some(id) => self.entry_value(id, span),
            None => unknown(format!("`{qn}` is not found")),
        }
    }

    fn entry_value(&self, id: EntryId, span: Span) -> R<'m> {
        let entry = self.index.entry(id);
        let MemberKind::Usage(_) = &entry.member.kind else {
            return unknown(format!("`{}` is a type, not a value", entry.name));
        };
        let Some(parent) = entry.parent else { return Ok(Value::Instance(self.static_inst(id))) };
        if let MemberKind::Definition(d) = &self.index.entry(parent).member.kind
            && d.kind == DefKind::Enum
        {
            return Ok(Value::Enum(entry.qualified_name.clone()));
        }
        let p = self.static_inst(parent);
        self.feature(&p, entry.name, span).unwrap_or_else(|| unknown(format!("`{}` has no value", entry.name)))
    }

    fn member_of(&self, base: Value<'m>, name: &str, span: Span) -> R<'m> {
        match base {
            Value::Instance(i) => self.feature(&i, name, span).unwrap_or_else(|| unknown(format!("{} has no feature `{name}`", i.path))),
            Value::Seq(items) => {
                let mut out = Vec::new();
                for item in items {
                    match self.member_of(item, name, span)? {
                        Value::Seq(v) => out.extend(v),
                        v => out.push(v),
                    }
                }
                Ok(Value::Seq(out))
            }
            v => Err(self.error(format!("{} has no feature `{name}`", describe(&v)), span)),
        }
    }

    fn number(&self, v: &Value<'m>, span: Span) -> Result<(f64, Dim, Option<Rc<Unit>>), Problem> {
        match v {
            Value::Number { value, dim, unit } => Ok((*value, *dim, unit.clone())),
            Value::Seq(items) if items.len() == 1 => self.number(&items[0], span),
            Value::Seq(items) if items.is_empty() => unknown("a value is missing"),
            Value::Seq(items) => Err(self.error(format!("expected one number, found {} values (use ->sum())", items.len()), span)),
            v => Err(self.error(format!("expected a number, found {}", describe(v)), span)),
        }
    }

    fn boolean(&self, v: &Value<'m>, span: Span) -> Result<bool, Problem> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Seq(items) if items.len() == 1 => self.boolean(&items[0], span),
            Value::Seq(items) if items.is_empty() => unknown("a value is missing"),
            v => Err(self.error(format!("expected true or false, found {}", describe(v)), span)),
        }
    }

    fn unit_name(dim: Dim, unit: &Option<Rc<Unit>>) -> String {
        match unit {
            Some(u) => u.name.clone(),
            None if dim.is_none() => "a plain number".into(),
            None => dim.to_string(),
        }
    }

    fn same_dims(&self, verb: &str, a: (Dim, &Option<Rc<Unit>>), b: (Dim, &Option<Rc<Unit>>), span: Span) -> Result<(), Problem> {
        if a.0 == b.0 {
            return Ok(());
        }
        Err(self.error(format!("cannot {verb} {} and {}: their units differ", Self::unit_name(a.0, a.1), Self::unit_name(b.0, b.1)), span))
    }

    fn binary(&self, op: BinaryOp, lhs: &'m Expr, rhs: &'m Expr, inst: &Rc<Instance<'m>>, ns: Option<EntryId>, span: Span) -> R<'m> {
        use BinaryOp::*;
        let l = self.eval(lhs, inst, ns);
        // Short-circuit: `false and x` holds even when `x` is not known.
        match op {
            And | Or | Implies => {
                let a = self.boolean(&l?, lhs.span)?;
                let decided = match op {
                    And => (!a).then_some(false),
                    Or => a.then_some(true),
                    _ => (!a).then_some(true),
                };
                if let Some(v) = decided {
                    return Ok(Value::Bool(v));
                }
                let b = self.eval(rhs, inst, ns)?;
                return Ok(Value::Bool(self.boolean(&b, rhs.span)?));
            }
            NullCoalesce => {
                return match l? {
                    Value::Seq(items) if items.is_empty() => self.eval(rhs, inst, ns),
                    v => Ok(v),
                };
            }
            Range => return unknown("ranges are not evaluated"),
            _ => {}
        }
        let (a, b) = (l?, self.eval(rhs, inst, ns)?);
        self.apply(op, a, b, span)
    }

    fn apply(&self, op: BinaryOp, a: Value<'m>, b: Value<'m>, span: Span) -> R<'m> {
        use BinaryOp::*;
        if let Xor = op {
            return Ok(Value::Bool(self.boolean(&a, span)? != self.boolean(&b, span)?));
        }
        if matches!(op, Eq | Ne | Same | NotSame) {
            let eq = self.equal(&a, &b, span)?;
            return Ok(Value::Bool(eq == matches!(op, Eq | Same)));
        }
        let (x, dx, ux) = self.number(&a, span)?;
        let (y, dy, uy) = self.number(&b, span)?;
        let num = |value: f64, dim: Dim, unit: Option<Rc<Unit>>| Ok(Value::Number { value, dim, unit });
        match op {
            Add | Sub => {
                self.same_dims(if op == Add { "add" } else { "subtract" }, (dx, &ux), (dy, &uy), span)?;
                num(if op == Add { x + y } else { x - y }, dx, ux.or(uy))
            }
            Mul => num(x * y, dx * dy, if dy.is_none() { ux } else if dx.is_none() { uy } else { None }),
            Div => {
                if y == 0.0 {
                    return Err(self.error("division by zero", span));
                }
                num(x / y, dx / dy, if dy.is_none() { ux } else { None })
            }
            Rem => {
                if y == 0.0 {
                    return Err(self.error("division by zero", span));
                }
                if !dy.is_none() {
                    self.same_dims("take the remainder of", (dx, &ux), (dy, &uy), span)?;
                }
                num(x % y, dx, ux)
            }
            Pow => {
                if !dy.is_none() {
                    return Err(self.error(format!("an exponent must be a plain number, not {}", Self::unit_name(dy, &uy)), span));
                }
                if dx.is_none() {
                    return num(x.powf(y), dx, None);
                }
                if y.fract() != 0.0 || y.abs() > 16.0 {
                    return Err(self.error("a quantity with a unit can only be raised to a whole power", span));
                }
                num(x.powf(y), dx.pow(y as i8), None)
            }
            Lt | Gt | Le | Ge => {
                self.same_dims("compare", (dx, &ux), (dy, &uy), span)?;
                Ok(Value::Bool(match op {
                    Lt => x < y,
                    Gt => x > y,
                    Le => x <= y || close(x, y),
                    _ => x >= y || close(x, y),
                }))
            }
            _ => unknown(format!("`{}` is not evaluated", op.symbol())),
        }
    }

    fn equal(&self, a: &Value<'m>, b: &Value<'m>, span: Span) -> Result<bool, Problem> {
        Ok(match (a, b) {
            (Value::Number { .. }, _) | (_, Value::Number { .. }) => {
                let (x, dx, ux) = self.number(a, span)?;
                let (y, dy, uy) = self.number(b, span)?;
                self.same_dims("compare", (dx, &ux), (dy, &uy), span)?;
                close(x, y)
            }
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::String(x), Value::String(y)) => x == y,
            (Value::Enum(x), Value::Enum(y)) => x == y,
            (Value::Instance(x), Value::Instance(y)) => x.path == y.path,
            (Value::Seq(x), Value::Seq(y)) if x.len() == y.len() => {
                for (p, q) in x.iter().zip(y) {
                    if !self.equal(p, q, span)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Value::Seq(x), y) | (y, Value::Seq(x)) if x.len() == 1 => self.equal(&x[0], y, span)?,
            _ => false,
        })
    }

    /// `x->sum()`, `x->size()`, ...
    fn arrow(&self, base: Value<'m>, func: &str, args: Vec<Value<'m>>, span: Span) -> R<'m> {
        let items = match base {
            Value::Seq(items) => items,
            v => vec![v],
        };
        match func {
            "size" => Ok(plain(items.len() as f64)),
            "isEmpty" => Ok(Value::Bool(items.is_empty())),
            "notEmpty" => Ok(Value::Bool(!items.is_empty())),
            "first" => items.into_iter().next().ok_or(Problem::Unknown("the sequence is empty".into())),
            "last" => items.into_iter().last().ok_or(Problem::Unknown("the sequence is empty".into())),
            "includes" => {
                let Some(x) = args.first() else { return unknown("`includes` needs an argument") };
                for item in &items {
                    if self.equal(item, x, span)? {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "sum" | "avg" | "mean" => {
                let mut total = plain(0.0);
                for (i, item) in items.iter().enumerate() {
                    total = if i == 0 { Value::Seq(vec![item.clone()]) } else { self.apply(BinaryOp::Add, total, item.clone(), span)? };
                }
                let total = match total {
                    Value::Seq(mut v) => v.pop().unwrap_or(plain(0.0)),
                    v => v,
                };
                if func == "sum" {
                    self.number(&total, span)?;
                    return Ok(total);
                }
                if items.is_empty() {
                    return unknown("the average of no values");
                }
                self.apply(BinaryOp::Div, total, plain(items.len() as f64), span)
            }
            "max" | "min" => {
                let mut best: Option<Value<'m>> = None;
                for item in items {
                    self.number(&item, span)?;
                    best = Some(match best {
                        None => item,
                        Some(b) => {
                            let greater = self.apply(BinaryOp::Gt, item.clone(), b.clone(), span)?;
                            if matches!(greater, Value::Bool(g) if g == (func == "max")) { item } else { b }
                        }
                    });
                }
                best.ok_or(Problem::Unknown("the sequence is empty".into()))
            }
            _ => unknown(format!("`->{func}` is not evaluated")),
        }
    }

    fn call(&self, callee: &QualifiedName, args: &'m [Argument], inst: &Rc<Instance<'m>>, ns: Option<EntryId>, span: Span) -> R<'m> {
        let mut argv = Vec::new();
        for a in args {
            argv.push((a.name.as_ref().map(|n| n.text.as_str()), self.eval(&a.value, inst, ns)?));
        }
        if let Some(id) = self.index.resolve(ns, callee) {
            let entry = self.index.entry(id);
            let callable = match &entry.member.kind {
                MemberKind::Definition(d) => matches!(d.kind, DefKind::Calc | DefKind::Constraint),
                MemberKind::Usage(u) => matches!(u.kind, UsageKind::Calc | UsageKind::Constraint),
                _ => false,
            };
            if !callable {
                return unknown(format!("calling `{callee}` is not evaluated"));
            }
            return self.invoke(id, argv, span);
        }
        self.builtin(&callee.last().text, argv.into_iter().map(|(_, v)| v).collect(), span)
    }

    /// Calls a `calc` or `constraint`: binds its `in` features to the arguments
    /// and works out its result expression.
    fn invoke(&self, id: EntryId, args: Vec<(Option<&str>, Value<'m>)>, span: Span) -> R<'m> {
        let entry = self.index.entry(id);
        let mut layers = Vec::new();
        self.push_layers(entry.member, entry.parent, &mut layers);
        // Parameters in declaration order, from the most general definition.
        let mut params: Vec<&str> = Vec::new();
        for l in layers.iter().rev() {
            for m in l.member.kind.body() {
                if let MemberKind::Usage(u) = &m.kind
                    && u.prefix.direction == Some(Direction::In)
                    && let Some(n) = m.kind.effective_name()
                    && !params.contains(&n)
                {
                    params.push(n);
                }
            }
        }
        let mut bound = Vec::new();
        let mut next = 0;
        for (name, v) in args {
            let name = match name {
                Some(n) => n,
                None => {
                    let Some(&n) = params.get(next) else {
                        return Err(self.error(format!("too many arguments for `{}`", entry.name), span));
                    };
                    next += 1;
                    n
                }
            };
            bound.push((name.to_string(), v));
        }
        // A distinct path per call keeps recursion apart from cycles.
        self.calls.set(self.calls.get() + 1);
        let inst = Rc::new(Instance {
            layers,
            parent: entry.parent.map(|p| self.static_inst(p)),
            bound,
            path: format!("{}#{}", entry.name, self.calls.get()),
            is_namespace: false,
            count: Count::One,
        });
        for l in &inst.layers {
            if let Some(r) = result_expr(l.member) {
                return self.nested(span, || self.eval(r, &inst, l.ns));
            }
        }
        unknown(format!("`{}` has no result expression", entry.name))
    }

    fn builtin(&self, name: &str, args: Vec<Value<'m>>, span: Span) -> R<'m> {
        let arg = |i: usize| -> Result<(f64, Dim, Option<Rc<Unit>>), Problem> {
            match args.get(i) {
                Some(v) => self.number(v, span),
                None => unknown(format!("`{name}` needs {} argument(s)", i + 1)),
            }
        };
        let num = |value: f64, dim: Dim, unit: Option<Rc<Unit>>| Ok(Value::Number { value, dim, unit });
        match name {
            "abs" => arg(0).and_then(|(v, d, u)| num(v.abs(), d, u)),
            "floor" => arg(0).and_then(|(v, d, u)| num(v.floor(), d, u)),
            "ceil" | "ceiling" => arg(0).and_then(|(v, d, u)| num(v.ceil(), d, u)),
            "round" => arg(0).and_then(|(v, d, u)| num(v.round(), d, u)),
            "sqrt" => {
                let (v, d, _) = arg(0)?;
                if d.0.iter().any(|e| e % 2 != 0) {
                    return Err(self.error(format!("cannot take the square root of {d}"), span));
                }
                num(v.sqrt(), Dim(d.0.map(|e| e / 2)), None)
            }
            "max" | "min" => {
                let mut items = args.clone();
                if items.len() == 1
                    && let Value::Seq(v) = &items[0]
                {
                    items = v.clone();
                }
                self.arrow(Value::Seq(items), name, Vec::new(), span)
            }
            _ => unknown(format!("`{name}` is not found")),
        }
    }

    // ---- units ---------------------------------------------------------------

    fn unit(&self, e: &Expr, ns: Option<EntryId>) -> Result<Unit, Problem> {
        let (scale, dim) = self.unit_factor(e, ns)?;
        Ok(Unit { name: unit_text(e), scale, dim })
    }

    fn unit_factor(&self, e: &Expr, ns: Option<EntryId>) -> Result<(f64, Dim), Problem> {
        match &e.kind {
            ExprKind::Name(qn) => {
                let found = units::lookup(&qn.last().text).or_else(|| {
                    // An alias such as `alias Kg for SI::kg`.
                    let id = self.index.resolve(ns, qn)?;
                    units::lookup(self.index.entry(id).name)
                });
                match found {
                    Some(u) => Ok((u.scale, u.dim)),
                    None => Err(self.report(Severity::Warning, format!("unknown unit `{qn}`"), e.span)),
                }
            }
            ExprKind::Literal(Literal::Integer(n)) => Ok((*n as f64, Dim::NONE)),
            ExprKind::Literal(Literal::Real(v)) => Ok((*v, Dim::NONE)),
            ExprKind::Binary { op: BinaryOp::Mul, lhs, rhs } => {
                let (a, da) = self.unit_factor(lhs, ns)?;
                let (b, db) = self.unit_factor(rhs, ns)?;
                Ok((a * b, da * db))
            }
            ExprKind::Binary { op: BinaryOp::Div, lhs, rhs } => {
                let (a, da) = self.unit_factor(lhs, ns)?;
                let (b, db) = self.unit_factor(rhs, ns)?;
                Ok((a / b, da / db))
            }
            ExprKind::Binary { op: BinaryOp::Pow, lhs, rhs } => {
                let n = match &rhs.kind {
                    ExprKind::Literal(Literal::Integer(n)) => *n as i64,
                    ExprKind::Unary { op: UnaryOp::Minus, operand } => -(int_literal(operand).unwrap_or(99) as i64),
                    _ => 99,
                };
                if n.abs() > 16 {
                    return Err(self.error("a unit power must be a small whole number", rhs.span));
                }
                let (a, da) = self.unit_factor(lhs, ns)?;
                Ok((a.powi(n as i32), da.pow(n as i8)))
            }
            _ => Err(self.error(format!("`{e}` is not a unit"), e.span)),
        }
    }

    fn with_unit(&self, v: Value<'m>, unit: &Rc<Unit>, span: Span) -> R<'m> {
        match v {
            Value::Number { value, dim, unit: None } if dim.is_none() => {
                Ok(Value::Number { value: value * unit.scale, dim: unit.dim, unit: Some(unit.clone()) })
            }
            Value::Number { value, dim, unit: old } => {
                if dim != unit.dim {
                    return Err(self.error(format!("a value in {} cannot be given in {}", Self::unit_name(dim, &old), unit.name), span));
                }
                Ok(Value::Number { value, dim, unit: Some(unit.clone()) })
            }
            Value::Seq(items) => {
                let mut out = Vec::new();
                for item in items {
                    out.push(self.with_unit(item, unit, span)?);
                }
                Ok(Value::Seq(out))
            }
            v => Err(self.error(format!("{} cannot have a unit", describe(&v)), span)),
        }
    }
}

/// A unit expression written compactly: `km/h`, `m/s^2`, `N*m`.
fn unit_text(e: &Expr) -> String {
    let wrap = |e: &Expr| match &e.kind {
        ExprKind::Binary { op: BinaryOp::Mul | BinaryOp::Div, .. } => format!("({})", unit_text(e)),
        _ => unit_text(e),
    };
    match &e.kind {
        ExprKind::Name(qn) => qn.last().text.clone(),
        ExprKind::Binary { op: BinaryOp::Mul, lhs, rhs } => format!("{}*{}", unit_text(lhs), unit_text(rhs)),
        ExprKind::Binary { op: BinaryOp::Div, lhs, rhs } => format!("{}/{}", unit_text(lhs), wrap(rhs)),
        ExprKind::Binary { op: BinaryOp::Pow, lhs, rhs } => format!("{}^{}", wrap(lhs), rhs.to_string().replace(' ', "")),
        _ => e.to_string().replace(' ', ""),
    }
}

fn close(x: f64, y: f64) -> bool {
    x == y || (x - y).abs() <= 1e-9 * x.abs().max(y.abs())
}

// ---------------------------------------------------------------------------
// Whole-model report
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Not enough information in the model.
    Unknown,
    Error,
}

/// The value of an attribute's expression, to show next to it.
#[derive(Clone, Debug)]
pub struct Note {
    /// The value expression.
    pub span: Span,
    /// `6.2 kg`, or why it is not known.
    pub text: String,
    pub status: Status,
}

#[derive(Clone, Debug)]
pub struct NamedValue {
    pub name: String,
    /// The value, `?` if unknown.
    pub text: String,
    /// Why the value is unknown or wrong.
    pub reason: Option<String>,
    pub status: Status,
}

/// The attribute values of a package-level part, item, ... including the
/// ones its definitions declare.
#[derive(Clone, Debug)]
pub struct InstanceValues {
    pub qualified_name: String,
    pub values: Vec<NamedValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Satisfied,
    Violated,
    /// Some value the constraints need is not in the model.
    Unknown,
    Error,
}

/// The outcome of `satisfy R by x`.
#[derive(Clone, Debug)]
pub struct Check {
    /// The `satisfy` member.
    pub span: Span,
    pub requirement: String,
    pub subject: String,
    pub verdict: Verdict,
    /// The compared values, e.g. `6.2 kg <= 5 kg`, or why there is no verdict.
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub notes: Vec<Note>,
    pub instances: Vec<InstanceValues>,
    pub checks: Vec<Check>,
    /// Unit and type errors, sorted by position.
    pub diagnostics: Vec<Diagnostic>,
}

/// Evaluates every value, package-level instance and `satisfy` in `model`.
pub fn evaluate(model: &Model, index: &Index) -> Report {
    let ev = Evaluator::new(index);
    let mut report = Report::default();
    ev.walk(&model.members, None, true, &mut report);
    for (id, entry) in index.entries().iter().enumerate() {
        let MemberKind::Usage(u) = &entry.member.kind else { continue };
        let in_package = entry.parent.is_none_or(|p| matches!(index.entry(p).member.kind, MemberKind::Package(_)));
        if !in_package || u.prefix.role.is_some() || matches!(u.kind, UsageKind::Attribute | UsageKind::Enum | UsageKind::Metadata) {
            continue;
        }
        let values = ev.instance_values(id);
        if !values.is_empty() {
            report.instances.push(InstanceValues { qualified_name: entry.qualified_name.clone(), values });
        }
    }
    report.diagnostics = ev.diagnostics();
    report.diagnostics.sort_by_key(|d| d.span.start);
    report
}

/// Whether showing `value` next to `expr` would only repeat it.
fn repeats(expr: &Expr, value: &Value) -> bool {
    let squash = |s: String| s.chars().filter(|c| !c.is_whitespace() && *c != '[' && *c != ']').collect::<String>();
    match &expr.kind {
        ExprKind::Literal(_) => true,
        ExprKind::Unary { operand, .. } if matches!(operand.kind, ExprKind::Literal(_)) => true,
        _ => squash(expr.to_string()) == squash(value.to_string()),
    }
}

impl<'m> Evaluator<'_, 'm> {
    /// `owner` is the entry whose body `members` is; `None` inside anonymous
    /// elements (or at the top level). `concrete`: not inside a definition, so
    /// a missing value is worth pointing out.
    fn walk(&self, members: &'m [Member], owner: Option<EntryId>, concrete: bool, report: &mut Report) {
        for m in members {
            let m = unwrap_then(m);
            if let MemberKind::Usage(u) = &m.kind {
                if u.prefix.role == Some(Role::Satisfy) {
                    report.checks.push(self.check(u, m.span, owner));
                } else if let (Some(v), Some(o)) = (&u.value, owner)
                    && u.connector.is_none()
                    && let Some(name) = m.kind.effective_name()
                {
                    let inst = self.static_inst(o);
                    match self.member_value(&inst, name, m, Some(o), Span::default()) {
                        Ok(value) if !repeats(&v.expr, &value) => {
                            report.notes.push(Note { span: v.expr.span, text: value.to_string(), status: Status::Ok })
                        }
                        Ok(_) => {}
                        Err(Problem::Unknown(why)) if concrete => {
                            report.notes.push(Note { span: v.expr.span, text: format!("? {why}"), status: Status::Unknown })
                        }
                        Err(Problem::Unknown(_)) => {}
                        Err(Problem::Error(why)) => report.notes.push(Note { span: v.expr.span, text: why, status: Status::Error }),
                    }
                }
            }
            let id = self.entry_of.get(&(m as *const Member)).copied();
            let concrete = concrete && !matches!(m.kind, MemberKind::Definition(_));
            self.walk(m.kind.body(), id, concrete, report);
        }
    }

    fn instance_values(&self, id: EntryId) -> Vec<NamedValue> {
        let inst = self.static_inst(id);
        // Names of features with a value, most general definition first.
        let mut names: Vec<&str> = Vec::new();
        for layer in inst.layers.iter().rev() {
            for m in layer.member.kind.body() {
                let m = unwrap_then(m);
                if let MemberKind::Usage(u) = &m.kind
                    && u.value.is_some()
                    && u.connector.is_none()
                    && u.prefix.role.is_none()
                    && let Some(n) = m.kind.effective_name()
                    && !names.contains(&n)
                {
                    names.push(n);
                }
            }
        }
        names
            .into_iter()
            .filter(|n| self.is_value_like(&self.members_named(&inst, n)))
            .map(|name| {
                let r = self.feature(&inst, name, Span::default()).unwrap_or_else(|| unknown("not found"));
                let (text, reason, status) = match r {
                    Ok(v) => (v.to_string(), None, Status::Ok),
                    Err(Problem::Unknown(why)) => ("?".into(), Some(why), Status::Unknown),
                    Err(Problem::Error(why)) => ("!".into(), Some(why), Status::Error),
                };
                NamedValue { name: name.to_string(), text, reason, status }
            })
            .collect()
    }

    fn check(&self, u: &'m Usage, span: Span, owner: Option<EntryId>) -> Check {
        let req = u.references.first().and_then(|r| self.index.resolve_reference(owner, r));
        // `satisfy R;` inside a part: the part is the subject.
        let by = match &u.by {
            Some(r) => self.index.resolve_reference(owner, r),
            None => owner,
        };
        let name = |id: Option<EntryId>| id.map(|i| self.index.entry(i).qualified_name.clone()).unwrap_or_default();
        let mut check = Check { span, requirement: name(req), subject: name(by), verdict: Verdict::Unknown, detail: String::new() };
        let (Some(req), Some(by)) = (req, by) else {
            check.detail = "the requirement or its subject is not found".into();
            return check;
        };
        let (verdict, detail) = self.verify(req, by);
        check.verdict = verdict;
        check.detail = detail;
        check
    }

    /// Evaluates the `require` constraints of requirement `req` with `subject` bound.
    fn verify(&self, req: EntryId, subject: EntryId) -> (Verdict, String) {
        let r = self.static_inst(req);
        let subject_name = r.layers.iter().flat_map(|l| l.member.kind.body()).find_map(|m| match &m.kind {
            MemberKind::Usage(u) if u.kind == UsageKind::Subject => m.kind.effective_name(),
            _ => None,
        });
        let Some(subject_name) = subject_name else { return (Verdict::Unknown, "the requirement has no subject".into()) };
        let subject_value = self.entry_value(subject, Span::default()).unwrap_or_else(|_| Value::Instance(self.static_inst(subject)));
        let mut bound = (*r).clone();
        bound.bound.push((subject_name.to_string(), subject_value));
        let r = Rc::new(bound);

        let mut results = Vec::new();
        for layer in &r.layers {
            for m in layer.member.kind.body() {
                let m = unwrap_then(m);
                match &m.kind {
                    MemberKind::Usage(u) if matches!(u.prefix.role, Some(Role::Require | Role::Assert)) => {
                        let mut layers = Vec::new();
                        self.push_layers(m, layer.ns, &mut layers);
                        let c = Rc::new(Instance {
                            layers,
                            parent: Some(r.clone()),
                            bound: Vec::new(),
                            path: r.path.clone(),
                            is_namespace: false,
                            count: Count::One,
                        });
                        let found = c.layers.iter().find_map(|l| result_expr(l.member).map(|e| (e, l.ns)));
                        if let Some((e, ns)) = found {
                            results.push(self.explain(e, &c, ns));
                        }
                    }
                    MemberKind::Expression(e) => results.push(self.explain(e, &r, layer.ns)),
                    _ => {}
                }
            }
        }
        if results.is_empty() {
            return (Verdict::Unknown, "the requirement has no constraints".into());
        }
        let verdict = if results.iter().any(|(v, _)| *v == Verdict::Violated) {
            Verdict::Violated
        } else if results.iter().any(|(v, _)| *v == Verdict::Error) {
            Verdict::Error
        } else if results.iter().any(|(v, _)| *v == Verdict::Unknown) {
            Verdict::Unknown
        } else {
            Verdict::Satisfied
        };
        let detail = results.into_iter().map(|(_, d)| d).collect::<Vec<_>>().join("; ");
        (verdict, detail)
    }

    /// Evaluates a constraint expression and says why it holds or not.
    fn explain(&self, e: &'m Expr, inst: &Rc<Instance<'m>>, ns: Option<EntryId>) -> (Verdict, String) {
        let verdict = match self.eval(e, inst, ns) {
            Ok(Value::Bool(true)) => Verdict::Satisfied,
            Ok(Value::Bool(false)) => Verdict::Violated,
            Ok(v) => return (Verdict::Error, format!("the constraint gives {}, not true or false", describe(&v))),
            Err(Problem::Unknown(why)) => return (Verdict::Unknown, why),
            Err(Problem::Error(why)) => return (Verdict::Error, why),
        };
        let detail = match &e.kind {
            ExprKind::Binary { op, lhs, rhs } if is_comparison(*op) => {
                match (self.eval(lhs, inst, ns), self.eval(rhs, inst, ns)) {
                    (Ok(a), Ok(b)) => format!("{a} {} {b}", op.symbol()),
                    _ => e.to_string(),
                }
            }
            _ => e.to_string(),
        };
        (verdict, detail)
    }
}
