use crate::{EvalErr, EvalResult, Expr, ParseErr, ParseResult};

use std::collections::HashMap;

use pest::{
    iterators::Pairs,
    pratt_parser::{Assoc, Op, PrattParser},
};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct PredicateParser;

pub fn build_ast(pair: Pairs<Rule>) -> ParseResult<Expr> {
    let climber = PrattParser::new()
        .op(Op::infix(Rule::or, Assoc::Left))
        .op(Op::infix(Rule::and, Assoc::Left))
        .op(Op::infix(Rule::equal, Assoc::Right) | Op::infix(Rule::neq, Assoc::Right))
        // .op(Op::postfix(Rule::not));
        .op(Op::prefix(Rule::not));

    parse_expr(pair, &climber)
}

#[derive(Debug)]
pub enum Val {
    Bool(bool),
    String(String),
    Int(u64),
}

fn is_bool(val: Val) -> EvalResult<bool> {
    match val {
        Val::Bool(b) => Ok(b),
        Val::String(s) => Err(EvalErr::ExpectedBool(s)),
        Val::Int(i) => Err(EvalErr::ExpectedBool(i.to_string())),
    }
}
fn is_str(val: Val) -> EvalResult<String> {
    match val {
        Val::Bool(b) => Err(EvalErr::ExpectedString(b.to_string())),
        Val::String(s) => Ok(s),
        Val::Int(i) => Err(EvalErr::ExpectedString(i.to_string())),
    }
}
fn is_int(val: Val) -> EvalResult<u64> {
    match val {
        Val::Bool(b) => Err(EvalErr::ExpectedInt(b.to_string())),
        Val::String(s) => Err(EvalErr::ExpectedInt(s)),
        Val::Int(i) => Ok(i),
    }
}

pub fn eval_ast(expr: Expr, chaddr: &str, opts: &HashMap<u8, String>) -> Result<Val, EvalErr> {
    use Expr::*;
    Ok(match expr {
        Bool(b) => Val::Bool(b),
        String(s) => Val::String(s),
        Int(i) => Val::Int(i),
        Hex(h) => Val::String(h),
        Option(o) => Val::String(opts[&o].clone()),
        Mac() => Val::String(chaddr.to_string()),
        // prefix
        Not(rhs) => Val::Bool(!is_bool(eval_ast(*rhs, chaddr, opts)?)?),
        // infix
        And(lhs, rhs) => Val::Bool(
            is_bool(eval_ast(*lhs, chaddr, opts)?)? && is_bool(eval_ast(*rhs, chaddr, opts)?)?,
        ),
        Or(lhs, rhs) => Val::Bool(
            is_bool(eval_ast(*lhs, chaddr, opts)?)? || is_bool(eval_ast(*rhs, chaddr, opts)?)?,
        ),
        Equal(lhs, rhs) => Val::Bool(match eval_ast(*lhs, chaddr, opts)? {
            Val::String(a) => a == is_str(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Bool(a) => a == is_bool(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Int(a) => a == is_int(eval_ast(*rhs, chaddr, opts)?)?,
        }),
        NEqual(lhs, rhs) => Val::Bool(match eval_ast(*lhs, chaddr, opts)? {
            Val::String(a) => a != is_str(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Bool(a) => a != is_bool(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Int(a) => a != is_int(eval_ast(*rhs, chaddr, opts)?)?,
        }),
        Substring(lhs, i, j) => {
            Val::String(is_str(eval_ast(*lhs, chaddr, opts)?)?[i..j].to_string())
        }
    })
}

fn parse_expr(pairs: Pairs<Rule>, pratt: &PrattParser<Rule>) -> ParseResult<Expr> {
    pratt
        .map_primary(|primary| {
            Ok(match primary.as_rule() {
                Rule::integer => Expr::Int(primary.as_str().parse()?),
                Rule::boolean => Expr::Bool(match primary.as_str() {
                    "true" => true,
                    "false" => false,
                    err => return Err(ParseErr::Bool(err.to_string())),
                }),
                Rule::mac => Expr::Mac(),
                Rule::string => Expr::String(
                    primary
                        .as_str()
                        .trim_start_matches('\'')
                        .trim_end_matches('\'')
                        .to_string(),
                ),
                Rule::option => Expr::Option(primary.into_inner().as_str().parse()?),
                // trim off '0x'. hex decode?
                Rule::hex => Expr::Hex(primary.as_str()[2..].to_string()),
                Rule::substring => {
                    let mut inner = primary.into_inner();
                    let j = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Substring(inner.to_string()))?;
                    let i = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Substring(inner.to_string()))?;
                    Expr::Substring(
                        Box::new(parse_expr(inner, pratt)?),
                        i.as_str().parse()?,
                        j.as_str().parse()?,
                    )
                }
                Rule::expr => parse_expr(primary.into_inner(), pratt)?, // from "(" ~ expr ~ ")"
                rule => return Err(ParseErr::Undefined(rule)),
            })
        })
        .map_prefix(|op, rhs| {
            Ok(match op.as_rule() {
                Rule::not => Expr::Not(Box::new(rhs?)),
                rule => return Err(ParseErr::Undefined(rule)),
            })
        })
        // .map_postfix(|lhs, op| match op.as_rule() {
        //     Rule::fac => (1..lhs + 1).product(),
        //     _ => unreachable!(),
        // })
        .map_infix(|lhs, op, rhs| {
            Ok(match op.as_rule() {
                Rule::and => Expr::And(Box::new(lhs?), Box::new(rhs?)),
                Rule::or => Expr::Or(Box::new(lhs?), Box::new(rhs?)),
                Rule::equal => Expr::Equal(Box::new(lhs?), Box::new(rhs?)),
                Rule::neq => Expr::NEqual(Box::new(lhs?), Box::new(rhs?)),
                rule => return Err(ParseErr::Undefined(rule)),
            })
        })
        .parse(pairs)
}
