use crate::{EvalErr, EvalResult, Expr, ParseErr, ParseResult};

use std::collections::HashMap;

use dhcproto::{v4, Decoder};
pub use pest::{
    pratt_parser::{Assoc, Op, PrattParser},
    {iterators::Pairs, Parser},
};
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct PredicateParser;

#[allow(clippy::result_large_err)]
pub fn parse(expr: &str) -> ParseResult<Expr> {
    build_ast(PredicateParser::parse(Rule::expr, expr)?)
}

#[allow(clippy::result_large_err)]
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

fn parse_sub_opts(buf: &[u8], sub_code: u8) -> Result<Option<Vec<u8>>, EvalErr> {
    let mut d = Decoder::new(buf);
    while let Ok(code) = d.read_u8() {
        let len = d.read_u8()?;
        if len != 0 {
            let slice = d.read_slice(len as usize)?;
            if sub_code == code {
                return Ok(Some(slice.to_owned()));
            }
        }
    }
    Ok(None)
}

/// evaluate the AST, using values from this DHCP message
pub fn eval_ast(
    expr: Expr,
    chaddr: &str,
    opts: &HashMap<v4::OptionCode, v4::UnknownOption>,
) -> Result<Val, EvalErr> {
    use Expr::*;
    Ok(match expr {
        Bool(b) => Val::Bool(b),
        String(s) => Val::String(s.to_lowercase()),
        Int(i) => Val::Int(i),
        Hex(h) => Val::String(h.to_lowercase()),
        Relay(o) => match opts
            .get(&v4::OptionCode::RelayAgentInformation)
            .and_then(|info| parse_sub_opts(info.data(), o).transpose())
        {
            Some(v) => Val::Bytes(v?),
            None => Val::Empty,
        },
        Option(o) => match opts.get(&o.into()) {
            Some(v) => Val::Bytes(v.data().to_owned()),
            None => Val::Empty,
        },
        Mac() => Val::String(chaddr.to_lowercase()),
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

#[allow(clippy::result_large_err)]
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
                Rule::relay => Expr::Relay(primary.into_inner().as_str().parse()?),
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
                // Rule::sub_opt => Expr::SubOpt(Box::new(lhs?)),
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
    use dhcproto::v4::UnknownOption;
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
        options.insert(
            61.into(),
            UnknownOption::new(61.into(), b"some_client_id".to_vec()),
        );

        let tokens = PredicateParser::parse(
            Rule::expr,
            "substring(pkt4.mac, 0, 6) == '001122' and option[61].hex == 'some_client_id'",
        )
        .unwrap();

        let val = eval_ast(build_ast(tokens).unwrap(), "001122334455", &options).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_relay_opts() {
        let mut options = HashMap::new();
        let mut data: Vec<u8> = vec![12];

        let sub_opt = "foo".as_bytes();
        data.push(sub_opt.len() as u8);
        data.extend(sub_opt);
        data.extend(&[
            23, 3, 1, 2, 3, // two
            45, 0, // three
            123, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ]);

        options.insert(
            v4::OptionCode::RelayAgentInformation,
            UnknownOption::new(v4::OptionCode::RelayAgentInformation, data),
        );
        let expr = super::parse("relay4[12].exists").unwrap();
        let val = eval_ast(expr, "001122334455", &options).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = super::parse("relay4[12].hex == 'foo'").unwrap();
        let val = eval_ast(expr, "001122334455", &options).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_sub_opts() {
        let buf = vec![
            12, 2, 1, 2, // one
            23, 3, 1, 2, 3, // two
            45, 0, // three
            123, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ];
        assert_eq!(&parse_sub_opts(&buf, 12).unwrap().unwrap(), &[1, 2]);
        assert_eq!(&parse_sub_opts(&buf, 23).unwrap().unwrap(), &[1, 2, 3]);
        assert_eq!(&parse_sub_opts(&buf, 45).unwrap(), &None);
        assert_eq!(
            &parse_sub_opts(&buf, 123).unwrap().unwrap(),
            &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
    }
}
