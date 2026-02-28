#![warn(
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![allow(clippy::cognitive_complexity)]

//! Host-option sync plugin for clustered DHCP.
//!
//! This plugin performs host-specific option lookups via NATS coordination
//! and enriches DHCP responses with matching special options (e.g. boot/provision
//! directives).
//!
//! ## Identity Resolution
//!
//! For DHCPv4: client identifier (option 61) first, MAC address fallback.
//! For DHCPv6: DUID from client-id option.
//!
//! ## Failure Semantics
//!
//! Lookup miss, error, or timeout never blocks normal DHCP response generation.
//! The plugin logs the outcome and continues without injecting special options.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use lazy_static::lazy_static;
use prometheus::{IntCounter, register_int_counter};

use dora_core::{
    async_trait,
    dhcproto::{
        v4::{self, DhcpOption, Message, OptionCode},
        v6,
    },
    handler::{Action, Plugin},
    prelude::*,
    tracing::{debug, info, warn},
};

// Plugin-local metrics with lazy initialization.
lazy_static! {
    /// Count of host-option lookup hits
    static ref HOST_OPTION_LOOKUP_HIT: IntCounter = register_int_counter!(
        "host_option_lookup_hit",
        "count of host-option lookup hits"
    ).unwrap();

    /// Count of host-option lookup misses
    static ref HOST_OPTION_LOOKUP_MISS: IntCounter = register_int_counter!(
        "host_option_lookup_miss",
        "count of host-option lookup misses"
    ).unwrap();

    /// Count of host-option lookup errors (including timeouts)
    static ref HOST_OPTION_LOOKUP_ERROR: IntCounter = register_int_counter!(
        "host_option_lookup_error",
        "count of host-option lookup errors/timeouts"
    ).unwrap();
}

use nats_coordination::{HostOptionClient, HostOptionOutcome, ProtocolFamily};

// ---------------------------------------------------------------------------
// Identity resolution helpers (T022)
// ---------------------------------------------------------------------------

/// Resolved host identity for option lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentity {
    /// Client identifier (option 61 for v4, hex-encoded).
    pub client_identifier: Option<String>,
    /// MAC address (hex-encoded, colon-separated).
    pub mac_address: Option<String>,
    /// DHCPv6 DUID (hex-encoded).
    pub duid: Option<String>,
    /// DHCPv6 IAID.
    pub iaid: Option<u32>,
}

/// Extract host identity from a DHCPv4 message.
///
/// Precedence: client identifier (option 61) first, then MAC (chaddr) fallback.
/// Both are always populated when available, but the lookup service uses
/// client_identifier with higher priority.
pub fn resolve_v4_identity(msg: &Message) -> HostIdentity {
    let client_identifier = msg
        .opts()
        .get(OptionCode::ClientIdentifier)
        .and_then(|opt| {
            if let DhcpOption::ClientIdentifier(id) = opt {
                Some(hex::encode(id))
            } else {
                None
            }
        });

    let chaddr = msg.chaddr();
    let mac_address = if chaddr.len() >= 6 && chaddr.iter().any(|b| *b != 0) {
        Some(format_mac(chaddr))
    } else {
        None
    };

    HostIdentity {
        client_identifier,
        mac_address,
        duid: None,
        iaid: None,
    }
}

/// Extract host identity from a DHCPv6 message.
///
/// Uses the DUID from the ClientId option. IAID is extracted from the
/// first IA_NA or IA_PD option if present.
pub fn resolve_v6_identity(msg: &v6::Message) -> HostIdentity {
    let duid = msg.opts().get(v6::OptionCode::ClientId).and_then(|opt| {
        if let v6::DhcpOption::ClientId(id) = opt {
            Some(hex::encode(id))
        } else {
            None
        }
    });

    // Extract IAID from IA_NA if present
    let iaid = msg.opts().get(v6::OptionCode::IANA).and_then(|opt| {
        if let v6::DhcpOption::IANA(iana) = opt {
            Some(iana.id)
        } else {
            None
        }
    });

    HostIdentity {
        client_identifier: None,
        mac_address: None,
        duid,
        iaid,
    }
}

