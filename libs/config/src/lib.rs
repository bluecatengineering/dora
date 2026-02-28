use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rand::{self, RngCore};
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;
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

/// Normalized nats-mode settings, populated only when backend_mode is nats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsConfig {
    /// NATS server URL(s).
    pub servers: Vec<String>,
    /// Subject prefix.
    pub subject_prefix: String,
    /// Resolved subject names.
    pub subjects: wire::NatsSubjects,
    /// JetStream KV bucket for lease records and indexes.
    pub leases_bucket: String,
    /// JetStream KV bucket for host-option records.
    pub host_options_bucket: String,
    /// Lease garbage-collection interval.
    pub lease_gc_interval: Duration,
    /// Interval for polling coordination state (connection status).
    pub coordination_state_poll_interval: Duration,
    /// Contract version string.
    pub contract_version: String,
    /// Security mode.
    pub security_mode: wire::NatsSecurityMode,
    /// Username (for user_password mode).
    pub username: Option<String>,
    /// Password (for user_password mode).
    pub password: Option<String>,
    /// Token (for token mode).
    pub token: Option<String>,
    /// NKey seed file path.
    pub nkey_seed_path: Option<PathBuf>,
    /// TLS client certificate path.
    pub tls_cert_path: Option<PathBuf>,
    /// TLS client key path.
    pub tls_key_path: Option<PathBuf>,
    /// TLS CA certificate path.
    pub tls_ca_path: Option<PathBuf>,
    /// Credentials file path.
    pub creds_file_path: Option<PathBuf>,
    /// Connection timeout.
    pub connect_timeout: Option<Duration>,
    /// Maximum retries for initial NATS connection attempts.
    pub connect_retry_max: u32,
    /// Request timeout.
    pub request_timeout: Option<Duration>,
}

