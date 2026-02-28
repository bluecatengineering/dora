use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

pub const ALL_DHCP_RELAY_AGENTS_AND_SERVERS: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 1, 2);

pub const DEFAULT_CONCURRENCY: usize = 256;
pub const DEFAULT_RAMP_PER_SEC: usize = 200;
pub const DEFAULT_TIMEOUT_MS: u64 = 1000;
pub const DEFAULT_RETRIES: usize = 2;
pub const DEFAULT_MAX_ERROR_RATE: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolSelection {
    V4,
    V6,
    Both,
}

impl ProtocolSelection {
    pub const fn includes_v4(self) -> bool {
        matches!(self, Self::V4 | Self::Both)
    }

    pub const fn includes_v6(self) -> bool {
        matches!(self, Self::V6 | Self::Both)
    }
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "dhcp-loadtest",
    about = "Async DHCPv4/v6 load and integration client"
)]
pub struct Cli {
    #[arg(long)]
    pub iface: String,
    #[arg(long)]
    pub clients: usize,
    #[arg(long, value_enum)]
    pub protocol: ProtocolSelection,

    #[arg(long)]
    pub server_v4: Option<SocketAddr>,
    #[arg(long)]
    pub server_v6: Option<SocketAddr>,

    #[arg(long, default_value_t = DEFAULT_CONCURRENCY)]
    pub concurrency: usize,
    #[arg(long, default_value_t = DEFAULT_RAMP_PER_SEC)]
    pub ramp_per_sec: usize,
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_MS)]
    pub timeout_ms: u64,
    #[arg(long, default_value_t = DEFAULT_RETRIES)]
    pub retries: usize,

    #[arg(long)]
    pub renew: bool,
    #[arg(long)]
    pub release: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub dry_run: bool,

    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    #[arg(long, default_value_t = DEFAULT_MAX_ERROR_RATE)]
    pub max_error_rate: f64,
    #[arg(long)]
    pub allow_renew_reassign: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadTestConfig {
    pub iface: String,
    pub iface_index: u32,
    pub clients: usize,
    pub protocol: ProtocolSelection,
    pub server_v4: Option<SocketAddr>,
    pub server_v6: Option<SocketAddr>,
    pub concurrency: usize,
    pub ramp_per_sec: usize,
    pub timeout_ms: u64,
    pub retries: usize,
    pub renew: bool,
    pub release: bool,
    pub json: bool,
    pub dry_run: bool,
    pub seed: u64,
    pub max_error_rate: f64,
    pub allow_renew_reassign: bool,
}

impl LoadTestConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

impl TryFrom<Cli> for LoadTestConfig {
    type Error = anyhow::Error;

    fn try_from(args: Cli) -> Result<Self> {
        if args.clients == 0 {
            bail!("--clients must be greater than 0");
        }
        if args.concurrency == 0 {
            bail!("--concurrency must be greater than 0");
        }
        if args.timeout_ms == 0 {
            bail!("--timeout-ms must be greater than 0");
        }
        if !(0.0..=1.0).contains(&args.max_error_rate) {
            bail!("--max-error-rate must be between 0.0 and 1.0");
        }

        let iface_index = resolve_interface_index(&args.iface)
            .with_context(|| format!("failed to resolve interface `{}`", args.iface))?;

        let server_v4 = if args.protocol.includes_v4() {
            match args.server_v4 {
                Some(SocketAddr::V4(addr)) => Some(SocketAddr::V4(addr)),
                Some(SocketAddr::V6(_)) => bail!("--server-v4 must be an IPv4 socket address"),
                None => Some(SocketAddr::from((
                    Ipv4Addr::BROADCAST,
                    dhcproto::v4::SERVER_PORT,
                ))),
            }
        } else {
            None
        };

        let server_v6 = if args.protocol.includes_v6() {
            match args.server_v6 {
                Some(SocketAddr::V6(addr)) => Some(SocketAddr::V6(addr)),
                Some(SocketAddr::V4(_)) => bail!("--server-v6 must be an IPv6 socket address"),
                None => Some(SocketAddr::V6(SocketAddrV6::new(
                    ALL_DHCP_RELAY_AGENTS_AND_SERVERS,
                    dhcproto::v6::SERVER_PORT,
                    0,
                    iface_index,
                ))),
            }
        } else {
            None
        };

        Ok(Self {
            iface: args.iface,
            iface_index,
            clients: args.clients,
            protocol: args.protocol,
            server_v4,
            server_v6,
            concurrency: args.concurrency,
            ramp_per_sec: args.ramp_per_sec,
            timeout_ms: args.timeout_ms,
            retries: args.retries,
            renew: args.renew,
            release: args.release,
            json: args.json,
            dry_run: args.dry_run,
            seed: args.seed,
            max_error_rate: args.max_error_rate,
            allow_renew_reassign: args.allow_renew_reassign,
        })
    }
}

fn resolve_interface_index(iface: &str) -> Result<u32> {
    let path = format!("/sys/class/net/{iface}/ifindex");
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read interface index from `{path}`"))?;
    let index = raw
        .trim()
        .parse::<u32>()
        .with_context(|| format!("failed to parse interface index from `{path}`"))?;
    if index == 0 {
        bail!("interface index must be non-zero for `{iface}`");
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, LoadTestConfig, ProtocolSelection};

    #[test]
    fn parse_v4_defaults() {
        let cli = Cli::try_parse_from([
            "dhcp-loadtest",
            "--iface",
            "lo",
            "--clients",
            "8",
            "--protocol",
            "v4",
        ])
        .expect("cli should parse");
        let cfg = LoadTestConfig::try_from(cli).expect("config should build");

        assert_eq!(cfg.clients, 8);
        assert_eq!(cfg.protocol, ProtocolSelection::V4);
        assert!(cfg.server_v4.is_some());
        assert!(cfg.server_v6.is_none());
    }

    #[test]
    fn parse_both_defaults() {
        let cli = Cli::try_parse_from([
            "dhcp-loadtest",
            "--iface",
            "lo",
            "--clients",
            "2",
            "--protocol",
            "both",
        ])
        .expect("cli should parse");
        let cfg = LoadTestConfig::try_from(cli).expect("config should build");

        assert!(cfg.server_v4.is_some());
        assert!(cfg.server_v6.is_some());
    }

    #[test]
    fn reject_wrong_server_family() {
        let cli = Cli::try_parse_from([
            "dhcp-loadtest",
            "--iface",
            "lo",
            "--clients",
            "2",
            "--protocol",
            "v4",
            "--server-v4",
            "[::1]:67",
        ])
        .expect("cli should parse");
        let err = LoadTestConfig::try_from(cli).expect_err("expected v4 family validation error");

        assert!(err.to_string().contains("--server-v4"));
    }
}
