use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    ops::RangeInclusive,
    time::Duration,
};

use anyhow::{Context, Result};
use client_classification::PacketDetails;
use dora_core::{
    dhcproto::{
        self,
        v4::{DhcpOption, DhcpOptions, Message, OptionCode},
    },
    pnet::{
        datalink::NetworkInterface,
        ipnetwork::{IpNetwork, Ipv4Network},
        util::MacAddr,
    },
};
use ipnet::{Ipv4AddrRange, Ipv4Net};
use tracing::debug;

use crate::{client_classes::ClientClasses, wire, LeaseTime};

// re-export wire Ddns since it doesn't need to be modified (yet)
pub use wire::v4::ddns::Ddns;

pub const DEFAULT_LEASE_TIME: Duration = Duration::from_secs(86_400);

/// server config for dhcpv4
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// interfaces that are either explicitly bound by the config or
    /// are up & ipv4
    interfaces: Vec<NetworkInterface>,
    chaddr_only: bool,
    bootp_enable: bool,
    rapid_commit: bool,
    flood_threshold: Option<FloodThreshold>,
    cache_threshold: Option<u32>,
    /// used to make a selection on which network or subnet to use
    networks: HashMap<Ipv4Net, Network>,
    v6: Option<crate::v6::Config>,
    client_classes: Option<ClientClasses>,
    ddns: Option<Ddns>,
}

impl TryFrom<wire::Config> for Config {
    type Error = anyhow::Error;
    fn try_from(cfg: wire::Config) -> Result<Self> {
        let interfaces = crate::v4_find_interfaces(cfg.interfaces.clone())?;

        debug!(?interfaces, "using v4 interfaces");
        // transform wire::Config into a more optimized format
        let networks = cfg
            .networks
            .into_iter()
            .map(|(subnet, net)| {
                let wire::v4::Net {
                    ranges,
                    reservations,
                    ping_check,
                    probation_period,
                    authoritative,
                    server_id,
                    ping_timeout_ms,
                    server_name,
                    file_name,
                } = net;

                let ranges = ranges.into_iter().map(|range| range.into()).collect();
                let reserved_macs = reservations
                    .iter()
                    .filter_map(|res| match &res.condition {
                        wire::v4::Condition::Mac(mac) => Some((*mac, res.into())),
                        _ => None,
                    })
                    .collect();
                let reserved_opts = reservations
                    .iter()
                    .filter_map(|res| {
                        match &res.condition {
                            wire::v4::Condition::Options(match_opts) => {
                                // TODO: we only support matching on a single option currently.
                                // A reservation can match on chaddr OR a single option value.
                                match match_opts.values.0.iter().next() {
                                    Some((code, opt)) => Some((*code, (opt.clone(), res.into()))),
                                    _ => None,
                                }
                            }
                            _ => None,
                        }
                    })
                    .collect();
                let network = Network {
                    server_id,
                    subnet,
                    ping_check,
                    probation_period: Duration::from_secs(probation_period),
                    ranges,
                    reserved_macs,
                    reserved_opts,
                    authoritative,
                    ping_timeout_ms: Duration::from_millis(ping_timeout_ms),
                    server_name,
                    file_name,
                };
                // set total addr space for metrics
                dora_core::metrics::TOTAL_AVAILABLE_ADDRS.set(network.total_addrs() as i64);
                (subnet, network)
            })
            .collect();

        Ok(Self {
            interfaces,
            networks,
            chaddr_only: cfg.chaddr_only,
            bootp_enable: cfg.bootp_enable,
            rapid_commit: cfg.rapid_commit,
            flood_threshold: cfg.flood_protection_threshold.map(|f| FloodThreshold {
                packets: f.packets.get(),
                period: Duration::from_secs(f.secs.get() as u64),
            }),
            // error if threshold exists and > 100
            cache_threshold: {
                let threshold = cfg.cache_threshold;
                if threshold > 100 {
                    Some(Err(anyhow::anyhow!(
                        "cache_threshold must be between 0 and 100"
                    )))
                } else if threshold == 0 {
                    None
                } else {
                    Some(Ok(threshold.clamp(0, 100)))
                }
            }
            .transpose()?,
            v6: cfg
                .v6
                .map(crate::v6::Config::try_from)
                .transpose()
                .context("unable to parse v6 config")?,
            client_classes: cfg
                .client_classes
                .map(ClientClasses::try_from)
                .transpose()
                .context("unable to parse client_classes config")?,
            ddns: cfg.ddns,
        })
    }
}

