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
        .op(Op::prefix(Rule::not))
        .op(Op::postfix(Rule::to_hex) | Op::postfix(Rule::exists));

    parse_expr(pair, &climber)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Val {
    Empty,
    Bool(bool),
    String(String),
    Bytes(Vec<u8>),
    Int(u32),
}

fn is_bool(val: Val) -> EvalResult<bool> {
    match val {
        Val::Bool(b) => Ok(b),
        err => Err(EvalErr::ExpectedBool(format!("{err:?}"))),
    }
}
fn is_str(val: Val) -> EvalResult<String> {
    match val {
        Val::String(s) => Ok(s),
        err => Err(EvalErr::ExpectedString(format!("{err:?}"))),
    }
}
fn is_int(val: Val) -> EvalResult<u32> {
    match val {
        Val::Int(i) => Ok(i),
        err => Err(EvalErr::ExpectedInt(format!("{err:?}"))),
    }
}
fn is_empty(val: Val) -> EvalResult<()> {
    match val {
        Val::Empty => Ok(()),
        err => Err(EvalErr::ExpectedEmpty(format!("{err:?}"))),
    }
}

/// evaluate the AST, using values from this DHCP message
pub fn eval_ast(expr: Expr, chaddr: &str, opts: &HashMap<u8, Vec<u8>>) -> Result<Val, EvalErr> {
    use Expr::*;
    Ok(match expr {
        Bool(b) => Val::Bool(b),
        String(s) => Val::String(s),
        Int(i) => Val::Int(i),
        Hex(h) => Val::String(h),
        Option(o) => match opts.get(&o) {
            Some(v) => Val::Bytes(v.clone()),
            None => Val::Empty,
        },
        Mac() => Val::String(chaddr.to_string()),
        Ip(ip) => Val::Int(u32::from_be_bytes(ip.octets())),
        // prefix
        Not(rhs) => Val::Bool(!is_bool(eval_ast(*rhs, chaddr, opts)?)?),
        // postfix
        Exists(lhs) => Val::Bool(is_empty(eval_ast(*lhs, chaddr, opts)?).is_err()),
        ToHex(lhs) => match eval_ast(*lhs, chaddr, opts)? {
            Val::String(s) => Val::Bytes(s.as_bytes().to_vec()),
            Val::Bytes(b) => Val::Bytes(b),
            Val::Int(i) => Val::Bytes(i.to_be_bytes().to_vec()),
            err => return Err(EvalErr::ExpectedBytes(format!("{err:?}"))),
        },
        // infix
        And(lhs, rhs) => Val::Bool(
            is_bool(eval_ast(*lhs, chaddr, opts)?)? && is_bool(eval_ast(*rhs, chaddr, opts)?)?,
        ),
        Or(lhs, rhs) => Val::Bool(
            is_bool(eval_ast(*lhs, chaddr, opts)?)? || is_bool(eval_ast(*rhs, chaddr, opts)?)?,
        ),
        Equal(lhs, rhs) => Val::Bool(match eval_ast(*lhs, chaddr, opts)? {
            Val::String(a) => match eval_ast(*rhs, chaddr, opts)? {
                Val::String(b) => a == b,
                Val::Bytes(b) => a.as_bytes() == b,
                err => return Err(EvalErr::ExpectedString(format!("{err:?}"))),
            },
            Val::Bool(a) => a == is_bool(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Int(a) => a == is_int(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Empty => is_empty(eval_ast(*rhs, chaddr, opts)?).is_ok(),
            Val::Bytes(a) => match eval_ast(*rhs, chaddr, opts)? {
                Val::String(b) => a == b.as_bytes(),
                Val::Bytes(b) => a == b,
                err => return Err(EvalErr::ExpectedBytes(format!("{err:?}"))),
            },
        }),
        NEqual(lhs, rhs) => Val::Bool(match eval_ast(*lhs, chaddr, opts)? {
            Val::String(a) => match eval_ast(*rhs, chaddr, opts)? {
                Val::String(b) => a != b,
                Val::Bytes(b) => a.as_bytes() != b,
                err => return Err(EvalErr::ExpectedString(format!("{err:?}"))),
            },
            Val::Bool(a) => a != is_bool(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Int(a) => a != is_int(eval_ast(*rhs, chaddr, opts)?)?,
            Val::Empty => is_empty(eval_ast(*rhs, chaddr, opts)?).is_err(),
            Val::Bytes(a) => match eval_ast(*rhs, chaddr, opts)? {
                Val::String(b) => a != b.as_bytes(),
                Val::Bytes(b) => a != b,
                err => return Err(EvalErr::ExpectedBytes(format!("{err:?}"))),
            },
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
                // Rule::mac => Expr::Mac(),
                Rule::pkt_mac => Expr::Mac(),
                Rule::ip => Expr::Ip(primary.as_str().parse()?),
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
        .map_postfix(|lhs, op| {
            Ok(match op.as_rule() {
                Rule::to_hex => Expr::ToHex(Box::new(lhs?)),
                Rule::exists => Expr::Exists(Box::new(lhs?)),
                rule => return Err(ParseErr::Undefined(rule)),
            })
        })
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

#[cfg(test)]
mod tests {
    use pest::Parser;

    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_opt_exists() {
        let tokens = PredicateParser::parse(Rule::expr, "not option[123].exists").unwrap();

        let val = eval_ast(
            dbg!(build_ast(tokens).unwrap()),
            "001122334455",
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(val, Val::Bool(true));
    }
    #[test]
    fn test_ip_parser() {
        let tokens = PredicateParser::parse(Rule::expr, "100.10.10.10 == 100.10.10.10").unwrap();

        let val = eval_ast(build_ast(tokens).unwrap(), "001122334455", &HashMap::new()).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_substring_opts() {
        let mut options = HashMap::new();
        options.insert(61, b"some_client_id".to_vec());

        let tokens = PredicateParser::parse(
            Rule::expr,
            "substring(pkt4.mac, 0, 6) == '001122' and option[61].hex == 'some_client_id'",
        )
        .unwrap();

        let val = eval_ast(build_ast(tokens).unwrap(), "001122334455", &options).unwrap();
        assert_eq!(val, Val::Bool(true));
    }
}
