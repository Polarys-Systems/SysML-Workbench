//! Syntax tree for the SysML v2 textual notation.
//!
//! The tree stays close to the source: references are kept as written (not
//! resolved), and every node carries its [`Span`]. See [`crate::Index`] for name
//! lookup.

use crate::span::Span;

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// A simple name. Quoted names (`'my name'`) are stored unquoted and unescaped.
#[derive(Clone, Debug, PartialEq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

/// `A::B::C`
#[derive(Clone, Debug, PartialEq)]
pub struct QualifiedName {
    pub segments: Vec<Name>,
    pub span: Span,
}

impl QualifiedName {
    /// The last segment, i.e. the referenced element's own name.
    pub fn last(&self) -> &Name {
        self.segments.last().expect("qualified names are never empty")
    }
}

/// A feature reference: qualified names joined by `.`, e.g. `vehicle.engine::port`.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    pub chain: Vec<QualifiedName>,
    pub span: Span,
}

impl Reference {
    /// The name of the referenced feature (last segment of the chain).
    pub fn last(&self) -> &Name {
        self.chain.last().expect("references are never empty").last()
    }
}

/// `<shortName> name` — both parts are optional.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Identification {
    pub short_name: Option<Name>,
    pub name: Option<Name>,
}

impl Identification {
    /// The declared name, falling back to the short name.
    pub fn display_name(&self) -> Option<&Name> {
        self.name.as_ref().or(self.short_name.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Model and members
// ---------------------------------------------------------------------------

/// A parsed file: its top-level members.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Model {
    pub members: Vec<Member>,
}

impl Model {
    /// Visits every member depth-first, with its nesting depth (0 = top level).
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Member, usize)) {
        walk_members(&self.members, 0, f);
    }
}

fn walk_members<'a>(members: &'a [Member], depth: usize, f: &mut impl FnMut(&'a Member, usize)) {
    for m in members {
        f(m, depth);
        walk_members(m.kind.body(), depth + 1, f);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
    Protected,
}

/// One entry in a namespace body.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub visibility: Option<Visibility>,
    /// `#Name` metadata prefixes.
    pub metadata: Vec<QualifiedName>,
    pub kind: MemberKind,
    pub span: Span,
}

// Usages dominate real models, so boxing them would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum MemberKind {
    Package(Package),
    Import(Import),
    Alias(Alias),
    Comment(Comment),
    Dependency(Dependency),
    Definition(Definition),
    Usage(Usage),
    Transition(Transition),
    /// `entry`, `do` or `exit` inside a state.
    StateAction(StateAction),
    /// `accept ...;` action (not followed by `then`).
    Accept(Accept),
    /// `send ...;` action.
    Send(Send),
    /// `then <member>`: the member follows the previous one in sequence.
    Then(Box<Member>),
    /// `filter <expr>;`
    Filter(Expr),
    /// A bare expression, e.g. the result of a `calc` or `constraint` body.
    Expression(Expr),
}

impl MemberKind {
    /// Declared identification, if this kind of member has one.
    pub fn identification(&self) -> Option<&Identification> {
        match self {
            MemberKind::Package(p) => Some(&p.id),
            MemberKind::Alias(a) => Some(&a.id),
            MemberKind::Comment(c) => Some(&c.id),
            MemberKind::Dependency(d) => Some(&d.id),
            MemberKind::Definition(d) => Some(&d.id),
            MemberKind::Usage(u) => Some(&u.id),
            MemberKind::Transition(t) => Some(&t.id),
            MemberKind::Then(m) => m.kind.identification(),
            _ => None,
        }
    }

    /// Declared name (or short name).
    pub fn name(&self) -> Option<&Name> {
        self.identification().and_then(Identification::display_name)
    }

    /// The name this member is known by in its namespace: the declared name, or
    /// for an anonymous redefinition (`:>> mass = 10;`) the redefined feature's name.
    pub fn effective_name(&self) -> Option<&str> {
        if let Some(n) = self.name() {
            return Some(&n.text);
        }
        match self {
            MemberKind::Usage(u) => u.redefines.first().map(|r| r.last().text.as_str()),
            MemberKind::Then(m) => m.kind.effective_name(),
            _ => None,
        }
    }