impl Config {
    pub fn ddns(&self) -> Option<&Ddns> {
        self.ddns.as_ref()
    }
    pub fn v6(&self) -> Option<&crate::v6::Config> {
        self.v6.as_ref()
    }
    /// return the flood threshold config
    pub fn flood_threshold(&self) -> Option<FloodThreshold> {
        self.flood_threshold.clone()
    }
    /// return the renew threshold config
    pub fn cache_threshold(&self) -> Option<u32> {
        self.cache_threshold
    }
    /// eval all client classes, return names of classes that evaluate to true
    pub fn eval_client_classes(
        &self,
        req: &dhcproto::v4::Message,
        pkt: PacketDetails,
    ) -> Option<Result<Vec<String>>> {
        self.client_classes
            .as_ref()
            .map(|classes| classes.eval(req, pkt, self.bootp_enabled()))
    }
    pub fn classes(&self) -> Option<&ClientClasses> {
        self.client_classes.as_ref()
    }
    /// Returns:
    ///     - `server_id` of `Network` belonging to `ip`
    ///     - OR interface at index `iface`
    pub fn server_id(&self, iface: u32, ip: Ipv4Addr) -> Option<Ipv4Addr> {
        self.network(ip)
            .and_then(|net| net.server_id)
            .or_else(|| self.find_network(iface).map(|i| i.ip()))
    }

    /// return the optional explicitly bound interfaces if there are any
    pub fn interfaces(&self) -> &[NetworkInterface] {
        self.interfaces.as_slice()
    }
    /// Returns:
    ///     - if the config has an interface, return that
    ///     - OR find iface_index and return that
    ///     - OR use default interface
    pub fn find_network(&self, iface_index: u32) -> Option<Ipv4Network> {
        self.find_interface(iface_index).and_then(|int| {
            int.ips.iter().find_map(|ip| match ip {
                IpNetwork::V4(ip) => Some(*ip),
                _ => None,
            })
        })
    }

    /// find the interface at the index `iface_index`
    pub fn find_interface(&self, iface_index: u32) -> Option<&NetworkInterface> {
        self.interfaces.iter().find(|e| e.index == iface_index)
    }

    /// Whether the server is configured to use `chaddr` only or look at Client ID
    pub fn chaddr_only(&self) -> bool {
        self.chaddr_only
    }

    /// Whether the server is configured to use bootp
    pub fn bootp_enabled(&self) -> bool {
        self.bootp_enable
    }

    /// Whether the server is configured to use bootp
    pub fn rapid_commit(&self) -> bool {
        self.rapid_commit
    }

