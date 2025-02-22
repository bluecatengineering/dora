use hex;
use std::{
    collections::HashMap,
    net::Ipv6Addr,
    path::Path,
    str::FromStr,
    time::{Duration, SystemTime},
};

use anyhow::{Context, bail};
use dora_core::{
    anyhow::Result,
    dhcproto::{
        v4::HType,
        v6::{DhcpOptions, duid::Duid},
    },
    pnet::ipnetwork::{IpNetwork, Ipv6Network},
    pnet::{self, datalink::NetworkInterface},
};
use ipnet::Ipv6Net;
use tracing::debug;

use crate::{
    LeaseTime, PersistIdentifier, generate_random_bytes,
    wire::{self, v6::ServerDuidInfo},
};
/// the default path to  server identifier file path
pub static DEFAULT_SERVER_ID_FILE_PATH: &str = "/var/lib/dora/server_id";
// const DEFAULT_VALID: Duration = Duration::from_secs(12 * 24 * 60 * 60); // 12 days
// const DEFAULT_PREFERRED: Duration = Duration::from_secs(8 * 24 * 60 * 60); // 8 days

/// server config for dhcpv6
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// interfaces that are either explicitly bound by the config or
    /// are up & ipv6
    interfaces: Vec<NetworkInterface>,
    /// global dhcp options
    opts: Option<DhcpOptions>,
    /// used to make a selection on which network or subnet to use
    networks: HashMap<Ipv6Net, Network>,
    server_id: Duid,
}

impl Config {
    /// return server id as a slice of bytes
    pub fn server_id(&self) -> &[u8] {
        self.server_id.as_ref()
    }
    /// return the optional explicitly bound interfaces if there are any
    pub fn interfaces(&self) -> &[NetworkInterface] {
        self.interfaces.as_slice()
    }
    /// Returns:
    ///     - if the config has an interface, return that
    ///     - OR find iface_index and return that
    ///     - OR use default interface
    pub fn get_interface_global(&self, iface_index: u32) -> Option<Ipv6Network> {
        self.find_interface(iface_index).and_then(|int| {
            int.ips.iter().find_map(|ip| match ip {
                IpNetwork::V6(ip) if is_unicast_global(&ip.ip()) => Some(*ip),
                _ => None,
            })
        })
    }
    pub fn get_interface_link_local(&self, iface_index: u32) -> Option<Ipv6Network> {
        self.find_interface(iface_index).and_then(|int| {
            int.ips.iter().find_map(|ip| match ip {
                IpNetwork::V6(ip) if is_unicast_link_local(&ip.ip()) => Some(*ip),
                _ => None,
            })
        })
    }
    pub fn get_interface_ips(&self, iface_index: u32) -> Option<Vec<Ipv6Network>> {
        self.find_interface(iface_index).map(|int| {
            int.ips
                .iter()
                .filter_map(|ip| match ip {
                    IpNetwork::V6(ip) => Some(*ip),
                    _ => None,
                })
                .collect()
        })
    }
    // find the interface at the index `iface_index`
    fn find_interface(&self, iface_index: u32) -> Option<&NetworkInterface> {
        self.interfaces.iter().find(|e| e.index == iface_index)
    }

    /// get a `Network` configured for a given interface index. If the config doesn't specify
    /// an interface, use the IPs (local/global) of the receiving interface
    pub fn get_network(&self, iface_index: u32) -> Option<&Network> {
        let ifs = self.get_interface_ips(iface_index)?;
        self.networks.iter().find_map(|(subnet, network)| {
            // if the configured interface index matches the one we received the packet on
            if matches!(&network.interfaces, Some(ints) if ints.iter().any(|i| i.index == iface_index)) {
                return Some(network);
            }
            if ifs.iter().any(|ip| subnet.contains(&ip.ip())) { // or if no configured interfaces, one of the subnets matches (either global or link-local)
                return Some(network);
            }
            None
        })
    }

    /// gets options (which have been already merged with global opts) for the network of `iface_index` or the global options
    pub fn get_opts(&self, iface_index: u32) -> Option<&DhcpOptions> {
        self.get_network(iface_index)
            .map(|n| n.opts())
            .or(self.opts.as_ref())
    }