    /// Nested members (empty for members without a body).
    pub fn body(&self) -> &[Member] {
        match self {
            MemberKind::Package(p) => &p.body,
            MemberKind::Dependency(d) => &d.body,
            MemberKind::Definition(d) => &d.body,
            MemberKind::Usage(u) => &u.body,
            MemberKind::Transition(t) => &t.body,
            MemberKind::Accept(a) => &a.body,
            MemberKind::Send(s) => &s.body,
            MemberKind::Then(m) => m.kind.body(),
            MemberKind::StateAction(StateAction { action: Some(ActionRef::Inline(m)), .. }) => {
                m.kind.body()
            }
            _ => &[],
        }
    }
}

// ---------------------------------------------------------------------------
// Namespaces and relationships
// ---------------------------------------------------------------------------

/// `[standard] [library] package Name { ... }`
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Package {
    pub is_library: bool,
    pub is_standard: bool,
    pub id: Identification,
    pub body: Vec<Member>,
}

/// `import [all] A::B;`, `import A::*;`, `import A::**;`, `expose A::*;`
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    /// `expose` (in views) rather than `import`.
    pub is_expose: bool,
    pub is_all: bool,
    /// The imported element, or the namespace for `::*`.
    pub target: QualifiedName,
    /// `::*`: import the members of `target` rather than `target` itself.
    pub is_namespace: bool,
    /// `::**`: also import everything nested.
    pub is_recursive: bool,
    /// `[expr]` filter conditions.
    pub filters: Vec<Expr>,
}

/// `alias Name for A::B;`
#[derive(Clone, Debug, PartialEq)]
pub struct Alias {
    pub id: Identification,
    pub target: QualifiedName,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentKind {
    /// `doc /* ... */`
    Doc,
    /// `comment ... /* ... */` or a bare `/* ... */`
    Comment,
    /// `rep ... language "..." /* ... */`
    Rep,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub kind: CommentKind,
    pub id: Identification,
    /// `about a, b` targets.
    pub about: Vec<Reference>,
    pub locale: Option<String>,
    /// `language "..."` of a textual representation.
    pub language: Option<String>,
    /// The comment body without delimiters.
    pub text: String,
}

/// `dependency [Name from] a, b to c, d;`
#[derive(Clone, Debug, PartialEq)]
pub struct Dependency {
    pub id: Identification,
    pub clients: Vec<Reference>,
    pub suppliers: Vec<Reference>,
    pub body: Vec<Member>,
}

// ---------------------------------------------------------------------------
// Definitions and usages
// ---------------------------------------------------------------------------

/// The keyword before `def`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DefKind {
    Attribute,
    Enum,
    Occurrence,
    Item,
    Part,
    Port,
    Connection,
    Flow,
    Interface,
    Allocation,
    Action,
    State,
    Calc,
    Constraint,
    Requirement,
    Concern,
    Case,
    AnalysisCase,
    VerificationCase,
    UseCase,
    View,
    Viewpoint,
    Rendering,
    Metadata,
}

impl DefKind {
    pub fn keyword(self) -> &'static str {
        self.usage_kind().keyword()
    }

    /// The usage kind declared with the same keyword.
    pub fn usage_kind(self) -> UsageKind {
        use DefKind as D;
        use UsageKind as U;
        match self {
            D::Attribute => U::Attribute,
            D::Enum => U::Enum,
            D::Occurrence => U::Occurrence,
            D::Item => U::Item,
            D::Part => U::Part,
            D::Port => U::Port,
            D::Connection => U::Connection,
            D::Flow => U::Flow,
            D::Interface => U::Interface,
            D::Allocation => U::Allocation,
            D::Action => U::Action,
            D::State => U::State,
            D::Calc => U::Calc,
            D::Constraint => U::Constraint,
            D::Requirement => U::Requirement,
            D::Concern => U::Concern,
            D::Case => U::Case,
            D::AnalysisCase => U::AnalysisCase,
            D::VerificationCase => U::VerificationCase,
            D::UseCase => U::UseCase,
            D::View => U::View,
            D::Viewpoint => U::Viewpoint,
            D::Rendering => U::Rendering,
            D::Metadata => U::Metadata,
        }
    }
}

