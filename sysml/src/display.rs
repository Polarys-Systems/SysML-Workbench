//! `Display` for names, references and expressions, printed back in SysML syntax.

use crate::ast::*;
use crate::lexer::is_keyword;
use std::fmt::{self, Display, Formatter, Write};

impl Display for Name {
    /// Prints the name, quoting it (`'my name'`) when it is not a basic name.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let t = &self.text;
        let basic = t.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !is_keyword(t);
        if basic {
            return f.write_str(t);
        }
        f.write_char('\'')?;
        for c in t.chars() {
            match c {
                '\'' | '\\' => write!(f, "\\{c}")?,
                '\n' => f.write_str("\\n")?,
                _ => f.write_char(c)?,
            }
        }
        f.write_char('\'')
    }
}

fn join<T: Display>(f: &mut Formatter<'_>, items: &[T], sep: &str) -> fmt::Result {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            f.write_str(sep)?;
        }
        write!(f, "{item}")?;
    }
    Ok(())
}

impl Display for QualifiedName {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        join(f, &self.segments, "::")
    }
}

impl Display for Reference {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        join(f, &self.chain, ".")
    }
}

impl Display for TypeRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if self.conjugated {
            f.write_char('~')?;
        }
        write!(f, "{}", self.name)
    }
}

impl Display for Multiplicity {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.lower {
            Some(lower) => write!(f, "[{lower}..{}]", self.upper),
            None => write!(f, "[{}]", self.upper),
        }
    }
}

impl Display for DefKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

impl Display for UsageKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.keyword())
    }
}

impl Display for Literal {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Literal::Integer(v) => write!(f, "{v}"),
            Literal::Real(v) => write!(f, "{v:?}"),
            Literal::Bool(v) => write!(f, "{v}"),
            Literal::Null => f.write_str("null"),
            Literal::String(s) => {
                f.write_char('"')?;
                for c in s.chars() {
                    match c {
                        '"' | '\\' => write!(f, "\\{c}")?,
                        '\n' => f.write_str("\\n")?,
                        _ => f.write_char(c)?,
                    }
                }
                f.write_char('"')
            }
        }
    }
}

impl Display for Argument {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(f, "{name} = ")?;
        }
        write!(f, "{}", self.value)
    }
}

const UNARY_PREC: u8 = 13;
const POSTFIX_PREC: u8 = 14;
const PRIMARY_PREC: u8 = 15;

fn precedence(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Conditional { .. } => 0,
        ExprKind::Binary { op, .. } => op.precedence(),
        ExprKind::Classification { operand: Some(_), .. } => ClassificationOp::PRECEDENCE,
        ExprKind::Unary { .. } => UNARY_PREC,
        ExprKind::Member { .. }
        | ExprKind::Arrow { .. }
        | ExprKind::Index { .. }
        | ExprKind::Quantity { .. } => POSTFIX_PREC,
        _ => PRIMARY_PREC,
    }
}

/// Writes `e`, parenthesized if it binds looser than `min`.
fn write_expr(f: &mut Formatter<'_>, e: &Expr, min: u8) -> fmt::Result {
    let paren = precedence(e) < min;
    if paren {
        f.write_char('(')?;
    }
    match &e.kind {
        ExprKind::Literal(l) => write!(f, "{l}")?,
        ExprKind::Infinity => f.write_char('*')?,
        ExprKind::Name(n) => write!(f, "{n}")?,
        ExprKind::Unary { op, operand } => {
            f.write_str(op.symbol())?;
            if *op == UnaryOp::Not {
                f.write_char(' ')?;
            }
            write_expr(f, operand, UNARY_PREC)?;
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let p = op.precedence();
            let (lmin, rmin) = if op.is_right_assoc() { (p + 1, p) } else { (p, p + 1) };
            write_expr(f, lhs, lmin)?;
            if *op == BinaryOp::Range {
                f.write_str("..")?;
            } else {
                write!(f, " {} ", op.symbol())?;
            }
            write_expr(f, rhs, rmin)?;
        }
        ExprKind::Classification { op, operand, ty } => match operand {
            Some(operand) => {
                write_expr(f, operand, ClassificationOp::PRECEDENCE + 1)?;
                write!(f, " {} {ty}", op.symbol())?;
            }
            None if *op == ClassificationOp::At => write!(f, "@{ty}")?,
            None => write!(f, "{} {ty}", op.symbol())?,
        },
        ExprKind::Conditional { cond, then, otherwise } => {
            f.write_str("if ")?;
            write_expr(f, cond, 1)?;
            write!(f, " ? {then} else {otherwise}")?;
        }
        ExprKind::Member { base, member } => {
            write_expr(f, base, POSTFIX_PREC)?;
            write!(f, ".{member}")?;
        }
        ExprKind::Call { callee, args } => {
            write!(f, "{callee}(")?;
            join(f, args, ", ")?;
            f.write_char(')')?;
        }
        ExprKind::Arrow { base, func, args } => {
            write_expr(f, base, POSTFIX_PREC)?;
            write!(f, "->{func}(")?;
            join(f, args, ", ")?;
            f.write_char(')')?;
        }
        ExprKind::Index { base, index } => {
            write_expr(f, base, POSTFIX_PREC)?;
            write!(f, "#({index})")?;
        }
        ExprKind::Quantity { value, unit } => {
            write_expr(f, value, POSTFIX_PREC)?;
            write!(f, " [{unit}]")?;
        }
        ExprKind::Sequence(items) => {
            f.write_char('(')?;
            join(f, items, ", ")?;
            f.write_char(')')?;
        }
    }
    if paren {
        f.write_char(')')?;
    }
    Ok(())
}

impl Display for Expr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write_expr(f, self, 0)
    }
}
