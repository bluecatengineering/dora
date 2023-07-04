use std::{
    collections::{HashMap, HashSet},
    ops::{Range, RangeFrom, RangeTo},
    str,
};

use dhcproto::{v4, Decoder};
use thiserror::Error;

pub mod ast;
pub use ast::{Expr, ParseErr, ParseResult};

pub const DROP_CLASS: &str = "DROP";
// the following classes can be used in `member()`
pub const VENDOR_PREFIX_CLASS: &str = "VENDOR_CLASS_";
pub const ALL_CLASS: &str = "ALL";
pub const KNOWN_CLASS: &str = "KNOWN";
pub const UNKNOWN_CLASS: &str = "UNKNOWN";
pub const BOOTP_CLASS: &str = "BOOTP";

pub fn parse_builtin_vendor(s: &str) -> Option<&str> {
    s.strip_prefix(VENDOR_PREFIX_CLASS)
}

pub fn create_builtin_vendor(req: &v4::Message) -> Result<Option<String>, std::str::Utf8Error> {
    Ok(req
        .opts()
        .get(v4::OptionCode::ClassIdentifier)
        .and_then(|class| {
            if let v4::DhcpOption::ClassIdentifier(class) = class {
                Some(std::str::from_utf8(class))
            } else {
                None
            }
        })
        .transpose()?
        .map(|class| format!("{VENDOR_PREFIX_CLASS}{class}")))
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
fn is_bytes(val: Val) -> EvalResult<Vec<u8>> {
    match val {
        Val::Bytes(s) => Ok(s),
        err => Err(EvalErr::ExpectedBytes(err)),
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
    /// packet mac addr
    pub chaddr: &'a [u8],
    /// packet options as UnknownOption
    pub opts: HashMap<v4::OptionCode, v4::UnknownOption>,
    /// decoded packet Message
    pub msg: &'a v4::Message,
    /// all classes that eval'd to true for this packet
    pub member: HashSet<String>,
}

/// evaluate the AST, using values from this DHCP message
pub fn eval(expr: &Expr, args: &Args) -> Result<Val, EvalErr> {
    // TODO: should this fn impl Expr and take &self?
    use Expr::*;
    Ok(match expr {
        Bool(b) => Val::Bool(*b),
        String(s) => Val::String(s.clone()),
        Int(i) => Val::Int(*i),
        Hex(h) => Val::Bytes(h.to_vec()),
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
        Mac() => Val::Bytes(args.chaddr.to_vec()),
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
        Substring(lhs, start, len) => match eval(lhs, args)? {
            Val::Bytes(b) => Val::Bytes(slice(b, *start, *len)),
            Val::String(s) => Val::String(substring(&s, *start, *len)),
            err => return Err(EvalErr::ExpectedString(err)),
        },
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
        Hexstring(expr, sep) => Val::String(
            hex::encode(is_bytes(eval(expr, args)?)?)
                .as_bytes()
                .chunks_exact(2)
                .map(std::str::from_utf8)
                .collect::<Result<Vec<_>, _>>()?
                .join(sep),
        ),
        Member(s) => Val::Bool(args.member.contains(s)),
    })
}

fn substring(s: &str, start: isize, j: Option<isize>) -> String {
    match get_pos(s.len(), start, j) {
        None => String::default(),
        Some(SliceSubstr::To(r)) => s[r].to_owned(),
        Some(SliceSubstr::From(r)) => s[r].to_owned(),
        Some(SliceSubstr::Slice(r)) => s[r].to_owned(),
    }
}

fn slice<T>(s: T, start: isize, j: Option<isize>) -> Vec<u8>
where
    T: AsRef<[u8]>,
{
    let s = s.as_ref();
    match get_pos(s.len(), start, j) {
        None => Vec::default(),
        Some(SliceSubstr::To(r)) => s[r].to_owned(),
        Some(SliceSubstr::From(r)) => s[r].to_owned(),
        Some(SliceSubstr::Slice(r)) => s[r].to_owned(),
    }
}

enum SliceSubstr {
    To(RangeTo<usize>),
    From(RangeFrom<usize>),
    Slice(Range<usize>),
}

fn get_pos(s_len: usize, mut start: isize, j: Option<isize>) -> Option<SliceSubstr> {
    if start.unsigned_abs() >= s_len {
        return None;
    }
    let mut neg = false;
    if start.is_negative() {
        // start is neg
        neg = true;
        start += s_len as isize;
    }

    match j {
        None => {
            if neg {
                Some(SliceSubstr::To(..start.unsigned_abs()))
            } else {
                Some(SliceSubstr::From(start.unsigned_abs()..))
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
            if start + len > s_len as isize {
                len = len.clamp(0, s_len as isize - start);
            }
            Some(SliceSubstr::Slice(
                start.unsigned_abs()..(start.unsigned_abs() + len.unsigned_abs()),
            ))
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
            chaddr: "001122334455".as_bytes(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            member: HashSet::new(),
        };

        let val = eval(&dbg!(build_ast(tokens).unwrap()), &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }
    #[test]
    fn test_ip_parser() {
        let tokens = PredicateParser::parse(Rule::expr, "100.10.10.10 == 100.10.10.10").unwrap();
        let args = Args {
            chaddr: "001122334455".as_bytes(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            member: HashSet::new(),
        };
        let val = eval(&build_ast(tokens).unwrap(), &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }
    #[test]
    fn test_mac() {
        let args = Args {
            chaddr: &hex::decode("010203040506").unwrap(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            member: HashSet::new(),
        };
        let expr = ast::parse("pkt4.mac == 0x010203040506").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));
    }

    #[test]
    fn test_substring_opts() {
        let mut opts = HashMap::new();
        opts.insert(
            61.into(),
            UnknownOption::new(61.into(), b"some_client_id".to_vec()),
        );

        let tokens =
            ast::parse("substring('foobar', 0, 3) == 'foo' and option[61].hex == 'some_client_id'")
                .unwrap();
        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts,
            msg: &v4::Message::default(),
            member: HashSet::new(),
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
        data.extend([
            23, 3, 1, 2, 3, // two
            45, 0, // three
            123, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ]);

        opts.insert(
            v4::OptionCode::RelayAgentInformation,
            UnknownOption::new(v4::OptionCode::RelayAgentInformation, data),
        );
        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts,
            msg: &v4::Message::default(),
            member: HashSet::new(),
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
        data.extend([
            23, 3, 1, 2, 3, // two
            45, 0, // three
            123, 10, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ]);

        opts.insert(
            v4::OptionCode::RelayAgentInformation,
            UnknownOption::new(v4::OptionCode::RelayAgentInformation, data),
        );
        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts,
            msg: &v4::Message::default(),
            member: HashSet::new(),
        };
        // test that we can address sub options through the sub-opt postfix
        let expr = ast::parse("option[82].option[12] == 'foo'").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        let expr = ast::parse("option[82].option[23].hex").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bytes(vec![1, 2, 3]));

        let r = hex::encode([1, 2, 3]);
        let expr = ast::parse(format!("option[82].option[23].hex == 0x{r}")).unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

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
        msg.set_xid(1234).set_htype(v4::HType::Eth).set_opts({
            let mut opts = v4::DhcpOptions::new();
            opts.insert(v4::DhcpOption::MessageType(v4::MessageType::Offer));
            opts
        });

        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts: options,
            msg: &msg,
            member: HashSet::new(),
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

        let expr = ast::parse("pkt4.transid == 1234").unwrap();
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
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts: opts.clone(),
            msg: &msg,
            member: ["foobar", "bazz", "bingo", "bongo"]
                .into_iter()
                .map(|s| s.to_owned())
                .collect(),
        };
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        // remove one member from `or`, should eval to true still
        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts: opts.clone(),
            msg: &msg,
            member: ["foobar", "bazz", "bingo"]
                .into_iter()
                .map(|s| s.to_owned())
                .collect(),
        };
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::Bool(true));

        // remove one of members so eval == false
        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts,
            msg: &msg,
            member: ["foobar", "bingo"]
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
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            member: HashSet::new(),
        };

        let expr = ast::parse("concat('foo', 'bar')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("foobar".to_owned()));
    }

    #[test]
    fn test_ifelse() {
        let args = Args {
            chaddr: &hex::decode("DEADBEEF").unwrap(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            member: HashSet::new(),
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

    #[test]
    fn test_hexstring() {
        let args = Args {
            chaddr: &hex::decode(hex::encode("foo")).unwrap(),
            opts: HashMap::new(),
            msg: &v4::Message::default(),
            member: HashSet::new(),
        };

        let expr = ast::parse("hexstring(0x1234,':')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("12:34".to_owned()));

        let expr = ast::parse("hexstring(0x56789a,'-')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("56-78-9a".to_owned()));

        let expr = ast::parse("hexstring(0xbcde,'')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("bcde".to_owned()));

        let expr = ast::parse("hexstring(0xf01234,'..')").unwrap();
        let val = eval(&expr, &args).unwrap();
        assert_eq!(val, Val::String("f0..12..34".to_owned()));

        let expr = ast::parse("hexstring(pkt4.mac,':')").unwrap();
        let val = eval(&expr, &args).unwrap();
        // foo -> 666f6f
        assert_eq!(val, Val::String("66:6f:6f".to_owned()));
    }

    #[test]
    fn test_builtin_vendor() {
        let uns = Ipv4Addr::UNSPECIFIED;
        let mut req = v4::Message::new(
            uns,
            uns,
            uns,
            uns,
            &hex::decode(hex::encode("foo")).unwrap(),
        );
        req.opts_mut()
            .insert(v4::DhcpOption::ClassIdentifier(b"docsis3.0".to_vec()));
        let vendor = super::create_builtin_vendor(&req).unwrap();
        assert_eq!(vendor, Some("VENDOR_CLASS_docsis3.0".to_owned()));

        assert_eq!(
            super::parse_builtin_vendor(&vendor.unwrap()).unwrap(),
            "docsis3.0"
        );
    }
}
