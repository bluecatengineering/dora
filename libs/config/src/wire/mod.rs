use std::{collections::HashMap, net::IpAddr, num::NonZeroU32, path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use ipnet::Ipv4Net;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{LeaseTime, wire::client_classes::ClientClasses};

pub mod client_classes;
pub mod v4;
pub mod v6;

/// Lease backend mode: standalone (SQLite, default) or nats (NATS-backed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendMode {
    /// Traditional single-server mode with local SQLite storage (default).
    #[default]
    Standalone,
    /// NATS mode using NATS for lease coordination and persistence.
    Nats,
}

/// NATS security mode selector for NATS operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NatsSecurityMode {
    /// No authentication or encryption (default).
    #[default]
    None,
    /// Username/password authentication.
    UserPassword,
    /// Token-based authentication.
    Token,
    /// NKey-based authentication.
    Nkey,
    /// TLS client certificate authentication.
    Tls,
    /// Credentials file-based authentication (JWT + NKey).
    CredsFile,
}

/// Configurable NATS subject templates for NATS coordination channels.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NatsSubjects {
    /// Subject for lease upsert operations.
    #[serde(default = "default_lease_upsert_subject")]
    pub lease_upsert: String,
    /// Subject for lease release operations.
    #[serde(default = "default_lease_release_subject")]
    pub lease_release: String,
    /// Subject for lease snapshot request.
    #[serde(default = "default_lease_snapshot_request_subject")]
    pub lease_snapshot_request: String,
    /// Subject for lease snapshot response.
    #[serde(default = "default_lease_snapshot_response_subject")]
    pub lease_snapshot_response: String,
}

impl Default for NatsSubjects {
    fn default() -> Self {
        Self {
            lease_upsert: default_lease_upsert_subject(),
            lease_release: default_lease_release_subject(),
            lease_snapshot_request: default_lease_snapshot_request_subject(),
            lease_snapshot_response: default_lease_snapshot_response_subject(),
        }
    }
}

/// Default NATS subject prefix used in templates.
pub const DEFAULT_SUBJECT_PREFIX: &str = "dora.cluster";

fn default_lease_upsert_subject() -> String {
    format!("{DEFAULT_SUBJECT_PREFIX}.lease.upsert")
}
fn default_lease_release_subject() -> String {
    format!("{DEFAULT_SUBJECT_PREFIX}.lease.release")
}
fn default_lease_snapshot_request_subject() -> String {
    format!("{DEFAULT_SUBJECT_PREFIX}.lease.snapshot.request")
}
fn default_lease_snapshot_response_subject() -> String {
    format!("{DEFAULT_SUBJECT_PREFIX}.lease.snapshot.response")
}

pub const DEFAULT_LEASES_BUCKET: &str = "dora_leases";
pub const DEFAULT_HOST_OPTIONS_BUCKET: &str = "dora_host_options";

fn default_leases_bucket() -> String {
    DEFAULT_LEASES_BUCKET.to_owned()
}

fn default_host_options_bucket() -> String {
    DEFAULT_HOST_OPTIONS_BUCKET.to_owned()
}

pub const DEFAULT_LEASE_GC_INTERVAL_MS: u64 = 60_000;

fn default_lease_gc_interval_ms() -> u64 {
    DEFAULT_LEASE_GC_INTERVAL_MS
}

/// Default interval for polling coordination state (1s).
pub const DEFAULT_COORDINATION_STATE_POLL_INTERVAL_MS: u64 = 1000;

fn default_coordination_state_poll_interval_ms() -> u64 {
    DEFAULT_COORDINATION_STATE_POLL_INTERVAL_MS
}

/// Default maximum retries for initial NATS connection attempts.
pub const DEFAULT_CONNECT_RETRY_MAX: u32 = 10;

/// Default contract version for the NATS clustering protocol.
pub const DEFAULT_CONTRACT_VERSION: &str = "1.0.0";

fn default_contract_version() -> String {
    DEFAULT_CONTRACT_VERSION.to_owned()
}