/// server config
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DhcpConfig {
    v4: v4::Config,
    path: Option<PathBuf>,
    backend_mode: wire::BackendMode,
    nats: Option<NatsConfig>,
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
    /// Returns the configured backend mode (standalone or nats).
    pub fn backend_mode(&self) -> wire::BackendMode {
        self.backend_mode
    }
    /// Returns true when operating in nats mode.
    pub fn is_nats(&self) -> bool {
        self.backend_mode == wire::BackendMode::Nats
    }
    /// Returns true when operating in standalone mode.
    pub fn is_standalone(&self) -> bool {
        self.backend_mode == wire::BackendMode::Standalone
    }
    /// Returns the nats configuration, if present (only in nats mode).
    pub fn nats(&self) -> Option<&NatsConfig> {
        self.nats.as_ref()
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

/// Validate and normalize nats-mode configuration from the wire config.
/// Returns Ok(None) for standalone mode, Ok(Some(..)) for valid nats mode,
/// or Err for invalid nats config.
fn validate_nats_config(wire_cfg: &wire::Config) -> Result<Option<NatsConfig>> {
    match wire_cfg.backend_mode {
        wire::BackendMode::Standalone => {
            // Standalone mode: no nats validation required.
            Ok(None)
        }
        wire::BackendMode::Nats => {
            let nats = wire_cfg.nats.as_ref().ok_or_else(|| {
                anyhow::anyhow!("nats mode requires a 'nats' configuration section")
            })?;

            if nats.servers.is_empty() {
                bail!("nats mode requires at least one NATS server URL in 'nats.servers'");
            }

            let normalized_servers = normalize_nats_servers(&nats.servers)?;

            if nats.contract_version.trim().is_empty() {
                bail!("nats mode requires a non-empty 'nats.contract_version'");
            }

            // Resolve subject templates from prefix for fields that were left at defaults.
            //
            // Detection works by comparing each subject against its hardcoded default
            // value. If a subject still equals the default, it is re-derived from
            // `subject_prefix`. This means an explicitly-set value that happens to
            // match the default is indistinguishable from "not set" and will be
            // re-derived — which only matters if DEFAULT_SUBJECT_PREFIX changes in
            // a future version (the previously-default subjects would then be
            // re-derived with the new prefix instead of being preserved).
            let defaults = wire::NatsSubjects::default();
            let mut resolved_subjects = nats.subjects.clone();
            if resolved_subjects.lease_upsert == defaults.lease_upsert {
                resolved_subjects.lease_upsert = format!("{}.lease.upsert", nats.subject_prefix);
            }
            if resolved_subjects.lease_release == defaults.lease_release {
                resolved_subjects.lease_release = format!("{}.lease.release", nats.subject_prefix);
            }
            if resolved_subjects.lease_snapshot_request == defaults.lease_snapshot_request {
                resolved_subjects.lease_snapshot_request =
                    format!("{}.lease.snapshot.request", nats.subject_prefix);
            }
            if resolved_subjects.lease_snapshot_response == defaults.lease_snapshot_response {
                resolved_subjects.lease_snapshot_response =
                    format!("{}.lease.snapshot.response", nats.subject_prefix);
            }

            // Validate subject templates are non-empty.
            let subj = &resolved_subjects;
            let subject_fields = [
                ("lease_upsert", &subj.lease_upsert),
                ("lease_release", &subj.lease_release),
                ("lease_snapshot_request", &subj.lease_snapshot_request),
                ("lease_snapshot_response", &subj.lease_snapshot_response),
            ];
            for (name, value) in &subject_fields {
                if value.trim().is_empty() {
                    bail!(
                        "nats mode requires a non-empty NATS subject for '{}'; \
                         configure it in 'nats.subjects.{}' or use default",
                        name,
                        name
                    );
                }
            }

            if nats.leases_bucket.trim().is_empty() {
                bail!("nats mode requires a non-empty 'nats.leases_bucket'");
            }
            if nats.host_options_bucket.trim().is_empty() {
                bail!("nats mode requires a non-empty 'nats.host_options_bucket'");
            }
            if nats.lease_gc_interval_ms == 0 {
                bail!("nats mode requires 'nats.lease_gc_interval_ms' > 0");
            }

            Ok(Some(NatsConfig {
                servers: normalized_servers,
                subject_prefix: nats.subject_prefix.clone(),
                subjects: resolved_subjects,
                leases_bucket: nats.leases_bucket.clone(),
                host_options_bucket: nats.host_options_bucket.clone(),
                lease_gc_interval: Duration::from_millis(nats.lease_gc_interval_ms),
                coordination_state_poll_interval: Duration::from_millis(
                    nats.coordination_state_poll_interval_ms,
                ),
                contract_version: nats.contract_version.clone(),
                security_mode: nats.security_mode.clone(),
                username: nats.username.clone(),
                password: nats.password.clone(),
                token: nats.token.clone(),
                nkey_seed_path: nats.nkey_seed_path.clone(),
                tls_cert_path: nats.tls_cert_path.clone(),
                tls_key_path: nats.tls_key_path.clone(),
                tls_ca_path: nats.tls_ca_path.clone(),
                creds_file_path: nats.creds_file_path.clone(),
                connect_timeout: nats.connect_timeout_ms.map(Duration::from_millis),
                connect_retry_max: nats
                    .connect_retry_max
                    .unwrap_or(wire::DEFAULT_CONNECT_RETRY_MAX),
                request_timeout: nats.request_timeout_ms.map(Duration::from_millis),
            }))
        }
    }
}

fn normalize_nats_servers(raw_servers: &[String]) -> Result<Vec<String>> {
    let mut servers = Vec::new();

    for (idx, raw) in raw_servers.iter().enumerate() {
        let mut split_any = false;
        for (part_idx, part) in raw.split(',').enumerate() {
            split_any = true;
            let server = part.trim();
            if server.is_empty() {
                bail!(
                    "NATS server URL at index {idx} contains an empty entry at position {part_idx}; remove extra commas or whitespace"
                );
            }

            validate_single_nats_server(server).with_context(|| {
                format!("invalid NATS server URL at index {idx} position {part_idx}: `{server}`")
            })?;

            servers.push(server.to_string());
        }

        if !split_any {
            bail!("NATS server URL at index {idx} is empty; all server URLs must be non-empty");
        }
    }

    if servers.is_empty() {
        bail!("nats mode requires at least one NATS server URL in 'nats.servers'");
    }

    Ok(servers)
}

fn validate_single_nats_server(server: &str) -> Result<()> {
    // Match async-nats behavior: if no scheme is provided, default to nats://
    let parse_input = if server.contains("://") {
        server.to_string()
    } else {
        format!("nats://{server}")
    };

    let parsed: Url = parse_input
        .parse()
        .with_context(|| "NATS server URL is invalid")?;

    let scheme = parsed.scheme();
    if !matches!(scheme, "nats" | "tls" | "ws" | "wss") {
        bail!(
            "NATS server URL has invalid scheme `{scheme}`; expected one of nats://, tls://, ws://, wss://"
        );
    }

    if parsed.host_str().is_none() {
        bail!("NATS server URL is missing host");
    }

    Ok(())
}

impl DhcpConfig {
    /// attempts to decode the config first as JSON, then YAML, finally erroring if neither work
    pub fn parse<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to find config at {}", &path.display()))?;

        // Parse wire config for nats validation before normalized parse
        let wire_cfg: wire::Config = match serde_json::from_str(&raw) {
            Ok(c) => c,
            Err(_) => {
                serde_yaml::from_str(&raw).context("failed to parse config as JSON or YAML")?
            }
        };

        let backend_mode = wire_cfg.backend_mode;
        let nats = validate_nats_config(&wire_cfg)?;

        let config = v4::Config::try_from(wire_cfg)?;
        debug!(?config);

        Ok(Self {
            v4: config,
            path: Some(path.to_path_buf()),
            backend_mode,
            nats,
        })
    }
    /// attempts to decode the config first as JSON, then YAML, finally erroring if neither work
    pub fn parse_str<S: AsRef<str>>(s: S) -> Result<Self> {
        let raw = s.as_ref();

        // Parse wire config for nats validation before normalized parse
        let wire_cfg: wire::Config = match serde_json::from_str(raw) {
            Ok(c) => c,
            Err(_) => {
                serde_yaml::from_str(raw).context("failed to parse config as JSON or YAML")?
            }
        };

        let backend_mode = wire_cfg.backend_mode;
        let nats = validate_nats_config(&wire_cfg)?;

        let config = v4::Config::try_from(wire_cfg)?;
        debug!(?config);

        Ok(Self {
            v4: config,
            path: None,
            backend_mode,
            nats,
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

#[cfg(test)]
mod test {
    use std::net::IpAddr;

    use dora_core::{pnet::ipnetwork::IpNetwork, prelude::NetworkInterface};

    use crate::wire;

    // --- NATS config validation regression tests ---

    #[test]
    fn test_standalone_config_no_cluster_fields() {
        let yaml = r#"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let cfg = crate::DhcpConfig::parse_str(yaml).unwrap();
        assert!(cfg.is_standalone());
        assert!(!cfg.is_nats());
        assert!(cfg.nats().is_none());
    }

    #[test]
    fn test_nats_config_valid() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://127.0.0.1:4222"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let cfg = crate::DhcpConfig::parse_str(yaml).unwrap();
        assert!(cfg.is_nats());
        let nats = cfg.nats().unwrap();
        assert_eq!(nats.servers, vec!["nats://127.0.0.1:4222"]);
        assert_eq!(nats.contract_version, "1.0.0");
        assert_eq!(nats.subjects.lease_upsert, "dora.cluster.lease.upsert");
    }

    #[test]
    fn test_nats_config_missing_nats_section() {
        let yaml = r#"
backend_mode: nats
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let result = crate::DhcpConfig::parse_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("nats"),
            "Error should mention missing nats config: {err}"
        );
    }

    #[test]
    fn test_nats_config_empty_servers() {
        let yaml = r#"
backend_mode: nats
nats:
    servers: []
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let result = crate::DhcpConfig::parse_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("server"),
            "Error should mention empty servers: {err}"
        );
    }

    #[test]
    fn test_nats_config_empty_contract_version() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://127.0.0.1:4222"
    contract_version: "   "
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let result = crate::DhcpConfig::parse_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("contract_version"),
            "Error should mention contract_version: {err}"
        );
    }

    #[test]
    fn test_nats_config_subject_prefix_derives_subjects() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://127.0.0.1:4222"
    subject_prefix: "myorg.edge"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let cfg = crate::DhcpConfig::parse_str(yaml).unwrap();
        let nats = cfg.nats().unwrap();
        assert_eq!(nats.subjects.lease_upsert, "myorg.edge.lease.upsert");
        assert_eq!(nats.connect_retry_max, wire::DEFAULT_CONNECT_RETRY_MAX);
        assert_eq!(
            nats.subjects.lease_snapshot_response,
            "myorg.edge.lease.snapshot.response"
        );
    }

    #[test]
    fn test_nats_config_custom_subjects_valid() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://nats1:4222"
    subject_prefix: "myorg.dhcp"
    contract_version: "1.0.0"
    leases_bucket: "myorg.leases"
    host_options_bucket: "myorg.hostopts"
    lease_gc_interval_ms: 10000
    connect_retry_max: 7
    subjects:
        lease_upsert: "myorg.dhcp.v1.lease.upsert"
        lease_release: "myorg.dhcp.v1.lease.release"
        lease_snapshot_request: "myorg.dhcp.v1.snap.req"
        lease_snapshot_response: "myorg.dhcp.v1.snap.res"
    security_mode: user_password
    username: "dora"
    password: "secret"
    connect_timeout_ms: 5000
    request_timeout_ms: 3000
