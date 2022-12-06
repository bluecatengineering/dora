mod common;

use std::{
    env,
    process::{Command, Stdio},
};

// #[test]
// fn basic() {
//     let server_path = env::var("WORKSPACE_ROOT").unwrap_or_else(|_| "..".to_owned());
//     println!("using server src path: {}", server_path);

//     let mut command = Command::new(&format!("{}/target/debug/dora", server_path));
//     command
//         .stdout(Stdio::piped())
//         .env("DORA_LOG", "debug")
//         .arg("-d=basic.db")
//         .arg(&format!(
//             "--config-path={}/tests/test_configs/basic.yaml",
//             server_path
//         ))
//         .arg("--threads=2")
//         .arg(&format!("--v4-addr={}", "0.0.0.0:67"));

//     println!("named cli options: {command:#?}", command = command);

//     let mut named = command.spawn().expect("failed to start named");
//     println!("dora started");
// }

use anyhow::Result;
use common::{builder::*, client::Client, env::DhcpServerEnv};
use dora_core::{dhcproto::v4, tracing};
use tracing_test::traced_test;

#[test]
#[traced_test]
/// runs through a standard Discover Offer Request Ack,
/// then resends Request to renew the lease
fn test_basic_dhcpv4() -> Result<()> {
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
