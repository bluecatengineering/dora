use std::{collections::HashMap, num::NonZeroU32, time::Duration};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

use crate::{wire::client_classes::ClientClasses, LeaseTime};

pub mod client_classes;
pub mod v4;
pub mod v6;

/// top-level config type
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Config {
    pub interfaces: Option<Vec<String>>,
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
    25
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
}