networks:
    10.0.0.0/24:
        ranges:
            -
                start: 10.0.0.10
                end: 10.0.0.200
                config:
                    lease_time:
                        default: 7200
                options:
                    values:
                        3:
                            type: ip
                            value: 10.0.0.1
"#;
        let cfg = crate::DhcpConfig::parse_str(yaml).unwrap();
        assert!(cfg.is_nats());
        let nats = cfg.nats().unwrap();
        assert_eq!(nats.subjects.lease_upsert, "myorg.dhcp.v1.lease.upsert");
        assert_eq!(nats.leases_bucket, "myorg.leases");
        assert_eq!(nats.host_options_bucket, "myorg.hostopts");
        assert_eq!(nats.lease_gc_interval, std::time::Duration::from_secs(10));
        assert_eq!(nats.security_mode, wire::NatsSecurityMode::UserPassword);
        assert_eq!(nats.username.as_deref(), Some("dora"));
        assert_eq!(nats.connect_retry_max, 7);
        assert_eq!(
            nats.connect_timeout,
            Some(std::time::Duration::from_millis(5000))
        );
    }

    #[test]
    fn test_standalone_ignores_nats_section() {
        // Standalone mode with nats section present should still parse as standalone
        let yaml = r#"
backend_mode: standalone
nats:
    servers:
        - "nats://127.0.0.1:4222"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let cfg = crate::DhcpConfig::parse_str(yaml).unwrap();
        assert!(cfg.is_standalone());
        assert!(cfg.nats().is_none());
    }

    #[test]
    fn test_nats_config_blank_server_url() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://127.0.0.1:4222"
        - "  "
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;
        let result = crate::DhcpConfig::parse_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("empty"),
            "Error should mention empty server URL: {err}"
        );
    }

    #[test]
    fn test_nats_config_accepts_typical_server_url_forms() {
        // Typical forms seen in NATS docs and clients:
        // - nats://host:port
        // - tls://host:port
        // - ws://host:port and wss://host:port
        // - host or host:port (defaults to nats:// in async-nats)
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "  nats://127.0.0.1:4222  "
        - "tls://nats.example.com:4222"
        - "ws://nats.example.com:80"
        - "wss://nats.example.com:443"
        - "demo.nats.io"
        - "localhost:4222"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;

        let cfg = crate::DhcpConfig::parse_str(yaml).expect("config should parse");
        let nats = cfg.nats().expect("nats config should exist");
        assert_eq!(
            nats.servers,
            vec![
                "nats://127.0.0.1:4222",
                "tls://nats.example.com:4222",
                "ws://nats.example.com:80",
                "wss://nats.example.com:443",
                "demo.nats.io",
                "localhost:4222",
            ]
        );
    }

    #[test]
    fn test_nats_config_accepts_comma_separated_server_entry() {
        // docs.nats.io examples often show comma-separated seed URLs in one string.
        // Accept that style and normalize into distinct server entries.
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://192.168.1.4:4222,nats://192.168.1.5:4222"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;

        let cfg = crate::DhcpConfig::parse_str(yaml).expect("config should parse");
        let nats = cfg.nats().expect("nats config should exist");
        assert_eq!(
            nats.servers,
            vec!["nats://192.168.1.4:4222", "nats://192.168.1.5:4222"]
        );
    }

    #[test]
    fn test_nats_config_rejects_invalid_server_scheme() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "http://127.0.0.1:4222"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;

        let err = crate::DhcpConfig::parse_str(yaml).expect_err("invalid scheme must fail");
        let err = format!("{err:#}");
        assert!(err.contains("invalid scheme"), "unexpected error: {err}");
    }

    #[test]
    fn test_nats_config_rejects_invalid_server_port() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://127.0.0.1:70000"
    contract_version: "1.0.0"
networks:
    192.168.0.0/24:
        ranges:
            -
                start: 192.168.0.100
                end: 192.168.0.200
                config:
                    lease_time:
                        default: 3600
                options:
                    values:
                        3:
                            type: ip
                            value: 192.168.0.1
"#;

        let err = crate::DhcpConfig::parse_str(yaml).expect_err("invalid port must fail");
        let err = format!("{err:#}");
        assert!(
            err.contains("invalid") && err.contains("port"),
            "unexpected error: {err}"
        );
    }

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
