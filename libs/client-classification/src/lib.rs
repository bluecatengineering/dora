use std::net::Ipv4Addr;

use thiserror::Error;

use crate::ast::Val;

pub mod ast;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    String(String),
    Ip(Ipv4Addr),
    Int(u32),
    Hex(String),
    Bool(bool),
    Option(u8),
    Relay(u8),
    Mac(),
    Hlen(),
    HType(),
    CiAddr(),
    GiAddr(),
    YiAddr(),
    SiAddr(),
    MsgType(),
    TransId(),
    // operation
    Substring(Box<Expr>, usize, usize),
    // prefix
    Not(Box<Expr>),
    // postfix
    ToHex(Box<Expr>),
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
    #[error("substring parse error with: {0}")]
    Substring(String),
    #[error("expected option but found: {0}")]
    Option(Expr),
    #[error("bool parse error with: {0}")]
    Bool(String),
    #[error("undefined with: {0:?}")]
    Undefined(ast::Rule),
    #[error("pest error {0:?}")]
    PestErr(#[from] pest::error::Error<ast::Rule>),
}

pub type EvalResult<T> = Result<T, EvalErr>;

#[derive(Error, Debug)]
pub enum EvalErr {
    #[error("expected bool: got {0}")]
    ExpectedBool(Val),
    #[error("expected string: got {0}")]
    ExpectedString(Val),
    #[error("expected int: got {0}")]
    ExpectedInt(Val),
    #[error("expected ip: got {0}")]
    ExpectedEmpty(Val),
    #[error("expected ip: got {0}")]
    ExpectedBytes(Val),
    #[error("failed to get sub-opt")]
    SubOptionParseFail(#[from] dhcproto::error::DecodeError),
}
