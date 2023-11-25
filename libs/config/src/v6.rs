use hex;
use std::{
    collections::HashMap,
    net::Ipv6Addr,
    path::Path,
    str::FromStr,
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context};
use dora_core::{
    anyhow::Result,
    dhcproto::{
        v4::HType,
        v6::{duid::Duid, DhcpOptions},
    },
    pnet::ipnetwork::{IpNetwork, Ipv6Network},
    pnet::{self, datalink::NetworkInterface},
};
use ipnet::Ipv6Net;
use tracing::debug;

use crate::{
    generate_random_bytes,
    wire::{self, v6::ServerDuidInfo},
    LeaseTime, PersistIdentifier,
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

pub fn generate_duid_from_config(
    server_id_info: &ServerDuidInfo,
    link_layer_address: Ipv6Addr,
) -> Result<Duid> {
    match server_id_info {
        ServerDuidInfo::LLT {
            htype,
            identifier,
            time,
        } => {
            let _htype = if htype == &0 {
                HType::Eth
            } else {
                let htype_u8 = *htype as u8; //TODO: a compromise of v4 HType
                HType::from(htype_u8)
            };
            let _identifier = if identifier.is_empty() {
                link_layer_address
            } else {
                Ipv6Addr::from_str(identifier.as_str()).context("should be a valid ipv6 address")?
            };
            let _time = if time == &0 {
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .context("unable to get system time")?
                    .as_secs() as u32
            } else {
                *time
            };
            Ok(Duid::link_layer_time(_htype, _time, _identifier))
        }
        ServerDuidInfo::LL { htype, identifier } => {
            let _htype = if htype == &0 {
                HType::Eth
            } else {
                let htype_u8 = *htype as u8; //TODO: a compromise of v4 HType
                HType::from(htype_u8)
            };
            let _identifier = if identifier.is_empty() {
                link_layer_address
            } else {
                Ipv6Addr::from_str(identifier.as_str()).context("should be a valid ipv6 address")?
            };
            Ok(Duid::link_layer(_htype, _identifier))
        }
        ServerDuidInfo::EN {
            enterprise_id,
            identifier,
        } => {
            let _enterprise_id = if enterprise_id == &0 {
                1 //TODO: harewire to 1 temporarily
            } else {
                *enterprise_id
            };
            let _identifier = if identifier.is_empty() {
                generate_random_bytes(6)
            } else {
                hex::decode(identifier).context("should be a valid hex string")?
            };
            Ok(Duid::enterprise(_enterprise_id, &_identifier[..]))
        }
        ServerDuidInfo::UUID { identifier } => {
            if identifier.is_empty() {
                bail!("identifier must be specified for UUID type DUID");
            }
            let _identifier = hex::decode(identifier).context("should be a valid hex string")?;
            Ok(Duid::uuid(&_identifier[..]))
        }
    }
}

fn generate_duid_and_save_to_file(
    server_id_info: &ServerDuidInfo,
    link_layer_address: Ipv6Addr,
    server_id_path: &Path,
) -> Result<Duid> {
    let duid = generate_duid_from_config(server_id_info, link_layer_address)
        .context("can not generate duid from config")?;
    let duid_vec = duid.as_ref().to_vec();
    let duid_string = hex::encode(duid_vec);
    let new_identifier_file = PersistIdentifier {
        identifier: duid_string,
        duid_config: server_id_info.clone(),
    };
    new_identifier_file
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
                    generate_duid_and_save_to_file(
                        &server_id.info,
                        link_local.ip(),
                        server_id_path,
                    )?
                } else {
                    let identifier_file = PersistIdentifier::from_json(server_id_path)
                        .context("can not read server identifier json")?;
                    if identifier_file.duid_config == server_id.info {
                        identifier_file
                            .duid()
                            .context("can not get duid from server identifier file")?
                    } else {
                        generate_duid_and_save_to_file(
                            &server_id.info,
                            link_local.ip(),
                            server_id_path,
                        )?
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