/// `[abstract] part def Name :> Base { ... }`
#[derive(Clone, Debug, PartialEq)]
pub struct Definition {
    pub kind: DefKind,
    pub is_abstract: bool,
    pub is_variation: bool,
    pub is_individual: bool,
    pub id: Identification,
    /// `:>` / `specializes` targets.
    pub specializes: Vec<QualifiedName>,
    /// `state def S parallel { ... }`
    pub is_parallel: bool,
    pub body: Vec<Member>,
}

/// What kind of feature a usage declares (usually its keyword).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum UsageKind {
    /// No keyword: `x : T;`, `in x;`, `:>> mass = 1;`
    #[default]
    Feature,
    Ref,
    Attribute,
    /// `enum` usage, or a value inside an `enum def`.
    Enum,
    Occurrence,
    Item,
    Part,
    Port,
    Connection,
    Flow,
    Interface,
    Allocation,
    Action,
    State,
    Calc,
    Constraint,
    Requirement,
    Concern,
    Case,
    AnalysisCase,
    VerificationCase,
    UseCase,
    View,
    Viewpoint,
    Rendering,
    Metadata,
    Binding,
    Succession,
    Message,
    Subject,
    Actor,
    Stakeholder,
    Objective,
    Return,
    Fork,
    Join,
    Merge,
    Decide,
}

impl UsageKind {
    pub fn keyword(self) -> &'static str {
        use UsageKind::*;
        match self {
            Feature => "feature",
            Ref => "ref",
            Attribute => "attribute",
            Enum => "enum",
            Occurrence => "occurrence",
            Item => "item",
            Part => "part",
            Port => "port",
            Connection => "connection",
            Flow => "flow",
            Interface => "interface",
            Allocation => "allocation",
            Action => "action",
            State => "state",
            Calc => "calc",
            Constraint => "constraint",
            Requirement => "requirement",
            Concern => "concern",
            Case => "case",
            AnalysisCase => "analysis case",
            VerificationCase => "verification case",
            UseCase => "use case",
            View => "view",
            Viewpoint => "viewpoint",
            Rendering => "rendering",
            Metadata => "metadata",
            Binding => "binding",
            Succession => "succession",
            Message => "message",
            Subject => "subject",
            Actor => "actor",
            Stakeholder => "stakeholder",
            Objective => "objective",
            Return => "return",
            Fork => "fork",
            Join => "join",
            Merge => "merge",
            Decide => "decide",
        }
    }

    /// The definition kind for `<keyword> def`, if this keyword allows one.
    pub fn definition_kind(self) -> Option<DefKind> {
        use DefKind as D;
        use UsageKind as U;
        Some(match self {
            U::Attribute => D::Attribute,
            U::Enum => D::Enum,
            U::Occurrence => D::Occurrence,
            U::Item => D::Item,
            U::Part => D::Part,
            U::Port => D::Port,
            U::Connection => D::Connection,
            U::Flow => D::Flow,
            U::Interface => D::Interface,
            U::Allocation => D::Allocation,
            U::Action => D::Action,
            U::State => D::State,
            U::Calc => D::Calc,
            U::Constraint => D::Constraint,
            U::Requirement => D::Requirement,
            U::Concern => D::Concern,
            U::Case => D::Case,
            U::AnalysisCase => D::AnalysisCase,
            U::VerificationCase => D::VerificationCase,
            U::UseCase => D::UseCase,
            U::View => D::View,
            U::Viewpoint => D::Viewpoint,
            U::Rendering => D::Rendering,
            U::Metadata => D::Metadata,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    InOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Portion {
    Snapshot,
    Timeslice,
}

/// A keyword saying how a usage relates to its owner, e.g. `perform action x;`
/// or `satisfy R by v;`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Perform,
    Exhibit,
    Include,
    Satisfy,
    Verify,
    Assert,
    Assume,
    Require,
    Frame,
    Render,
}

impl Role {
    pub fn keyword(self) -> &'static str {
        match self {
            Role::Perform => "perform",
            Role::Exhibit => "exhibit",
            Role::Include => "include",
            Role::Satisfy => "satisfy",
            Role::Verify => "verify",
            Role::Assert => "assert",
            Role::Assume => "assume",
            Role::Require => "require",
            Role::Frame => "frame",
            Role::Render => "render",
        }
    }

    /// The usage kind implied when no kind keyword follows (`perform x;`).
    pub fn implied_kind(self) -> UsageKind {
        match self {
            Role::Perform => UsageKind::Action,
            Role::Exhibit => UsageKind::State,
            Role::Include => UsageKind::UseCase,
            Role::Satisfy | Role::Verify => UsageKind::Requirement,
            Role::Assert | Role::Assume | Role::Require => UsageKind::Constraint,
            Role::Frame => UsageKind::Concern,
            Role::Render => UsageKind::Rendering,
        }
    }
}

/// Modifier keywords before a usage's kind keyword.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsagePrefix {
    pub direction: Option<Direction>,
    pub role: Option<Role>,
    pub portion: Option<Portion>,
    pub is_abstract: bool,
    pub is_variation: bool,
    pub is_variant: bool,
    pub is_readonly: bool,
    pub is_derived: bool,
    pub is_end: bool,
    /// `ref` before another kind keyword (`ref part x;`).
    pub is_ref: bool,
    pub is_constant: bool,
    pub is_individual: bool,
    pub is_event: bool,
}

