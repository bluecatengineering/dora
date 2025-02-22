pub mod env;

pub mod builder;
#[allow(unused)]
pub mod client;

pub mod utils {
    use std::net::Ipv4Addr;

    use anyhow::{Result, bail};
    use dora_core::dhcproto::v4;
    use mac_address::MacAddress;

    pub fn get_mac() -> MacAddress {
        mac_address::get_mac_address()
            .expect("unable to get MAC addr")
            .unwrap()
    }

    pub fn rand_mac() -> MacAddress {
        let mut mac = [0; 6];
        for b in &mut mac {
            *b = rand::random::<u8>();
        }
        MacAddress::new(mac)
    }

    pub fn get_sident(msg: &v4::Message) -> Result<Ipv4Addr> {
        if let Some(v4::DhcpOption::ServerIdentifier(ip)) =
            msg.opts().get(v4::OptionCode::ServerIdentifier)
        {
            Ok(*ip)
        } else {
            bail!("unreachable")
        }
    }

    pub fn default_request_list() -> Vec<v4::OptionCode> {
        vec![
            v4::OptionCode::SubnetMask,
            v4::OptionCode::Router,
            v4::OptionCode::DomainNameServer,
            v4::OptionCode::DomainName,
        ]
    }
}
