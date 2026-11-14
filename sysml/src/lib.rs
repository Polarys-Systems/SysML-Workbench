//! Parser for the SysML v2 textual notation.
//!
//! Covers the commonly used part of the language rather than the full grammar:
//!
//! * packages, imports (`::*`, `::**`, filters), aliases, visibility
//! * `doc` / `comment` / `rep` comments, `#Meta` prefixes and `@Meta` usages
//! * definitions (`part def`, `port def`, `action def`, `state def`,
//!   `requirement def`, `enum def`, ...) with `:>` specialization
//! * usages with typing (`:`), subsetting (`:>`), redefinition (`:>>`),
//!   references (`::>`), multiplicity, `=` / `:=` / `default` values,
//!   direction and the usual modifier keywords
//! * connectors: `connect`, `bind`, `allocate`, `flow`, `message`, successions
//!   (`first a then b;`, `then x;`)
//! * behavior: `perform`, `exhibit`, `satisfy ... by`, transitions with
//!   `accept` / `if` / `do` / `then`, `entry` / `do` / `exit`, `send`, decisions
//! * expressions with operator precedence, units (`9.81 [m/s^2]`), calls,
//!   feature chains, `->` invocations and conditionals
//!
//! Not supported: loops (`while`, `for`, `loop`), `assign`, body expressions
//! (`{ ... }` lambdas), and the KerML-only notation. Unknown constructs produce a
//! diagnostic and are skipped; the rest of the file is still parsed.
//!
//! ```
//! let result = sysml::parse("package P { part def Car { attribute mass = 1200 [kg]; } }");
//! assert!(result.diagnostics.is_empty());
//! let index = sysml::Index::build(&result.model);
//! assert!(index.get("P::Car::mass").is_some());
//! ```

pub mod ast;
mod display;
pub mod eval;
pub mod index;
pub mod lexer;
mod parser;
pub mod span;
mod summary;
pub mod units;

pub use ast::*;
pub use summary::Category;
pub use index::{Entry, EntryId, Index};
pub use span::{LineIndex, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A problem found while lexing or parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: Span,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self { severity: Severity::Error, message: message.into(), span }
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{level}: {}", self.message)
    }
}

/// The model parsed from one source text, and any problems found.
#[derive(Clone, Debug)]
pub struct ParseResult {
    pub model: Model,
    /// Sorted by position.
    pub diagnostics: Vec<Diagnostic>,
}

impl ParseResult {
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(|d| d.severity == Severity::Error)
    }
}

/// Parses SysML v2 source text. Never fails: problems are returned as
/// diagnostics alongside the parts of the model that could be read.
pub fn parse(source: &str) -> ParseResult {
    let mut diagnostics = Vec::new();
    let tokens = lexer::lex(source, &mut diagnostics);
    let mut parser = parser::Parser::new(source, tokens);
    let model = parser.parse_model();
    diagnostics.append(&mut parser.diags);
    diagnostics.sort_by_key(|d| d.span.start);
    ParseResult { model, diagnostics }
}
