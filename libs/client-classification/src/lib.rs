use std::net::Ipv4Addr;

use thiserror::Error;

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
    // operation
    Substring(Box<Expr>, usize, usize),
    // prefix
    Not(Box<Expr>),
    // postfix
    ToHex(Box<Expr>),
    Exists(Box<Expr>),
    // SubOpt(Box<Expr>, u8),
    // infix
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Equal(Box<Expr>, Box<Expr>),
    NEqual(Box<Expr>, Box<Expr>),
}

pub type ParseResult<T> = Result<T, ParseErr>;

#[derive(Error, Debug)]
pub enum ParseErr {
    #[error("float parse error")]
    Float(#[from] std::num::ParseFloatError),
    #[error("int parse error")]
    Int(#[from] std::num::ParseIntError),
    #[error("addr parse error")]
    Ip(#[from] std::net::AddrParseError),
    #[error("substring parse error with: {0}")]
    Substring(String),
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
    ExpectedBool(String),
    #[error("expected string: got {0}")]
    ExpectedString(String),
    #[error("expected int: got {0}")]
    ExpectedInt(String),
    #[error("expected ip: got {0}")]
    ExpectedEmpty(String),
    #[error("expected ip: got {0}")]
    ExpectedBytes(String),
    #[error("failed to get sub-opt")]
    SubOptionParseFail(#[from] dhcproto::error::DecodeError),
}
