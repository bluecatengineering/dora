//! Typed models and codecs for NATS coordination payloads.
//!
//! These structures match the contract defined in
//! `contracts/dhcp-nats-clustering.asyncapi.yaml` and provide
//! serialization/deserialization for all message types exchanged over NATS.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{CoordinationError, CoordinationResult};

// ---------------------------------------------------------------------------
// Protocol family
// ---------------------------------------------------------------------------

/// DHCP protocol family discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolFamily {
    /// DHCPv4
    #[serde(rename = "dhcpv4")]
    Dhcpv4,
    /// DHCPv6
    #[serde(rename = "dhcpv6")]
    Dhcpv6,
}

impl std::fmt::Display for ProtocolFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolFamily::Dhcpv4 => write!(f, "dhcpv4"),
            ProtocolFamily::Dhcpv6 => write!(f, "dhcpv6"),
        }
    }
}

// ---------------------------------------------------------------------------
// Lease state
// ---------------------------------------------------------------------------

/// Lease lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseState {
    Reserved,
    Leased,
    Probated,
    Released,
    Expired,
}

impl LeaseState {
    /// Returns true for states that represent an active binding (reserved or leased).
    pub fn is_active(&self) -> bool {
        matches!(self, LeaseState::Reserved | LeaseState::Leased)
    }
}

impl std::fmt::Display for LeaseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseState::Reserved => write!(f, "reserved"),
            LeaseState::Leased => write!(f, "leased"),
            LeaseState::Probated => write!(f, "probated"),
            LeaseState::Released => write!(f, "released"),
            LeaseState::Expired => write!(f, "expired"),
        }
    }
}

// ---------------------------------------------------------------------------
// Lease record
// ---------------------------------------------------------------------------

/// Canonical shared lease record for clustered allocators.
///
/// Matches the `LeaseRecord` schema in the AsyncAPI contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    /// Unique lease identifier.
    pub lease_id: String,
    /// Protocol family (dhcpv4 or dhcpv6).
    pub protocol_family: ProtocolFamily,
    /// Subnet in CIDR notation.
    pub subnet: String,
    /// Assigned IP address.
    pub ip_address: String,
    /// Client key for DHCPv4 (hex-encoded). Required for DHCPv4, absent for DHCPv6.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_key_v4: Option<String>,
    /// DUID for DHCPv6 (hex-encoded). Required for DHCPv6, absent for DHCPv4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duid: Option<String>,
    /// IAID for DHCPv6. Required for DHCPv6, absent for DHCPv4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iaid: Option<u32>,
    /// Current lease state.
    pub state: LeaseState,
    /// Lease expiration timestamp.
    pub expires_at: DateTime<Utc>,
    /// Optional probation-period end timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probation_until: Option<DateTime<Utc>>,
    /// Server that last wrote this record.
    pub server_id: String,
    /// Monotonic revision for optimistic conflict checks.
    pub revision: u64,
    /// Last-updated timestamp.
    pub updated_at: DateTime<Utc>,
}

impl LeaseRecord {
    /// Validate protocol-family-specific field requirements.
    pub fn validate(&self) -> CoordinationResult<()> {
        match self.protocol_family {
            ProtocolFamily::Dhcpv4 => {
                if self.client_key_v4.is_none() {
                    return Err(CoordinationError::Codec(
                        "DHCPv4 lease record requires client_key_v4".into(),
                    ));
                }
            }
            ProtocolFamily::Dhcpv6 => {
                if self.duid.is_none() {
                    return Err(CoordinationError::Codec(
                        "DHCPv6 lease record requires duid".into(),
                    ));
                }
                if self.iaid.is_none() {
                    return Err(CoordinationError::Codec(
                        "DHCPv6 lease record requires iaid".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Lease snapshot messages
// ---------------------------------------------------------------------------

/// Request for a lease snapshot/convergence exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseSnapshotRequest {
    pub request_id: String,
    pub server_id: String,
    pub sent_at: DateTime<Utc>,
}

/// Response carrying a lease snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseSnapshotResponse {
    pub request_id: String,
    pub server_id: String,
    pub records: Vec<LeaseRecord>,
    pub sent_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Coordination events
// ---------------------------------------------------------------------------

/// Observable coordination event for audit/metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationEvent {
    pub event_id: String,
    pub event_type: CoordinationEventType,
    pub server_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_option_record_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub details: HashMap<String, String>,
}

/// Types of coordination events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationEventType {
    AllocationBlocked,
    RenewalAllowed,
    LookupHit,
    LookupMiss,
    LookupError,
    ConflictResolved,
}

impl std::fmt::Display for CoordinationEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoordinationEventType::AllocationBlocked => write!(f, "allocation_blocked"),
            CoordinationEventType::RenewalAllowed => write!(f, "renewal_allowed"),
            CoordinationEventType::LookupHit => write!(f, "lookup_hit"),
            CoordinationEventType::LookupMiss => write!(f, "lookup_miss"),
            CoordinationEventType::LookupError => write!(f, "lookup_error"),
            CoordinationEventType::ConflictResolved => write!(f, "conflict_resolved"),
        }
    }
}

// ---------------------------------------------------------------------------
// Host-option lookup outcome (caller-friendly enum)
// ---------------------------------------------------------------------------

/// Caller-friendly outcome from a host-option lookup.
///
/// Plugins receive this instead of raw NATS messages. A `Miss` or `Error`
/// does not imply the DHCP request should fail - the caller decides whether
/// to proceed without special options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostOptionOutcome {
    /// A matching host option was found.
    Hit {
        option_payload: HashMap<String, serde_json::Value>,
    },
    /// No matching host option exists.
    Miss,
    /// The lookup failed (timeout, transport, or protocol error).
    Error { message: String },
}

