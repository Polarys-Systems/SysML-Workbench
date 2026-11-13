//! Recursive-descent parser producing the [`crate::ast`] tree.
//!
//! Each member is parsed on its own; when one fails, the error is recorded and
//! the parser skips to the end of that member (`;` or its closing `}`), so one
//! mistake does not hide the rest of the model.

mod expr;

use crate::Diagnostic;
use crate::ast::*;
use crate::lexer::{Token, TokenKind, comment_text, strip_delimiters, unescape};
use crate::span::Span;

pub(crate) type PResult<T> = Result<T, Diagnostic>;

/// Limit for nested bodies and expressions, so hostile input cannot overflow the stack.
const MAX_DEPTH: usize = 200;

pub(crate) struct Parser<'s> {
    src: &'s str,
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
    pub(crate) diags: Vec<Diagnostic>,
}

impl<'s> Parser<'s> {
    /// `tokens` must end with [`TokenKind::Eof`] (as [`crate::lexer::lex`] produces).
    pub(crate) fn new(src: &'s str, tokens: Vec<Token>) -> Self {
        assert!(matches!(tokens.last(), Some(t) if t.kind == TokenKind::Eof));
        Self { src, tokens, pos: 0, depth: 0, diags: Vec::new() }
    }

    pub(crate) fn parse_model(&mut self) -> Model {
        Model { members: self.parse_members(false) }
    }

    // -----------------------------------------------------------------------
    // Token helpers
    // -----------------------------------------------------------------------

    fn peek(&self) -> Token {
        self.peek_at(0)
    }

    fn peek_at(&self, n: usize) -> Token {
        self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    fn bump(&mut self) -> Token {
        let t = self.peek();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        t
    }

    fn current_keyword(&self) -> Option<&'static str> {
        match self.peek().kind {
            TokenKind::Keyword(k) => Some(k),
            _ => None,
        }
    }

