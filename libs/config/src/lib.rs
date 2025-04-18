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
pub fn v4_find_interfaces(interfaces: Option<Vec<String>>) -> Result<Vec<NetworkInterface>> {
    let found_interfaces = pnet::datalink::interfaces()
        .into_iter()
        .filter(|e| e.is_up() && !e.ips.is_empty() && e.ips.iter().any(|i| i.is_ipv4()))
        .collect::<Vec<_>>();
    found_or_default(found_interfaces, interfaces)
}

/// Returns:
/// - interfaces matching the list supplied that are 'up' and have an IPv6
/// - OR any 'up' interfaces that also have an IPv6
pub fn v6_find_interfaces(interfaces: Option<Vec<String>>) -> Result<Vec<NetworkInterface>> {
    let found_interfaces = pnet::datalink::interfaces()
        .into_iter()
        .filter(|e| e.is_up() && !e.ips.is_empty() && e.ips.iter().any(|i| i.is_ipv6()))
        .collect::<Vec<_>>();
    found_or_default(found_interfaces, interfaces)
}

fn found_or_default(
    found_interfaces: Vec<NetworkInterface>,
    interfaces: Option<Vec<String>>,
) -> Result<Vec<NetworkInterface>> {
    Ok(match interfaces {
        Some(interfaces) => interfaces
            .iter()
            .map(
                |interface| match found_interfaces.iter().find(|i| &i.name == interface) {
                    Some(i) => Ok(i.clone()),
                    None => bail!("unable to find interface {}", interface),
                },
            )
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
    let mut ident = Vec::with_capacity(len);
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
