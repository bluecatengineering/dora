use std::net::Ipv4Addr;

use pest::iterators::Pair;
pub use pest::{
    pratt_parser::{Assoc, Op, PrattParser},
    {iterators::Pairs, Parser},
};
use pest_derive::Parser;
use thiserror::Error;

#[derive(Parser)]
#[grammar = "grammar.pest"]
pub struct PredicateParser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    String(String),
    Ip(Ipv4Addr),
    Int(u32),
    Hex(Vec<u8>),
    Bool(bool),
    Option(u8),
    Member(String),
    Relay(u8),
    // pkt_base
    Iface,
    Src,
    Dst,
    Len,
    // pkt
    Mac,
    Hlen,
    HType,
    CiAddr,
    GiAddr,
    YiAddr,
    SiAddr,
    MsgType,
    TransId,
    // operation (expr, start, len) where len of None means 'all'
    Substring(Box<Expr>, isize, Option<isize>),
    Concat(Box<Expr>, Box<Expr>),
    IfElse(Box<Expr>, Box<Expr>, Box<Expr>),
    Hexstring(Box<Expr>, String),
    Split(Box<Expr>, Box<Expr>, usize),
    // prefix
    Not(Box<Expr>),
    // postfix
    ToHex(Box<Expr>),
    ToText(Box<Expr>),
    Exists(Box<Expr>),
    SubOpt(Box<Expr>, u8),
    // infix
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Equal(Box<Expr>, Box<Expr>),
    NEqual(Box<Expr>, Box<Expr>),
}

impl std::fmt::Display for Expr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub type ParseResult<T> = Result<T, ParseErr>;