// ---------------------------------------------------------------------------
// Codec helpers
// ---------------------------------------------------------------------------

/// Encode a model value to JSON bytes for NATS transport.
pub fn encode<T: Serialize>(value: &T) -> CoordinationResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|e| CoordinationError::Codec(e.to_string()))
}

/// Decode JSON bytes from NATS transport into a typed model.
pub fn decode<T: for<'de> Deserialize<'de>>(data: &[u8]) -> CoordinationResult<T> {
    serde_json::from_slice(data).map_err(|e| CoordinationError::Codec(e.to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_v4_lease() -> LeaseRecord {
        LeaseRecord {
            lease_id: "lease-001".into(),
            protocol_family: ProtocolFamily::Dhcpv4,
            subnet: "192.168.1.0/24".into(),
            ip_address: "192.168.1.100".into(),
            client_key_v4: Some("aabbccdd".into()),
            duid: None,
            iaid: None,
            state: LeaseState::Leased,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            probation_until: None,
            server_id: "server-1".into(),
            revision: 1,
            updated_at: Utc::now(),
        }
    }

    fn sample_v6_lease() -> LeaseRecord {
        LeaseRecord {
            lease_id: "lease-v6-001".into(),
            protocol_family: ProtocolFamily::Dhcpv6,
            subnet: "2001:db8::/64".into(),
            ip_address: "2001:db8::100".into(),
            client_key_v4: None,
            duid: Some("00010001aabbccdd".into()),
            iaid: Some(1),
            state: LeaseState::Reserved,
            expires_at: Utc::now() + chrono::Duration::hours(2),
            probation_until: None,
            server_id: "server-2".into(),
            revision: 0,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn test_lease_record_v4_roundtrip() {
        let lease = sample_v4_lease();
        let bytes = encode(&lease).unwrap();
        let decoded: LeaseRecord = decode(&bytes).unwrap();
        assert_eq!(decoded.lease_id, lease.lease_id);
        assert_eq!(decoded.protocol_family, ProtocolFamily::Dhcpv4);
        assert_eq!(decoded.client_key_v4, Some("aabbccdd".into()));
        assert_eq!(decoded.state, LeaseState::Leased);
        assert_eq!(decoded.revision, 1);
    }

    #[test]
    fn test_lease_record_v6_roundtrip() {
        let lease = sample_v6_lease();
        let bytes = encode(&lease).unwrap();
        let decoded: LeaseRecord = decode(&bytes).unwrap();
        assert_eq!(decoded.lease_id, lease.lease_id);
        assert_eq!(decoded.protocol_family, ProtocolFamily::Dhcpv6);
        assert_eq!(decoded.duid, Some("00010001aabbccdd".into()));
        assert_eq!(decoded.iaid, Some(1));
        assert_eq!(decoded.state, LeaseState::Reserved);
    }

    #[test]
    fn test_lease_record_validate_v4_missing_client_key() {
        let mut lease = sample_v4_lease();
        lease.client_key_v4 = None;
        assert!(lease.validate().is_err());
    }

    #[test]
    fn test_lease_record_validate_v6_missing_duid() {
        let mut lease = sample_v6_lease();
        lease.duid = None;
        assert!(lease.validate().is_err());
    }

    #[test]
    fn test_lease_record_validate_v6_missing_iaid() {
        let mut lease = sample_v6_lease();
        lease.iaid = None;
        assert!(lease.validate().is_err());
    }

    #[test]
    fn test_lease_record_validate_ok() {
        assert!(sample_v4_lease().validate().is_ok());
        assert!(sample_v6_lease().validate().is_ok());
    }

    #[test]
    fn test_lease_state_is_active() {
        assert!(LeaseState::Reserved.is_active());
        assert!(LeaseState::Leased.is_active());
        assert!(!LeaseState::Probated.is_active());
        assert!(!LeaseState::Released.is_active());
        assert!(!LeaseState::Expired.is_active());
    }

    #[test]
    fn test_snapshot_request_roundtrip() {
        let req = LeaseSnapshotRequest {
            request_id: "snap-001".into(),
            server_id: "server-1".into(),
            sent_at: Utc::now(),
        };
        let bytes = encode(&req).unwrap();
        let decoded: LeaseSnapshotRequest = decode(&bytes).unwrap();
        assert_eq!(decoded.request_id, "snap-001");
        assert_eq!(decoded.server_id, "server-1");
    }

    #[test]
    fn test_snapshot_response_roundtrip() {
        let resp = LeaseSnapshotResponse {
            request_id: "snap-001".into(),
            server_id: "server-2".into(),
            records: vec![sample_v4_lease(), sample_v6_lease()],
            sent_at: Utc::now(),
        };
        let bytes = encode(&resp).unwrap();
        let decoded: LeaseSnapshotResponse = decode(&bytes).unwrap();
        assert_eq!(decoded.records.len(), 2);
        assert_eq!(decoded.records[0].protocol_family, ProtocolFamily::Dhcpv4);
        assert_eq!(decoded.records[1].protocol_family, ProtocolFamily::Dhcpv6);
    }

    #[test]
    fn test_coordination_event_roundtrip() {
        let mut details = HashMap::new();
        details.insert("reason".into(), "nats_unreachable".into());
        let event = CoordinationEvent {
            event_id: "evt-001".into(),
            event_type: CoordinationEventType::AllocationBlocked,
            server_id: "server-1".into(),
            lease_id: None,
            host_option_record_id: None,
            occurred_at: Utc::now(),
            details,
        };
        let bytes = encode(&event).unwrap();
        let decoded: CoordinationEvent = decode(&bytes).unwrap();
        assert_eq!(decoded.event_type, CoordinationEventType::AllocationBlocked);
        assert_eq!(decoded.details.get("reason").unwrap(), "nats_unreachable");
    }

    #[test]
    fn test_protocol_family_display() {
        assert_eq!(ProtocolFamily::Dhcpv4.to_string(), "dhcpv4");
        assert_eq!(ProtocolFamily::Dhcpv6.to_string(), "dhcpv6");
    }

    #[test]
    fn test_lease_state_display() {
        assert_eq!(LeaseState::Reserved.to_string(), "reserved");
        assert_eq!(LeaseState::Leased.to_string(), "leased");
        assert_eq!(LeaseState::Probated.to_string(), "probated");
        assert_eq!(LeaseState::Released.to_string(), "released");
        assert_eq!(LeaseState::Expired.to_string(), "expired");
    }

    #[test]
    fn test_decode_invalid_json() {
        let bad = b"not json at all";
        let result: CoordinationResult<LeaseRecord> = decode(bad);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CoordinationError::Codec(_)));
    }

    #[test]
    fn test_v4_lease_json_has_no_duid_iaid() {
        let lease = sample_v4_lease();
        let json_str = serde_json::to_string(&lease).unwrap();
        // duid and iaid should be absent due to skip_serializing_if
        assert!(!json_str.contains("\"duid\""));
        assert!(!json_str.contains("\"iaid\""));
        assert!(json_str.contains("\"client_key_v4\""));
    }

    #[test]
    fn test_v6_lease_json_has_no_client_key_v4() {
        let lease = sample_v6_lease();
        let json_str = serde_json::to_string(&lease).unwrap();
        assert!(!json_str.contains("\"client_key_v4\""));
        assert!(json_str.contains("\"duid\""));
        assert!(json_str.contains("\"iaid\""));
    }
}
