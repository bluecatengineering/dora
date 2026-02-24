//! Contract-versioned subject resolver with configurable templates and defaults.
//!
//! Subject names are configurable per deployment. The resolver is a pure,
//! stateless translator from logical channel to concrete NATS subject string.
//! No hard-coded subject strings appear in lease/host-option runtime paths.

use config::wire::{DEFAULT_CONTRACT_VERSION, NatsSubjects};

use crate::error::{CoordinationError, CoordinationResult};

/// Logical coordination channels used by the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    LeaseUpsert,
    LeaseRelease,
    LeaseSnapshotRequest,
    LeaseSnapshotResponse,
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Channel::LeaseUpsert => write!(f, "lease_upsert"),
            Channel::LeaseRelease => write!(f, "lease_release"),
            Channel::LeaseSnapshotRequest => write!(f, "lease_snapshot_request"),
            Channel::LeaseSnapshotResponse => write!(f, "lease_snapshot_response"),
        }
    }
}

/// All logical channels, for iteration.
pub const ALL_CHANNELS: &[Channel] = &[
    Channel::LeaseUpsert,
    Channel::LeaseRelease,
    Channel::LeaseSnapshotRequest,
    Channel::LeaseSnapshotResponse,
];

/// Pure subject resolver: maps logical channels to concrete NATS subject strings.
///
/// Constructed from configuration. Validates that all subjects are non-empty
/// and contain no unresolved placeholders.
#[derive(Debug, Clone)]
pub struct SubjectResolver {
    subjects: NatsSubjects,
    contract_version: String,
}

impl SubjectResolver {
    /// Create a resolver from explicit subject configuration and contract version.
    ///
    /// Returns an error if any subject is empty or contains unresolved `{…}` placeholders.
    pub fn new(subjects: NatsSubjects, contract_version: String) -> CoordinationResult<Self> {
        let resolver = Self {
            subjects,
            contract_version,
        };
        resolver.validate()?;
        Ok(resolver)
    }

    /// Create a resolver using all defaults.
    pub fn with_defaults() -> Self {
        Self {
            subjects: NatsSubjects::default(),
            contract_version: DEFAULT_CONTRACT_VERSION.to_owned(),
        }
    }

    /// Create a resolver with a custom prefix, generating default subject templates
    /// from that prefix.
    pub fn with_prefix(prefix: &str) -> CoordinationResult<Self> {
        let subjects = NatsSubjects {
            lease_upsert: format!("{prefix}.lease.upsert"),
            lease_release: format!("{prefix}.lease.release"),
            lease_snapshot_request: format!("{prefix}.lease.snapshot.request"),
            lease_snapshot_response: format!("{prefix}.lease.snapshot.response"),
        };
        Self::new(subjects, DEFAULT_CONTRACT_VERSION.to_owned())
    }

    /// Resolve a logical channel to its concrete NATS subject string.
    pub fn resolve(&self, channel: Channel) -> &str {
        match channel {
            Channel::LeaseUpsert => &self.subjects.lease_upsert,
            Channel::LeaseRelease => &self.subjects.lease_release,
            Channel::LeaseSnapshotRequest => &self.subjects.lease_snapshot_request,
            Channel::LeaseSnapshotResponse => &self.subjects.lease_snapshot_response,
        }
    }

    /// Returns the contract version string.
    pub fn contract_version(&self) -> &str {
        &self.contract_version
    }

    /// Returns the underlying subjects configuration.
    pub fn subjects(&self) -> &NatsSubjects {
        &self.subjects
    }

    /// Validate that all subjects are non-empty and contain no unresolved `{…}` placeholders.
    fn validate(&self) -> CoordinationResult<()> {
        for channel in ALL_CHANNELS {
            let subject = self.resolve(*channel);
            if subject.trim().is_empty() {
                return Err(CoordinationError::Config(format!(
                    "subject for channel '{channel}' is empty"
                )));
            }
            if subject.contains('{') || subject.contains('}') {
                return Err(CoordinationError::Config(format!(
                    "subject for channel '{channel}' contains unresolved placeholder: {subject}"
                )));
            }
        }
        if self.contract_version.trim().is_empty() {
            return Err(CoordinationError::Config(
                "contract_version is empty".into(),
            ));
        }
        Ok(())
    }
}

