mod common;

use std::net::Ipv4Addr;

use anyhow::Result;
use common::{builder::*, client::Client, env::DhcpServerEnv};
use dora_core::{dhcproto::v4, prelude::MacAddr, tracing};
use tracing_test::traced_test;

use crate::common::utils;

// these tests currently unicast all exchanges on a non-standard port. It tests
// the logic of acquiring an IP, but not the broadcast send logic. There's likely
// a better way to do this so we can test both.

#[test]
#[traced_test]
/// runs through a standard Discover Offer Request Ack,
/// then resends Request to renew the lease
fn test_basic_dhcpv4_unicast() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);

    // create REQUEST & send
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    // renew
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    // pedantic drop
    drop(_srv);
    Ok(())
}

/// standard negotiation but with a chaddr present in the reserved space
#[test]
#[traced_test]
fn static_chaddr_dora() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;
    let chaddr = "aa:bb:cc:dd:ee:ff".parse::<MacAddr>()?.octets();

    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    // this is the addr the mac is configured to have
    assert_eq!(resp.yiaddr(), "192.168.2.170".parse::<Ipv4Addr>()?);

    // create REQUEST & send
    let sident = utils::get_sident(&resp)?;
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .chaddr(chaddr)
        .sident(sident)
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    Ok(())
}

/// standard negotiation but with matching on an option value for reserved IP
#[test]
#[traced_test]
fn static_opt_dora() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;

    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opts([v4::DhcpOption::ClientIdentifier(
            [0, 17, 34, 51, 68, 85].to_vec(), // hex encode of 001122334455
        )])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);

    // this is the addr the clientid is configured to have
    assert_eq!(resp.yiaddr(), "192.168.2.160".parse::<Ipv4Addr>()?);

    // create REQUEST & send
    let sident = utils::get_sident(&resp)?;
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .sident(sident)
        .opts([v4::DhcpOption::ClientIdentifier(
            [0, 17, 34, 51, 68, 85].to_vec(),
        )])
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    Ok(())
}

/// During Discover, fill the Requested IP option, this IP is
/// within a range in the config
#[test]
#[traced_test]
fn discover_req_addr() -> Result<()> {
    let chaddr = utils::rand_mac();
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;

    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg with a requested IP
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_addr([192, 168, 2, 140])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    assert_eq!(resp.yiaddr(), "192.168.2.140".parse::<Ipv4Addr>()?);

    // create REQUEST & send
    let sident = utils::get_sident(&resp)?;
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .sident(sident)
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    Ok(())
}

/// When we get back an Offer, send a Request for an IP different than one we got
#[test]
#[traced_test]
fn request_nak() -> Result<()> {
    let chaddr = utils::rand_mac();
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;

    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg with a requested IP
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    // reply but with the wrong addr
    let sident = utils::get_sident(&resp)?;
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .yiaddr([192, 168, 2, 130])
        .sident(sident)
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Nak);
    Ok(())
}

/// send a Discover with a lease time which is within the mix/max of
/// of the lease time for a range
/// the min/max functionality is handled by a unit test
#[test]
#[traced_test]
fn requested_lease_time() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        // request a lease time of 4500s
        .lease_time(4500_u32)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    assert_eq!(
        resp.opts().get(v4::OptionCode::AddressLeaseTime).unwrap(),
        &v4::DhcpOption::AddressLeaseTime(4500)
    );

    // create REQUEST & send
    let sident = utils::get_sident(&resp)?;
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .sident(sident)
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    // renew
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .sident(sident)
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    Ok(())
}

/// specified range has NISDomain option configured, include that optcode
/// as part of the parameter request list, then check it's value matches
#[test]
#[traced_test]
fn test_requested_opts() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_list([v4::OptionCode::NISDomain])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    assert_eq!(
        resp.opts().get(v4::OptionCode::NISDomain).unwrap(),
        &v4::DhcpOption::NISDomain("testdomain.com".to_string())
    );
    Ok(())
}

/// request an IP that is found in an exclusion, currently this just
/// gives the next available IP
#[test]
#[traced_test]
fn test_exclusions() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg with a requested IP
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_addr([192, 168, 2, 123])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    // assert that we didn't get 192.168.0.123
    assert_ne!(resp.yiaddr(), "192.168.2.123".parse::<Ipv4Addr>()?);
    assert_ne!(resp.yiaddr(), "0.0.0.0".parse::<Ipv4Addr>()?);
    Ok(())
}

/// DISCOVER IP x
/// REQUEST IP x
/// receive ACK
/// send DECLINE for IP x
/// different chaddr attempts to request DECLINED IP -> OFFER x+1
/// original chaddr sends DISCOVER -> OFFER x+2
#[test]
#[traced_test]
fn test_decline() -> Result<()> {
    let chaddr = utils::rand_mac();
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9901_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);

    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    let yiaddr = resp.yiaddr();
    // create REQUEST & send
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .chaddr(chaddr)
        .opt_req_addr(yiaddr)
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    // GET ACK
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    // create Decline & send
    let msg_args = DeclineBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(yiaddr)
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Decline(msg_args));
    assert!(resp.is_err()); // no response to decline

    // create DISCOVER msg with a requested IP
    // from a new client
    let new_chaddr = utils::rand_mac();
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_addr(yiaddr)
        .chaddr(new_chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;
    // requesting yiaddr will Offer a different addr
    assert!(resp.opts().has_msg_type(v4::MessageType::Offer));
    assert_ne!(resp.yiaddr(), yiaddr);

    // create DISCOVER msg & send from original chaddr
    // should return an IP that's not on probation
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;
    assert_ne!(resp.yiaddr(), yiaddr);
    Ok(())
}
