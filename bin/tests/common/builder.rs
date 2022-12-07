use std::net::{IpAddr, Ipv4Addr};

use derive_builder::Builder;
use dora_core::dhcproto::v4;
use mac_address::MacAddress;

use super::utils;

pub const SEND_COUNT: usize = 2;

#[derive(Builder, Debug, Clone)]
#[builder(setter(into))]
#[builder(field(private))]
pub struct ClientSettings {
    /// ip address to send to [default: 0.0.0.0]
    #[builder(default = "IpAddr::V4(Ipv4Addr::UNSPECIFIED)")]
    pub target: IpAddr,
    /// which port use. [default: 67]
    #[builder(default = "67")]
    pub port: u16,
    /// query timeout in ms [default: 500]
    #[builder(default = "default_timeout()")]
    pub timeout: u64,
    /// default # send retries [default: 2]
    #[builder(default = "default_send_count()")]
    pub send_retries: usize,
    #[builder(setter(into, strip_option))]
    pub iface_name: Option<String>,
}

fn default_timeout() -> u64 {
    500
}

fn default_send_count() -> usize {
    SEND_COUNT
}

#[derive(Debug, Clone)]
pub enum MsgType {
    Discover(Discover),
    Request(Request),
    Decline(Decline),
}

/// Send a DISCOVER msg
#[derive(Builder, PartialEq, Eq, Debug, Clone)]
#[builder(setter(into), field(private))]
pub struct Discover {
    /// supply a mac address for DHCPv4 [default: first avail mac]
    #[builder(default = "utils::get_mac()")]
    chaddr: MacAddress,
    /// request specific ip [default: None]
    #[builder(setter(strip_option), default)]
    req_addr: Option<Ipv4Addr>,
    /// giaddr is the Relay Agent IP [default: None]
    #[builder(setter(strip_option), default)]
    giaddr: Option<Ipv4Addr>,
    /// populate opt 51 [default: None]
    #[builder(setter(strip_option), default)]
    lease_time: Option<u32>,
    /// parameter request list [default: 1,3,6,15]
    #[builder(default = "utils::default_request_list()")]
    req_list: Vec<v4::OptionCode>,
    /// Add additional opts in message [default: None]
    #[builder(setter(strip_option), default)]
    opts: Vec<v4::DhcpOption>,
}

impl Discover {
    pub fn build(&self, broadcast: bool) -> v4::Message {
        let mut msg = v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            self.giaddr.unwrap_or(Ipv4Addr::UNSPECIFIED),
            &self.chaddr.bytes(),
        );

        if broadcast {
            msg.set_flags(v4::Flags::default().set_broadcast());
        }
        msg.opts_mut()
            .insert(v4::DhcpOption::MessageType(v4::MessageType::Discover));
        msg.opts_mut().insert(v4::DhcpOption::ClientIdentifier(
            self.chaddr.bytes().to_vec(),
        ));

        msg.opts_mut()
            .insert(v4::DhcpOption::ParameterRequestList(self.req_list.clone()));

        if let Some(t) = self.lease_time {
            msg.opts_mut().insert(v4::DhcpOption::AddressLeaseTime(t));
        }
        for opt in &self.opts {
            msg.opts_mut().insert(opt.clone());
        }
        // TODO: add more?
        // add requested ip
        if let Some(ip) = self.req_addr {
            msg.opts_mut()
                .insert(v4::DhcpOption::RequestedIpAddress(ip));
        }
        msg
    }
}

/// Send a REQUEST msg
#[derive(Builder, PartialEq, Eq, Debug, Clone)]
#[builder(setter(into), field(private))]
pub struct Request {
    /// supply a mac address for DHCPv4 [default: first avail mac]
    #[builder(default = "utils::get_mac()")]
    chaddr: MacAddress,
    /// address for client [default: None]
    #[builder(setter(strip_option), default)]
    yiaddr: Option<Ipv4Addr>,
    /// server identifier [default: None]
    #[builder(setter(strip_option), default)]
    sident: Option<Ipv4Addr>,
    /// giaddr is the Relay Agent IP [default: None]
    #[builder(setter(strip_option), default)]
    giaddr: Option<Ipv4Addr>,
    /// specify dhcp option for requesting ip [default: None]
    #[builder(setter(strip_option), default)]
    opt_req_addr: Option<Ipv4Addr>,
    /// parameter request list [default: 1,3,6,15]
    #[builder(default = "utils::default_request_list()")]
    req_list: Vec<v4::OptionCode>,
    /// Add additional opts in message [default: None]
    #[builder(setter(strip_option), default)]
    opts: Vec<v4::DhcpOption>,
}

