//! Host-option lookup client API with hit/miss/error outcomes and bounded
//! timeout behavior.
//!
//! Timeout/error does NOT imply DHCP request failure. The caller decides
//! whether to proceed without special options.

use std::collections::HashMap;

use tracing::{debug, info, warn};

use crate::client::NatsClient;
use crate::error::CoordinationError;
use crate::models::{HostOptionOutcome, ProtocolFamily};

/// Host-option lookup client.
///
/// Wraps the NATS request/reply flow for host-specific option lookups,
/// with correlation IDs, timeout enforcement, and outcome classification.
#[derive(Debug, Clone)]
pub struct HostOptionClient {
    nats_client: NatsClient,
}

impl HostOptionClient {
    /// Create a new host-option lookup client.
    pub fn new(nats_client: NatsClient) -> Self {
        Self { nats_client }
    }

    /// Perform a host-option lookup.
    ///
    /// Returns a caller-friendly `HostOptionOutcome` that classifies the result
    /// as hit, miss, or error. Timeout and transport failures are mapped to
    /// `HostOptionOutcome::Error` rather than propagated as hard failures.
    pub async fn lookup(
        &self,
        protocol_family: ProtocolFamily,
        subnet: &str,
        client_identifier: Option<&str>,
        mac_address: Option<&str>,
        duid: Option<&str>,
        iaid: Option<u32>,
    ) -> HostOptionOutcome {
        let request_id = uuid::Uuid::new_v4().to_string();

        debug!(
            request_id = %request_id,
            protocol = %protocol_family,
            subnet,
            "performing host-option lookup from JetStream KV"
        );

        let bucket = self.nats_client.host_options_bucket().await;
        let store = match self.nats_client.get_or_create_kv_bucket(&bucket, 1).await {
            Ok(store) => store,
            Err(CoordinationError::NotConnected(msg)) => {
                warn!(request_id = %request_id, "host-option lookup failed: not connected");
                return HostOptionOutcome::Error {
                    message: format!("not connected: {msg}"),
                };
            }
            Err(e) => {
                warn!(
                    request_id = %request_id,
                    error = %e,
                    bucket,
                    "host-option lookup failed to open KV bucket"
                );
                return HostOptionOutcome::Error {
                    message: format!("kv bucket error: {e}"),
                };
            }
        };

        let keys = candidate_keys(
            protocol_family,
            subnet,
            client_identifier,
            mac_address,
            duid,
            iaid,
        );

        for key in keys {
            match store.get(key.clone()).await {
                Ok(Some(bytes)) => {
                    match serde_json::from_slice::<HashMap<String, serde_json::Value>>(&bytes) {
                        Ok(option_payload) => {
                            info!(request_id = %request_id, key, "host-option lookup hit");
                            return HostOptionOutcome::Hit { option_payload };
                        }
                        Err(e) => {
                            warn!(request_id = %request_id, key, error = %e, "invalid host-option payload JSON in KV");
                            return HostOptionOutcome::Error {
                                message: format!("invalid host-option payload: {e}"),
                            };
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(request_id = %request_id, key, error = %e, "host-option KV read error");
                    return HostOptionOutcome::Error {
                        message: format!("kv read error: {e}"),
                    };
                }
            }
        }

        debug!(request_id = %request_id, "host-option lookup miss");
        HostOptionOutcome::Miss
    }
}

fn normalize_mac(mac: &str) -> String {
    mac.trim().to_ascii_lowercase()
}

fn sanitize_key_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn candidate_keys(
    protocol_family: ProtocolFamily,
    subnet: &str,
    client_identifier: Option<&str>,
    mac_address: Option<&str>,
    duid: Option<&str>,
    iaid: Option<u32>,
) -> Vec<String> {
    match protocol_family {
        ProtocolFamily::Dhcpv4 => {
            let mut out = Vec::new();
            let subnet = sanitize_key_component(subnet);
            if let Some(client_id) = client_identifier {
                let client_id = sanitize_key_component(client_id);
                out.push(format!("v4/{subnet}/client-id/{client_id}"));
                out.push(format!("v4/client-id/{client_id}"));
            }
            if let Some(mac) = mac_address {
                let mac = sanitize_key_component(&normalize_mac(mac));
                out.push(format!("v4/{subnet}/mac/{mac}"));
                out.push(format!("v4/mac/{mac}"));
            }
            out
        }
        ProtocolFamily::Dhcpv6 => {
            let mut out = Vec::new();
            let subnet = sanitize_key_component(subnet);
            if let Some(duid) = duid {
                let duid = sanitize_key_component(duid);
                if let Some(iaid) = iaid {
                    out.push(format!("v6/{subnet}/duid/{duid}/iaid/{iaid}"));
                    out.push(format!("v6/duid/{duid}/iaid/{iaid}"));
                }
                out.push(format!("v6/{subnet}/duid/{duid}"));
                out.push(format!("v6/duid/{duid}"));
            }
            out
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn test_nats_client() -> NatsClient {
        let config = config::NatsConfig {
            servers: vec!["nats://127.0.0.1:4222".into()],
            subject_prefix: "test".into(),
            subjects: config::wire::NatsSubjects::default(),
            leases_bucket: "test_leases".into(),
            host_options_bucket: "test_hostopts".into(),
            lease_gc_interval: Duration::from_secs(10),
            coordination_state_poll_interval: Duration::from_millis(500),
            contract_version: "1.0.0".into(),
            security_mode: config::wire::NatsSecurityMode::None,
            username: None,
            password: None,
            token: None,
            nkey_seed_path: None,
            tls_cert_path: None,
            tls_key_path: None,
            tls_ca_path: None,
            creds_file_path: None,
            connect_timeout: Some(Duration::from_secs(1)),
            request_timeout: Some(Duration::from_millis(200)),
        };
        let resolver = crate::subjects::SubjectResolver::with_defaults();
        NatsClient::new(config, resolver)
    }

    #[tokio::test]
    async fn test_lookup_not_connected_returns_error() {
        let client = HostOptionClient::new(test_nats_client());
        let outcome = client
            .lookup(
                ProtocolFamily::Dhcpv4,
                "10.0.0.0/24",
                Some("client-id"),
                Some("aa:bb:cc:dd:ee:ff"),
                None,
                None,
            )
            .await;

        match outcome {
            HostOptionOutcome::Error { message } => {
                assert!(message.contains("not connected"));
            }
            other => panic!("expected Error outcome, got: {other:?}"),
        }
    }

    #[test]
    fn test_host_option_outcome_variants() {
        let hit = HostOptionOutcome::Hit {
            option_payload: HashMap::new(),
        };
        assert!(matches!(hit, HostOptionOutcome::Hit { .. }));

        let miss = HostOptionOutcome::Miss;
        assert!(matches!(miss, HostOptionOutcome::Miss));

        let err = HostOptionOutcome::Error {
            message: "test error".into(),
        };
        assert!(matches!(err, HostOptionOutcome::Error { .. }));
    }

    #[test]
    fn test_candidate_keys_v4() {
        let keys = candidate_keys(
            ProtocolFamily::Dhcpv4,
            "10.0.0.0/24",
            Some("abcd"),
            Some("AA:BB:CC:DD:EE:FF"),
            None,
            None,
        );
        assert_eq!(
            keys,
            vec![
                "v4/10.0.0.0_24/client-id/abcd",
                "v4/client-id/abcd",
                "v4/10.0.0.0_24/mac/aa_bb_cc_dd_ee_ff",
                "v4/mac/aa_bb_cc_dd_ee_ff"
            ]
        );
    }

    #[test]
    fn test_candidate_keys_v6() {
        let keys = candidate_keys(
            ProtocolFamily::Dhcpv6,
            "2001:db8::/64",
            None,
            None,
            Some("duidhex"),
            Some(42),
        );
        assert_eq!(
            keys,
            vec![
                "v6/2001_db8___64/duid/duidhex/iaid/42",
                "v6/duid/duidhex/iaid/42",
                "v6/2001_db8___64/duid/duidhex",
                "v6/duid/duidhex"
            ]
        );
    }

    #[test]
    fn test_candidate_keys_v6_without_iaid() {
        let keys = candidate_keys(
            ProtocolFamily::Dhcpv6,
            "2001:db8::/64",
            None,
            None,
            Some("duidhex"),
            None,
        );
        assert_eq!(
            keys,
            vec!["v6/2001_db8___64/duid/duidhex", "v6/duid/duidhex"]
        );
    }

    #[test]
    fn test_normalize_mac() {
        assert_eq!(normalize_mac("AA:BB:CC:DD:EE:FF"), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn test_sanitize_key_component() {
        assert_eq!(sanitize_key_component("2001:db8::/64"), "2001_db8___64");
    }
}