/// Format a hardware address as colon-separated hex.
fn format_mac(chaddr: &[u8]) -> String {
    chaddr
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

// ---------------------------------------------------------------------------
// Response enrichment (T024)
// ---------------------------------------------------------------------------

/// Apply host-option payload to a DHCPv4 response message.
///
/// The payload is a map of string keys to JSON values. Known keys are mapped
/// to specific DHCPv4 options. Unknown keys are logged and skipped.
///
/// This function is idempotent: if the option is already set (e.g. by range
/// config), the host-specific value takes precedence and overwrites it.
pub fn enrich_v4_response(
    resp: &mut v4::Message,
    payload: &HashMap<String, serde_json::Value>,
) -> usize {
    let mut injected = 0;

    for (key, value) in payload {
        match key.as_str() {
            "boot_file" | "bootfile" | "filename" => {
                if let Some(s) = value.as_str() {
                    resp.set_fname_str(s);
                    injected += 1;
                    debug!(key, value = s, "injected boot_file into v4 response");
                }
            }
            "next_server" | "siaddr" => {
                if let Some(s) = value.as_str() {
                    if let Ok(ip) = s.parse::<std::net::Ipv4Addr>() {
                        resp.set_siaddr(ip);
                        injected += 1;
                        debug!(key, value = s, "injected next_server into v4 response");
                    }
                }
            }
            "server_name" | "sname" => {
                if let Some(s) = value.as_str() {
                    resp.set_sname_str(s);
                    injected += 1;
                    debug!(key, value = s, "injected server_name into v4 response");
                }
            }
            "tftp_server" => {
                // Map to sname header field (standard BOOTP/DHCP TFTP server)
                if let Some(s) = value.as_str() {
                    resp.set_sname_str(s);
                    injected += 1;
                    debug!(key, value = s, "injected tftp_server into v4 sname field");
                }
            }
            "bootfile_name" => {
                // Map to fname header field (standard BOOTP/DHCP bootfile)
                if let Some(s) = value.as_str() {
                    resp.set_fname_str(s);
                    injected += 1;
                    debug!(key, value = s, "injected bootfile_name into v4 fname field");
                }
            }
            _ => {
                debug!(key, "unknown host-option key, skipping");
            }
        }
    }

    injected
}

/// Apply host-option payload to a DHCPv6 response message.
///
/// For DHCPv6, the payload typically carries vendor-specific or boot-related
/// information. Known keys are mapped; unknown keys are skipped.
pub fn enrich_v6_response(
    resp: &mut v6::Message,
    payload: &HashMap<String, serde_json::Value>,
) -> usize {
    let mut injected = 0;

    for (key, value) in payload {
        match key.as_str() {
            "bootfile_url" | "boot_file_url" => {
                if let Some(s) = value.as_str() {
                    // OPT_BOOTFILE_URL = 59 (RFC 5970)
                    resp.opts_mut()
                        .insert(v6::DhcpOption::Unknown(v6::UnknownOption::new(
                            v6::OptionCode::from(59u16),
                            s.as_bytes().to_vec(),
                        )));
                    injected += 1;
                    debug!(key, value = s, "injected bootfile_url into v6 response");
                }
            }
            "bootfile_param" | "boot_file_param" => {
                if let Some(s) = value.as_str() {
                    // OPT_BOOTFILE_PARAM = 60 (RFC 5970)
                    resp.opts_mut()
                        .insert(v6::DhcpOption::Unknown(v6::UnknownOption::new(
                            v6::OptionCode::from(60u16),
                            s.as_bytes().to_vec(),
                        )));
                    injected += 1;
                    debug!(key, value = s, "injected bootfile_param into v6 response");
                }
            }
            _ => {
                debug!(key, "unknown host-option key for v6, skipping");
            }
        }
    }

    injected
}

// ---------------------------------------------------------------------------
// Metrics (T025)
// ---------------------------------------------------------------------------

/// Record a host-option lookup outcome in metrics.
fn record_lookup_metric(outcome: &HostOptionOutcome) {
    match outcome {
        HostOptionOutcome::Hit { .. } => HOST_OPTION_LOOKUP_HIT.inc(),
        HostOptionOutcome::Miss => HOST_OPTION_LOOKUP_MISS.inc(),
        HostOptionOutcome::Error { .. } => HOST_OPTION_LOOKUP_ERROR.inc(),
    }
}

// ---------------------------------------------------------------------------
// Plugin struct (T021, T023, T024, T025, T026)
// ---------------------------------------------------------------------------

/// Host-option sync plugin for clustered DHCP.
///
/// Performs host-specific option lookups via NATS and enriches DHCP responses.
/// Registered for both v4 and v6 message pipelines.
///
/// If lookup fails (miss/error/timeout), normal DHCP processing continues
/// without special options.
pub struct HostOptionSync {
    host_option_client: HostOptionClient,
}

impl fmt::Debug for HostOptionSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostOptionSync").finish()
    }
}