fn default_subject_prefix() -> String {
    DEFAULT_SUBJECT_PREFIX.to_owned()
}

/// NATS coordination configuration for nats mode.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct NatsConfig {
    /// NATS server URL(s). At least one required for nats mode.
    pub servers: Vec<String>,
    /// Subject prefix for all NATS subjects.
    #[serde(default = "default_subject_prefix")]
    pub subject_prefix: String,
    /// Configurable subject templates. Defaults are derived from subject_prefix.
    #[serde(default)]
    pub subjects: NatsSubjects,
    /// JetStream KV bucket for lease records and lease indexes.
    #[serde(default = "default_leases_bucket")]
    pub leases_bucket: String,
    /// JetStream KV bucket for host-option records.
    #[serde(default = "default_host_options_bucket")]
    pub host_options_bucket: String,
    /// Lease garbage-collection interval in milliseconds.
    #[serde(default = "default_lease_gc_interval_ms")]
    pub lease_gc_interval_ms: u64,
    /// Interval for polling coordination state (connection status) in milliseconds.
    /// Used by the background monitor to update is_coordination_available flag.
    #[serde(default = "default_coordination_state_poll_interval_ms")]
    pub coordination_state_poll_interval_ms: u64,
    /// Contract version for the clustering protocol.
    #[serde(default = "default_contract_version")]
    pub contract_version: String,
    /// Security mode for NATS connection.
    #[serde(default)]
    pub security_mode: NatsSecurityMode,
    /// Username for user_password security mode.
    pub username: Option<String>,
    /// Password for user_password security mode.
    pub password: Option<String>,
    /// Token for token security mode.
    pub token: Option<String>,
    /// Path to NKey seed file for nkey security mode.
    pub nkey_seed_path: Option<PathBuf>,
    /// Path to TLS client certificate for tls security mode.
    pub tls_cert_path: Option<PathBuf>,
    /// Path to TLS client key for tls security mode.
    pub tls_key_path: Option<PathBuf>,
    /// Path to TLS CA certificate for server verification.
    pub tls_ca_path: Option<PathBuf>,
    /// Path to credentials file for creds_file security mode.
    pub creds_file_path: Option<PathBuf>,
    /// Connection timeout in milliseconds (optional).
    pub connect_timeout_ms: Option<u64>,
    /// Maximum retries for initial NATS connection before startup fails.
    /// If unset, defaults to 10 retries.
    pub connect_retry_max: Option<u32>,
    /// Request timeout in milliseconds for coordination calls (optional).
    pub request_timeout_ms: Option<u64>,
}

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
    /// Lease backend mode: standalone (default) or nats.
    #[serde(default)]
    pub backend_mode: BackendMode,
    /// NATS coordination configuration. Required when backend_mode is nats.
    pub nats: Option<NatsConfig>,
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
    #[serde(deserialize_with = "deserialize_duration")]
    pub default: NonZeroU32,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
    pub min: Option<NonZeroU32>,
    #[serde(default, deserialize_with = "deserialize_optional_duration")]
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

impl Default for MinMax {
    fn default() -> Self {
        Self {
            default: NonZeroU32::new(86400).unwrap(),    // 24 hours
            min: Some(NonZeroU32::new(1200).unwrap()),   // 20 minutes
            max: Some(NonZeroU32::new(604800).unwrap()), // 7 days
        }
    }
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

/// Parse a duration string with optional time units
/// Accepts: "3600", "3600s", "60m", "24h"
/// If no unit is specified, assumes seconds
fn parse_duration(s: &str) -> Result<u32> {
    let s = s.trim();
    if s.is_empty() {
        return Err(anyhow::Error::msg("empty duration string"));
    }

    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    // split units
    let (num, unit) = s.split_at(end);
    let num = num.parse::<u32>().context("invalid number")?;

    let num_seconds = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        other => anyhow::bail!(
            "unknown time unit '{}', only 'h', 'm', or 's' are supported",
            other
        ),
    };