    /// get the first `Network`
    pub fn get_first(&self) -> Option<(&Ipv6Net, &Network)> {
        self.networks.iter().next()
    }
}

/// merge `b` into `a`, favoring `a` where there are duplicates
fn merge_opts(a: &DhcpOptions, b: DhcpOptions) -> DhcpOptions {
    let mut opts = a.clone();
    for opt in b.iter() {
        if opts.get(opt.into()).is_none() {
            opts.insert(opt.clone());
        }
    }
    opts
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    interfaces: Option<Vec<NetworkInterface>>,
    subnet: Ipv6Net,
    valid: LeaseTime,
    preferred: LeaseTime,
    options: DhcpOptions,
    ping_check: bool,
    /// default ping timeout in ms
    ping_timeout_ms: Duration,
    /// probation period in seconds
    probation_period: Duration,
    /// Whether we are authoritative for this network (default: true)
    authoritative: bool,
}

impl Network {
    pub fn subnet(&self) -> Ipv6Addr {
        self.subnet.network()
    }
    pub fn authoritative(&self) -> bool {
        self.authoritative
    }
    /// is ping check enabled for this range? should we ping an IP before offering?
    pub fn ping_check(&self) -> bool {
        self.ping_check
    }
    /// get the ping timeout
    pub fn ping_timeout(&self) -> Duration {
        self.ping_timeout_ms
    }
    /// Returns the configured probation period for decline's received on this network
    pub fn probation_period(&self) -> Duration {
        self.probation_period
    }
    /// return options configured for this network
    pub fn opts(&self) -> &DhcpOptions {
        &self.options
    }
}

// TODO: replace with is_unicast_global from std when released
pub const fn is_unicast_global(ip: &Ipv6Addr) -> bool {
    !(ip.is_multicast()
        || ip.is_loopback()
        || is_unicast_link_local(ip) // is_unicast_link_local
        || ((ip.segments()[0] & 0xfe00) == 0xfc00) // is_unique_local
        || ip.is_unspecified()
        || ((ip.segments()[0] == 0x2001) && (ip.segments()[1] == 0xdb8))) // is_documentation
}

// TODO: replace with is_unicast_link_local from std when released
pub const fn is_unicast_link_local(ip: &Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
}

pub fn generate_duid_from_config(server_id: &ServerDuidInfo, link_layer: Ipv6Addr) -> Result<Duid> {
    fn parse_id(id: &str, link_layer: Ipv6Addr) -> Result<Ipv6Addr> {
        Ok(if id.is_empty() {
            link_layer
        } else {
            Ipv6Addr::from_str(id).context("identifier must be a valid ipv6 address")?
        })
    }
    fn parse_htype(htype: u16) -> HType {
        if htype == 0 {
            HType::Eth
        } else {
            //TODO: This is a compromise of v4 HType. Should be changed to v6 HType after dhcproto is updated.
            HType::from(htype as u8)
        }
    }
    match server_id {
        ServerDuidInfo::LLT {
            htype,
            identifier,
            time,
        } => {
            let htype = parse_htype(*htype);
            let identifier = parse_id(identifier, link_layer)?;
            let time = if *time == 0 {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .context("unable to get system time")?
                    .as_secs() as u32
            } else {
                *time
            };
            Ok(Duid::link_layer_time(htype, time, identifier))
        }
        ServerDuidInfo::LL { htype, identifier } => {
            let htype = parse_htype(*htype);
            let identifier = parse_id(identifier, link_layer)?;
            Ok(Duid::link_layer(htype, identifier))
        }
        ServerDuidInfo::EN {
            enterprise_id,
            identifier,
        } => {
            let enterprise_id = if *enterprise_id == 0 {
                1 //TODO: harewire to 1 temporarily
            } else {
                *enterprise_id
            };
            let identifier = if identifier.is_empty() {
                generate_random_bytes(6)
            } else {
                hex::decode(identifier).context("identifier should be a valid hex string")?
            };
            Ok(Duid::enterprise(enterprise_id, &identifier[..]))
        }
        ServerDuidInfo::UUID { identifier } => {
            if identifier.is_empty() {
                bail!("identifier must be specified for UUID type DUID");
            }
            let identifier =
                hex::decode(identifier).context("identifier should be a valid hex string")?;
            Ok(Duid::uuid(&identifier[..]))
        }
    }
}