impl Request {
    pub fn build(&self) -> v4::Message {
        let mut msg = v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            self.yiaddr.unwrap_or(Ipv4Addr::UNSPECIFIED),
            Ipv4Addr::UNSPECIFIED,
            self.giaddr.unwrap_or(Ipv4Addr::UNSPECIFIED),
            &self.chaddr.bytes(),
        );

        msg.opts_mut()
            .insert(v4::DhcpOption::MessageType(v4::MessageType::Request));
        msg.opts_mut().insert(v4::DhcpOption::ClientIdentifier(
            self.chaddr.bytes().to_vec(),
        ));
        // add parameter request lists
        msg.opts_mut()
            .insert(v4::DhcpOption::ParameterRequestList(self.req_list.clone()));
        // add requested ip
        if let Some(ip) = self.opt_req_addr {
            msg.opts_mut()
                .insert(v4::DhcpOption::RequestedIpAddress(ip));
        }
        for opt in &self.opts {
            msg.opts_mut().insert(opt.clone());
        }
        if let Some(ip) = self.sident {
            msg.opts_mut().insert(v4::DhcpOption::ServerIdentifier(ip));
        }
        msg
    }
}

/// Send a DECLINE msg
#[derive(Builder, PartialEq, Eq, Debug, Clone)]
#[builder(setter(into), field(private))]
pub struct Decline {
    /// supply a mac address for DHCPv4 [default: first avail mac]
    #[builder(default = "utils::get_mac()")]
    chaddr: MacAddress,
    /// address for client [default: None]
    #[builder(setter(strip_option), default)]
    yiaddr: Option<Ipv4Addr>,
    /// server identifier [default: None]
    #[builder(setter(strip_option), default)]
    sident: Option<Ipv4Addr>,
    /// giaddr is the Relay Agent IP [default: None]
    #[builder(setter(strip_option), default)]
    giaddr: Option<Ipv4Addr>,
    /// specify dhcp option for requesting ip [default: None]
    #[builder(setter(strip_option), default)]
    opt_req_addr: Option<Ipv4Addr>,
    /// parameter request list [default: None]
    #[builder(default = "Vec::new()")]
    req_list: Vec<v4::OptionCode>,
    /// Add additional opts in message [default: None]
    #[builder(setter(strip_option), default)]
    opts: Vec<v4::DhcpOption>,
}

impl Decline {
    pub fn build(&self) -> v4::Message {
        let mut msg = v4::Message::new(
            Ipv4Addr::UNSPECIFIED,
            self.yiaddr.unwrap_or(Ipv4Addr::UNSPECIFIED),
            Ipv4Addr::UNSPECIFIED,
            self.giaddr.unwrap_or(Ipv4Addr::UNSPECIFIED),
            &self.chaddr.bytes(),
        );

        msg.opts_mut()
            .insert(v4::DhcpOption::MessageType(v4::MessageType::Decline));
        msg.opts_mut().insert(v4::DhcpOption::ClientIdentifier(
            self.chaddr.bytes().to_vec(),
        ));
        // add parameter request lists
        msg.opts_mut()
            .insert(v4::DhcpOption::ParameterRequestList(self.req_list.clone()));
        // add requested ip
        if let Some(ip) = self.opt_req_addr {
            msg.opts_mut()
                .insert(v4::DhcpOption::RequestedIpAddress(ip));
        }
        for opt in &self.opts {
            msg.opts_mut().insert(opt.clone());
        }
        if let Some(ip) = self.sident {
            msg.opts_mut().insert(v4::DhcpOption::ServerIdentifier(ip));
        }
        msg
    }
}
