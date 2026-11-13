//! Expression parsing by precedence climbing.

use super::{PResult, Parser};
use crate::ast::*;
use crate::lexer::{TokenKind, strip_delimiters, unescape};

/// Precedence of `..`; multiplicity bounds are parsed just above it.
const RANGE_PREC: u8 = 9;

#[derive(Clone, Copy)]
enum Op {
    Binary(BinaryOp),
    Classify(ClassificationOp),
}

impl Op {
    fn precedence(self) -> u8 {
        match self {
            Op::Binary(op) => op.precedence(),
            Op::Classify(_) => ClassificationOp::PRECEDENCE,
        }
    }
}

impl Parser<'_> {
    pub(crate) fn parse_expr(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.parse_conditional();
        self.leave();
        r
    }

    /// A multiplicity bound: an expression without a top-level `..`.
    pub(crate) fn parse_bound(&mut self) -> PResult<Expr> {
        self.enter()?;
        let r = self.parse_binary(RANGE_PREC + 1);
        self.leave();
        r
    }

    fn parse_conditional(&mut self) -> PResult<Expr> {
        if !self.is_kw("if") {
            return self.parse_binary(0);
        }
        let start = self.start();
        self.bump();
        let cond = self.parse_binary(0)?;
        self.expect_sym("?")?;
        let then = self.parse_expr()?;
        self.expect_kw("else")?;
        let otherwise = self.parse_expr()?;
        Ok(Expr {
            kind: ExprKind::Conditional {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            },
            span: self.span_from(start),
        })
    }

    fn peek_binary_op(&self) -> Option<Op> {
        use BinaryOp::*;
        Some(match self.peek().kind {
            TokenKind::Symbol(s) => Op::Binary(match s {
                "??" => NullCoalesce,
                "|" => Or,
                "&" => And,
                "==" => Eq,
                "!=" => Ne,
                "===" => Same,
                "!==" => NotSame,
                "<" => Lt,
                ">" => Gt,
                "<=" => Le,
                ">=" => Ge,
                ".." => Range,
                "+" => Add,
                "-" => Sub,
                "*" => Mul,
                "/" => Div,
                "%" => Rem,
                "**" | "^" => Pow,
                _ => return None,
            }),
            TokenKind::Keyword(k) => match k {
                "implies" => Op::Binary(Implies),
                "or" => Op::Binary(Or),
                "xor" => Op::Binary(Xor),
                "and" => Op::Binary(And),
                "istype" => Op::Classify(ClassificationOp::IsType),
                "hastype" => Op::Classify(ClassificationOp::HasType),
                "as" => Op::Classify(ClassificationOp::As),
                "meta" => Op::Classify(ClassificationOp::Meta),
                _ => return None,
            },
            _ => return None,
        })
    }

    fn parse_binary(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        while let Some(op) = self.peek_binary_op() {
            let prec = op.precedence();
            if prec < min_prec {
                break;
            }
            self.bump();
            let start = lhs.span.start;
            let kind = match op {
                Op::Classify(op) => ExprKind::Classification {
                    op,
                    operand: Some(Box::new(lhs)),
                    ty: self.parse_qualified_name()?,
                },
                Op::Binary(op) => {
                    let next = if op.is_right_assoc() { prec } else { prec + 1 };
                    let rhs = self.parse_binary(next)?;
                    ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }
                }
            };
            lhs = Expr { kind, span: self.span_from(start) };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        let op = match self.peek().kind {
            TokenKind::Symbol("-") => UnaryOp::Minus,
            TokenKind::Symbol("+") => UnaryOp::Plus,
            TokenKind::Symbol("~") => UnaryOp::BitNot,
            TokenKind::Symbol("!") | TokenKind::Keyword("not") => UnaryOp::Not,
            _ => return self.parse_postfix(),
        };
        let start = self.start();
        self.bump();
        self.enter()?;
        let operand = self.parse_unary();
        self.leave();
        Ok(Expr {
            kind: ExprKind::Unary { op, operand: Box::new(operand?) },
            span: self.span_from(start),
        })
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            let start = e.span.start;
            let kind = if self.is_sym(".") && self.name_at(1) {
                self.bump();
                ExprKind::Member { base: Box::new(e), member: self.parse_name()? }
            } else if self.eat_sym("->") {
                let func = self.parse_qualified_name()?;
                let args = self.parse_arguments()?;
                ExprKind::Arrow { base: Box::new(e), func, args }
            } else if self.eat_sym("[") {
                let unit = self.parse_expr()?;
                self.expect_sym("]")?;
                ExprKind::Quantity { value: Box::new(e), unit: Box::new(unit) }
            } else if self.is_sym("#") && self.sym_at(1, "(") {
                self.bump();
                self.bump();
                let index = self.parse_expr()?;
                self.expect_sym(")")?;
                ExprKind::Index { base: Box::new(e), index: Box::new(index) }
            } else {
                return Ok(e);
            };
            e = Expr { kind, span: self.span_from(start) };
        }
    }

    fn parse_arguments(&mut self) -> PResult<Vec<Argument>> {
        self.expect_sym("(")?;
        let mut args = Vec::new();
        if self.eat_sym(")") {
            return Ok(args);
        }
        loop {
            let name = if self.is_name() && self.sym_at(1, "=") {
                let n = self.parse_name()?;
                self.bump();
                Some(n)
            } else {
                None
            };
            args.push(Argument { name, value: self.parse_expr()? });
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_sym(")")?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let tok = self.peek();
        let start = tok.span.start;
        let text = tok.span.text(self.src);
        let kind = match tok.kind {
            TokenKind::Integer => {
                self.bump();
                match text.parse::<u64>() {
                    Ok(v) => ExprKind::Literal(Literal::Integer(v)),
                    Err(_) => ExprKind::Literal(Literal::Real(text.parse().unwrap_or(f64::INFINITY))),
                }
            }
            TokenKind::Real => {
                self.bump();
                ExprKind::Literal(Literal::Real(text.parse().unwrap_or(f64::NAN)))
            }
            TokenKind::String => {
                self.bump();
                ExprKind::Literal(Literal::String(unescape(strip_delimiters(text, '"'))))
            }
            TokenKind::Keyword("true") => {
                self.bump();
                ExprKind::Literal(Literal::Bool(true))
            }
            TokenKind::Keyword("false") => {
                self.bump();
                ExprKind::Literal(Literal::Bool(false))
            }
            TokenKind::Keyword("null") => {
                self.bump();
                ExprKind::Literal(Literal::Null)
            }
            TokenKind::Symbol("*") => {
                self.bump();
                ExprKind::Infinity
            }
            TokenKind::Symbol("@") => {
                self.bump();
                let ty = self.parse_qualified_name()?;
                ExprKind::Classification { op: ClassificationOp::At, operand: None, ty }
            }
            TokenKind::Symbol("(") => {
                self.bump();
                if self.eat_sym(")") {
                    ExprKind::Sequence(Vec::new())
                } else {
                    let first = self.parse_expr()?;
                    if self.is_sym(",") {
                        let mut items = vec![first];
                        while self.eat_sym(",") {
                            if self.is_sym(")") {
                                break;
                            }
                            items.push(self.parse_expr()?);
                        }
                        self.expect_sym(")")?;
                        ExprKind::Sequence(items)
                    } else {
                        self.expect_sym(")")?;
                        first.kind
                    }
                }
            }
            TokenKind::Ident | TokenKind::UnrestrictedName => {
                let name = self.parse_qualified_name()?;
                if self.is_sym("(") {
                    ExprKind::Call { callee: name, args: self.parse_arguments()? }
                } else {
                    ExprKind::Name(name)
                }
            }
            _ => return Err(self.expected("an expression")),
        };
        Ok(Expr { kind, span: self.span_from(start) })
    }
}
