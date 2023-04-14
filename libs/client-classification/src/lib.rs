use std::{
    collections::{HashMap, HashSet},
    str,
};

use dhcproto::{v4, Decoder};
use thiserror::Error;

pub mod ast;
pub use ast::{Expr, ParseErr, ParseResult};

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
    #[error("utf8 error {0}")]
    Utf8Error(#[from] str::Utf8Error),
    #[error("failed to get sub-opt")]
    SubOptionParseFail(#[from] dhcproto::error::DecodeError),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Val {
    Empty,
    Bool(bool),
    String(String),
    Bytes(Vec<u8>),
    Int(u32),
}

impl std::fmt::Display for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

fn is_bool(val: Val) -> EvalResult<bool> {
    match val {
        Val::Bool(b) => Ok(b),
        err => Err(EvalErr::ExpectedBool(err)),
    }
}
fn is_str(val: Val) -> EvalResult<String> {
    match val {
        Val::String(s) => Ok(s),
        err => Err(EvalErr::ExpectedString(err)),
    }
}
fn is_int(val: Val) -> EvalResult<u32> {
    match val {
        Val::Int(i) => Ok(i),
        err => Err(EvalErr::ExpectedInt(err)),
    }
}
fn is_empty(val: Val) -> EvalResult<()> {
    match val {
        Val::Empty => Ok(()),
        err => Err(EvalErr::ExpectedEmpty(err)),
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

/// get all the `member` classes used in the expression
pub fn get_class_dependencies(expr: &Expr) -> Vec<String> {
    use Expr::*;
    match expr {
        Member(s) => vec![s.to_owned()],
        Substring(lhs, _, _) => get_class_dependencies(lhs),
        Not(rhs) => get_class_dependencies(rhs),
        ToHex(rhs) => get_class_dependencies(rhs),
        Exists(rhs) => get_class_dependencies(rhs),
        SubOpt(lhs, _) => get_class_dependencies(lhs),
        And(lhs, rhs) => {
            let mut r = get_class_dependencies(lhs);
            r.append(&mut get_class_dependencies(rhs));
            r
        }
        Or(lhs, rhs) => {
            let mut r = get_class_dependencies(lhs);
            r.append(&mut get_class_dependencies(rhs));
            r
        }
        Equal(lhs, rhs) => {
            let mut r = get_class_dependencies(lhs);
            r.append(&mut get_class_dependencies(rhs));
            r
        }
        NEqual(lhs, rhs) => {
            let mut r = get_class_dependencies(lhs);
            r.append(&mut get_class_dependencies(rhs));
            r
        }
        _ => vec![],
    }
}

pub struct Args<'a> {
    pub chaddr: String,
    pub opts: HashMap<v4::OptionCode, v4::UnknownOption>,
    pub msg: &'a v4::Message,
    pub deps: HashSet<String>,
}

/// evaluate the AST, using values from this DHCP message
pub fn eval(expr: &Expr, args: &Args) -> Result<Val, EvalErr> {
    // TODO: should this fn impl Expr and take &self?
    use Expr::*;
    Ok(match expr {
        Bool(b) => Val::Bool(*b),
        String(s) => Val::String(s.to_lowercase()),
        Int(i) => Val::Int(*i),
        Hex(h) => Val::String(h.to_lowercase()),
        Relay(o) => match args
            .opts
            .get(&v4::OptionCode::RelayAgentInformation)
            .and_then(|info| parse_sub_opts(info.data(), *o).transpose())
        {
            Some(v) => Val::Bytes(v?),
            None => Val::Empty,
        },
        Option(o) => match args.opts.get(&(*o).into()) {
            Some(v) => Val::Bytes(v.data().to_owned()),
            None => Val::Empty,
        },
        // TODO: can probably use msg.chaddr() instead of an explicit param here
        Mac() => Val::String(args.chaddr.to_lowercase()),
        Hlen() => Val::Int(args.msg.hlen() as u32),
        HType() => Val::Int(u8::from(args.msg.htype()) as u32),
        CiAddr() => Val::Int(u32::from(args.msg.ciaddr())),
        GiAddr() => Val::Int(u32::from(args.msg.giaddr())),
        YiAddr() => Val::Int(u32::from(args.msg.yiaddr())),
        SiAddr() => Val::Int(u32::from(args.msg.siaddr())),
        MsgType() => match args.msg.opts().msg_type() {
            Some(ty) => Val::Int(u8::from(ty) as u32),
            None => Val::Empty,
        },
        TransId() => Val::Int(args.msg.xid()),
        Ip(ip) => Val::Int(u32::from_be_bytes(ip.octets())),
        // prefix
        Not(rhs) => Val::Bool(!is_bool(eval(rhs, args)?)?),
        // postfix
        Exists(lhs) => Val::Bool(is_empty(eval(lhs, args)?).is_err()),
        ToHex(lhs) => match eval(lhs, args)? {
            Val::String(s) => Val::Bytes(s.as_bytes().to_vec()),
            Val::Bytes(b) => Val::Bytes(b),
            Val::Int(i) => Val::Bytes(i.to_be_bytes().to_vec()),
            err => return Err(EvalErr::ExpectedBytes(err)),
        },
        SubOpt(lhs, o) => {
            let bytes = match eval(lhs, args)? {
                Val::String(s) => s.as_bytes().to_vec(),
                Val::Bytes(b) => b,
                err => return Err(EvalErr::ExpectedBytes(err)),
            };
            match parse_sub_opts(&bytes, *o)? {
                Some(v) => Val::Bytes(v),
                None => Val::Empty,
            }
        }
        // infix
        And(lhs, rhs) => Val::Bool(is_bool(eval(lhs, args)?)? && is_bool(eval(rhs, args)?)?),
        Or(lhs, rhs) => Val::Bool(is_bool(eval(lhs, args)?)? || is_bool(eval(rhs, args)?)?),
        Equal(lhs, rhs) => Val::Bool(eval_bool(lhs, rhs, args)?),
        NEqual(lhs, rhs) => Val::Bool(!eval_bool(lhs, rhs, args)?),
        Substring(lhs, start, len) => {
            // TODO: add case for Val::Bytes
            Val::String(substring(&is_str(eval(lhs, args)?)?, *start, *len))
        }
        Concat(lhs, rhs) => match (eval(lhs, args)?, eval(rhs, args)?) {
            (Val::String(mut a), Val::String(b)) => {
                a.push_str(&b);
                Val::String(a)
            }
            (Val::Bytes(mut a), Val::Bytes(mut b)) => {
                a.append(&mut b);
                Val::Bytes(a)
            }
            (Val::String(mut a), Val::Bytes(b)) => {
                a.push_str(str::from_utf8(&b)?);
                Val::String(a)
            }
            (Val::Bytes(mut a), Val::String(b)) => {
                a.extend_from_slice(b.as_bytes());
                Val::Bytes(a)
            }
            (a, _b) => return Err(EvalErr::ExpectedString(a)),
        },
        IfElse(expr, a, b) => {
            if is_bool(eval(expr, args)?)? {
                eval(a, args)?
            } else {
                eval(b, args)?
            }
        }
        Member(s) => Val::Bool(args.deps.contains(s)),
    })
}

fn substring(s: &str, mut start: isize, j: Option<isize>) -> String {
    if start.unsigned_abs() >= s.len() {
        return String::default();
    }
    let mut neg = false;
    if start.is_negative() {
        // start is neg
        neg = true;
        start += s.len() as isize;
    }

    match j {
        None => {
            if neg {
                s[..start.unsigned_abs()].to_owned()
            } else {
                s[start.unsigned_abs()..].to_owned()
            }
        }
        Some(mut len) => {
            if len.is_negative() {
                len = len.abs();
                if len <= start {
                    start -= len;
                } else {
                    len = start;
                    start = 0;
                }
            }
            if start + len > s.len() as isize {
                len = len.clamp(0, s.len() as isize - start);
            }
            s[start.unsigned_abs()..(start.unsigned_abs() + len.unsigned_abs())].to_owned()
        }
    }
}

fn eval_bool(lhs: &Expr, rhs: &Expr, args: &Args) -> Result<bool, EvalErr> {
    Ok(match eval(lhs, args)? {
        Val::String(a) => match eval(rhs, args)? {
            Val::String(b) => a == b,
            Val::Bytes(b) => a.as_bytes() == b,
            err => return Err(EvalErr::ExpectedString(err)),
        },
        Val::Bool(a) => a == is_bool(eval(rhs, args)?)?,
        Val::Int(a) => a == is_int(eval(rhs, args)?)?,
        Val::Empty => is_empty(eval(rhs, args)?).is_ok(),
        Val::Bytes(a) => match eval(rhs, args)? {
            Val::String(b) => a == b.as_bytes(),
            Val::Bytes(b) => a == b,
            err => return Err(EvalErr::ExpectedBytes(err)),
        },
    })
}

#[cfg(test)]
mod tests {
    use dhcproto::v4::UnknownOption;
    use pest::Parser;

    use super::*;
    use crate::ast::*;
    use std::{collections::HashMap, net::Ipv4Addr};

    #[test]
    fn test_opt_exists() {
        let tokens = PredicateParser::parse(Rule::expr, "not option[123].exists").unwrap();
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };

        let val = eval(&dbg!(build_ast(tokens).unwrap()), &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }
    #[test]
    fn test_ip_parser() {
        let tokens = PredicateParser::parse(Rule::expr, "100.10.10.10 == 100.10.10.10").unwrap();
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };
        let val = eval(&build_ast(tokens).unwrap(), &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_substring_opts() {
        let mut opts = HashMap::new();
        opts.insert(
            61.into(),
            UnknownOption::new(61.into(), b"some_client_id".to_vec()),
        );

        let tokens = ast::parse(
            "substring(pkt4.mac, 0, 6) == '001122' and option[61].hex == 'some_client_id'",
        )
        .unwrap();
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts,
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };
        let val = eval(&tokens, &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_relay_opts() {
        let mut opts = HashMap::new();
        let mut data: Vec<u8> = vec![12];

        let sub_opt = "foo".as_bytes();
        data.push(sub_opt.len() as u8);
        data.extend(sub_opt);
        data.extend(&[
            23, 3, 1, 2, 3, // two
            45, 0, // three
            123, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ]);

        opts.insert(
            v4::OptionCode::RelayAgentInformation,
            UnknownOption::new(v4::OptionCode::RelayAgentInformation, data),
        );
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts,
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };

        let expr = ast::parse("relay4[12].exists").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse("relay4[12].hex == 'foo'").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_sub_opts_postfix() {
        let mut opts = HashMap::new();
        let mut data: Vec<u8> = vec![12];

        let sub_opt = "foo".as_bytes();
        data.push(sub_opt.len() as u8);
        data.extend(sub_opt);
        data.extend(&[
            23, 3, 1, 2, 3, // two
            45, 0, // three
            123, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ]);

        opts.insert(
            v4::OptionCode::RelayAgentInformation,
            UnknownOption::new(v4::OptionCode::RelayAgentInformation, data),
        );
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts,
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };
        // test that we can address sub options through the sub-opt postfix
        let expr = ast::parse("option[82].option[12] == 'foo'").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse("option[82].option[23].hex").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bytes(vec![1, 2, 3]));

        let expr = ast::parse("option[82].option[23].exists").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse("option[82].option[25].exists").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(false));

        // the parent opt 81 does not exist, no sub-opts to address
        let expr = ast::parse("option[81].option[25].exists").unwrap();
        let val = eval(&expr, &args);
        // should error
        assert!(val.is_err());
        if let Err(err) = val {
            match err {
                EvalErr::ExpectedBytes(b) => assert_eq!(b, Val::Empty),
                _ => panic!("must be expectedbytes"),
            }
        };
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

    #[test]
    fn test_msg_hdr() {
        let options = HashMap::new();
        let mut msg = v4::Message::new_with_id(
            123,
            Ipv4Addr::new(1, 2, 3, 4),
            Ipv4Addr::new(2, 2, 2, 2),
            Ipv4Addr::new(3, 3, 3, 3),
            Ipv4Addr::new(4, 4, 4, 4),
            "123456".as_bytes(),
        );
        msg.set_htype(v4::HType::Eth);
        let mut opts = v4::DhcpOptions::new();
        opts.insert(v4::DhcpOption::MessageType(v4::MessageType::Offer));
        msg.set_opts(opts);

        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: options,
            msg: &msg,
            deps: HashSet::new(),
        };

        let expr = ast::parse("pkt4.hlen == 6").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse("pkt4.ciaddr == 1.2.3.4").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse(
            "pkt4.yiaddr == 2.2.2.2 and pkt4.siaddr == 3.3.3.3 and pkt4.giaddr == 4.4.4.4",
        )
        .unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse("pkt4.msgtype == 2").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_class_dependencies() {
        let opts = HashMap::new();
        let msg = v4::Message::new_with_id(
            123,
            Ipv4Addr::new(1, 2, 3, 4),
            Ipv4Addr::new(2, 2, 2, 2),
            Ipv4Addr::new(3, 3, 3, 3),
            Ipv4Addr::new(4, 4, 4, 4),
            "123456".as_bytes(),
        );

        let expr = ast::parse(
            "member('foobar') and member('bazz') and (member('bingo') or (member('bongo')))",
        )
        .unwrap();

        let deps = crate::get_class_dependencies(&expr);
        assert_eq!(
            deps.into_iter().collect::<std::collections::HashSet<_>>(),
            [
                "foobar".to_owned(),
                "bazz".to_owned(),
                "bingo".to_owned(),
                "bongo".to_owned()
            ]
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
        );

        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: opts.clone(),
            msg: &msg,
            deps: ["foobar", "bazz", "bingo", "bongo"]
                .into_iter()
                .map(|s| s.to_owned())
                .collect(),
        };
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        // remove one member from `or`, should eval to true still
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: opts.clone(),
            msg: &msg,
            deps: ["foobar", "bazz", "bingo"]
                .into_iter()
                .map(|s| s.to_owned())
                .collect(),
        };
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        // remove one of members so eval == false
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts,
            msg: &msg,
            deps: ["foobar", "bingo"]
                .into_iter()
                .map(|s| s.to_owned())
                .collect(),
        };
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(false));
    }

    #[test]
    fn test_substring() {
        assert_eq!(substring("foobar", 0, Some(6)), "foobar");
        assert_eq!(substring("foobar", 3, Some(3)), "bar");
        assert_eq!(substring("foobar", 3, None), "bar"); // "all"
        assert_eq!(substring("foobar", 0, None), "foobar"); // "all"
        assert_eq!(substring("foobar", 1, Some(4)), "ooba");
        assert_eq!(substring("foobar", -5, Some(4)), "ooba");
        assert_eq!(substring("foobar", -1, Some(-3)), "oba");
        assert_eq!(substring("foobar", 4, Some(-2)), "ob");
        assert_eq!(substring("foobar", 10, Some(2)), "");
        assert_eq!(substring("foobar", -1, None), "fooba");
        assert_eq!(substring("foobar", 0, Some(10)), "foobar");
    }

    #[test]
    fn test_concat() {
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };

        let expr = ast::parse("concat('foo', 'bar')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("foobar".to_owned()));
    }

    #[test]
    fn test_ifelse() {
        let args = Args {
            chaddr: "001122334455".to_owned(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            deps: HashSet::new(),
        };

        let expr = ast::parse("ifelse(true, 'foo', 'bar')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("foo".to_owned()));

        let expr = ast::parse("ifelse(false, 'foo', 'bar')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("bar".to_owned()));

        let expr = ast::parse("ifelse((not option[123].exists), 'foo', 'bar')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("foo".to_owned()));

        let expr = ast::parse("ifelse((not option[123].exists), option[1].exists, 'bar')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(false));
    }
}