    /// If opt 61 (client id) exists return that, otherwise return `chaddr` from the message
    /// header.
    pub fn client_id<'a>(&self, msg: &'a Message) -> &'a [u8] {
        if self.chaddr_only {
            msg.chaddr()
        } else if let Some(DhcpOption::ClientIdentifier(id)) =
            msg.opts().get(OptionCode::ClientIdentifier)
        {
            id
        } else {
            msg.chaddr()
        }
    }

    /// get a `Network` with a subnet that contains the given IP
    pub fn network<I: Into<Ipv4Addr>>(&self, subnet: I) -> Option<&Network> {
        let contains = subnet.into();
        self.networks.iter().find_map(|(network_subnet, network)| {
            if network_subnet.contains(&contains) {
                Some(network)
            } else {
                None
            }
        })
    }

    /// given a list of matched classes and a range
    /// return all options merged for parameter request list
    pub fn collect_opts(
        &self,
        opts: &dhcproto::v4::DhcpOptions,
        matched_classes: Option<&[String]>,
    ) -> dhcproto::v4::DhcpOptions {
        // TODO: there may be a way to reduce the use of `clone` here
        // maybe by providing the config to MsgContext
        // in reality, we only need to clone the messages that actually match the param request list
        self.client_classes
            .as_ref()
            // range opts
            .map(|classes| merge_opts(opts.clone(), classes.collect_opts(matched_classes)))
            .unwrap_or_else(|| opts.clone())
    }

    /// get a `NetRange` within a subnet that contains the given IP & any matching client classes
    pub fn range<I: Into<Ipv4Addr>>(
        &self,
        subnet: I,
        ip: I,
        classes: Option<&[String]>,
    ) -> Option<&NetRange> {
        let subnet = subnet.into();
        let ip = ip.into();
        self.network(subnet).and_then(|net| net.range(ip, classes))
    }

    /// get the first `Network`
    pub fn get_first(&self) -> Option<(&Ipv4Net, &Network)> {
        self.networks.iter().next()
    }
    /// Create a new DhcpConfig for the server. Pass in the wire
    /// config format from yaml
    pub fn yaml<S: AsRef<str>>(input: S) -> Result<Self> {
        Self::try_from(serde_yaml::from_str::<wire::Config>(input.as_ref())?)
    }
    /// Create a new DhcpConfig for the server. Pass in the wire
    /// config format from json
    pub fn json<S: AsRef<str>>(input: S) -> Result<Self> {
        Self::try_from(serde_json::from_str::<wire::Config>(input.as_ref())?)
    }
    /// Create a new DhcpConfig for the server. Attempts to decode path
    /// as json, then yaml, and if both fail will return Err
    pub fn new<S: AsRef<str>>(input: S) -> Result<Self> {
        match Self::json(input.as_ref()) {
            Ok(r) => Ok(r),
            Err(_err) => Self::yaml(input.as_ref()),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Network {
    /// optional server id that will be used when talking with clients on this network
    server_id: Option<Ipv4Addr>,
    /// the subnet that this network owns, all ranges be within this subnet
    subnet: Ipv4Net,
    /// A list of ranges available on the network
    ranges: Vec<NetRange>,
    /// Reserved addresses based on MAC
    reserved_macs: HashMap<MacAddr, Reserved>,
    /// Reserved addresses based on opt
    /// Currently only support matching on a single option
    reserved_opts: HashMap<OptionCode, (DhcpOption, Reserved)>,
    /// Will send an ICMP echo request to an IP before OFFER
    /// Should this be a global configuration?
    ping_check: bool,
    ping_timeout_ms: Duration,
    /// how long a DECLINE or ping check will be put on probation for
    probation_period: Duration,
    /// with authoritative == true then dora will always try to respond
    /// to REQUEST/INFORM
    authoritative: bool,
    server_name: Option<String>,
    file_name: Option<String>,
}

impl Network {
    pub fn set_subnet(&mut self, subnet: Ipv4Net) -> &mut Self {
        self.subnet = subnet;
        self
    }
    pub fn set_ranges(&mut self, ranges: Vec<NetRange>) -> &mut Self {
        self.ranges = ranges;
        self
    }
    pub fn set_ping_check(&mut self, ping_check: bool) -> &mut Self {
        self.ping_check = ping_check;
        self
    }
    pub fn set_authoritative(&mut self, authoritative: bool) -> &mut Self {
        self.authoritative = authoritative;
        self
    }
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
    }
    pub fn file_name(&self) -> Option<&str> {
        self.file_name.as_deref()
    }
    pub fn subnet(&self) -> Ipv4Addr {
        self.subnet.network()
    }
    pub fn authoritative(&self) -> bool {
        self.authoritative
    }
    pub fn ranges(&self) -> &[NetRange] {
        &self.ranges
    }
    pub fn ranges_with_class<'a, 'b: 'a>(
        &'a self,
        classes: Option<&'b [String]>,
    ) -> impl Iterator<Item = &'a NetRange> + 'a {
        // TODO: don't allocate returning Vec?

        self.ranges
            .iter()
            .filter(move |range| range.match_class(classes))
    }
    /// get reservation based on mac & matched client classes
    pub fn get_reserved_mac(&self, mac: MacAddr, classes: Option<&[String]>) -> Option<&Reserved> {
        let res = self.reserved_macs.get(&mac)?;
        if res.match_class(classes) {
            Some(res)
        } else {
            None
        }
    }
    /// Based on a `DhcpOption`, find if there is a reservation where
    /// the value matches
    pub fn get_reserved_opt(&self, opt: &DhcpOption) -> Option<&Reserved> {
        match self.reserved_opts.get(&opt.into()) {
            Some((val, res)) if val == opt => Some(res),
            _ => None,
        }
    }
    /// Given some `opts`, search to see if there is a match with a reservation
    /// client classes must also match
    pub fn search_reserved_opt(
        &self,
        opts: &DhcpOptions,
        classes: Option<&[String]>,
    ) -> Option<&Reserved> {
        for (_, opt) in opts.iter() {
            if let Some(res) = self.get_reserved_opt(opt) {
                if res.match_class(classes) {
                    return Some(res);
                }
            }
        }
        None
    }
    /// Return `true` if ip is in a range for a given `network`, `false` otherwise
    pub fn in_range<I: Into<Ipv4Addr>>(&self, ip: I) -> bool {
        let ip = ip.into();
        self.ranges.iter().any(|r| r.contains(&ip))
    }

    /// Returns the range of which this `ip` is a member
    pub fn range<I: Into<Ipv4Addr>>(&self, ip: I, classes: Option<&[String]>) -> Option<&NetRange> {
        let ip = ip.into();
        // must not be present in `exclude` & must be present in `addrs`
        // if classes exist, look for a range that contains `ip` and matching class
        self.ranges.iter().find(|r| r.contains_class(&ip, classes))
    }
    /// is ping check enabled for this range? should we ping an IP before offering?
    pub fn ping_check(&self) -> bool {
        self.ping_check
    }
    /// get the ping timeout
    pub fn ping_timeout(&self) -> Duration {
        self.ping_timeout_ms
    }
    /// Returns the configured probation period for decline's received on this network
    pub fn probation_period(&self) -> Duration {
        self.probation_period
    }
    pub fn total_addrs(&self) -> usize {
        self.ranges.iter().map(|range| range.total_addrs()).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRange {
    addrs: RangeInclusive<Ipv4Addr>,
    /// default lease time for ips in this range,
    /// min/max specified in case the client requests
    /// a lease time
    lease: LeaseTime,
    opts: DhcpOptions,
    exclude: HashSet<Ipv4Addr>,
    class: Option<String>,
}

impl NetRange {
    pub fn new(addrs: RangeInclusive<Ipv4Addr>, lease: LeaseTime) -> Self {
        Self {
            addrs,
            lease,
            opts: DhcpOptions::default(),
            exclude: HashSet::default(),
            class: None,
        }
    }
    /// get the range of IPs this range offers
    pub fn addrs(&self) -> RangeInclusive<Ipv4Addr> {
        self.addrs.clone()
    }
    /// get the starting IP of the range
    pub fn start(&self) -> Ipv4Addr {
        *self.addrs.start()
    }
    /// get the ending IP of the range
    pub fn end(&self) -> Ipv4Addr {
        *self.addrs.end()
    }
    /// return the option parameters that should be included (if requested)
    pub fn opts(&self) -> &DhcpOptions {
        &self.opts
    }
    /// get the lease time
    pub fn lease(&self) -> LeaseTime {
        self.lease
    }
    /// returns true if the range contains a given IP
    pub fn contains(&self, ip: &Ipv4Addr) -> bool {
        !self.exclude.contains(ip) && self.addrs.contains(ip)
    }

    /// contains the IP and matches a class
    pub fn contains_class(&self, ip: &Ipv4Addr, classes: Option<&[String]>) -> bool {
        self.contains(ip) && self.match_class(classes)
    }
    pub fn match_class(&self, classes: Option<&[String]>) -> bool {
        // if range has no class, this expression is always true
        // if range has a class, it must match an entry in the list
        self.class
            .as_ref()
            .map(|name| {
                classes
                    .as_ref()
                    .map(|classes| classes.contains(name))
                    .unwrap_or(false)
            })
            .unwrap_or(true)
    }
    /// return an iterator over the range
    pub fn iter(&self) -> NetRangeIter<'_> {
        NetRangeIter {
            exclusions: &self.exclude,
            iter: Ipv4AddrRange::new(self.start(), self.end()),
        }
    }
    /// returns a set of excluded ipv4 addrs
    pub fn exclusions(&self) -> &HashSet<Ipv4Addr> {
        &self.exclude
    }
    /// count the total number of addresses that could possibly be
    /// handed out minus exclusions
    pub fn total_addrs(&self) -> usize {
        self.iter().count()
    }
    /// return configured class if present
    pub fn class(&self) -> Option<&str> {
        self.class.as_deref()
    }
}

#[derive(Debug)]
pub struct NetRangeIter<'a> {
    exclusions: &'a HashSet<Ipv4Addr>,
    iter: Ipv4AddrRange,
}