impl Default for SubjectResolver {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use config::wire::DEFAULT_SUBJECT_PREFIX;

    #[test]
    fn test_default_subjects() {
        let resolver = SubjectResolver::with_defaults();
        assert_eq!(
            resolver.resolve(Channel::LeaseUpsert),
            "dora.cluster.lease.upsert"
        );
        assert_eq!(
            resolver.resolve(Channel::LeaseRelease),
            "dora.cluster.lease.release"
        );
        assert_eq!(
            resolver.resolve(Channel::LeaseSnapshotRequest),
            "dora.cluster.lease.snapshot.request"
        );
        assert_eq!(
            resolver.resolve(Channel::LeaseSnapshotResponse),
            "dora.cluster.lease.snapshot.response"
        );
        assert_eq!(resolver.contract_version(), "1.0.0");
    }

    #[test]
    fn test_custom_prefix() {
        let resolver = SubjectResolver::with_prefix("myorg.dhcp").unwrap();
        assert_eq!(
            resolver.resolve(Channel::LeaseUpsert),
            "myorg.dhcp.lease.upsert"
        );
        assert_eq!(
            resolver.resolve(Channel::LeaseRelease),
            "myorg.dhcp.lease.release"
        );
    }

    #[test]
    fn test_fully_custom_subjects() {
        let subjects = NatsSubjects {
            lease_upsert: "custom.lu".into(),
            lease_release: "custom.lr".into(),
            lease_snapshot_request: "custom.lsr".into(),
            lease_snapshot_response: "custom.lsresp".into(),
        };
        let resolver = SubjectResolver::new(subjects, "2.0.0".into()).unwrap();
        assert_eq!(resolver.resolve(Channel::LeaseUpsert), "custom.lu");
        assert_eq!(resolver.resolve(Channel::LeaseRelease), "custom.lr");
        assert_eq!(resolver.contract_version(), "2.0.0");
    }

    #[test]
    fn test_empty_subject_rejected() {
        let subjects = NatsSubjects {
            lease_upsert: "".into(),
            ..NatsSubjects::default()
        };
        let result = SubjectResolver::new(subjects, "1.0.0".into());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, CoordinationError::Config(_)));
        let msg = format!("{err}");
        assert!(msg.contains("lease_upsert"));
    }

    #[test]
    fn test_unresolved_placeholder_rejected() {
        let subjects = NatsSubjects {
            lease_upsert: "{prefix}.lease.upsert".into(),
            ..NatsSubjects::default()
        };
        let result = SubjectResolver::new(subjects, "1.0.0".into());
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("unresolved placeholder"));
    }

    #[test]
    fn test_empty_contract_version_rejected() {
        let result = SubjectResolver::new(NatsSubjects::default(), "".into());
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("contract_version"));
    }

    #[test]
    fn test_all_channels_covered() {
        let resolver = SubjectResolver::with_defaults();
        for channel in ALL_CHANNELS {
            let subject = resolver.resolve(*channel);
            assert!(
                subject.starts_with(DEFAULT_SUBJECT_PREFIX),
                "channel {channel} subject '{subject}' missing expected prefix"
            );
        }
    }

    #[test]
    fn test_channel_display() {
        assert_eq!(Channel::LeaseUpsert.to_string(), "lease_upsert");
        assert_eq!(Channel::LeaseRelease.to_string(), "lease_release");
        assert_eq!(
            Channel::LeaseSnapshotRequest.to_string(),
            "lease_snapshot_request"
        );
        assert_eq!(
            Channel::LeaseSnapshotResponse.to_string(),
            "lease_snapshot_response"
        );
    }
}