/// `T` or `~T` (conjugated port type).
#[derive(Clone, Debug, PartialEq)]
pub struct TypeRef {
    pub conjugated: bool,
    pub name: QualifiedName,
    pub span: Span,
}

/// `[n]`, `[lower..upper]`, `[*]`
#[derive(Clone, Debug, PartialEq)]
pub struct Multiplicity {
    pub lower: Option<Expr>,
    pub upper: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// `= expr`: the value is bound for the feature's whole lifetime.
    Bound,
    /// `:= expr`: initial value.
    Initial,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeatureValue {
    pub kind: ValueKind,
    /// `default = expr` / `default := expr`
    pub is_default: bool,
    pub expr: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorKind {
    /// `connect a to b` / `connect (a, b, c)`
    Connect,
    /// `bind a = b`
    Bind,
    /// `allocate a to b`
    Allocate,
    /// `[of T] from a to b` / `a to b` on a flow
    Flow,
    /// `[of T] from a to b` on a message
    Message,
    /// `first a then b`, or `then b` with an implicit source
    Succession,
}

/// One end of a connector: `[mult] name ::> target`.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectorEnd {
    pub name: Option<Name>,
    pub multiplicity: Option<Multiplicity>,
    pub target: Reference,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Connector {
    pub kind: ConnectorKind,
    /// `of T` on flows and messages.
    pub item_type: Option<TypeRef>,
    pub ends: Vec<ConnectorEnd>,
    /// `then x;`: a succession whose source is the preceding member.
    pub implicit_source: bool,
}

/// A feature declaration: `part engine : Engine[1] :> parts { ... }`,
/// `attribute mass = 1500 [kg];`, `connect a to b;`, `perform action drive;` ...
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub kind: UsageKind,
    pub prefix: UsagePrefix,
    pub id: Identification,
    /// `:` / `defined by` / `typed by`
    pub typed_by: Vec<TypeRef>,
    /// `:>` / `subsets`
    pub subsets: Vec<Reference>,
    /// `:>>` / `redefines`
    pub redefines: Vec<Reference>,
    /// `::>` / `references`; also the target of `perform x;`, `satisfy R by v;` etc.
    pub references: Vec<Reference>,
    pub multiplicity: Option<Multiplicity>,
    pub is_ordered: bool,
    pub is_nonunique: bool,
    pub value: Option<FeatureValue>,
    pub connector: Option<Connector>,
    /// `about a, b` (metadata usages).
    pub about: Vec<Reference>,
    /// `satisfy R by v` / `verify R by v`
    pub by: Option<Reference>,
    pub is_parallel: bool,
    pub body: Vec<Member>,
}

// ---------------------------------------------------------------------------
// Behavior
// ---------------------------------------------------------------------------

/// `transition [name first] source accept trigger if guard do effect then target;`
/// and the short forms `accept T then s;`, `if g then s;`, `else s;`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transition {
    pub id: Identification,
    pub source: Option<Reference>,
    pub trigger: Option<Trigger>,
    pub guard: Option<Expr>,
    pub effect: Option<ActionRef>,
    pub target: Option<Reference>,
    /// `else target;` branch of a decision.
    pub is_else: bool,
    pub body: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TriggerKind {
    /// `accept [name :] Type`
    Signal { name: Option<Name>, ty: Option<TypeRef> },
    /// `accept when <condition>`
    When(Expr),
    /// `accept at <time>`
    At(Expr),
    /// `accept after <duration>`
    After(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// `via port`
    pub via: Option<Reference>,
    pub span: Span,
}

/// An action given by reference (`do drive`) or declared in place (`entry action a { }`).
#[derive(Clone, Debug, PartialEq)]
pub enum ActionRef {
    Ref(Reference),
    Inline(Box<Member>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateActionKind {
    Entry,
    Do,
    Exit,
}

/// `entry;`, `entry action init;`, `do monitor;`, `exit action { ... }`
#[derive(Clone, Debug, PartialEq)]
pub struct StateAction {
    pub kind: StateActionKind,
    pub action: Option<ActionRef>,
}

/// `accept sig : Signal via port;`
#[derive(Clone, Debug, PartialEq)]
pub struct Accept {
    pub trigger: Trigger,
    pub body: Vec<Member>,
}

/// `send Signal() via port to target;`
#[derive(Clone, Debug, PartialEq)]
pub struct Send {
    pub payload: Expr,
    pub via: Option<Reference>,
    pub to: Option<Reference>,
    pub body: Vec<Member>,
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Literal(Literal),
    /// `*` (unbounded, in multiplicities).
    Infinity,
    Name(QualifiedName),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `x istype T`, `x as T`, `@T` (operand omitted).
    Classification {
        op: ClassificationOp,
        operand: Option<Box<Expr>>,
        ty: QualifiedName,
    },
    /// `if cond ? then else otherwise`
    Conditional {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    /// `base.member`
    Member {
        base: Box<Expr>,
        member: Name,
    },
    /// `F(args)`
    Call {
        callee: QualifiedName,
        args: Vec<Argument>,
    },
    /// `base->func(args)`
    Arrow {
        base: Box<Expr>,
        func: QualifiedName,
        args: Vec<Argument>,
    },
    /// `base#(index)`
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// `value [unit]`, e.g. `9.81 [m/s^2]`
    Quantity {
        value: Box<Expr>,
        unit: Box<Expr>,
    },
    /// `(a, b, c)`; `()` is the empty sequence (null).
    Sequence(Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    Integer(u64),
    Real(f64),
    String(String),
    Bool(bool),
    Null,
}

/// `name = value` or a positional `value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Argument {
    pub name: Option<Name>,
    pub value: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
    Not,
    BitNot,
}

impl UnaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            UnaryOp::Plus => "+",
            UnaryOp::Minus => "-",
            UnaryOp::Not => "not",
            UnaryOp::BitNot => "~",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    NullCoalesce,
    Implies,
    Or,
    Xor,
    And,
    Eq,
    Ne,
    Same,
    NotSame,
    Lt,
    Gt,
    Le,
    Ge,
    Range,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        use BinaryOp::*;
        match self {
            NullCoalesce => "??",
            Implies => "implies",
            Or => "or",
            Xor => "xor",
            And => "and",
            Eq => "==",
            Ne => "!=",
            Same => "===",
            NotSame => "!==",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            Range => "..",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Rem => "%",
            Pow => "**",
        }
    }

    /// Binding strength; higher binds tighter.
    pub fn precedence(self) -> u8 {
        use BinaryOp::*;
        match self {
            NullCoalesce => 1,
            Implies => 2,
            Or => 3,
            Xor => 4,
            And => 5,
            Eq | Ne | Same | NotSame => 6,
            Lt | Gt | Le | Ge => 8,
            Range => 9,
            Add | Sub => 10,
            Mul | Div | Rem => 11,
            Pow => 12,
        }
    }

    pub fn is_right_assoc(self) -> bool {
        self == BinaryOp::Pow
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassificationOp {
    IsType,
    HasType,
    As,
    Meta,
    /// `@T`
    At,
}

impl ClassificationOp {
    /// Sits between equality and relational operators.
    pub const PRECEDENCE: u8 = 7;

    pub fn symbol(self) -> &'static str {
        match self {
            ClassificationOp::IsType => "istype",
            ClassificationOp::HasType => "hastype",
            ClassificationOp::As => "as",
            ClassificationOp::Meta => "meta",
            ClassificationOp::At => "@",
        }
    }
}
