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
//! ## BOOTP enable
//!
//! Enable BOOTP for dora, only support for RFC1497.
//!
//! ## Authoritative
//!
//! When the DHCP server is configured as authoritative, the server will respond with
//! ACK or NACK as appropriate for all the received REQUEST and INFORM messages
//!  belonging to the subnet.
//! Non-authoritative INFORM packets received from the clients on a
//! non-authoritative network will be ignored.
use std::{collections::HashMap, hash::Hash, net::Ipv4Addr, ops::RangeInclusive};

use anyhow::Result;
use base64::Engine;
use dora_core::{
    dhcproto::{
        Decodable, Decoder, Encodable, Encoder,
        v4::{self, DhcpOption, DhcpOptions, OptionCode},
    },
    pnet::util::MacAddr,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use tracing::warn;
use trust_dns_proto::{
    rr,
    serialize::binary::{BinEncodable, BinEncoder},
};

use crate::wire::{MaybeList, MinMax};

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
    pub server_name: Option<String>,
    pub file_name: Option<String>,
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
    pub class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NetworkConfig {
    pub lease_time: MinMax,
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReservedIp {
    pub ip: Ipv4Addr,
    pub options: Options,
    #[serde(rename = "match")]
    pub condition: Condition,
    pub config: NetworkConfig,
    pub class: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    #[serde(rename = "chaddr")]
    Mac(MacAddr),
    Options(Options),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Opts(pub DhcpOptions);

/// this type is only used as an intermediate representation
/// Opts are received as essentially a HashMap<u8, Opt>
/// and transformed into DhcpOptions
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum Opt {
    Ip(MaybeList<Ipv4Addr>),
    IpList(Vec<Ipv4Addr>), // keep for backwards compatibility
    Domain(MaybeList<String>),
    DomainList(Vec<String>), // keep for backwards compatibility
    U8(MaybeList<u8>),
    I8(MaybeList<i8>),
    U16(MaybeList<u16>),
    I16(MaybeList<i16>),
    U32(MaybeList<u32>),
    I32(MaybeList<i32>),
    Bool(MaybeList<bool>),
    Str(MaybeList<String>),
    B64(String),
    Hex(String),
    SubOption(HashMap<u8, Opt>),
}

impl<'de> serde::Deserialize<'de> for Opts {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        static NAME_MAP: phf::Map<&'static str, u8> = phf::phf_map! {
            "subnet_mask" => 1,
            "time_offset" => 2,
            "routers" => 3,
            "time_servers" => 4,
            "name_servers" => 5,
            "domain_name_servers" => 6,
            "log_servers" => 7,
            "quote_servers" => 8,
            "lpr_servers" => 9,
            "impress_servers" => 10,
            "resource_location_servers" => 11,
            "hostname" => 12,
            "boot_size" => 13,
            "merit_dump" => 14,
            "domain_name" => 15,
            "swap_server" => 16,
            "root_path" => 17,
            "extensions_path" => 18,
            "ip_forwarding" => 19,
            "non_local_source_routing" => 20,
            "default_ip_ttl" => 23,
            "interface_mtu" => 26,
            "all_subnets_local" => 27,
            "broadcast_addr" => 28,
            "static_routing_table" => 33,
            "arp_cache_timeout" => 35,
            "default_tcp_ttl" => 37,
            "nis_domain" => 40,
            "nis_servers" => 41,
            "ntp_servers" => 42,
            "vendor_extensions" => 43,
            "netbios_name_servers" => 44,
            "domain_search" => 119,
        };

        // inner key type to handle string name or number
        #[derive(Serialize, Debug, PartialEq, Eq, Hash)]
        struct OptKey(u8);
        impl<'de> serde::Deserialize<'de> for OptKey {
            fn deserialize<D>(de: D) -> Result<OptKey, D::Error>
            where
                D: Deserializer<'de>,
            {
                let key: String = Deserialize::deserialize(de)?;
                Ok(OptKey(key.parse::<u8>().or_else(|_| {
                    NAME_MAP
                        .get(&key)
                        .cloned()
                        .ok_or_else(|| de::Error::custom(format!("unknown option key {}", key)))
                })?))
            }
        }
        // decode what was on the wire to a map
        let map: HashMap<OptKey, Opt> = Deserialize::deserialize(de)?;
        // we'll encode the map to buf so we can use DhcpOptions::decode
        let mut buf = vec![];
        let mut enc = Encoder::new(&mut buf);
        for (code, opt) in map {
            write_opt(&mut enc, code.0, opt).map_err(de::Error::custom)?;
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
    match opt {
        Opt::Ip(MaybeList::Val(ip)) => {
            enc.write_u8(code)?;
            enc.write_u8(4)?;
            enc.write_slice(&ip.octets())?;
        }
        Opt::Ip(MaybeList::List(list)) | Opt::IpList(list) => {
            v4::encode_long_opt_chunks(
                OptionCode::from(code),
                4,
                &list,
                |ip, e| e.write_u32((*ip).into()),
                enc,
            )?;
        }
        Opt::Domain(MaybeList::Val(domain)) => {
            let mut buf = Vec::new();
            let mut name_encoder = BinEncoder::new(&mut buf);
            let name = domain.parse::<rr::Name>()?;
            name.emit(&mut name_encoder)?;
            v4::encode_long_opt_bytes(OptionCode::from(code), &buf, enc)?;
        }
        // encode in DNS format
        Opt::Domain(MaybeList::List(list)) | Opt::DomainList(list) => {
            let mut buf = Vec::new();
            let mut name_encoder = BinEncoder::new(&mut buf);
            for name in list {
                let name = name.parse::<rr::Name>()?;
                name.emit(&mut name_encoder)?;
            }
            v4::encode_long_opt_bytes(OptionCode::from(code), &buf, enc)?;
        }
        Opt::Str(MaybeList::Val(s)) => {
            v4::encode_long_opt_bytes(OptionCode::from(code), s.as_bytes(), enc)?;
        }
        Opt::Str(MaybeList::List(list)) => {
            let buf = list
                .into_iter()
                .flat_map(|s| s.as_bytes().to_vec())
                .collect::<Vec<_>>();
            v4::encode_long_opt_bytes(OptionCode::from(code), &buf, enc)?;
        }
        Opt::U32(MaybeList::Val(n)) => {
            enc.write_u8(code)?;
            enc.write_u8(4)?;
            enc.write_u32(n)?;
        }
        Opt::U32(MaybeList::List(list)) => {
            v4::encode_long_opt_chunks(
                OptionCode::from(code),
                4,
                &list,
                |n, e| e.write_u32(*n),
                enc,
            )?;
        }
        Opt::I32(MaybeList::Val(n)) => {
            enc.write_u8(code)?;
            enc.write_u8(4)?;
            enc.write_i32(n)?;
        }
        Opt::I32(MaybeList::List(list)) => {
            v4::encode_long_opt_chunks(
                OptionCode::from(code),
                4,
                &list,
                |n, e| e.write_i32(*n),
                enc,
            )?;
        }
        Opt::U8(MaybeList::Val(n)) => {
            enc.write_u8(code)?;
            enc.write_u8(1)?;
            enc.write_u8(n)?;
        }
        Opt::U8(MaybeList::List(list)) => {
            v4::encode_long_opt_bytes(OptionCode::from(code), &list, enc)?;
        }
        Opt::I8(MaybeList::Val(n)) => {
            enc.write_u8(code)?;
            enc.write_u8(1)?;
            enc.write(n.to_be_bytes())?;
        }
        Opt::I8(MaybeList::List(list)) => {
            let list = list
                .into_iter()
                .map(|b| b.to_be_bytes()[0])
                .collect::<Vec<u8>>();
            v4::encode_long_opt_bytes(OptionCode::from(code), &list, enc)?;
        }
        Opt::Bool(MaybeList::Val(b)) => {
            enc.write_u8(code)?;
            enc.write_u8(1)?;
            enc.write_u8(b.into())?;
        }
        Opt::Bool(MaybeList::List(list)) => {
            let list = list.into_iter().map(|b| b.into()).collect::<Vec<u8>>();
            v4::encode_long_opt_bytes(OptionCode::from(code), &list, enc)?;
        }
        Opt::U16(MaybeList::Val(n)) => {
            enc.write_u8(code)?;
            enc.write_u8(2)?;
            enc.write_u16(n)?;
        }
        Opt::U16(MaybeList::List(list)) => {
            v4::encode_long_opt_chunks(
                OptionCode::from(code),
                2,
                &list,
                |n, e| e.write_u16(*n),
                enc,
            )?;
        }
        Opt::I16(MaybeList::Val(n)) => {
            enc.write_u8(code)?;
            enc.write_u8(2)?;
            enc.write(n.to_be_bytes())?;
        }
        Opt::I16(MaybeList::List(list)) => {
            v4::encode_long_opt_chunks(
                OptionCode::from(code),
                2,
                &list,
                |n, e| e.write(n.to_be_bytes()),
                enc,
            )?;
        }
        Opt::B64(s) => {
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(s)?;
            v4::encode_long_opt_bytes(OptionCode::from(code), &bytes, enc)?;
        }
        Opt::Hex(s) => {
            let bytes = hex::decode(s)?;
            v4::encode_long_opt_bytes(OptionCode::from(code), &bytes, enc)?;
        }
        Opt::SubOption(sub_opts) => {
            // we'll encode the map to buf so we can use DhcpOptions::decode
            let mut sub_buf = vec![];
            let mut sub_enc = Encoder::new(&mut sub_buf);
            for (sub_code, sub_opt) in sub_opts {
                write_opt(&mut sub_enc, sub_code, sub_opt)?;
            }

            v4::encode_long_opt_bytes(OptionCode::from(code), &sub_buf, enc)?;
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
            .filter_map(|(code, opt)| to_opt(code, opt))
            .collect::<HashMap<u8, Opt>>();
        ser.collect_map(&map)
    }
}

fn to_opt(code: &OptionCode, opt: &DhcpOption) -> Option<(u8, Opt)> {
    use dora_core::dhcproto::v4::DhcpOption as O;
    match opt {
        O::Pad | O::End => None,
        O::SubnetMask(addr)
        | O::SwapServer(addr)
        | O::BroadcastAddr(addr)
        | O::RouterSolicitationAddr(addr)
        | O::RequestedIpAddress(addr)
        | O::ServerIdentifier(addr)
        | O::SubnetSelection(addr) => Some(((*code).into(), Opt::Ip(MaybeList::Val(*addr)))),
        O::TimeServer(ips)
        | O::NameServer(ips)
        | O::Router(ips)
        | O::DomainNameServer(ips)
        | O::LogServer(ips)
        | O::QuoteServer(ips)
        | O::LprServer(ips)
        | O::ImpressServer(ips)
        | O::ResourceLocationServer(ips)
        | O::XFontServer(ips)
        | O::XDisplayManager(ips)
        | O::NisServers(ips)
        | O::NtpServers(ips)
        | O::NetBiosNameServers(ips)
        | O::NetBiosDatagramDistributionServer(ips) => {
            Some(((*code).into(), Opt::Ip(MaybeList::List(ips.clone()))))
        }
        O::TimeOffset(num) => Some(((*code).into(), Opt::I32(MaybeList::Val(*num)))),
        O::DefaultTcpTtl(num) | O::DefaultIpTtl(num) | O::OptionOverload(num) => {
            Some(((*code).into(), Opt::U8(MaybeList::Val(*num))))
        }
        O::NetBiosNodeType(ntype) => {
            Some(((*code).into(), Opt::U8(MaybeList::Val((*ntype).into()))))
        }
        O::IpForwarding(b)
        | O::NonLocalSrcRouting(b)
        | O::AllSubnetsLocal(b)
        | O::PerformMaskDiscovery(b)
        | O::MaskSupplier(b)
        | O::PerformRouterDiscovery(b)
        | O::EthernetEncapsulation(b)
        | O::TcpKeepaliveGarbage(b) => Some(((*code).into(), Opt::Bool(MaybeList::Val(*b)))),
        O::ArpCacheTimeout(num)
        | O::TcpKeepaliveInterval(num)
        | O::AddressLeaseTime(num)
        | O::Renewal(num)
        | O::Rebinding(num) => Some(((*code).into(), Opt::U32(MaybeList::Val(*num)))),
        O::Hostname(s)
        | O::MeritDumpFile(s)
        | O::DomainName(s)
        | O::ExtensionsPath(s)
        | O::NisDomain(s)
        | O::RootPath(s)
        | O::NetBiosScope(s)
        | O::Message(s) => Some(((*code).into(), Opt::Str(MaybeList::Val(s.clone())))),
        O::BootFileSize(num)
        | O::MaxDatagramSize(num)
        | O::InterfaceMtu(num)
        | O::MaxMessageSize(num) => Some(((*code).into(), Opt::U16(MaybeList::Val(*num)))),
        O::Unknown(opt) => Some(((*code).into(), Opt::Hex(hex::encode(opt.data())))),
        _ => {
            // the data includes the code & len, let's slice that off
            match opt.to_vec() {
                Ok(buf) => Some((
                    (*code).into(),
                    Opt::Hex(if buf.is_empty() {
                        "".into()
                    } else {
                        // [code: u8][len: u8][data...]
                        hex::encode(&buf[2..])
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

pub mod ddns {
    use std::net::SocketAddr;

    use super::*;

    use dora_core::dhcproto::Name;
    pub use dora_core::trust_dns_proto::rr::dnssec::rdata::tsig::TsigAlgorithm;

    fn default_true() -> bool {
        true
    }
    fn default_false() -> bool {
        false
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
    pub struct Ddns {
        #[serde(default = "default_true")]
        pub enable_updates: bool,
        #[serde(default = "default_false")]
        pub override_client_updates: bool,
        #[serde(default = "default_false")]
        pub override_no_updates: bool,
        pub forward: Vec<DdnsServer>,
        pub reverse: Vec<DdnsServer>,
        pub tsig_keys: HashMap<String, TsigKey>,
    }

    impl Default for Ddns {
        fn default() -> Self {
            Self {
                enable_updates: true,
                override_client_updates: false,
                override_no_updates: false,
                forward: Vec::new(),
                reverse: Vec::new(),
                tsig_keys: HashMap::default(),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
    pub struct DdnsServer {
        pub name: Name,
        pub key: Option<String>,
        pub ip: SocketAddr,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
    pub struct TsigKey {
        #[serde(with = "tsig_algo")]
        pub algorithm: TsigAlgorithm,
        pub data: String,
    }

    mod tsig_algo {
        use super::*;
        use serde::Serializer;

        /// serialize
        pub fn serialize<S>(algo: &TsigAlgorithm, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            let algo: Algorithm = algo
                .try_into()
                .map_err(|_| serde::ser::Error::custom("unsupported tsig algorithm"))?;
            algo.serialize(serializer)
        }

        /// deserialize
        pub fn deserialize<'de, D>(deserializer: D) -> Result<TsigAlgorithm, D::Error>
        where
            D: Deserializer<'de>,
        {
            Ok(Algorithm::deserialize(deserializer)?.into())
        }
    }

    #[derive(Debug, Copy, Clone, PartialEq, Eq, Deserialize, Serialize)]
    #[serde(rename_all = "kebab-case")]
    pub enum Algorithm {
        #[serde(rename = "hmac-md5")]
        HmacMd5,
        #[serde(rename = "hmac-sha1")]
        HmacSha1,
        #[serde(rename = "hmac-sha256")]
        HmacSha256,
        #[serde(rename = "hmac-sha384")]
        HmacSha384,
        #[serde(rename = "hmac-sha512")]
        HmacSha512,
    }

    impl From<Algorithm> for TsigAlgorithm {
        fn from(value: Algorithm) -> Self {
            match value {
                Algorithm::HmacMd5 => TsigAlgorithm::HmacMd5,
                Algorithm::HmacSha1 => TsigAlgorithm::HmacSha1,
                Algorithm::HmacSha256 => TsigAlgorithm::HmacSha256,
                Algorithm::HmacSha384 => TsigAlgorithm::HmacSha384,
                Algorithm::HmacSha512 => TsigAlgorithm::HmacSha512,
            }
        }
    }

    impl TryFrom<&TsigAlgorithm> for Algorithm {
        type Error = anyhow::Error;

        fn try_from(value: &TsigAlgorithm) -> Result<Self, Self::Error> {
            match value {
                TsigAlgorithm::HmacMd5 => Ok(Algorithm::HmacMd5),
                TsigAlgorithm::HmacSha1 => Ok(Algorithm::HmacSha1),
                TsigAlgorithm::HmacSha256 => Ok(Algorithm::HmacSha256),
                TsigAlgorithm::HmacSha384 => Ok(Algorithm::HmacSha384),
                TsigAlgorithm::HmacSha512 => Ok(Algorithm::HmacSha512),
                e => Err(anyhow::anyhow!("unsupported tsig type {:?}", e)),
            }
        }
    }

    impl Ddns {
        pub fn enable_updates(&self) -> bool {
            self.enable_updates
        }
        pub fn override_client_updates(&self) -> bool {
            self.override_client_updates
        }
        pub fn override_no_updates(&self) -> bool {
            self.override_no_updates
        }
        pub fn keys(&self) -> impl Iterator<Item = (&str, &TsigKey)> {
            self.tsig_keys.iter().map(|(name, k)| (name.as_str(), k))
        }
        pub fn key(&self, name: &str) -> Option<&TsigKey> {
            self.tsig_keys.get(name)
        }
        pub fn forward(&self) -> &[DdnsServer] {
            &self.forward
        }
        pub fn match_longest_forward(&self, fqdn: &Name) -> Option<&DdnsServer> {
            match_longest_fqdn(&self.forward, fqdn)
        }
        pub fn reverse(&self) -> &[DdnsServer] {
            &self.reverse
        }
        pub fn match_longest_reverse(&self, arpa_domain: &Name) -> Option<&DdnsServer> {
            match_longest_fqdn(&self.reverse, arpa_domain)
        }
    }

    fn match_longest_fqdn<'a>(list: &'a [DdnsServer], fqdn: &Name) -> Option<&'a DdnsServer> {
        let mut best_match = None;
        let mut match_len = 0;
        for srv in list {
            let fqdn_len = fqdn.num_labels();
            let srv_len = srv.name.num_labels();

            if srv.name.is_wildcard() && srv_len == 0 {
                return Some(srv);
            }
            // srv len is longer than fqdn, can't match
            if srv_len > fqdn_len {
                continue;
            }
            // if fqdn & srv have same # of labels & they are equal, then
            // we found a match
            if fqdn_len == srv_len {
                if fqdn == &srv.name {
                    return Some(srv);
                }
                continue;
            } else {
                let offset = fqdn_len - srv_len;
                // fqdn contains the srv name
                // count the # of matching labels
                if fqdn
                    .iter()
                    .skip(offset as usize)
                    .zip(srv.name.iter())
                    .all(|(a, b)| a == b)
                    && srv_len > match_len
                {
                    best_match = Some(srv);
                    match_len = srv_len;
                }
            }
        }
        best_match
    }
}

#[cfg(test)]
mod tests {
    use super::ddns::*;
    use super::*;
    use ipnet::Ipv4Net;

    use dora_core::dhcproto::Name;

    pub static SAMPLE_YAML: &str = include_str!("../../sample/config.yaml");
    pub static LONG_OPTS: &str = include_str!("../../sample/long_opts.yaml");

    #[test]
    fn test_untagged_opt() {
        let v: Opt =
            serde_json::from_str("{\"type\": \"ip\", \"value\": [\"1.2.3.4\", \"2.3.4.5\" ] }")
                .unwrap();
        assert!(matches!(v, Opt::Ip(MaybeList::List(_))));
    }

    #[test]
    fn test_forward_match() {
        let ddns = Ddns {
            forward: vec![
                DdnsServer {
                    name: "example.com.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "other.example.com.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "foo.example.com.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "a.baz.foo.example.com.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "bing.net.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
            ],
            ..Ddns::default()
        };
        let fwd = ddns.match_longest_forward(&"example.com.".parse::<Name>().unwrap());
        assert_eq!(
            fwd.unwrap(),
            &DdnsServer {
                name: "example.com.".parse().unwrap(),
                key: None,
                ip: ([8, 8, 8, 8], 53).into(),
            }
        );
        let fwd = ddns.match_longest_forward(&"other.example.com.".parse::<Name>().unwrap());
        assert_eq!(
            fwd.unwrap(),
            &DdnsServer {
                name: "other.example.com.".parse().unwrap(),
                key: None,
                ip: ([8, 8, 8, 8], 53).into(),
            }
        );
        let fwd = ddns.match_longest_forward(&"b.foo.example.com.".parse::<Name>().unwrap());
        assert_eq!(
            fwd.unwrap(),
            &DdnsServer {
                name: "foo.example.com.".parse().unwrap(),
                key: None,
                ip: ([8, 8, 8, 8], 53).into(),
            }
        );
        let fwd = ddns.match_longest_forward(&"bang.net.".parse::<Name>().unwrap());
        assert_eq!(fwd, None);
    }

    #[test]
    fn test_forward_match_wildcard() {
        let ddns = Ddns {
            forward: vec![
                DdnsServer {
                    name: "example.com.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "*".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
            ],
            ..Ddns::default()
        };
        let fwd = ddns.match_longest_forward(&"stuff.com.".parse::<Name>().unwrap());
        assert_eq!(
            fwd.unwrap(),
            &DdnsServer {
                name: "*".parse().unwrap(),
                key: None,
                ip: ([8, 8, 8, 8], 53).into(),
            }
        );
    }

    #[test]
    fn test_reverse() {
        let ddns = Ddns {
            reverse: vec![
                DdnsServer {
                    name: "8.8.8.8.in-addr.arpa.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "168.192.in-addr.arpa.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
                DdnsServer {
                    name: "1.192.in-addr.arpa.".parse().unwrap(),
                    key: None,
                    ip: ([8, 8, 8, 8], 53).into(),
                },
            ],
            ..Ddns::default()
        };
        let rev = ddns.match_longest_reverse(&"1.2.168.192.in-addr.arpa.".parse::<Name>().unwrap());
        assert_eq!(
            rev.unwrap(),
            &DdnsServer {
                name: "168.192.in-addr.arpa.".parse().unwrap(),
                key: None,
                ip: ([8, 8, 8, 8], 53).into(),
            }
        );

        let rev = ddns.match_longest_reverse(&"4.4.8.8.in-addr.arpa.".parse::<Name>().unwrap());
        assert_eq!(rev, None);

        let rev = ddns.match_longest_reverse(&"10.192.in-addr.arpa.".parse::<Name>().unwrap());
        assert_eq!(rev, None);

        let rev = ddns.match_longest_reverse(&"3.1.192.in-addr.arpa.".parse::<Name>().unwrap());
        assert_eq!(
            rev.unwrap(),
            &DdnsServer {
                name: "1.192.in-addr.arpa.".parse().unwrap(),
                key: None,
                ip: ([8, 8, 8, 8], 53).into(),
            }
        );
    }

    // test we can encode/decode sample
    #[test]
    fn test_sample() {
        let cfg: crate::wire::Config = serde_yaml::from_str(SAMPLE_YAML).unwrap();
        println!("{cfg:#?}");
        // back to the yaml
        let s = serde_yaml::to_string(&cfg).unwrap();
        println!("{s}");
    }

    #[test]
    fn test_long_opts() {
        let cfg: crate::wire::Config = serde_yaml::from_str(LONG_OPTS).unwrap();
        let opts = cfg
            .networks
            .get(&Ipv4Net::new([192, 168, 1, 100].into(), 30).unwrap())
            .unwrap()
            .ranges
            .first()
            .unwrap()
            .clone()
            .options
            .get();
        let vendor = opts.get(v4::OptionCode::VendorExtensions);
        println!("{opts:?}");
        println!("{vendor:?}");
        // TODO: add test for sub-opts in vendor extensions
    }
}
