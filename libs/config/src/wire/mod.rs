use std::{collections::HashMap, time::Duration};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};

use crate::LeaseTime;

pub mod v4;
pub mod v6;

/// top-level config type
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Config {
    pub interfaces: Option<Vec<String>>,
    #[serde(default = "default_chaddr_only")]
    pub chaddr_only: bool,
    #[serde(default)]
    pub networks: HashMap<Ipv4Net, v4::Net>,
    pub v6: Option<v6::Config>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct MinMax {
    pub default: u32,
    pub min: Option<u32>,
    pub max: Option<u32>,
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

pub const fn default_enable_ra() -> bool {
    false
}

impl From<MinMax> for LeaseTime {
    fn from(lease_time: MinMax) -> Self {
        let default = Duration::from_secs(lease_time.default as u64);
        let min = lease_time
            .min
            .map(|n| Duration::from_secs(n as u64))
            .unwrap_or(default);
        let max = lease_time
            .max
            .map(|n| Duration::from_secs(n as u64))
            .unwrap_or(default);
        LeaseTime { default, min, max }
    }
}
