use std::{collections::HashMap, net::IpAddr, num::NonZeroU32, time::Duration};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

use crate::{LeaseTime, wire::client_classes::ClientClasses};

pub mod client_classes;
pub mod v4;
pub mod v6;

/// top-level config type
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Config {
    pub interfaces: Option<Vec<Interface>>,
    #[serde(default = "default_chaddr_only")]
    pub chaddr_only: bool,
    pub flood_protection_threshold: Option<FloodThreshold>,
    #[serde(default = "default_cache_threshold")]
    pub cache_threshold: u32,
    #[serde(default = "default_bootp_enable")]
    pub bootp_enable: bool,
    #[serde(default = "default_rapid_commit")]
    pub rapid_commit: bool,
    #[serde(default)]
    pub networks: HashMap<Ipv4Net, v4::Net>,
    pub v6: Option<v6::Config>,
    pub client_classes: Option<ClientClasses>,
    pub ddns: Option<v4::ddns::Ddns>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Interface {
    pub name: String,
    pub addr: Option<IpAddr>,
}

impl TryFrom<String> for Interface {
    type Error = anyhow::Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let mut iter = s.split('@');
        let name = iter
            .next()
            .ok_or_else(|| anyhow::Error::msg("missing interface"))?
            .to_owned();
        if name.is_empty() {
            return Err(anyhow::Error::msg("missing interface"));
        }
        Ok(Self {
            name,
            addr: iter.next().map(|s| s.parse::<IpAddr>()).transpose()?,
        })
    }
}

impl From<Interface> for String {
    fn from(iface: Interface) -> Self {
        if let Some(addr) = iface.addr {
            format!("{}@{}", iface.name, addr)
        } else {
            iface.name
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FloodThreshold {
    pub packets: NonZeroU32,
    pub secs: NonZeroU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct MinMax {
    pub default: NonZeroU32,
    pub min: Option<NonZeroU32>,
    pub max: Option<NonZeroU32>,
}

pub const fn default_ping_to() -> u64 {
    500
}

pub const fn default_authoritative() -> bool {
    true
}

pub const fn default_probation() -> u64 {
    86_400
}

pub const fn default_chaddr_only() -> bool {
    false
}

pub const fn default_bootp_enable() -> bool {
    true
}

pub const fn default_rapid_commit() -> bool {
    false
}

pub fn default_cache_threshold() -> u32 {
    0
}

impl From<MinMax> for LeaseTime {
    fn from(lease_time: MinMax) -> Self {
        let default = Duration::from_secs(lease_time.default.get() as u64);
        let min = lease_time
            .min
            .map(|n| Duration::from_secs(n.get() as u64))
            .unwrap_or(default);
        let max = lease_time
            .max
            .map(|n| Duration::from_secs(n.get() as u64))
            .unwrap_or(default);
        Self { default, min, max }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
pub(crate) enum MaybeList<T> {
    Val(T),
    List(Vec<T>),
}

#[cfg(test)]
mod tests {
    use super::*;

    pub static EXAMPLE: &str = include_str!("../../../../example.yaml");

    // test we can encode/decode example file
    #[test]
    fn test_example() {
        let cfg: crate::wire::Config = serde_yaml::from_str(EXAMPLE).unwrap();
        println!("{cfg:#?}");
        // back to the yaml
        let s = serde_yaml::to_string(&cfg).unwrap();
        println!("{s}");
    }

    #[test]
    fn test_interface() {
        let iface = Interface {
            name: "eth0".to_string(),
            addr: Some([192, 168, 1, 1].into()),
        };

        let s = serde_json::to_string(&iface).unwrap();
        assert_eq!(s, "\"eth0@192.168.1.1\"");

        let err = serde_json::from_str::<Interface>("\"@192.168.1.1\"");
        assert!(err.is_err());

        let json_test: Interface = serde_json::from_str(&s).unwrap();
        assert_eq!(iface, json_test);

        let no_addr = Interface {
            name: "lo".to_string(),
            addr: None,
        };
        let json_no_addr = serde_json::to_string(&no_addr).unwrap();
        assert_eq!(json_no_addr, "\"lo\"");
        let test_no_addr: Interface = serde_json::from_str(&json_no_addr).unwrap();
        assert_eq!(no_addr, test_no_addr);
    }
}
