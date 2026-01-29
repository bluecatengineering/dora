use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rand::{self, RngCore};
use serde::{Deserialize, Serialize};
use tracing::debug;
use wire::v6::ServerDuidInfo;

pub mod client_classes;
pub mod v4;
pub mod v6;
pub mod wire;

use dora_core::dhcproto::v6::duid::Duid;
use dora_core::pnet::{
    self,
    datalink::NetworkInterface,
    ipnetwork::{IpNetwork, Ipv4Network},
};

/// server config
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DhcpConfig {
    v4: v4::Config,
    path: Option<PathBuf>,
}

impl DhcpConfig {
    pub fn v4(&self) -> &v4::Config {
        &self.v4
    }
    pub fn has_v6(&self) -> bool {
        self.v4.v6().is_some()
    }
    pub fn v6(&self) -> &v6::Config {
        self.v4.v6().unwrap() // v6 existence checked before starting plugins
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// server instance config
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvConfig {
    pub customer_id: String,
    pub fleet_id: String,
    pub branch_id: String,
    pub dora_id: String,
}

impl EnvConfig {
    pub fn new() -> Result<Self> {
        Ok(Self {
            customer_id: env::var("CUSTOMER_ID")?,
            fleet_id: env::var("FLEET_ID")?,
            branch_id: env::var("BRANCH_ID")?,
            dora_id: env::var("DORA_ID")?,
        })
    }
}

impl DhcpConfig {
    /// attempts to decode the config first as JSON, then YAML, finally erroring if neither work
    pub fn parse<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let config = v4::Config::new(
            std::fs::read_to_string(path)
                .with_context(|| format!("failed to find config at {}", &path.display()))?,
        )?;
        debug!(?config);

        Ok(Self {
            v4: config,
            path: Some(path.to_path_buf()),
        })
    }
    /// attempts to decode the config first as JSON, then YAML, finally erroring if neither work
    pub fn parse_str<S: AsRef<str>>(s: S) -> Result<Self> {
        let config = v4::Config::new(s.as_ref())?;
        debug!(?config);

        Ok(Self {
            v4: config,
            path: None,
        })
    }
}

/// find the first up non-loopback interface, if a name is provided it must also match
pub fn backup_ivp4_interface(interface: Option<&str>) -> Result<Ipv4Network> {
    let interface = pnet::datalink::interfaces().into_iter().find(|e| {
        e.is_up()
            && !e.is_loopback()
            && !e.ips.is_empty()
            && interface.map(|i| i == e.name).unwrap_or(true)
    });

    debug!(?interface);

    let ips = interface
        .as_ref()
        .map(|int| &int.ips)
        .context("no interface found")?;
    let ipv4 = ips
        .iter()
        .find_map(|net| match net {
            IpNetwork::V4(net) => Some(*net),
            _ => None,
        })
        .with_context(|| format!("no IPv4 interface {:?}", interface.clone()))?;

    Ok(ipv4)
}

/// Returns:
/// - interfaces matching the list supplied that are 'up' and have an IPv4
/// - OR any 'up' interfaces that also have an IPv4
pub fn v4_find_interfaces(interfaces: Option<&[wire::Interface]>) -> Result<Vec<NetworkInterface>> {
    let found_interfaces = pnet::datalink::interfaces()
        .into_iter()
        .filter(|e| e.is_up() && !e.ips.is_empty() && e.ips.iter().any(|i| i.is_ipv4()))
        .collect::<Vec<_>>();
    found_or_default(found_interfaces, interfaces)
}

/// Returns:
/// - interfaces matching the list supplied that are 'up' and have an IPv6
/// - OR any 'up' interfaces that also have an IPv6
pub fn v6_find_interfaces(interfaces: Option<&[wire::Interface]>) -> Result<Vec<NetworkInterface>> {
    let found_interfaces = pnet::datalink::interfaces()
        .into_iter()
        .filter(|e| e.is_up() && !e.ips.is_empty() && e.ips.iter().any(|i| i.is_ipv6()))
        .collect::<Vec<_>>();
    found_or_default(found_interfaces, interfaces)
}

fn found_or_default(
    found_interfaces: Vec<NetworkInterface>,
    interfaces: Option<&[wire::Interface]>,
) -> Result<Vec<NetworkInterface>> {
    Ok(match interfaces {
        Some(interfaces) => interfaces
            .iter()
            .map(|interface| {
                match found_interfaces.iter().find(|i| {
                    i.name == interface.name
                        && interface
                            .addr
                            .map(|addr| i.ips.iter().any(|ip| ip.contains(addr)))
                            .unwrap_or(true)
                }) {
                    Some(i) => Ok(i.clone()),
                    None => bail!(
                        "unable to find interface {} with ip {:#?}",
                        interface.name,
                        interface.addr
                    ),
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => found_interfaces,
    })
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct LeaseTime {
    default: Duration,
    min: Duration,
    max: Duration,
}

impl LeaseTime {
    pub fn new(default: Duration, min: Duration, max: Duration) -> Self {
        Self { default, min, max }
    }
    pub fn get_default(&self) -> Duration {
        self.default
    }
    pub fn get_min(&self) -> Duration {
        self.min
    }
    pub fn get_max(&self) -> Duration {
        self.max
    }
    /// calculate the lease time based on a possible requested time
    pub fn determine_lease(&self, requested: Option<Duration>) -> (Duration, Duration, Duration) {
        let LeaseTime { default, min, max } = *self;
        match requested {
            // time must be larger than `min` and smaller than `max`
            Some(req) => {
                let t = req.clamp(min, max);
                (t, renew(t), rebind(t))
            }
            None => (default, renew(default), rebind(default)),
        }
    }
}

pub fn renew(t: Duration) -> Duration {
    t / 2
}

pub fn rebind(t: Duration) -> Duration {
    t * 7 / 8
}

pub fn generate_random_bytes(len: usize) -> Vec<u8> {
    let mut ident = vec![0;len];
    rand::thread_rng().fill_bytes(&mut ident);
    ident
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct PersistIdentifier {
    pub identifier: String,
    pub duid_config: ServerDuidInfo,
}

impl PersistIdentifier {
    pub fn to_json(&self, path: &Path) -> Result<()> {
        let file = std::fs::File::create(path)?;
        serde_json::to_writer_pretty(file, self)?;
        Ok(())
    }

    pub fn from_json(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Ok(serde_json::from_reader(file)?)
    }

    pub fn duid(&self) -> Result<Duid> {
        let duid_bytes = hex::decode(&self.identifier)
            .context("server identifier should be a valid hex string")?;
        Ok(Duid::from(duid_bytes))
    }
}

#[cfg(test)]
mod test {
    use std::net::IpAddr;

    use dora_core::{pnet::ipnetwork::IpNetwork, prelude::NetworkInterface};

    use crate::wire;

    fn mock_interface(name: &str, ip_str: &str, prefix: u8) -> NetworkInterface {
        let ip = ip_str.parse::<IpAddr>().unwrap();
        NetworkInterface {
            name: name.to_string(),
            description: String::new(),
            index: 0,
            mac: None,
            ips: vec![IpNetwork::new(ip, prefix).unwrap()],
            flags: 0,
        }
    }

    #[test]
    fn test_found_or_default() {
        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let result = crate::found_or_default(found.clone(), None).unwrap();
        assert!(!result.is_empty());

        // no IP
        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let config = vec![wire::Interface {
            name: "eth0".to_string(),
            addr: None,
        }];
        let result = crate::found_or_default(found, Some(&config)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "eth0");

        // matching ip
        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let config = vec![wire::Interface {
            name: "eth0".to_string(),
            addr: Some("192.168.1.10".parse().unwrap()),
        }];
        let result = crate::found_or_default(found, Some(&config)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "eth0");

        // System interface has 192.168.1.1/24, config asks for 192.168.1.50
        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let config = vec![wire::Interface {
            name: "eth0".to_string(),
            addr: Some("192.168.1.50".parse().unwrap()),
        }];
        let result = crate::found_or_default(found, Some(&config)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "eth0");

        // System interface has 192.168.1.10, config asks for 10.0.0.1
        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let config = vec![wire::Interface {
            name: "eth0".to_string(),
            addr: Some("10.0.0.1".parse().unwrap()),
        }];
        let result = crate::found_or_default(found, Some(&config));
        assert!(result.is_err());
    }

    #[test]
    fn test_not_found_interface() {
        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let config = vec![wire::Interface {
            name: "eth0".to_string(),
            addr: Some([192, 168, 0, 10].into()),
        }];
        let result = crate::found_or_default(found, Some(&config));
        assert!(result.is_err());

        let found = vec![mock_interface("eth0", "192.168.1.10", 24)];
        let config = vec![wire::Interface {
            name: "eth1".to_string(), // Wrong name
            addr: None,
        }];
        let result = crate::found_or_default(found, Some(&config));
        assert!(result.is_err());
    }

    #[test]
    fn test_find_by_name_and_ipv6_in_subnet() {
        // System interface has 2001:db8::1/64, config asks for 2001:db8::dead:beef
        let found = vec![mock_interface("eth1", "2001:db8::1", 64)];
        let config = vec![wire::Interface {
            name: "eth1".to_string(),
            addr: Some("2001:db8::dead:beef".parse().unwrap()),
        }];
        let result = crate::found_or_default(found, Some(&config)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "eth1");
    }

    #[test]
    fn test_fail_on_ipv6_mismatch() {
        // System interface has 2001:db8::1, config asks for fd00::1
        let found = vec![mock_interface("eth1", "2001:db8::1", 64)];
        let config = vec![wire::Interface {
            name: "eth1".to_string(),
            addr: Some("fd00::1".parse().unwrap()),
        }];
        let result = crate::found_or_default(found, Some(&config));
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_interfaces_find_by_ip() {
        let found = vec![
            mock_interface("eth0", "192.168.1.10", 24),
            mock_interface("eth1", "10.0.0.5", 8),
        ];
        let config = vec![wire::Interface {
            name: "eth1".to_string(),
            addr: Some("10.0.0.5".parse().unwrap()),
        }];
        let result = crate::found_or_default(found, Some(&config)).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "eth1");
    }

    #[test]
    fn test_multiple_config_interfaces_selects_all() {
        let found = vec![
            mock_interface("eth0", "192.168.1.10", 24),
            mock_interface("eth1", "10.0.0.5", 8),
            mock_interface("lo", "127.0.0.1", 8),
        ];
        let config = vec![
            wire::Interface {
                name: "eth0".to_string(),
                addr: None,
            },
            wire::Interface {
                name: "eth1".to_string(),
                addr: Some("10.0.0.5".parse().unwrap()),
            },
        ];
        let result = crate::found_or_default(found, Some(&config)).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|i| i.name == "eth0"));
        assert!(result.iter().any(|i| i.name == "eth1"));
    }
}
