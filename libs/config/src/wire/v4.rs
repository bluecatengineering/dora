//! # Config docs
//!
//! ## Reservations
//!
//! Reservations are supported based on `chaddr`, or `options`. Currently, only a single
//! options may be specified for a match. There is no AND/OR logic for matching on options.
//!
//! ## Parameter request options
//!
//! Both reservations & ranges can include an options map, if an incoming dhcp msg gets
//! an IP from that reservation or range, it will also use the corresponding `options`
//! to respond to any parameter request list values.
//!
//! ## Ping check
//!
//! `ping_check` set to true will ping before assigning an IP
//!
//! ## Decline & Duplicate Address Detection
//!
//! `probation_period` is defined per-network. If any DHCP messages are received from
//! this network with a message type of DECLINE, or if a ping check is successful
//! (meaning the address is in use), dora will not attempt to lease the IP inside of
//! the probation period.
//!
//! ## Chaddr Only
//!
//! Normally, client id is determined by (opt 60) client identifier, if it is
//! available, or the DHCP header field `chaddr`. Sometimes, we want to configure
//! the server to only look at the `chaddr` field. Setting `chaddr_only` to true
//! will do that.
//!
//! ## Authoritative
//!
//! When the DHCP server is configured as authoritative, the server will respond with
//! ACK or NACK as appropriate for all the received REQUEST and INFORM messages
//!  belonging to the subnet.
//! Non-authoritative INFORM packets received from the clients on a
//! non-authoritative network will be ignored.
use std::{collections::HashMap, net::Ipv4Addr, ops::RangeInclusive};

use anyhow::Result;
use dora_core::{
    dhcproto::{
        v4::{DhcpOption, DhcpOptions, OptionCode},
        Decodable, Decoder, Encodable, Encoder,
    },
    pnet::util::MacAddr,
};
use serde::{de, Deserialize, Deserializer, Serialize};
use tracing::warn;

use crate::wire::MinMax;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Net {
    pub server_id: Option<Ipv4Addr>,
    #[serde(default)]
    pub ranges: Vec<IpRange>,
    #[serde(default)]
    pub reservations: Vec<ReservedIp>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IpRange {
    // RangeInclusive includes `start`/`end` so flatten will parse those fields
    #[serde(flatten)]
    pub range: RangeInclusive<Ipv4Addr>,
    pub options: Options,
    pub config: NetworkConfig,
    #[serde(default)]
    pub except: Vec<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub lease_time: MinMax,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReservedIp {
    pub ip: Ipv4Addr,
    pub options: Options,
    #[serde(rename = "match")]
    pub condition: Condition,
    pub config: NetworkConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    #[serde(rename = "chaddr")]
    Mac(MacAddr),
    Options(Options),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opts(pub DhcpOptions);

/// this type is only used as an intermediate representation
/// Opts are received as essentially a HashMap<u8, Opt>
/// and transformed into DhcpOptions
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum Opt {
    Ip(Ipv4Addr),
    IpList(Vec<Ipv4Addr>),
    U32(u32),
    U16(u16),
    Str(String),
    B64(String),
    Hex(String),
}

impl<'de> serde::Deserialize<'de> for Opts {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // decode what was on the wire to a map
        let map: HashMap<u8, Opt> = Deserialize::deserialize(de)?;
        // we'll encode the map to buf so we can use DhcpOptions::decode
        let mut buf = vec![];
        let mut enc = Encoder::new(&mut buf);
        for (code, opt) in map {
            write_opt(&mut enc, code, opt).map_err(de::Error::custom)?;
        }
        // write `End` so DhcpOptions can decode
        enc.write_u8(OptionCode::End.into())
            .map_err(de::Error::custom)?;

        // buffer now has binary data for DhcpOptions -- decode it
        let opts = DhcpOptions::decode(&mut Decoder::new(&buf)).map_err(de::Error::custom)?;
        Ok(Self(opts))
    }
}

fn write_opt(enc: &mut Encoder<'_>, code: u8, opt: Opt) -> anyhow::Result<()> {
    enc.write_u8(code)?;
    match opt {
        Opt::Ip(ip) => {
            enc.write_u8(4)?;
            enc.write_slice(&ip.octets())?;
        }
        Opt::IpList(list) => {
            enc.write_u8(list.len() as u8 * 4)?;
            for ip in list {
                enc.write_u32(ip.into())?;
            }
        }
        Opt::Str(s) => {
            enc.write_u8(s.as_bytes().len() as u8)?;
            enc.write_slice(s.as_bytes())?;
        }
        Opt::U32(n) => {
            enc.write_u8(4)?;
            enc.write_u32(n)?;
        }
        Opt::U16(n) => {
            enc.write_u8(2)?;
            enc.write_u16(n)?;
        }
        Opt::B64(s) => {
            let bytes = base64::decode(s)?;
            enc.write_u8(bytes.len() as u8)?;
            enc.write_slice(&bytes)?;
        }
        Opt::Hex(s) => {
            let bytes = hex::decode(s)?;
            enc.write_u8(bytes.len() as u8)?;
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
            .filter_map(|(code, opt)| decode_opt(code, opt))
            .collect::<HashMap<u8, Opt>>();
        ser.collect_map(&map)
    }
}

fn decode_opt(code: &OptionCode, opt: &DhcpOption) -> Option<(u8, Opt)> {
    use dora_core::dhcproto::v4::DhcpOption::*;
    match opt {
        Pad | End => None,
        SubnetMask(addr)
        | SwapServer(addr)
        | BroadcastAddr(addr)
        | RouterSolicitationAddr(addr)
        | RequestedIpAddress(addr)
        | ServerIdentifier(addr)
        | SubnetSelection(addr) => Some(((*code).into(), Opt::Ip(*addr))),
        TimeServer(ips)
        | NameServer(ips)
        | Router(ips)
        | DomainNameServer(ips)
        | LogServer(ips)
        | QuoteServer(ips)
        | LprServer(ips)
        | ImpressServer(ips)
        | ResourceLocationServer(ips)
        | XFontServer(ips)
        | XDisplayManager(ips)
        | NIS(ips)
        | NTPServers(ips)
        | NetBiosNameServers(ips)
        | NetBiosDatagramDistributionServer(ips) => {
            Some(((*code).into(), Opt::IpList(ips.clone())))
        }
        ArpCacheTimeout(num)
        | TcpKeepaliveInterval(num)
        | AddressLeaseTime(num)
        | Renewal(num)
        | Rebinding(num) => Some(((*code).into(), Opt::U32(*num))),
        Hostname(s) | MeritDumpFile(s) | DomainName(s) | ExtensionsPath(s) | NISDomain(s)
        | RootPath(s) | NetBiosScope(s) | Message(s) => Some(((*code).into(), Opt::Str(s.clone()))),
        BootFileSize(num) | MaxDatagramSize(num) | InterfaceMtu(num) | MaxMessageSize(num) => {
            Some(((*code).into(), Opt::U16(*num)))
        }
        Unknown(opt) => Some(((*code).into(), Opt::Hex(hex::encode(opt.data())))),
        _ => {
            // the data includes the code value, let's slice that off
            match opt.to_vec() {
                Ok(buf) => Some((
                    (*code).into(),
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
    pub static SAMPLE_YAML: &str = include_str!("../../sample/config.yaml");

    // test we can encode/decode sample
    #[test]
    fn test_sample() {
        let cfg: crate::wire::Config = serde_yaml::from_str(SAMPLE_YAML).unwrap();
        println!("{:#?}", cfg);
        // back to the yaml
        let s = serde_yaml::to_string(&cfg).unwrap();
        println!("{}", s);
    }
}
