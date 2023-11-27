use base64::Engine;
use dora_core::dhcproto::{
    v6::{DhcpOption, DhcpOptions, EncodeResult, OptionCode},
    Decodable, Decoder, Encodable, Encoder,
};
use ipnet::Ipv6Net;
use serde::{de, Deserialize, Deserializer, Serialize};
use tracing::warn;

use std::{collections::HashMap, net::Ipv6Addr, ops::RangeInclusive};

use crate::{
    v6::DEFAULT_SERVER_ID_FILE_PATH,
    wire::{MaybeList, MinMax},
};

/// top-level config type
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct Config {
    pub interfaces: Option<Vec<String>>,
    pub server_id: Option<ServerDuid>,
    pub networks: HashMap<Ipv6Net, Net>,
    // TODO: better defaults than blank? pull information from the system
    #[serde(default)]
    pub options: Option<Options>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Net {
    pub config: NetworkConfig,
    #[serde(default)]
    pub options: Options,
    pub interfaces: Option<Vec<String>>,
    /// ping check is an optional value, when turned on an ICMP echo request will be sent
    /// before OFFER for this network
    #[serde(default)]
    pub ping_check: bool,
    /// default ping timeout in ms
    #[serde(default = "super::default_ping_to")]
    pub ping_timeout_ms: u64,
    /// probation period in seconds
    #[serde(default = "super::default_probation")]
    pub probation_period: u64,
    /// Whether we are authoritative for this network (default: true)
    #[serde(default = "super::default_authoritative")]
    pub authoritative: bool,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum DuidType {
    LLT,
    LL,
    EN,
    UUID,
}

impl Default for DuidType {
    fn default() -> Self {
        Self::LLT
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(tag = "type")]
pub enum ServerDuidInfo {
    LLT {
        #[serde(default)]
        htype: u16,
        #[serde(default)]
        time: u32,
        #[serde(default)]
        identifier: String,
    },
    LL {
        #[serde(default)]
        htype: u16,
        #[serde(default)]
        identifier: String,
    },
    EN {
        #[serde(default)]
        enterprise_id: u32,
        #[serde(default)]
        identifier: String,
    },
    UUID {
        // identifier must be supplied for UUID
        identifier: String,
    },
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(tag = "type")]
pub struct ServerDuid {
    #[serde(flatten)]
    pub info: ServerDuidInfo,
    #[serde(default = "default_persist")]
    pub persist: bool,
    #[serde(default = "default_path")]
    pub path: String,
}

fn default_persist() -> bool {
    true
}

fn default_path() -> String {
    DEFAULT_SERVER_ID_FILE_PATH.to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub lease_time: MinMax,
    pub preferred_time: MinMax,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IpRange {
    // RangeInclusive includes `start`/`end` so flatten will parse those fields
    #[serde(flatten)]
    pub range: RangeInclusive<Ipv6Addr>,
    pub options: Options,
    pub config: NetworkConfig,
    #[serde(default)]
    pub except: Vec<Ipv6Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
pub struct Options {
    pub values: Opts,
}

impl Options {
    pub fn get(self) -> DhcpOptions {
        self.values.0
    }
}

impl AsRef<DhcpOptions> for Options {
    fn as_ref(&self) -> &DhcpOptions {
        &self.values.0
    }
}

impl From<Options> for DhcpOptions {
    fn from(o: Options) -> Self {
        o.values.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Opts(pub DhcpOptions);

/// this type is only used as an intermediate representation
/// Opts are received as essentially a HashMap<u8, Opt>
/// and transformed into DhcpOptions
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum Opt {
    Ip(MaybeList<Ipv6Addr>),
    IpList(Vec<Ipv6Addr>),
    U8(MaybeList<u8>),
    U32(MaybeList<u32>),
    U16(MaybeList<u16>),
    Str(MaybeList<String>),
    B64(String),
    Hex(String),
}

impl<'de> serde::Deserialize<'de> for Opts {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // decode what was on the wire to a map
        let map: HashMap<u16, Opt> = Deserialize::deserialize(de)?;
        // we'll encode the map to buf so we can use DhcpOptions::decode
        let mut buf = vec![];
        let mut enc = Encoder::new(&mut buf);
        for (code, opt) in map {
            write_opt(&mut enc, code, opt).map_err(de::Error::custom)?;
        }

        // buffer now has binary data for DhcpOptions -- decode it
        let opts = DhcpOptions::decode(&mut Decoder::new(&buf)).map_err(de::Error::custom)?;
        Ok(Self(opts))
    }
}

fn encode_opt<'a, T, F>(data: &[T], f: F, e: &mut Encoder<'a>) -> EncodeResult<()>
where
    F: Fn(&T, &mut Encoder<'a>) -> EncodeResult<()>,
{
    // size_of_val removes data.len() * mem::size_of::<T>()
    e.write_u16((std::mem::size_of_val(data)) as u16)?;
    for thing in data {
        f(thing, e)?;
    }
    Ok(())
}

fn write_opt(enc: &mut Encoder<'_>, code: u16, opt: Opt) -> anyhow::Result<()> {
    enc.write_u16(code)?;
    match opt {
        Opt::Ip(MaybeList::Val(ip)) => {
            enc.write_u16(16)?;
            enc.write_u128(ip.into())?;
        }
        Opt::IpList(list) | Opt::Ip(MaybeList::List(list)) => {
            enc.write_u16(list.len() as u16 * 16)?;
            for ip in list {
                enc.write_u128(ip.into())?;
            }
        }
        Opt::U8(MaybeList::Val(n)) => {
            enc.write_u16(1)?;
            enc.write_u8(n)?;
        }
        Opt::U8(MaybeList::List(list)) => {
            enc.write_u16(list.len() as u16)?;
            enc.write_slice(&list)?;
        }
        Opt::U32(MaybeList::Val(n)) => {
            enc.write_u16(4)?;
            enc.write_u32(n)?;
        }
        Opt::U32(MaybeList::List(list)) => {
            encode_opt(&list, |n, e| e.write_u32(*n), enc)?;
        }
        Opt::U16(MaybeList::Val(n)) => {
            enc.write_u16(2)?;
            enc.write_u16(n)?;
        }
        Opt::U16(MaybeList::List(list)) => {
            encode_opt(&list, |n, e| e.write_u16(*n), enc)?;
        }
        Opt::Str(MaybeList::Val(s)) => {
            enc.write_u16(s.as_bytes().len() as u16)?;
            enc.write_slice(s.as_bytes())?;
        }
        Opt::Str(MaybeList::List(list)) => {
            encode_opt(&list, |n, e| e.write_slice(n.as_bytes()), enc)?;
        }
        Opt::B64(s) => {
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(s)?;
            enc.write_u16(bytes.len() as u16)?;
            enc.write_slice(&bytes)?;
        }
        Opt::Hex(s) => {
            let bytes = hex::decode(s)?;
            enc.write_u16(bytes.len() as u16)?;
            enc.write_slice(&bytes)?;
        }
    }
    Ok(())
}

// NOTE: this will be used in tests, so a complete mapping of different
// opt types is not necessary. Using B64, everything will still be decoded
// to it's proper type
impl Serialize for Opts {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let map = self
            .0
            .iter()
            .filter_map(decode_opt)
            .collect::<HashMap<u16, Opt>>();
        ser.collect_map(&map)
    }
}

fn decode_opt(opt: &DhcpOption) -> Option<(u16, Opt)> {
    use dora_core::dhcproto::v6::DhcpOption::*;
    let code: OptionCode = opt.into();
    match opt {
        // inspiration: https://kea.readthedocs.io/en/kea-2.2.0/arm/dhcp6-srv.html?highlight=router%20advertisement#dhcp6-std-options-list
        Preference(n) => Some((code.into(), Opt::U8(MaybeList::Val(*n)))),
        ServerUnicast(ip) => Some((code.into(), Opt::Ip(MaybeList::Val(*ip)))),
        DomainNameServers(addrs) => Some((code.into(), Opt::Ip(MaybeList::List(addrs.clone())))),
        Unknown(opt) => Some((code.into(), Opt::Hex(hex::encode(opt.data())))),
        _ => {
            // the data includes the code value, let's slice that off
            match opt.to_vec() {
                Ok(buf) => Some((
                    code.into(),
                    Opt::Hex(if buf.is_empty() {
                        "".into()
                    } else {
                        hex::encode(&buf[1..])
                    }),
                )),
                Err(err) => {
                    warn!(?err);
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode() {
        let mut buf = Vec::new();
        let mut e = Encoder::new(&mut buf);
        let opt = Opt::Ip(MaybeList::List(vec![
            Ipv6Addr::UNSPECIFIED,
            Ipv6Addr::LOCALHOST,
        ]));
        write_opt(&mut e, 23, opt).unwrap();
        dbg!(std::mem::size_of::<Ipv6Addr>());
        assert_eq!(
            // [<2 byte code><2 byte len><data>]
            &[
                0, 23, // code
                0, 32, // len in bytes
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // first addr
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1 // second addr
            ],
            &buf[..]
        );
    }
}