impl<'a> NetRangeIter<'a> {
    pub fn new(iter: Ipv4AddrRange, exclusions: &'a HashSet<Ipv4Addr>) -> Self {
        Self { iter, exclusions }
    }
}

impl<'a> Iterator for NetRangeIter<'a> {
    type Item = Ipv4Addr;

    // skips any IPs in exclusions
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let next = self.iter.next()?;
            if !self.exclusions.contains(&next) {
                return Some(next);
            }
        }
    }
    fn count(self) -> usize {
        self.iter.count() - self.exclusions.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reserved {
    /// The currently reserved IP
    ip: Ipv4Addr,
    /// default lease time for ips in this range,
    /// min/max specified in case the client requests
    /// a lease time
    lease: LeaseTime,
    opts: DhcpOptions,
    class: Option<String>,
}

impl Reserved {
    /// get the IP for this reservation
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }
    /// return the option parameters that should be included (if requested)
    pub fn opts(&self) -> &DhcpOptions {
        &self.opts
    }
    /// get the lease time. This value is here for convenience,
    /// it will also be set in the range options
    pub fn lease(&self) -> LeaseTime {
        self.lease
    }
    /// return configured class if present
    pub fn class(&self) -> Option<&str> {
        self.class.as_deref()
    }
    /// given a list of matched classes, determine if this reservation has a match
    ///         if reservation has no class, this expression is always true
    /// if reservation has a class, it must match an entry in the list
    pub fn match_class(&self, classes: Option<&[String]>) -> bool {
        self.class
            .as_ref()
            .map(|name| {
                classes
                    .as_ref()
                    .map(|classes| classes.contains(name))
                    .unwrap_or(false)
            })
            .unwrap_or(true)
    }
}