#[derive(Error, Debug, PartialEq)]
pub enum ParseErr {
    #[error("float parse error")]
    Float(#[from] std::num::ParseFloatError),
    #[error("int parse error")]
    Int(#[from] std::num::ParseIntError),
    #[error("addr parse error")]
    Ip(#[from] std::net::AddrParseError),
    #[error("hex conversion error")]
    HexErr(#[from] hex::FromHexError),
    #[error("substring parse error with: {0}")]
    Substring(String),
    #[error("string parse error with: {0}")]
    String(String),
    #[error("ifelse parse error with: {0}")]
    IfElse(String),
    #[error("'concat parse error with: {0}")]
    Concat(String),
    #[error("'split parse error with: {0}")]
    Split(String),
    #[error("expected option but found: {0}")]
    Option(Expr),
    #[error("bool parse error with: {0}")]
    Bool(String),
    #[error("undefined with: {0:?}")]
    Undefined(Rule),
    #[error("pest error {0:?}")]
    PestErr(#[from] pest::error::Error<Rule>),
}

#[allow(clippy::result_large_err)]
pub fn parse<S: AsRef<str>>(expr: S) -> ParseResult<Expr> {
    build_ast(PredicateParser::parse(Rule::expr, expr.as_ref())?)
}

#[allow(clippy::result_large_err)]
pub fn build_ast(pair: Pairs<Rule>) -> ParseResult<Expr> {
    let climber = PrattParser::new()
        .op(Op::infix(Rule::or, Assoc::Left))
        .op(Op::infix(Rule::and, Assoc::Left))
        .op(Op::infix(Rule::equal, Assoc::Right) | Op::infix(Rule::neq, Assoc::Right))
        .op(Op::prefix(Rule::not))
        .op(Op::postfix(Rule::to_hex)
            | Op::postfix(Rule::exists)
            | Op::postfix(Rule::sub_opt)
            | Op::postfix(Rule::to_text));

    parse_expr(pair, &climber)
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
                // pkt_base
                Rule::pkt_base_iface => Expr::Iface,
                Rule::pkt_base_src => Expr::Src,
                Rule::pkt_base_dst => Expr::Dst,
                Rule::pkt_base_len => Expr::Len,
                // pkt
                Rule::pkt_mac => Expr::Mac,
                Rule::pkt_hlen => Expr::Hlen,
                Rule::pkt_htype => Expr::HType,
                Rule::pkt_ciaddr => Expr::CiAddr,
                Rule::pkt_giaddr => Expr::GiAddr,
                Rule::pkt_yiaddr => Expr::YiAddr,
                Rule::pkt_siaddr => Expr::SiAddr,
                Rule::pkt_msgtype => Expr::MsgType,
                Rule::pkt_transid => Expr::TransId,
                Rule::ip => Expr::Ip(primary.as_str().parse()?),
                Rule::string => Expr::String(parse_string(primary)),
                Rule::option => Expr::Option(parse_num(primary)?),
                Rule::relay => Expr::Relay(parse_num(primary)?),
                Rule::member => Expr::Member(parse_string_inner(primary)),
                // trim off '0x'. hex decode?
                Rule::hex => Expr::Hex(hex::decode(&primary.as_str()[2..])?),
                Rule::substring => {
                    let mut inner = primary.into_inner();
                    let len = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Substring(inner.to_string()))?;
                    let start = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Substring(inner.to_string()))?;

                    Expr::Substring(
                        Box::new(parse_expr(inner, pratt)?),
                        start.as_str().parse()?,
                        if len.as_str() == "all" {
                            None
                        } else {
                            Some(len.as_str().parse()?)
                        },
                    )
                }
                Rule::concat => {
                    let mut inner = primary.into_inner();
                    let b = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Concat(inner.to_string()))?;
                    let a = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Concat(inner.to_string()))?;
                    Expr::Concat(
                        Box::new(parse_expr(a.into_inner(), pratt)?),
                        Box::new(parse_expr(b.into_inner(), pratt)?),
                    )
                }
                Rule::split => {
                    let mut inner = primary.into_inner();
                    let n = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Split(inner.to_string()))?;
                    let del = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Split(inner.to_string()))?;
                    let s = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::Split(inner.to_string()))?;
                    Expr::Split(
                        Box::new(parse_expr(s.into_inner(), pratt)?),
                        Box::new(parse_expr(del.into_inner(), pratt)?),
                        n.as_str().parse()?,
                    )
                }
                Rule::hexstring => {
                    let mut inner = primary.into_inner();
                    let separator = parse_string(
                        inner
                            .next_back()
                            .ok_or_else(|| ParseErr::String(inner.to_string()))?,
                    );
                    let expr = parse_expr(
                        inner
                            .next_back()
                            .ok_or_else(|| ParseErr::String(inner.to_string()))?
                            .into_inner(),
                        pratt,
                    )?;
                    Expr::Hexstring(Box::new(expr), separator)
                }
                Rule::ifelse => {
                    let mut inner = primary.into_inner();
                    let c = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::IfElse(inner.to_string()))?;
                    let b = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::IfElse(inner.to_string()))?;
                    let a = inner
                        .next_back()
                        .ok_or_else(|| ParseErr::IfElse(inner.to_string()))?;
                    Expr::IfElse(
                        Box::new(parse_expr(a.into_inner(), pratt)?),
                        Box::new(parse_expr(b.into_inner(), pratt)?),
                        Box::new(parse_expr(c.into_inner(), pratt)?),
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
                Rule::to_text => Expr::ToText(Box::new(lhs?)),
                Rule::exists => Expr::Exists(Box::new(lhs?)),
                Rule::sub_opt => {
                    // parse inner op (".option[_]"), should return Expr::Option(_)
                    let sub_opt = match parse_expr(op.into_inner(), pratt)? {
                        Expr::Option(n) => n,
                        other => return Err(ParseErr::Option(other)),
                    };
                    Expr::SubOpt(Box::new(lhs?), sub_opt)
                }
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

fn parse_str(str: &str) -> String {
    str.trim_start_matches('\'')
        .trim_end_matches('\'')
        .to_string()
}

fn parse_string(primary: Pair<Rule>) -> String {
    parse_str(primary.as_str())
}

fn parse_string_inner(primary: Pair<Rule>) -> String {
    parse_str(primary.into_inner().as_str())
}

fn parse_num<F: std::str::FromStr>(primary: Pair<Rule>) -> Result<F, F::Err> {
    primary.into_inner().as_str().parse()
}