fn generate_duid_and_persist(
    server_id_info: &ServerDuidInfo,
    link_layer_address: Ipv6Addr,
    server_id_path: &Path,
) -> Result<Duid> {
    let duid = generate_duid_from_config(server_id_info, link_layer_address)
        .context("can not generate duid from config")?;
    PersistIdentifier {
        identifier: hex::encode(duid.as_ref()),
        duid_config: server_id_info.clone(),
    }
    .to_json(server_id_path)
    .context("can not write server identifier json")?;
    Ok(duid)
}

impl TryFrom<wire::v6::Config> for Config {
    type Error = anyhow::Error;

    fn try_from(cfg: wire::v6::Config) -> Result<Self> {
        let interfaces = crate::v6_find_interfaces(cfg.interfaces)?;
        // DUID-LLT is the default, will need config options to do others
        let link_local = interfaces
            .iter()
            .find_map(|int| {
                int.ips.iter().find_map(|ip| match ip {
                    IpNetwork::V6(ip) if is_unicast_link_local(&ip.ip()) => Some(*ip),
                    _ => None,
                })
            })
            .context("unable to find a link local ip")?;
        let server_id = match cfg.server_id {
            None => {
                // if server id file exists, then use it
                let server_id_path = Path::new(DEFAULT_SERVER_ID_FILE_PATH);
                if server_id_path.exists() {
                    let identifier_file = PersistIdentifier::from_json(server_id_path)
                        .context("can not read server identifier json")?;
                    identifier_file
                        .duid()
                        .context("can not get duid from server identifier file")?
                } else {
                    // https://www.rfc-editor.org/rfc/rfc8415#section-11.2
                    Duid::link_layer_time(
                        HType::Eth,
                        SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .context("unable to get system time")?
                            .as_secs() as u32,
                        link_local.ip(),
                    )
                }
            }
            Some(server_id) => {
                let server_id_path = if server_id.path.is_empty() {
                    Path::new(DEFAULT_SERVER_ID_FILE_PATH)
                } else {
                    Path::new(&server_id.path)
                };
                if !server_id.persist {
                    generate_duid_from_config(&server_id.info, link_local.ip())
                        .context("can not generate duid from config")?
                } else if !server_id_path.exists() {
                    generate_duid_and_persist(&server_id.info, link_local.ip(), server_id_path)?
                } else {
                    let identifier_file = PersistIdentifier::from_json(server_id_path)
                        .context("can not read server identifier json")?;
                    if identifier_file.duid_config == server_id.info {
                        // Here, server_id.info is read from a YAML file and the fields like time, identifier, enterprise_id, etc. have not been processed yet (i.e., 0 has not been replaced with the corresponding default values). Therefore, a comparison can be made. For example, if the server_id type is set to LLT and all other values are empty, then both the persisted file and server_id.info will have all fields as 0 or empty string, making them equal. The difference in time or local link layer address due to changes in time or adapter will not affect the comparison.
                        identifier_file
                            .duid()
                            .context("can not get duid from server identifier file")?
                    } else {
                        generate_duid_and_persist(&server_id.info, link_local.ip(), server_id_path)?
                    }
                }
            }
        };
        let global_opts = cfg.options;
        debug!(?interfaces, ?server_id, "v6 interfaces that will be used");
        let networks = cfg
            .networks
            .into_iter()
            .map(|(subnet, net)| {
                let wire::v6::Net {
                    ping_check,
                    probation_period,
                    authoritative,
                    ping_timeout_ms,
                    config,
                    options,
                    interfaces: net_interfaces,
                } = net;

                // If any interfaces are explicitly set for the network,
                // find them. If the interface can't be found return an error.
                let net_interfaces = net_interfaces
                    .map(|net_interfaces| {
                        let found_interfaces = pnet::datalink::interfaces()
                            .into_iter()
                            .filter(|e| {
                                e.is_up() && !e.ips.is_empty() && e.ips.iter().any(|i| i.is_ipv6())
                            })
                            .collect::<Vec<_>>();

                        net_interfaces
                            .into_iter()
                            .map(|int| {
                                if let Some(interface) =
                                    found_interfaces.iter().find(|i| i.name == int)
                                {
                                    Ok(interface.clone())
                                } else {
                                    bail!("unable to find interface {} for network", int)
                                }
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?;

                let (valid, preferred) = (config.lease_time.into(), config.preferred_time.into());

                let network = Network {
                    interfaces: net_interfaces,
                    subnet,
                    valid,
                    preferred,
                    ping_check,
                    probation_period: Duration::from_secs(probation_period),
                    authoritative,
                    ping_timeout_ms: Duration::from_millis(ping_timeout_ms),
                    // merge global with network opts OR just return network options if no global exist
                    options: match &global_opts {
                        Some(a) => merge_opts(a.as_ref(), options.get()),
                        None => options.get(),
                    },
                };
                Ok((subnet, network))
            })
            .collect::<Result<_, anyhow::Error>>()?;

        Ok(Self {
            interfaces,
            networks,
            opts: global_opts.map(|o| o.get()),
            server_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{PersistIdentifier, v4::Config};
    use std::path::Path;

    pub static TEST_SERVER_ID_FILE_PATH: &str = "./server_id"; //can not use include_str because sometimes it doesn't exist.
    pub static CONFIG_V6_YAML: &str = include_str!("../sample/config_v6.yaml");
    pub static CONFIG_V6_LL_YAML: &str = include_str!("../sample/config_v6_LL.yaml");
    pub static CONFIG_V6_EN_YAML: &str = include_str!("../sample/config_v6_EN.yaml");
    pub static CONFIG_V6_UUID_YAML: &str = include_str!("../sample/config_v6_UUID.yaml");
    pub static CONFIG_V6_NO_PERSIST_YAML: &str =
        include_str!("../sample/config_v6_no_persist.yaml");

    /// test if v6_config can generate a server_id; and if it can dump it to a file
    #[test]
    fn test_v6_config() {
        let path = Path::new(TEST_SERVER_ID_FILE_PATH);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }

        let cfg = Config::new(CONFIG_V6_YAML).unwrap();
        // test a range decoded properly
        match cfg.v6() {
            Some(v6_config) => {
                println!("{:?}", v6_config);
            }
            None => {
                panic!("expected v6 config")
            }
        };

        let identifier_file = PersistIdentifier::from_json(path).unwrap();
        let file_server_id = identifier_file.duid().unwrap();
        let file_server_id = file_server_id.as_ref();
        let server_id = cfg.v6().unwrap().server_id();
        assert_eq!(server_id, file_server_id);
    }

    /// test if we can generate a different server_id using different config rather than using the config file that exists
    #[test]
    fn test_v6_generate_different_server_id() {
        let cfg1 = Config::new(CONFIG_V6_YAML).unwrap();
        let cfg2 = Config::new(CONFIG_V6_LL_YAML).unwrap();
        let server_id1 = cfg1.v6().unwrap().server_id();
        let server_id2 = cfg2.v6().unwrap().server_id();
        println!("server_id1: {:?}", server_id1);
        println!("server_id2: {:?}", server_id2);
        assert_ne!(server_id1, server_id2);
    }
    /// test if we can generate EN type server_id
    #[test]
    fn test_v6_generate_en_server_id() {
        let cfg = Config::new(CONFIG_V6_EN_YAML).unwrap();
        let server_id = cfg.v6().unwrap().server_id();
        println!("server_id: {:?}", server_id);
    }
    /// test if we can generate UUID type server_id
    #[test]
    fn test_v6_generate_uuid_server_id() {
        let cfg = Config::new(CONFIG_V6_UUID_YAML).unwrap();
        let server_id = cfg.v6().unwrap().server_id();
        println!("server_id: {:?}", server_id);
    }
    /// test if wen can generate server_id without persisting it to a file
    #[test]
    fn test_v6_generate_server_id_without_persist() {
        let server_id_path = Path::new(TEST_SERVER_ID_FILE_PATH);
        if server_id_path.exists() {
            std::fs::remove_file(server_id_path).unwrap();
        }
        let cfg = Config::new(CONFIG_V6_NO_PERSIST_YAML).unwrap();
        let server_id = cfg.v6().unwrap().server_id();
        println!("server_id: {:?}", server_id);
        assert!(!server_id_path.exists());
    }
}