    fn kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_at(n).kind, TokenKind::Keyword(k) if k == kw)
    }

    fn is_kw(&self, kw: &str) -> bool {
        self.kw_at(0, kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        let hit = self.is_kw(kw);
        if hit {
            self.bump();
        }
        hit
    }

    fn expect_kw(&mut self, kw: &str) -> PResult<Token> {
        if self.is_kw(kw) { Ok(self.bump()) } else { Err(self.expected(&format!("'{kw}'"))) }
    }

    fn sym_at(&self, n: usize, sym: &str) -> bool {
        matches!(self.peek_at(n).kind, TokenKind::Symbol(s) if s == sym)
    }

    fn is_sym(&self, sym: &str) -> bool {
        self.sym_at(0, sym)
    }

    fn eat_sym(&mut self, sym: &str) -> bool {
        let hit = self.is_sym(sym);
        if hit {
            self.bump();
        }
        hit
    }

    fn expect_sym(&mut self, sym: &str) -> PResult<Token> {
        if self.is_sym(sym) { Ok(self.bump()) } else { Err(self.expected(&format!("'{sym}'"))) }
    }

    fn name_at(&self, n: usize) -> bool {
        matches!(self.peek_at(n).kind, TokenKind::Ident | TokenKind::UnrestrictedName)
    }

    fn is_name(&self) -> bool {
        self.name_at(0)
    }

    fn start(&self) -> usize {
        self.peek().span.start
    }

    fn prev_end(&self) -> usize {
        if self.pos == 0 { 0 } else { self.tokens[self.pos - 1].span.end }
    }

    fn span_from(&self, start: usize) -> Span {
        Span::new(start, self.prev_end().max(start))
    }

    fn expected(&self, what: &str) -> Diagnostic {
        let t = self.peek();
        let found = match t.kind {
            TokenKind::Eof => "end of input".to_string(),
            _ => format!("'{}'", t.span.text(self.src)),
        };
        Diagnostic::error(format!("expected {what}, found {found}"), t.span)
    }

    fn enter(&mut self) -> PResult<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Diagnostic::error("nesting too deep", self.peek().span));
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // -----------------------------------------------------------------------
    // Names and references
    // -----------------------------------------------------------------------

    fn parse_name(&mut self) -> PResult<Name> {
        let t = self.peek();
        let raw = t.span.text(self.src);
        let text = match t.kind {
            TokenKind::Ident => raw.to_string(),
            TokenKind::UnrestrictedName => unescape(strip_delimiters(raw, '\'')),
            _ => return Err(self.expected("a name")),
        };
        self.bump();
        Ok(Name { text, span: t.span })
    }

    fn parse_qualified_name(&mut self) -> PResult<QualifiedName> {
        let first = self.parse_name()?;
        let start = first.span.start;
        let mut segments = vec![first];
        while self.is_sym("::") && self.name_at(1) {
            self.bump();
            segments.push(self.parse_name()?);
        }
        Ok(QualifiedName { segments, span: self.span_from(start) })
    }

    fn parse_reference(&mut self) -> PResult<Reference> {
        let first = self.parse_qualified_name()?;
        let start = first.span.start;
        let mut chain = vec![first];
        while self.is_sym(".") && self.name_at(1) {
            self.bump();
            chain.push(self.parse_qualified_name()?);
        }
        Ok(Reference { chain, span: self.span_from(start) })
    }

    fn parse_reference_list(&mut self) -> PResult<Vec<Reference>> {
        let mut list = vec![self.parse_reference()?];
        while self.eat_sym(",") {
            list.push(self.parse_reference()?);
        }
        Ok(list)
    }

    fn parse_qualified_name_list(&mut self) -> PResult<Vec<QualifiedName>> {
        let mut list = vec![self.parse_qualified_name()?];
        while self.eat_sym(",") {
            list.push(self.parse_qualified_name()?);
        }
        Ok(list)
    }

    fn parse_type_ref(&mut self) -> PResult<TypeRef> {
        let start = self.start();
        let conjugated = self.eat_sym("~");
        let name = self.parse_qualified_name()?;
        Ok(TypeRef { conjugated, name, span: self.span_from(start) })
    }

    fn parse_type_list(&mut self, out: &mut Vec<TypeRef>) -> PResult<()> {
        out.push(self.parse_type_ref()?);
        while self.eat_sym(",") {
            out.push(self.parse_type_ref()?);
        }
        Ok(())
    }

    fn parse_identification(&mut self) -> PResult<Identification> {
        let mut id = Identification::default();
        if self.eat_sym("<") {
            id.short_name = Some(self.parse_name()?);
            self.expect_sym(">")?;
        }
        if self.is_name() {
            id.name = Some(self.parse_name()?);
        }
        Ok(id)
    }

    fn parse_string(&mut self) -> PResult<String> {
        let t = self.peek();
        if t.kind != TokenKind::String {
            return Err(self.expected("a string"));
        }
        self.bump();
        Ok(unescape(strip_delimiters(t.span.text(self.src), '"')))
    }

    // -----------------------------------------------------------------------
    // Bodies and members
    // -----------------------------------------------------------------------

    /// `;` or `{ members }`.
    fn parse_body(&mut self) -> PResult<Vec<Member>> {
        if self.eat_sym(";") {
            return Ok(Vec::new());
        }
        if !self.is_sym("{") {
            return Err(self.expected("';' or '{'"));
        }
        self.enter()?;
        self.bump();
        let members = self.parse_members(true);
        self.leave();
        if !self.eat_sym("}") {
            let d = self.expected("'}'");
            self.diags.push(d);
        }
        Ok(members)
    }

    fn parse_members(&mut self, in_braces: bool) -> Vec<Member> {
        let mut members = Vec::new();
        loop {
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Symbol("}") if in_braces => break,
                TokenKind::Symbol("}") => {
                    let t = self.bump();
                    self.diags.push(Diagnostic::error("unmatched '}'", t.span));
                    continue;
                }
                _ => {}
            }
            let before = self.pos;
            match self.parse_member() {
                Ok(m) => members.push(m),
                Err(d) => {
                    self.diags.push(d);
                    self.recover(before);
                }
            }
        }
        members
    }

    /// Skips the rest of a broken member: up to and including the next `;`, or
    /// through a `{ ... }` block, stopping before the enclosing `}`.
    fn recover(&mut self, before: usize) {
        let mut depth = 0usize;
        loop {
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Symbol("{") => depth += 1,
                TokenKind::Symbol("}") => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                TokenKind::Symbol(";") if depth == 0 => {
                    self.bump();
                    break;
                }
                _ => {}
            }
            self.bump();
        }
        if self.pos == before && !self.is_sym("}") {
            self.bump();
        }
    }

    fn parse_member(&mut self) -> PResult<Member> {
        let start = self.start();
        let visibility = match self.current_keyword() {
            Some("public") => Some(Visibility::Public),
            Some("private") => Some(Visibility::Private),
            Some("protected") => Some(Visibility::Protected),
            _ => None,
        };
        if visibility.is_some() {
            self.bump();
        }
        let mut metadata = Vec::new();
        while self.eat_sym("#") {
            metadata.push(self.parse_qualified_name()?);
        }
        let kind = self.parse_member_kind()?;
        Ok(Member { visibility, metadata, kind, span: self.span_from(start) })
    }

    fn parse_member_kind(&mut self) -> PResult<MemberKind> {
        match self.peek().kind {
            TokenKind::BlockComment => self.parse_comment().map(MemberKind::Comment),
            TokenKind::Symbol("@") => self.parse_metadata_shorthand().map(MemberKind::Usage),
            TokenKind::Keyword(k) => match k {
                "package" | "library" | "standard" => self.parse_package().map(MemberKind::Package),
                "import" | "expose" => self.parse_import().map(MemberKind::Import),
                "alias" => self.parse_alias().map(MemberKind::Alias),
                "doc" | "comment" | "rep" => self.parse_comment().map(MemberKind::Comment),
                "dependency" => self.parse_dependency().map(MemberKind::Dependency),
                "transition" => self.parse_transition().map(MemberKind::Transition),
                "entry" | "do" | "exit" => self.parse_state_action().map(MemberKind::StateAction),
                "accept" => self.parse_accept(),
                "send" => self.parse_send(true).map(MemberKind::Send),
                "then" => self.parse_then(),
                "if" => self.parse_if(),
                "else" => self.parse_else().map(MemberKind::Transition),
                "filter" => {
                    self.bump();
                    let e = self.parse_expr()?;
                    self.expect_sym(";")?;
                    Ok(MemberKind::Filter(e))
                }
                _ => self.parse_def_or_usage(),
            },
            _ => self.parse_def_or_usage(),
        }
    }

    // -----------------------------------------------------------------------
    // Packages, imports, comments, dependencies
    // -----------------------------------------------------------------------

    fn parse_package(&mut self) -> PResult<Package> {
        let is_standard = self.eat_kw("standard");
        let is_library = self.eat_kw("library");
        self.expect_kw("package")?;
        let id = self.parse_identification()?;
        let body = self.parse_body()?;
        Ok(Package { is_library, is_standard, id, body })
    }

    fn parse_import(&mut self) -> PResult<Import> {
        let is_expose = self.bump().kind == TokenKind::Keyword("expose");
        let is_all = self.eat_kw("all");
        let first = self.parse_name()?;
        let start = first.span.start;
        let mut end = first.span.end;
        let mut segments = vec![first];
        let (mut is_namespace, mut is_recursive) = (false, false);
        while self.is_sym("::") {
            if self.name_at(1) {
                self.bump();
                let n = self.parse_name()?;
                end = n.span.end;
                segments.push(n);
            } else if self.sym_at(1, "*") {
                self.bump();
                self.bump();
                is_namespace = true;
                if self.is_sym("::") && self.sym_at(1, "**") {
                    self.bump();
                    self.bump();
                    is_recursive = true;
                }
                break;
            } else if self.sym_at(1, "**") {
                self.bump();
                self.bump();
                is_recursive = true;
                break;
            } else {
                self.bump();
                return Err(self.expected("a name, '*' or '**'"));
            }
        }
        let target = QualifiedName { segments, span: Span::new(start, end) };
        let mut filters = Vec::new();
        while self.eat_sym("[") {
            filters.push(self.parse_expr()?);
            self.expect_sym("]")?;
        }
        self.parse_body()?;
        Ok(Import { is_expose, is_all, target, is_namespace, is_recursive, filters })
    }

    fn parse_alias(&mut self) -> PResult<Alias> {
        self.expect_kw("alias")?;
        let id = self.parse_identification()?;
        self.expect_kw("for")?;
        let target = self.parse_qualified_name()?;
        self.parse_body()?;
        Ok(Alias { id, target })
    }

    fn parse_comment(&mut self) -> PResult<Comment> {
        let (kind, bare) = match self.peek().kind {
            TokenKind::Keyword("doc") => (CommentKind::Doc, false),
            TokenKind::Keyword("rep") => (CommentKind::Rep, false),
            TokenKind::Keyword("comment") => (CommentKind::Comment, false),
            _ => (CommentKind::Comment, true),
        };
        let mut c = Comment {
            kind,
            id: Identification::default(),
            about: Vec::new(),
            locale: None,
            language: None,
            text: String::new(),
        };
        if !bare {
            self.bump();
            c.id = self.parse_identification()?;
            if kind == CommentKind::Comment && self.eat_kw("about") {
                c.about = self.parse_reference_list()?;
            }
            if kind == CommentKind::Rep {
                self.expect_kw("language")?;
                c.language = Some(self.parse_string()?);
            }
            if self.eat_kw("locale") {
                c.locale = Some(self.parse_string()?);
            }
        }
        let t = self.peek();
        if t.kind != TokenKind::BlockComment {
            return Err(self.expected("a comment body '/* ... */'"));
        }
        self.bump();
        c.text = comment_text(t.span.text(self.src));
        Ok(c)
    }

    fn parse_dependency(&mut self) -> PResult<Dependency> {
        self.expect_kw("dependency")?;
        // `dependency [id from] clients to suppliers`: the id is only present if `from` follows.
        let save = self.pos;
        let id = match self.parse_identification() {
            Ok(id) if self.eat_kw("from") => id,
            _ => {
                self.pos = save;
                self.eat_kw("from");
                Identification::default()
            }
        };
        let clients = self.parse_reference_list()?;
        self.expect_kw("to")?;
        let suppliers = self.parse_reference_list()?;
        let body = self.parse_body()?;
        Ok(Dependency { id, clients, suppliers, body })
    }

    // -----------------------------------------------------------------------
    // Definitions and usages
    // -----------------------------------------------------------------------

    /// Does the current position start a bare expression rather than a feature
    /// declaration? Used for result expressions in `calc` / `constraint` bodies.
    fn starts_expression(&self) -> bool {
        match self.peek().kind {
            TokenKind::Integer | TokenKind::Real | TokenKind::String => true,
            TokenKind::Symbol(s) => matches!(s, "(" | "-" | "+" | "!"),
            TokenKind::Keyword(k) => matches!(k, "true" | "false" | "null" | "not"),
            TokenKind::Ident | TokenKind::UnrestrictedName => {
                let mut i = 1;
                while self.sym_at(i, "::") && self.name_at(i + 1) {
                    i += 2;
                }
                if i > 1 {
                    return true; // a qualified name cannot be declared
                }
                match self.peek_at(i).kind {
                    TokenKind::Symbol(s) => {
                        !matches!(s, ":" | ":>" | ":>>" | "::>" | ";" | "{" | "[" | "=" | ":=")
                    }
                    TokenKind::Keyword(k) => !matches!(
                        k,
                        "redefines"
                            | "subsets"
                            | "references"
                            | "defined"
                            | "typed"
                            | "default"
                            | "ordered"
                            | "nonunique"
                            | "about"
                            | "parallel"
                    ),
                    TokenKind::Eof => false,
                    _ => true,
                }
            }
            _ => false,
        }
    }

    /// Can a keyword-less feature declaration start here (`x : T`, `:>> y`, ...)?
    fn at_feature_declaration(&self) -> bool {
        self.is_name()
            || matches!(self.peek().kind, TokenKind::Symbol("<" | ":" | ":>" | ":>>" | "::>" | "["))
            || matches!(
                self.current_keyword(),
                Some("redefines" | "subsets" | "references" | "defined" | "typed")
            )
    }

    /// A kind keyword at offset `n` and how many tokens it spans.
    fn kind_keyword_at(&self, n: usize) -> Option<(UsageKind, usize)> {
        use UsageKind as U;
        let TokenKind::Keyword(k) = self.peek_at(n).kind else { return None };
        let two = |second: &str, kind| self.kw_at(n + 1, second).then_some((kind, 2));
        let kind = match k {
            "attribute" => U::Attribute,
            "enum" => U::Enum,
            "occurrence" => U::Occurrence,
            "item" => U::Item,
            "part" => U::Part,
            "port" => U::Port,
            "connection" => U::Connection,
            "flow" => U::Flow,
            "interface" => U::Interface,
            "allocation" => U::Allocation,
            "action" => U::Action,
            "state" => U::State,
            "calc" => U::Calc,
            "constraint" => U::Constraint,
            "requirement" => U::Requirement,
            "concern" => U::Concern,
            "case" => U::Case,
            "view" => U::View,
            "viewpoint" => U::Viewpoint,
            "rendering" => U::Rendering,
            "metadata" => U::Metadata,
            "analysis" => return two("case", U::AnalysisCase),
            "verification" => return two("case", U::VerificationCase),
            "use" => return two("case", U::UseCase),
            "succession" => return Some(two("flow", U::Flow).unwrap_or((U::Succession, 1))),
            "ref" => U::Ref,
            "binding" => U::Binding,
            "message" => U::Message,
            "subject" => U::Subject,
            "actor" => U::Actor,
            "stakeholder" => U::Stakeholder,
            "objective" => U::Objective,
            "return" => U::Return,
            "fork" => U::Fork,
            "join" => U::Join,
            "merge" => U::Merge,
            "decide" => U::Decide,
            _ => return None,
        };
        Some((kind, 1))
    }

    fn parse_usage_prefix(&mut self) -> UsagePrefix {
        let mut p = UsagePrefix::default();
        while let Some(k) = self.current_keyword() {
            match k {
                "in" => p.direction = Some(Direction::In),
                "out" => p.direction = Some(Direction::Out),
                "inout" => p.direction = Some(Direction::InOut),
                "abstract" => p.is_abstract = true,
                "variation" => p.is_variation = true,
                "variant" => p.is_variant = true,
                "readonly" => p.is_readonly = true,
                "derived" => p.is_derived = true,
                "end" => p.is_end = true,
                "constant" => p.is_constant = true,
                "individual" => p.is_individual = true,
                "event" => p.is_event = true,
                "snapshot" => p.portion = Some(Portion::Snapshot),
                "timeslice" => p.portion = Some(Portion::Timeslice),
                "perform" => p.role = Some(Role::Perform),
                "exhibit" => p.role = Some(Role::Exhibit),
                "include" => p.role = Some(Role::Include),
                "satisfy" => p.role = Some(Role::Satisfy),
                "verify" => p.role = Some(Role::Verify),
                "assert" => p.role = Some(Role::Assert),
                "assume" => p.role = Some(Role::Assume),
                "require" => p.role = Some(Role::Require),
                "frame" => p.role = Some(Role::Frame),
                "render" => p.role = Some(Role::Render),
                // `ref part x` vs. `ref x : T`
                "ref" if self.kind_keyword_at(1).is_some_and(|(k, _)| k != UsageKind::Ref) => {
                    p.is_ref = true
                }
                _ => break,
            }
            self.bump();
        }
        p
    }

    fn parse_def_or_usage(&mut self) -> PResult<MemberKind> {
        if self.starts_expression() {
            let e = self.parse_expr()?;
            self.eat_sym(";");
            return Ok(MemberKind::Expression(e));
        }
        let prefix = self.parse_usage_prefix();
        let kind = self.kind_keyword_at(0).map(|(kind, len)| {
            for _ in 0..len {
                self.bump();
            }
            kind
        });

        if self.is_kw("def") {
            let def = self.bump();
            let Some(def_kind) = kind.and_then(UsageKind::definition_kind) else {
                return Err(Diagnostic::error(
                    "'def' must follow a definition keyword such as 'part' or 'action'",
                    def.span,
                ));
            };
            return self.parse_definition_rest(def_kind, &prefix).map(MemberKind::Definition);
        }

        let kind = match kind {
            Some(kind) => kind,
            None if self.is_kw("connect") => UsageKind::Connection,
            None if self.is_kw("bind") => UsageKind::Binding,
            None if self.is_kw("allocate") => UsageKind::Allocation,
            None if self.is_kw("first") => UsageKind::Succession,
            None if prefix.role.is_some() && self.is_name() => {
                return self.parse_role_reference(prefix).map(MemberKind::Usage);
            }
            None if self.at_feature_declaration() => UsageKind::Feature,
            None => return Err(self.expected("a member declaration")),
        };
        self.parse_usage_rest(kind, prefix).map(MemberKind::Usage)
    }

    fn parse_definition_rest(&mut self, kind: DefKind, prefix: &UsagePrefix) -> PResult<Definition> {
        let id = self.parse_identification()?;
        let mut specializes = Vec::new();
        while self.eat_sym(":>") || self.eat_kw("specializes") {
            specializes.extend(self.parse_qualified_name_list()?);
        }
        let is_parallel = self.eat_kw("parallel");
        let mut body = self.parse_body()?;
        if kind == DefKind::Enum {
            // Inside an enum def, `red;` declares an enumeration value.
            for m in &mut body {
                if let MemberKind::Usage(u) = &mut m.kind
                    && u.kind == UsageKind::Feature
                {
                    u.kind = UsageKind::Enum;
                }
            }
        }
        Ok(Definition {
            kind,
            is_abstract: prefix.is_abstract,
            is_variation: prefix.is_variation,
            is_individual: prefix.is_individual,
            id,
            specializes,
            is_parallel,
            body,
        })
    }

    /// `perform x;`, `satisfy R by v;`, `exhibit s;` — a role keyword followed by
    /// a reference to an existing feature instead of a declaration.
    fn parse_role_reference(&mut self, prefix: UsagePrefix) -> PResult<Usage> {
        let kind = prefix.role.map(Role::implied_kind).unwrap_or_default();
        let mut u = Usage { kind, prefix, ..Default::default() };
        u.references.push(self.parse_reference()?);
        self.parse_usage_tail(&mut u)?;
        Ok(u)
    }

    fn parse_usage_rest(&mut self, kind: UsageKind, prefix: UsagePrefix) -> PResult<Usage> {
        let mut u = Usage { kind, prefix, ..Default::default() };
        // `flow a.out to b.in;` starts with an end, not a name.
        let starts_with_end = matches!(kind, UsageKind::Flow | UsageKind::Message)
            && self.is_name()
            && (self.sym_at(1, ".") || self.sym_at(1, "::") || self.kw_at(1, "to"));
        if !starts_with_end {
            u.id = self.parse_identification()?;
        }
        self.parse_usage_tail(&mut u)?;
        Ok(u)
    }

    fn parse_usage_tail(&mut self, u: &mut Usage) -> PResult<()> {
        self.parse_specializations(u)?;
        self.parse_feature_value(u)?;
        self.parse_connector_part(u)?;
        if self.eat_kw("about") {
            u.about = self.parse_reference_list()?;
        }
        if self.eat_kw("by") {
            u.by = Some(self.parse_reference()?);
        }
        if self.eat_kw("parallel") {
            u.is_parallel = true;
        }
        u.body = self.parse_body()?;
        Ok(())
    }

    fn parse_specializations(&mut self, u: &mut Usage) -> PResult<()> {
        loop {
            if self.eat_sym(":") {
                self.parse_type_list(&mut u.typed_by)?;
            } else if self.is_kw("defined") || self.is_kw("typed") {
                self.bump();
                self.expect_kw("by")?;
                self.parse_type_list(&mut u.typed_by)?;
            } else if self.eat_sym(":>") || self.eat_kw("subsets") {
                u.subsets.extend(self.parse_reference_list()?);
            } else if self.eat_sym(":>>") || self.eat_kw("redefines") {
                u.redefines.extend(self.parse_reference_list()?);
            } else if self.eat_sym("::>") || self.eat_kw("references") {
                u.references.extend(self.parse_reference_list()?);
            } else if self.is_sym("[") {
                u.multiplicity = Some(self.parse_multiplicity()?);
            } else if self.eat_kw("ordered") {
                u.is_ordered = true;
            } else if self.eat_kw("nonunique") {
                u.is_nonunique = true;
            } else {
                return Ok(());
            }
        }
    }

    fn parse_multiplicity(&mut self) -> PResult<Multiplicity> {
        let start = self.start();
        self.expect_sym("[")?;
        let first = self.parse_bound()?;
        let (lower, upper) =
            if self.eat_sym("..") { (Some(first), self.parse_bound()?) } else { (None, first) };
        self.expect_sym("]")?;
        Ok(Multiplicity { lower, upper, span: self.span_from(start) })
    }

    fn parse_feature_value(&mut self, u: &mut Usage) -> PResult<()> {
        let start = self.start();
        let is_default = self.eat_kw("default");
        let kind = if self.eat_sym("=") {
            ValueKind::Bound
        } else if self.eat_sym(":=") {
            ValueKind::Initial
        } else if is_default {
            ValueKind::Bound
        } else {
            return Ok(());
        };
        let expr = self.parse_expr()?;
        u.value = Some(FeatureValue { kind, is_default, expr, span: self.span_from(start) });
        Ok(())
    }

    fn parse_connector_part(&mut self, u: &mut Usage) -> PResult<()> {
        let flow_kind =
            if u.kind == UsageKind::Message { ConnectorKind::Message } else { ConnectorKind::Flow };
        let is_flow = matches!(u.kind, UsageKind::Flow | UsageKind::Message);
        let kind = match self.current_keyword() {
            Some("connect") => ConnectorKind::Connect,
            Some("bind") => ConnectorKind::Bind,
            Some("allocate") => ConnectorKind::Allocate,
            Some("first") => ConnectorKind::Succession,
            Some("of" | "from") => flow_kind,
            _ if is_flow && self.is_name() => flow_kind,
            _ => return Ok(()),
        };
        let mut c = Connector { kind, item_type: None, ends: Vec::new(), implicit_source: false };
        match kind {
            ConnectorKind::Connect | ConnectorKind::Allocate => {
                self.bump();
                if self.eat_sym("(") {
                    loop {
                        c.ends.push(self.parse_connector_end()?);
                        if !self.eat_sym(",") {
                            break;
                        }
                    }
                    self.expect_sym(")")?;
                } else {
                    c.ends.push(self.parse_connector_end()?);
                    self.expect_kw("to")?;
                    c.ends.push(self.parse_connector_end()?);
                }
            }
            ConnectorKind::Bind => {
                self.bump();
                c.ends.push(self.parse_connector_end()?);
                self.expect_sym("=")?;
                c.ends.push(self.parse_connector_end()?);
            }
            ConnectorKind::Succession => {
                self.bump();
                c.ends.push(self.parse_connector_end()?);
                if self.eat_kw("then") {
                    c.ends.push(self.parse_connector_end()?);
                }
            }
            ConnectorKind::Flow | ConnectorKind::Message => {
                if self.eat_kw("of") {
                    c.item_type = Some(self.parse_type_ref()?);
                }
                let has_ends = self.eat_kw("from") || self.is_name();
                if has_ends {
                    c.ends.push(self.parse_connector_end()?);
                    self.expect_kw("to")?;
                    c.ends.push(self.parse_connector_end()?);
                }
            }
        }
        u.connector = Some(c);
        Ok(())
    }

    /// `[mult] [name ::>] target [mult]`
    fn parse_connector_end(&mut self) -> PResult<ConnectorEnd> {
        let start = self.start();
        let mut multiplicity =
            if self.is_sym("[") { Some(self.parse_multiplicity()?) } else { None };
        let mut name = None;
        if self.is_name() && (self.sym_at(1, "::>") || self.kw_at(1, "references")) {
            name = Some(self.parse_name()?);
            self.bump();
        }
        let target = self.parse_reference()?;
        if multiplicity.is_none() && self.is_sym("[") {
            multiplicity = Some(self.parse_multiplicity()?);
        }
        Ok(ConnectorEnd { name, multiplicity, target, span: self.span_from(start) })
    }

    /// `@Type;`, `@name : Type about x { ... }`
    fn parse_metadata_shorthand(&mut self) -> PResult<Usage> {
        self.expect_sym("@")?;
        let mut u = Usage { kind: UsageKind::Metadata, ..Default::default() };
        if self.is_name() && (self.sym_at(1, ":") || self.kw_at(1, "typed")) {
            u.id.name = Some(self.parse_name()?);
            if !self.eat_sym(":") {
                self.bump();
                self.expect_kw("by")?;
            }
        }
        u.typed_by.push(self.parse_type_ref()?);
        if self.eat_kw("about") {
            u.about = self.parse_reference_list()?;
        }
        u.body = self.parse_body()?;
        Ok(u)
    }

    // -----------------------------------------------------------------------
    // Behavior: successions, transitions, state and action members
    // -----------------------------------------------------------------------

    /// `then x;` (succession to an existing member) or `then <member>`.
    fn parse_then(&mut self) -> PResult<MemberKind> {
        self.expect_kw("then")?;
        if !self.is_name() {
            return Ok(MemberKind::Then(Box::new(self.parse_member()?)));
        }
        let end = self.parse_connector_end()?;
        let body = self.parse_body()?;
        Ok(MemberKind::Usage(Usage {
            kind: UsageKind::Succession,
            connector: Some(Connector {
                kind: ConnectorKind::Succession,
                item_type: None,
                ends: vec![end],
                implicit_source: true,
            }),
            body,
            ..Default::default()
        }))
    }

    /// `if guard then target;` in a decision, or a conditional expression `if c ? a else b`.
    fn parse_if(&mut self) -> PResult<MemberKind> {
        let save = self.pos;
        self.expect_kw("if")?;
        if let Ok(guard) = self.parse_expr()
            && self.eat_kw("then")
        {
            let target = self.parse_reference()?;
            let body = self.parse_body()?;
            return Ok(MemberKind::Transition(Transition {
                guard: Some(guard),
                target: Some(target),
                body,
                ..Default::default()
            }));
        }
        self.pos = save;
        let e = self.parse_expr()?;
        self.eat_sym(";");
        Ok(MemberKind::Expression(e))
    }

    fn parse_else(&mut self) -> PResult<Transition> {
        self.expect_kw("else")?;
        let target = self.parse_reference()?;
        let body = self.parse_body()?;
        Ok(Transition { is_else: true, target: Some(target), body, ..Default::default() })
    }

    fn parse_transition(&mut self) -> PResult<Transition> {
        self.expect_kw("transition")?;
        let mut t = Transition::default();
        if self.eat_kw("first") {
            t.source = Some(self.parse_reference()?);
        } else {
            // `transition name first source ...` or `transition source ...`
            let save = self.pos;
            match self.parse_identification() {
                Ok(id) if self.eat_kw("first") => {
                    t.id = id;
                    t.source = Some(self.parse_reference()?);
                }
                _ => {
                    self.pos = save;
                    if self.is_name() {
                        t.source = Some(self.parse_reference()?);
                    }
                }
            }
        }
        self.parse_transition_tail(&mut t)?;
        Ok(t)
    }

    /// `[accept trigger] [if guard] [do effect] then target body`
    fn parse_transition_tail(&mut self, t: &mut Transition) -> PResult<()> {
        if t.trigger.is_none() && self.eat_kw("accept") {
            t.trigger = Some(self.parse_trigger()?);
        }
        if self.eat_kw("if") {
            t.guard = Some(self.parse_expr()?);
        }
        if self.eat_kw("do") {
            t.effect = Some(self.parse_effect()?);
        }
        self.expect_kw("then")?;
        t.target = Some(self.parse_reference()?);
        t.body = self.parse_body()?;
        Ok(())
    }

    fn parse_trigger(&mut self) -> PResult<Trigger> {
        let start = self.start();
        let kind = if self.eat_kw("when") {
            TriggerKind::When(self.parse_expr()?)
        } else if self.eat_kw("at") {
            TriggerKind::At(self.parse_expr()?)
        } else if self.eat_kw("after") {
            TriggerKind::After(self.parse_expr()?)
        } else if self.is_name() && self.sym_at(1, ":") {
            let name = self.parse_name()?;
            self.bump();
            TriggerKind::Signal { name: Some(name), ty: Some(self.parse_type_ref()?) }
        } else if self.is_name() || self.is_sym("~") {
            TriggerKind::Signal { name: None, ty: Some(self.parse_type_ref()?) }
        } else {
            TriggerKind::Signal { name: None, ty: None }
        };
        let via = if self.eat_kw("via") { Some(self.parse_reference()?) } else { None };
        Ok(Trigger { kind, via, span: self.span_from(start) })
    }

    /// Transition effect after `do`: a reference, `send ...` or `action name : T`.
    fn parse_effect(&mut self) -> PResult<ActionRef> {
        let start = self.start();
        let kind = if self.is_kw("send") {
            MemberKind::Send(self.parse_send(false)?)
        } else if self.eat_kw("action") {
            let mut u = Usage { kind: UsageKind::Action, ..Default::default() };
            u.id = self.parse_identification()?;
            self.parse_specializations(&mut u)?;
            MemberKind::Usage(u)
        } else {
            return Ok(ActionRef::Ref(self.parse_reference()?));
        };
        Ok(ActionRef::Inline(Box::new(Member {
            visibility: None,
            metadata: Vec::new(),
            kind,
            span: self.span_from(start),
        })))
    }

    fn parse_accept(&mut self) -> PResult<MemberKind> {
        self.expect_kw("accept")?;
        let trigger = self.parse_trigger()?;
        if self.is_kw("if") || self.is_kw("do") || self.is_kw("then") {
            let mut t = Transition { trigger: Some(trigger), ..Default::default() };
            self.parse_transition_tail(&mut t)?;
            return Ok(MemberKind::Transition(t));
        }
        let body = self.parse_body()?;
        Ok(MemberKind::Accept(Accept { trigger, body }))
    }

    fn parse_send(&mut self, with_body: bool) -> PResult<Send> {
        self.expect_kw("send")?;
        let payload = self.parse_expr()?;
        let via = if self.eat_kw("via") { Some(self.parse_reference()?) } else { None };
        let to = if self.eat_kw("to") { Some(self.parse_reference()?) } else { None };
        let body = if with_body { self.parse_body()? } else { Vec::new() };
        Ok(Send { payload, via, to, body })
    }

    fn parse_state_action(&mut self) -> PResult<StateAction> {
        let kind = match self.bump().kind {
            TokenKind::Keyword("entry") => StateActionKind::Entry,
            TokenKind::Keyword("do") => StateActionKind::Do,
            _ => StateActionKind::Exit,
        };
        if self.eat_sym(";") {
            return Ok(StateAction { kind, action: None });
        }
        if self.is_name() {
            let r = self.parse_reference()?;
            self.parse_body()?;
            return Ok(StateAction { kind, action: Some(ActionRef::Ref(r)) });
        }
        let m = self.parse_member()?;
        Ok(StateAction { kind, action: Some(ActionRef::Inline(Box::new(m))) })
    }
}