impl HostOptionSync {
    /// Create a new host-option sync plugin.
    pub fn new(host_option_client: HostOptionClient) -> Self {
        Self { host_option_client }
    }
}

// ---------------------------------------------------------------------------
// DHCPv4 Plugin implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Plugin<Message> for HostOptionSync {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<Message>) -> Result<Action> {
        // Only enrich responses that are being built (resp_msg exists)
        if ctx.resp_msg().is_none() {
            return Ok(Action::Continue);
        }

        // Extract identity
        let identity = resolve_v4_identity(ctx.msg());

        // We need at least one identity field to do a lookup
        if identity.client_identifier.is_none() && identity.mac_address.is_none() {
            debug!("no client identity available for host-option lookup, skipping");
            return Ok(Action::Continue);
        }

        // Determine subnet for scope checking
        let subnet = match ctx.subnet() {
            Ok(s) => s.to_string(),
            Err(_) => {
                debug!("cannot determine subnet for host-option lookup, skipping");
                return Ok(Action::Continue);
            }
        };

        // Perform lookup
        let outcome = self
            .host_option_client
            .lookup(
                ProtocolFamily::Dhcpv4,
                &subnet,
                identity.client_identifier.as_deref(),
                identity.mac_address.as_deref(),
                None,
                None,
            )
            .await;

        // Record metrics
        record_lookup_metric(&outcome);

        // Process outcome
        match outcome {
            HostOptionOutcome::Hit { option_payload } => {
                info!(
                    client_id = ?identity.client_identifier,
                    mac = ?identity.mac_address,
                    "host-option lookup hit, enriching v4 response"
                );
                if let Some(resp) = ctx.resp_msg_mut() {
                    let count = enrich_v4_response(resp, &option_payload);
                    debug!(options_injected = count, "v4 response enrichment complete");
                }
            }
            HostOptionOutcome::Miss => {
                debug!(
                    client_id = ?identity.client_identifier,
                    mac = ?identity.mac_address,
                    "host-option lookup miss, continuing without special options"
                );
            }
            HostOptionOutcome::Error { message } => {
                warn!(
                    error = %message,
                    client_id = ?identity.client_identifier,
                    mac = ?identity.mac_address,
                    "host-option lookup error, continuing without special options"
                );
            }
        }

        Ok(Action::Continue)
    }
}