    num.checked_mul(num_seconds)
        .context("duration value overflow")
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LeaseDuration {
    Seconds(u64),
    String(String),
}

impl LeaseDuration {
    fn into_nonzero<E: de::Error>(self) -> Result<NonZeroU32, E> {
        match self {
            LeaseDuration::Seconds(val) => NonZeroU32::new(
                u32::try_from(val).map_err(|_| E::custom("duration value too large"))?,
            )
            .ok_or_else(|| E::custom("duration cannot be zero")),
            LeaseDuration::String(s) => NonZeroU32::new(parse_duration(&s).map_err(E::custom)?)
                .ok_or_else(|| E::custom("duration cannot be zero")),
        }
    }
}

fn deserialize_duration<'de, D>(de: D) -> Result<NonZeroU32, D::Error>
where
    D: Deserializer<'de>,
{
    LeaseDuration::deserialize(de)?.into_nonzero()
}

fn deserialize_optional_duration<'de, D>(de: D) -> Result<Option<NonZeroU32>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<LeaseDuration>::deserialize(de)?
        .map(LeaseDuration::into_nonzero)
        .transpose()
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

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("3600s").unwrap(), 3600);
        assert_eq!(parse_duration("60s").unwrap(), 60);
        assert_eq!(parse_duration("1s").unwrap(), 1);

        assert_eq!(parse_duration("60m").unwrap(), 3600);
        assert_eq!(parse_duration("1m").unwrap(), 60);
        assert_eq!(parse_duration("90m").unwrap(), 5400);

        assert_eq!(parse_duration("24h").unwrap(), 86400);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("48h").unwrap(), 172800);
    }

    #[test]
    fn test_parse_duration_invalid_unit() {
        assert!(parse_duration("60d").is_err());
        assert!(parse_duration("60w").is_err());
        assert!(parse_duration("60x").is_err());
        assert!(parse_duration("60mins").is_err());
    }

    #[test]
    fn test_minmax() {
        let json = r#"{"default": 3600, "min": 1200, "max": 7200}"#;
        let minmax: MinMax = serde_json::from_str(json).unwrap();
        assert_eq!(minmax.default.get(), 3600);
        assert_eq!(minmax.min.unwrap().get(), 1200);
        assert_eq!(minmax.max.unwrap().get(), 7200);
    }

    #[test]
    fn test_minmax_strings() {
        let json = r#"{"default": "1h", "min": "20m", "max": "2h"}"#;
        let minmax: MinMax = serde_json::from_str(json).unwrap();
        assert_eq!(minmax.default.get(), 3600);
        assert_eq!(minmax.min.unwrap().get(), 1200);
        assert_eq!(minmax.max.unwrap().get(), 7200);
    }

    // --- Regression: legacy standalone configs still parse ---

    #[test]
    fn test_legacy_standalone_config_no_backend_mode() {
        // Config without backend_mode field should default to standalone
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
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.backend_mode, BackendMode::Standalone);
        assert!(cfg.nats.is_none());
    }

    #[test]
    fn test_explicit_standalone_config() {
        let yaml = r#"
backend_mode: standalone
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
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.backend_mode, BackendMode::Standalone);
        assert!(cfg.nats.is_none());
    }

    // --- NATS config wire parsing ---

    #[test]
    fn test_nats_config_wire_parse() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://127.0.0.1:4222"
    subject_prefix: "dora.cluster"
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
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.backend_mode, BackendMode::Nats);
        let nats = cfg.nats.as_ref().unwrap();
        assert_eq!(nats.servers, vec!["nats://127.0.0.1:4222"]);
        assert_eq!(nats.subject_prefix, "dora.cluster");
        assert_eq!(nats.contract_version, "1.0.0");
        assert_eq!(nats.security_mode, NatsSecurityMode::None);
        assert_eq!(nats.leases_bucket, DEFAULT_LEASES_BUCKET);
        assert_eq!(nats.host_options_bucket, DEFAULT_HOST_OPTIONS_BUCKET);
        assert_eq!(nats.lease_gc_interval_ms, DEFAULT_LEASE_GC_INTERVAL_MS);
        // Default subjects should be populated
        assert_eq!(nats.subjects.lease_upsert, "dora.cluster.lease.upsert");
        assert_eq!(nats.subjects.lease_release, "dora.cluster.lease.release");
    }

    #[test]
    fn test_nats_config_custom_subjects() {
        let yaml = r#"
backend_mode: nats
nats:
    servers:
        - "nats://nats1:4222"
    subject_prefix: "myorg.dhcp"
    contract_version: "1.0.0"
    leases_bucket: "myorg.leases"
    host_options_bucket: "myorg.hostopts"
    lease_gc_interval_ms: 15000
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
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.backend_mode, BackendMode::Nats);
        let nats = cfg.nats.as_ref().unwrap();
        assert_eq!(nats.subjects.lease_upsert, "myorg.dhcp.v1.lease.upsert");
        assert_eq!(nats.leases_bucket, "myorg.leases");
        assert_eq!(nats.host_options_bucket, "myorg.hostopts");
        assert_eq!(nats.lease_gc_interval_ms, 15000);
        assert_eq!(nats.security_mode, NatsSecurityMode::UserPassword);
        assert_eq!(nats.username.as_deref(), Some("dora"));
        assert_eq!(nats.password.as_deref(), Some("secret"));
        assert_eq!(nats.connect_timeout_ms, Some(5000));
        assert_eq!(nats.request_timeout_ms, Some(3000));
    }

    #[test]
    fn test_backend_mode_roundtrip() {
        // Verify BackendMode serializes/deserializes correctly
        let standalone: BackendMode = serde_json::from_str("\"standalone\"").unwrap();
        assert_eq!(standalone, BackendMode::Standalone);

        let nats: BackendMode = serde_json::from_str("\"nats\"").unwrap();
        assert_eq!(nats, BackendMode::Nats);

        let legacy_clustered = serde_json::from_str::<BackendMode>("\"clustered\"");
        assert!(legacy_clustered.is_err());

        let s = serde_json::to_string(&BackendMode::Nats).unwrap();
        assert_eq!(s, "\"nats\"");

        let s = serde_json::to_string(&BackendMode::Standalone).unwrap();
        assert_eq!(s, "\"standalone\"");
    }

    #[test]
    fn test_nats_security_mode_roundtrip() {
        let modes = [
            ("\"none\"", NatsSecurityMode::None),
            ("\"user_password\"", NatsSecurityMode::UserPassword),
            ("\"token\"", NatsSecurityMode::Token),
            ("\"nkey\"", NatsSecurityMode::Nkey),
            ("\"tls\"", NatsSecurityMode::Tls),
            ("\"creds_file\"", NatsSecurityMode::CredsFile),
        ];
        for (json, expected) in &modes {
            let parsed: NatsSecurityMode = serde_json::from_str(json).unwrap();
            assert_eq!(&parsed, expected);
            let serialized = serde_json::to_string(expected).unwrap();
            assert_eq!(&serialized, json);
        }
    }

    #[test]
    fn test_nats_subjects_defaults() {
        let subjects = NatsSubjects::default();
        assert_eq!(subjects.lease_upsert, "dora.cluster.lease.upsert");
        assert_eq!(subjects.lease_release, "dora.cluster.lease.release");
        assert_eq!(
            subjects.lease_snapshot_request,
            "dora.cluster.lease.snapshot.request"
        );
        assert_eq!(
            subjects.lease_snapshot_response,
            "dora.cluster.lease.snapshot.response"
        );
    }

    #[test]
    fn test_example_still_parses_with_new_fields() {
        // This is the original example.yaml regression test - ensure it still parses
        let cfg: Config = serde_yaml::from_str(EXAMPLE).unwrap();
        assert_eq!(cfg.backend_mode, BackendMode::Standalone);
        assert!(cfg.nats.is_none());
        // Still has the expected network
        assert!(
            cfg.networks
                .contains_key(&"192.168.5.0/24".parse().unwrap())
        );
    }
}