impl From<wire::v4::IpRange> for NetRange {
    fn from(range: wire::v4::IpRange) -> Self {
        let lease = range.config.lease_time.into();
        let opts = range.options.get();
        NetRange {
            addrs: range.range,
            opts,
            lease,
            exclude: range.except.into_iter().collect(),
            class: range.class,
        }
    }
}

impl From<&wire::v4::ReservedIp> for Reserved {
    fn from(res: &wire::v4::ReservedIp) -> Self {
        let lease = res.config.lease_time.into();
        Reserved {
            lease,
            ip: res.ip,
            opts: res.options.as_ref().clone(),
            class: res.class.clone(),
        }
    }
}

/// merge `b` into `a`, favoring `a` where there are duplicates
fn merge_opts(mut a: DhcpOptions, b: Option<DhcpOptions>) -> DhcpOptions {
    match b {
        None => a,
        Some(b) => {
            for (code, opt) in b.into_iter() {
                if a.get(code).is_none() {
                    a.insert(opt);
                }
            }
            a
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloodThreshold {
    packets: u32,
    period: Duration,
}

impl Default for FloodThreshold {
    fn default() -> Self {
        Self {
            packets: 6,
            period: Duration::from_secs(5),
        }
    }
}

impl FloodThreshold {
    pub fn new(packets: u32, period: Duration) -> Self {
        Self { packets, period }
    }
    pub fn packets(&self) -> u32 {
        self.packets
    }
    pub fn period(&self) -> Duration {
        self.period
    }
}

#[cfg(test)]
mod tests {

    use dora_core::dhcproto::v4;

    use super::*;

    pub static SAMPLE_YAML: &str = include_str!("../sample/config.yaml");
    pub static CIRC_YAML: &str = include_str!("../sample/circular_deps.yaml");

    // test we can decode from wire
    #[test]
    fn test_sample() {
        let cfg = Config::new(SAMPLE_YAML).unwrap();
        // test a range decoded properly
        let net = cfg.network([192, 168, 0, 1]).unwrap();
        assert_eq!(net.ranges()[0].start(), Ipv4Addr::from([192, 168, 0, 100]));
        assert_eq!(
            net.ranges()[0].opts().get(v4::OptionCode::Router),
            Some(&v4::DhcpOption::Router(vec![Ipv4Addr::from([
                192, 168, 0, 1
            ])]))
        );
    }

    #[test]
    fn test_circular() {
        let cfg = Config::new(CIRC_YAML);
        assert_eq!(
            format!("{}", cfg.err().unwrap().source().unwrap()),
            "cycle detected in dependency map"
        );
    }

    #[test]
    fn test_range_class() {
        let range = NetRange {
            addrs: Ipv4Addr::new(192, 168, 0, 1)..=Ipv4Addr::new(192, 168, 0, 100),
            lease: LeaseTime {
                default: Duration::from_secs(5),
                min: Duration::from_secs(3),
                max: Duration::from_secs(10),
            },
            opts: DhcpOptions::new(),
            exclude: HashSet::new(),
            class: Some("foo".to_owned()),
        };
        // class matches
        assert!(range.match_class(Some(&["foo".to_owned()])));
        // wrong class means no match
        assert!(!range.match_class(Some(&["bar".to_owned()])));
        // must have class to match
        assert!(!range.match_class(None));

        let range = NetRange {
            addrs: Ipv4Addr::new(192, 168, 0, 1)..=Ipv4Addr::new(192, 168, 0, 100),
            lease: LeaseTime {
                default: Duration::from_secs(5),
                min: Duration::from_secs(3),
                max: Duration::from_secs(10),
            },
            opts: DhcpOptions::new(),
            exclude: HashSet::new(),
            class: None,
        };
        // no classes to match -> true
        assert!(range.match_class(None));
        // matched classes -> true
        assert!(range.match_class(Some(&["bar".to_owned()])));
    }

    #[test]
    fn test_class_find() {
        // has class `my_class` set to assert "pkt4.mac == 0xDEADBEEF"
        let cfg = Config::new(SAMPLE_YAML).unwrap();
        // test a range decoded properly
        let mut msg = v4::Message::default();
        msg.set_chaddr(&hex::decode("DEADBEEF").unwrap());
        msg.opts_mut()
            .insert(v4::DhcpOption::MessageType(v4::MessageType::Discover));
        // get matching classes
        // TODO: what should we do if there is an error processing client classes?
        let matched = cfg
            .eval_client_classes(&msg, PacketDetails::default())
            .unwrap()
            .ok();
        assert_eq!(
            matched
                .as_deref()
                .unwrap()
                .iter()
                .collect::<std::collections::HashSet<_>>(),
            ["my_class".to_owned(), "ALL".to_owned()]
                .iter()
                .collect::<std::collections::HashSet<_>>()
        );
        let net = cfg
            .range([10, 0, 0, 1], [10, 0, 0, 100], matched.as_deref())
            .unwrap();

        assert_eq!(net.class, Some("my_class".to_owned()));

        // gets 'unclassified' second range
        let net = cfg.range([10, 0, 0, 1], [10, 0, 1, 100], None).unwrap();

        assert_eq!(net.class, None);

        // a_class matches on 'hostname'
        msg.opts_mut()
            .insert(DhcpOption::Hostname("hostname".to_owned()));

        let matched = cfg
            .eval_client_classes(&msg, PacketDetails::default())
            .unwrap()
            .ok();
        // if a_class matches, other classes using `member` will eval to true also
        assert_eq!(
            matched
                .unwrap()
                .into_iter()
                .collect::<std::collections::HashSet<_>>(),
            ["my_class", "a_class", "d_class", "b_class", "c_class", "ALL"]
                .into_iter()
                .map(|s| s.to_owned())
                .collect::<std::collections::HashSet<_>>()
        );
    }

    #[test]
    fn test_range_lease_time() {
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 0, 1)..=Ipv4Addr::new(192, 168, 0, 100),
            LeaseTime {
                default: Duration::from_secs(5),
                min: Duration::from_secs(3),
                max: Duration::from_secs(10),
            },
        );

        // selects max
        let (lease, renew, rebind) = range.lease().determine_lease(Some(Duration::from_secs(11)));
        assert_eq!(lease.as_secs(), 10);
        assert_eq!(renew.as_secs(), 5);
        assert_eq!(rebind.as_secs(), (10 * 7 / 8));
        // selects min
        let (lease, renew, rebind) = range.lease().determine_lease(Some(Duration::from_secs(2)));
        assert_eq!(lease.as_secs(), 3);
        assert_eq!(renew.as_secs(), 3 / 2);
        assert_eq!(rebind.as_secs(), (3 * 7 / 8));
        // select default
        let (lease, renew, rebind) = range.lease().determine_lease(None);
        assert_eq!(lease.as_secs(), 5);
        assert_eq!(renew.as_secs(), 5 / 2);
        assert_eq!(rebind.as_secs(), (5 * 7 / 8));
    }

    #[test]
    fn test_range_iter_exclude() {
        let range = NetRange {
            addrs: Ipv4Addr::new(192, 168, 0, 1)..=Ipv4Addr::new(192, 168, 0, 100),
            lease: LeaseTime {
                default: Duration::from_secs(5),
                min: Duration::from_secs(3),
                max: Duration::from_secs(10),
            },
            exclude: HashSet::from([
                [192, 168, 0, 1].into(),
                [192, 168, 0, 2].into(),
                [192, 168, 0, 3].into(),
                [192, 168, 0, 4].into(),
            ]),
            opts: DhcpOptions::default(),
            class: None,
        };
        // excluded causes us to skip 1-4
        assert!(range.iter().eq(Ipv4AddrRange::new(
            [192, 168, 0, 5].into(),
            [192, 168, 0, 100].into(),
        )));
        assert_eq!(range.total_addrs(), 100 - 4);
    }

    #[test]
    fn test_big_range() {
        let range = NetRange::new(
            Ipv4Addr::new(192, 168, 0, 0)..=Ipv4Addr::new(192, 168, 3, 255),
            LeaseTime {
                default: Duration::from_secs(5),
                min: Duration::from_secs(3),
                max: Duration::from_secs(10),
            },
        );
        assert_eq!(range.iter().count(), 256 * 4);
        assert_eq!(range.total_addrs(), 256 * 4);
    }

    #[test]
    fn test_reserved_opt() {
        let res = Reserved {
            ip: [192, 168, 0, 120].into(),
            lease: LeaseTime {
                default: Duration::from_secs(5),
                min: Duration::from_secs(3),
                max: Duration::from_secs(10),
            },
            opts: DhcpOptions::default(),
            class: None,
        };
        // another value just to make sure we select the right one
        let mut another = res.clone();
        another.ip = [192, 168, 0, 130].into();

        let mut reserved_opts = HashMap::new();
        reserved_opts.insert(
            OptionCode::DomainNameServer,
            (DhcpOption::DomainNameServer(vec![[8, 8, 8, 8].into()]), res),
        );
        reserved_opts.insert(
            OptionCode::NisDomain,
            (
                DhcpOption::NisDomain("testdomain.com.".to_string()),
                another,
            ),
        );
        let net = Network {
            subnet: "192.168.0.0/24".parse().unwrap(),
            reserved_opts,
            ..Default::default()
        };
        // now test we can match on 8.8.8.8
        let res = net
            .get_reserved_opt(&DhcpOption::DomainNameServer(vec![[8, 8, 8, 8].into()]))
            .unwrap();
        assert_eq!(res.ip, Ipv4Addr::new(192, 168, 0, 120));
    }
}