// ---------------------------------------------------------------------------
// DHCPv6 Plugin implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Plugin<v6::Message> for HostOptionSync {
    #[instrument(level = "debug", skip_all)]
    async fn handle(&self, ctx: &mut MsgContext<v6::Message>) -> Result<Action> {
        // Only enrich responses that are being built
        if ctx.resp_msg().is_none() {
            return Ok(Action::Continue);
        }

        // Extract identity
        let identity = resolve_v6_identity(ctx.msg());

        // We need at least a DUID to do a lookup
        if identity.duid.is_none() {
            debug!("no DUID available for host-option v6 lookup, skipping");
            return Ok(Action::Continue);
        }

        // Use global unicast address for subnet scope if available
        let subnet = ctx
            .global()
            .map(|g| g.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Perform lookup
        let outcome = self
            .host_option_client
            .lookup(
                ProtocolFamily::Dhcpv6,
                &subnet,
                None,
                None,
                identity.duid.as_deref(),
                identity.iaid,
            )
            .await;

        // Record metrics
        record_lookup_metric(&outcome);

        // Process outcome
        match outcome {
            HostOptionOutcome::Hit { option_payload } => {
                info!(
                    duid = ?identity.duid,
                    "host-option lookup hit, enriching v6 response"
                );
                if let Some(resp) = ctx.resp_msg_mut() {
                    let count = enrich_v6_response(resp, &option_payload);
                    debug!(options_injected = count, "v6 response enrichment complete");
                }
            }
            HostOptionOutcome::Miss => {
                debug!(
                    duid = ?identity.duid,
                    "host-option v6 lookup miss, continuing without special options"
                );
            }
            HostOptionOutcome::Error { message } => {
                warn!(
                    error = %message,
                    duid = ?identity.duid,
                    "host-option v6 lookup error, continuing without special options"
                );
            }
        }

        Ok(Action::Continue)
    }
}

// ---------------------------------------------------------------------------
// Register implementation (T021, T026)
// ---------------------------------------------------------------------------

// We manually implement Register for both v4 and v6 since the plugin needs
// to be registered in both pipelines but uses a single shared struct.
// The plugin runs after leases (for v4) and after MsgType (for v6).

impl dora_core::Register<Message> for HostOptionSync {
    fn register(self, srv: &mut dora_core::Server<Message>) {
        info!("HostOptionSync v4 plugin registered");
        let this = Arc::new(self);
        srv.plugin_order::<Self, _>(this, &[std::any::TypeId::of::<nats_leases::NatsV4Leases>()]);
    }
}

impl dora_core::Register<v6::Message> for HostOptionSync {
    fn register(self, srv: &mut dora_core::Server<v6::Message>) {
        info!("HostOptionSync v6 plugin registered");
        let this = Arc::new(self);
        srv.plugin_order::<Self, _>(
            this,
            &[
                std::any::TypeId::of::<message_type::MsgType>(),
                std::any::TypeId::of::<nats_leases::NatsV6Leases>(),
            ],
        );
    }
}

