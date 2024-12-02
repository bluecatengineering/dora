mod common;

use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
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
        .port(9900_u16)
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

#[test]
#[traced_test]
/// runs through a standard Discover Offer Request Ack,
/// then resends Request to renew the lease
fn test_rapid_commit() -> Result<()> {
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
        .port(9900_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opts(vec![v4::DhcpOption::RapidCommit])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

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
        .port(9900_u16)
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

/// send a BOOTP message
#[test]
#[traced_test]
fn static_bootp() -> Result<()> {
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
        .port(9900_u16)
        .build()?;
    let chaddr = "bb:bb:cc:dd:ee:ff".parse::<MacAddr>()?.octets();

    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create BOOTP msg & send
    let msg_args = BootPBuilder::default()
        .giaddr([192, 168, 2, 1])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::BootP(msg_args))?;

    assert_eq!(resp.opts().msg_type(), None);
    // this is the addr the mac is configured to have
    assert_eq!(resp.yiaddr(), "192.168.2.165".parse::<Ipv4Addr>()?);

    Ok(())
}

/// send a BOOTP message
#[test]
#[traced_test]
fn dynamic_bootp() -> Result<()> {
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
        .port(9900_u16)
        .build()?;

    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create BOOTP msg & send
    let resp = client.run(MsgType::BootP(
        BootPBuilder::default().giaddr([192, 168, 2, 1]).build()?,
    ))?;

    assert_eq!(resp.opts().msg_type(), None);
    assert!(resp.yiaddr().is_private());

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
        .port(9900_u16)
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
    dbg!("fooab");
    let chaddr = utils::rand_mac();
    let _srv = DhcpServerEnv::start(
        "basic.yaml",
        "basic.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    dbg!(&chaddr);
    dbg!("foo");
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
        .build()?;
    tracing::info!("here");
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg with a requested IP
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_addr([192, 168, 2, 140])
        .chaddr(chaddr)
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;
    tracing::info!("here");

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    assert_eq!(resp.yiaddr(), "192.168.2.140".parse::<Ipv4Addr>()?);
    tracing::info!("here");

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
        .port(9900_u16)
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
        .port(9900_u16)
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
        .port(9900_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_list([v4::OptionCode::NisDomain])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    assert_eq!(
        resp.opts().get(v4::OptionCode::NisDomain).unwrap(),
        &v4::DhcpOption::NisDomain("testdomain.com".to_string())
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
        .port(9900_u16)
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
        .port(9900_u16)
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

/// send a Discover with vendor id "android-dhcp-13"
/// we've got a class that has 1 range with this class guarding it,
/// an assertion on substring(option[60], 0, 7) == 'android'
#[test]
#[traced_test]
fn test_vendor_class() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "classes.yaml",
        "classes.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_list([v4::OptionCode::VendorExtensions])
        .opts([v4::DhcpOption::ClassIdentifier(b"android-dhcp-13".to_vec())])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    let v4::DhcpOption::VendorExtensions(vendor_ext) =
        resp.opts().get(v4::OptionCode::VendorExtensions).unwrap()
    else {
        bail!("vendor extensions not present");
    };
    // classes.yaml adds [1,2,3,4] as a Vec<u32>, translated into
    let ret = vec![0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4];
    assert_eq!(vendor_ext, &ret);
    Ok(())
}

/// send a Discover with vendor id "docsis3.0"
/// we've got a class that has 1 range with this class guarding it,
#[test]
#[traced_test]
fn test_vendor_class_builtin() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "vendor.yaml",
        "vendor.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_list([v4::OptionCode::VendorExtensions])
        .opts([v4::DhcpOption::ClassIdentifier(b"docsis3.0".to_vec())])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args))?;

    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Offer);
    let v4::DhcpOption::VendorExtensions(vendor_ext) =
        resp.opts().get(v4::OptionCode::VendorExtensions).unwrap()
    else {
        bail!("vendor extensions not present");
    };
    // classes.yaml adds [1,2,3,4] as a Vec<u32>, translated into
    let ret = vec![0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4];
    assert_eq!(vendor_ext, &ret);
    Ok(())
}

/// send a Discover with vendor class "foobar" that uses the DROP builtin
/// we've got a class that has 1 range with this class guarding it,
#[test]
#[traced_test]
fn test_vendor_class_drop() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "vendor.yaml",
        "vendor.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_list([v4::OptionCode::VendorExtensions])
        .opts([v4::DhcpOption::ClassIdentifier(b"foobar".to_vec())])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args));

    assert!(resp.is_err());
    Ok(())
}

/// send a Discover with vendor id "iphone-dhcp-13"
/// we've got a class that has 1 range with this class guarding it,
/// an assertion on substring(option[60], 0, 7) == 'android'
#[test]
#[traced_test]
fn test_vendor_class_not_match() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "classes.yaml",
        "classes.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
        .build()?;
    // create a client that sends dhcpv4 messages
    let mut client = Client::<v4::Message>::new(settings);
    // create DISCOVER msg & send
    let msg_args = DiscoverBuilder::default()
        .giaddr([192, 168, 2, 1])
        .req_list([v4::OptionCode::VendorExtensions])
        .opts([v4::DhcpOption::ClassIdentifier(b"iphone-dhcp-13".to_vec())])
        .build()?;
    let resp = client.run(MsgType::Discover(msg_args));
    // iphone-dhcp-13 doesn't match the configured vendor id
    assert!(resp.is_err());
    Ok(())
}

/// flood_protection_threshold set for 2 packets in 5 seconds
#[test]
#[traced_test]
fn test_flood_threshold() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "threshold.yaml",
        "threshold.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
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
    let resp = client.run(MsgType::Request(msg_args));
    assert!(resp.is_err());

    // pedantic drop
    drop(_srv);
    Ok(())
}

/// test cache threshold
#[test]
#[traced_test]
fn test_cache_threshold() -> Result<()> {
    let _srv = DhcpServerEnv::start(
        "cache_threshold.yaml",
        "cache_threshold.db",
        "dora_test",
        "dhcpcli",
        "dhcpsrv",
        "192.168.2.1",
    );
    // use veth_cli created in start()
    let settings = ClientSettingsBuilder::default()
        .iface_name("dhcpcli")
        .target("192.168.2.1".parse::<std::net::IpAddr>().unwrap())
        .port(9900_u16)
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
    let now = Instant::now();

    let resp = client.run(MsgType::Request(msg_args))?;
    let Some(v4::DhcpOption::AddressLeaseTime(lease_time_a)) =
        resp.opts().get(v4::OptionCode::AddressLeaseTime)
    else {
        bail!("invalid option")
    };
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);

    // sleep for 1s then get a renew, should use same lease
    std::thread::sleep(Duration::from_secs(2));
    // renew
    let msg_args = RequestBuilder::default()
        .giaddr([192, 168, 2, 1])
        .opt_req_addr(resp.yiaddr())
        .build()?;
    let resp = client.run(MsgType::Request(msg_args))?;
    let Some(v4::DhcpOption::AddressLeaseTime(lease_time_b)) =
        resp.opts().get(v4::OptionCode::AddressLeaseTime)
    else {
        bail!("invalid option")
    };
    assert_eq!(resp.opts().msg_type().unwrap(), v4::MessageType::Ack);
    // round off to the second and compare time left on lease, should equal the original lease
    // i.e. we got the original lease back
    assert_eq!(
        *lease_time_b,
        *lease_time_a - dbg!(now.elapsed().as_secs_f32()).round() as u32
    );

    // pedantic drop
    drop(_srv);
    Ok(())
}