// ---------------------------------------------------------------------------
// Tests (T027)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // ---- Identity resolution tests (T022) ----

    #[test]
    fn test_v4_identity_client_id_takes_precedence() {
        let mut msg = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        );
        msg.opts_mut()
            .insert(DhcpOption::ClientIdentifier(vec![0x01, 0x02, 0x03]));

        let identity = resolve_v4_identity(&msg);
        assert_eq!(identity.client_identifier, Some("010203".to_string()));
        assert_eq!(identity.mac_address, Some("aa:bb:cc:dd:ee:ff".to_string()));
        assert!(identity.duid.is_none());
        assert!(identity.iaid.is_none());
    }

    #[test]
    fn test_v4_identity_mac_fallback() {
        let msg = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff],
        );
        // No client identifier option

        let identity = resolve_v4_identity(&msg);
        assert!(identity.client_identifier.is_none());
        assert_eq!(identity.mac_address, Some("aa:bb:cc:dd:ee:ff".to_string()));
    }

    #[test]
    fn test_v4_identity_no_mac_no_client_id() {
        let msg = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );

        let identity = resolve_v4_identity(&msg);
        assert!(identity.client_identifier.is_none());
        assert!(identity.mac_address.is_none());
    }

    #[test]
    fn test_v6_identity_with_duid() {
        let mut msg = v6::Message::new(v6::MessageType::Solicit);
        msg.opts_mut()
            .insert(v6::DhcpOption::ClientId(vec![0x00, 0x01, 0xaa, 0xbb]));

        let identity = resolve_v6_identity(&msg);
        assert_eq!(identity.duid, Some("0001aabb".to_string()));
        assert!(identity.client_identifier.is_none());
        assert!(identity.mac_address.is_none());
    }

    #[test]
    fn test_v6_identity_no_duid() {
        let msg = v6::Message::new(v6::MessageType::Solicit);

        let identity = resolve_v6_identity(&msg);
        assert!(identity.duid.is_none());
        assert!(identity.iaid.is_none());
    }

    // ---- Response enrichment tests (T024) ----

    #[test]
    fn test_enrich_v4_boot_file() {
        let mut resp = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        );
        let mut payload = HashMap::new();
        payload.insert(
            "boot_file".to_string(),
            serde_json::Value::String("pxelinux.0".into()),
        );
        payload.insert(
            "next_server".to_string(),
            serde_json::Value::String("10.0.0.1".into()),
        );

        let count = enrich_v4_response(&mut resp, &payload);
        assert_eq!(count, 2);
        // The boot file is set via fname header
        assert_eq!(resp.fname().unwrap_or(b""), b"pxelinux.0");
        assert_eq!(
            resp.siaddr(),
            "10.0.0.1".parse::<std::net::Ipv4Addr>().unwrap()
        );
    }

    #[test]
    fn test_enrich_v4_tftp_server_to_sname() {
        let mut resp = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        );
        let mut payload = HashMap::new();
        payload.insert(
            "tftp_server".to_string(),
            serde_json::Value::String("tftp.example.com".into()),
        );

        let count = enrich_v4_response(&mut resp, &payload);
        assert_eq!(count, 1);
        // Check the TFTP server name was set in the sname field
        assert_eq!(resp.sname().unwrap_or(b""), b"tftp.example.com");
    }

    #[test]
    fn test_enrich_v4_unknown_key_skipped() {
        let mut resp = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        );
        let mut payload = HashMap::new();
        payload.insert(
            "unknown_option".to_string(),
            serde_json::Value::String("value".into()),
        );

        let count = enrich_v4_response(&mut resp, &payload);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_enrich_v4_empty_payload() {
        let mut resp = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        );
        let payload = HashMap::new();

        let count = enrich_v4_response(&mut resp, &payload);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_enrich_v4_idempotent() {
        let mut resp = v4::Message::new(
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            std::net::Ipv4Addr::UNSPECIFIED,
            &[1, 2, 3, 4, 5, 6],
        );
        let mut payload = HashMap::new();
        payload.insert(
            "boot_file".to_string(),
            serde_json::Value::String("pxelinux.0".into()),
        );

        // Apply twice
        enrich_v4_response(&mut resp, &payload);
        let count = enrich_v4_response(&mut resp, &payload);
        assert_eq!(count, 1);
        assert_eq!(resp.fname().unwrap_or(b""), b"pxelinux.0");
    }

    #[test]
    fn test_enrich_v6_bootfile_url() {
        let mut resp = v6::Message::new(v6::MessageType::Reply);
        let mut payload = HashMap::new();
        payload.insert(
            "bootfile_url".to_string(),
            serde_json::Value::String("http://boot.example.com/image".into()),
        );

        let count = enrich_v6_response(&mut resp, &payload);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_enrich_v6_empty_payload() {
        let mut resp = v6::Message::new(v6::MessageType::Reply);
        let payload = HashMap::new();

        let count = enrich_v6_response(&mut resp, &payload);
        assert_eq!(count, 0);
    }

    // ---- Metrics recording tests (T025) ----

    #[test]
    fn test_record_lookup_metric_hit() {
        let outcome = HostOptionOutcome::Hit {
            option_payload: HashMap::new(),
        };
        // Should not panic
        record_lookup_metric(&outcome);
    }

    #[test]
    fn test_record_lookup_metric_miss() {
        let outcome = HostOptionOutcome::Miss;
        record_lookup_metric(&outcome);
    }

    #[test]
    fn test_record_lookup_metric_error() {
        let outcome = HostOptionOutcome::Error {
            message: "test".into(),
        };
        record_lookup_metric(&outcome);
    }

    // ---- Format helpers ----

    #[test]
    fn test_format_mac() {
        let mac = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(format_mac(&mac), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn test_format_mac_short() {
        let mac = [0x01, 0x02, 0x03];
        assert_eq!(format_mac(&mac), "01:02:03");
    }
}
