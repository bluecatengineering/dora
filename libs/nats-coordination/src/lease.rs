//! Lease coordination APIs backed by JetStream KV.
//!
//! This replaces legacy request/reply coordination subjects for runtime lease
//! operations. Lease records and IP indexes are stored in a KV bucket.

use std::time::Duration;

use chrono::Utc;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::client::{ConnectionState, NatsClient};
use crate::error::{CoordinationError, CoordinationResult};
use crate::models::{
    self, LeaseRecord, LeaseSnapshotRequest, LeaseSnapshotResponse, LeaseState, ProtocolFamily,
};
use crate::subjects::Channel;

/// Default maximum retry attempts for conflicting lease operations.
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Retry policy configuration for lease conflict resolution.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts.
    pub max_retries: u32,
    /// Base delay between retries (actual delay uses exponential backoff).
    pub base_delay: std::time::Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: std::time::Duration::from_millis(50),
        }
    }
}

/// Outcome of a lease coordination operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseOutcome {
    /// Operation succeeded. Contains the updated lease record.
    Success(LeaseRecord),
    /// Revision or ownership conflict could not be resolved.
    Conflict {
        expected_revision: u64,
        actual_revision: u64,
    },
    /// NATS/JetStream is unavailable and operation is blocked.
    DegradedModeBlocked,
}

/// GC statistics returned by a sweep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseGcStats {
    pub expired_records: u64,
    pub orphan_indexes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IpIndexEntry {
    lease_key: String,
    updated_at: chrono::DateTime<Utc>,
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

/// Lease coordination client that wraps JetStream KV operations.
#[derive(Debug, Clone)]
pub struct LeaseCoordinator {
    client: NatsClient,
    server_id: String,
    retry_policy: RetryPolicy,
}

impl LeaseCoordinator {
    /// Create a new lease coordinator.
    pub fn new(client: NatsClient, server_id: String) -> Self {
        Self {
            client,
            server_id,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Create a new lease coordinator with custom retry policy.
    pub fn with_retry_policy(
        client: NatsClient,
        server_id: String,
        retry_policy: RetryPolicy,
    ) -> Self {
        Self {
            client,
            server_id,
            retry_policy,
        }
    }

    /// Returns true if NATS coordination is available (connected state).
    pub async fn is_available(&self) -> bool {
        self.client.is_connected().await
    }

    /// Check whether a renewal can proceed in degraded mode.
    pub async fn can_renew_in_degraded_mode(&self) -> bool {
        let state = self.client.connection_state().await;
        matches!(
            state,
            ConnectionState::Reconnecting | ConnectionState::Disconnected
        )
    }

    async fn leases_store(&self) -> CoordinationResult<async_nats::jetstream::kv::Store> {
        let bucket = self.client.leases_bucket().await;
        self.client.get_or_create_kv_bucket(&bucket, 16).await
    }

    fn lease_key(record: &LeaseRecord) -> CoordinationResult<String> {
        let subnet = sanitize_key_component(&record.subnet);
        match record.protocol_family {
            ProtocolFamily::Dhcpv4 => {
                let client_key = record.client_key_v4.as_ref().ok_or_else(|| {
                    CoordinationError::Codec("DHCPv4 lease record missing client_key_v4".into())
                })?;
                let client_key = sanitize_key_component(client_key);
                Ok(format!("v4/{subnet}/client/{client_key}"))
            }
            ProtocolFamily::Dhcpv6 => {
                let duid = record.duid.as_ref().ok_or_else(|| {
                    CoordinationError::Codec("DHCPv6 lease record missing duid".into())
                })?;
                let iaid = record.iaid.ok_or_else(|| {
                    CoordinationError::Codec("DHCPv6 lease record missing iaid".into())
                })?;
                let duid = sanitize_key_component(duid);
                Ok(format!("v6/{subnet}/duid/{duid}/iaid/{iaid}"))
            }
        }
    }

    fn ip_key(record: &LeaseRecord) -> String {
        let subnet = sanitize_key_component(&record.subnet);
        let ip = sanitize_key_component(&record.ip_address);
        match record.protocol_family {
            ProtocolFamily::Dhcpv4 => format!("v4/{subnet}/ip/{ip}"),
            ProtocolFamily::Dhcpv6 => format!("v6/{subnet}/ip/{ip}"),
        }
    }

    async fn load_record(
        &self,
        store: &async_nats::jetstream::kv::Store,
        key: &str,
    ) -> CoordinationResult<Option<LeaseRecord>> {
        let value = store.get(key.to_string()).await.map_err(|e| {
            CoordinationError::Transport(format!("KV read failed for key '{key}': {e}"))
        })?;
        match value {
            Some(bytes) => models::decode(&bytes).map(Some),
            None => Ok(None),
        }
    }

    async fn load_index(
        &self,
        store: &async_nats::jetstream::kv::Store,
        key: &str,
    ) -> CoordinationResult<Option<IpIndexEntry>> {
        let value = store.get(key.to_string()).await.map_err(|e| {
            CoordinationError::Transport(format!("KV read failed for key '{key}': {e}"))
        })?;
        match value {
            Some(bytes) => models::decode(&bytes).map(Some),
            None => Ok(None),
        }
    }

    async fn put_record(
        &self,
        store: &async_nats::jetstream::kv::Store,
        key: &str,
        record: &LeaseRecord,
    ) -> CoordinationResult<u64> {
        let payload = models::encode(record)?;
        store.put(key, payload.into()).await.map_err(|e| {
            CoordinationError::Transport(format!("KV write failed for key '{key}': {e}"))
        })
    }

    async fn put_index(
        &self,
        store: &async_nats::jetstream::kv::Store,
        key: &str,
        index: &IpIndexEntry,
    ) -> CoordinationResult<u64> {
        let payload = models::encode(index)?;
        store.put(key, payload.into()).await.map_err(|e| {
            CoordinationError::Transport(format!("KV write failed for key '{key}': {e}"))
        })
    }

    async fn delete_key(
        &self,
        store: &async_nats::jetstream::kv::Store,
        key: &str,
    ) -> CoordinationResult<()> {
        store.delete(key).await.map_err(|e| {
            CoordinationError::Transport(format!("KV delete failed for key '{key}': {e}"))
        })
    }

    async fn upsert_with_retry(&self, mut record: LeaseRecord) -> CoordinationResult<LeaseOutcome> {
        let mut attempts = 0u32;
        loop {
            match self.upsert_once(record.clone()).await? {
                LeaseOutcome::Conflict {
                    expected_revision,
                    actual_revision,
                } => {
                    attempts += 1;
                    if attempts >= self.retry_policy.max_retries {
                        return Ok(LeaseOutcome::Conflict {
                            expected_revision,
                            actual_revision,
                        });
                    }
                    record.revision = actual_revision;
                    tokio::time::sleep(
                        self.retry_policy.base_delay * 2u32.saturating_pow(attempts - 1),
                    )
                    .await;
                }
                other => return Ok(other),
            }
        }
    }

    async fn upsert_once(&self, mut record: LeaseRecord) -> CoordinationResult<LeaseOutcome> {
        if !self.is_available().await {
            return Ok(LeaseOutcome::DegradedModeBlocked);
        }

        record.server_id = self.server_id.clone();
        record.updated_at = Utc::now();
        record.validate()?;

        let store = self.leases_store().await?;
        let lease_key = Self::lease_key(&record)?;
        let ip_key = Self::ip_key(&record);

        let existing = self.load_record(&store, &lease_key).await?;
        let old_ip_key = existing
            .as_ref()
            .filter(|current| current.state.is_active() && current.ip_address != record.ip_address)
            .map(Self::ip_key);

        if let Some(index) = self.load_index(&store, &ip_key).await? {
            if index.lease_key != lease_key {
                if let Some(existing_owner) = self.load_record(&store, &index.lease_key).await? {
                    if existing_owner.state.is_active() && existing_owner.expires_at > Utc::now() {
                        return Ok(LeaseOutcome::Conflict {
                            expected_revision: record.revision,
                            actual_revision: existing_owner.revision,
                        });
                    }
                }
            }
        }

        record.revision = existing
            .map(|current| current.revision.saturating_add(1))
            .unwrap_or(1);

        self.put_record(&store, &lease_key, &record).await?;

        if record.state.is_active() || matches!(record.state, LeaseState::Probated) {
            self.put_index(
                &store,
                &ip_key,
                &IpIndexEntry {
                    lease_key,
                    updated_at: Utc::now(),
                },
            )
            .await?;
        } else {
            self.delete_key(&store, &ip_key).await?;
        }

        if let Some(old_ip_key) = old_ip_key {
            let _ = self.delete_key(&store, &old_ip_key).await;
        }

        Ok(LeaseOutcome::Success(record))
    }

    /// Reserve a lease (initial allocation step).
    pub async fn reserve(&self, mut record: LeaseRecord) -> CoordinationResult<LeaseOutcome> {
        record.state = LeaseState::Reserved;
        self.upsert_with_retry(record).await
    }

    /// Confirm a lease (transition reserved -> leased).
    pub async fn lease(&self, mut record: LeaseRecord) -> CoordinationResult<LeaseOutcome> {
        record.state = LeaseState::Leased;
        self.upsert_with_retry(record).await
    }

    /// Release a lease (client-initiated release).
    pub async fn release(&self, mut record: LeaseRecord) -> CoordinationResult<LeaseOutcome> {
        record.state = LeaseState::Released;
        self.upsert_with_retry(record).await
    }

    /// Probate a lease (mark as declined/conflicted).
    pub async fn probate(
        &self,
        mut record: LeaseRecord,
        probation_until: chrono::DateTime<Utc>,
    ) -> CoordinationResult<LeaseOutcome> {
        record.state = LeaseState::Probated;
        record.probation_until = Some(probation_until);
        self.upsert_with_retry(record).await
    }

    /// Request a lease snapshot from KV for reconciliation.
    pub async fn request_snapshot(&self) -> CoordinationResult<LeaseSnapshotResponse> {
        if !self.is_available().await {
            return Err(CoordinationError::NotConnected(
                "cannot request snapshot: NATS not connected".into(),
            ));
        }

        let request = LeaseSnapshotRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            server_id: self.server_id.clone(),
            sent_at: Utc::now(),
        };

        let store = self.leases_store().await?;
        let mut records = Vec::new();
        let mut keys = store.keys().await.map_err(|e| {
            CoordinationError::Transport(format!("failed to list lease KV keys: {e}"))
        })?;

        while let Some(key) = keys.try_next().await.map_err(|e| {
            CoordinationError::Transport(format!("failed reading lease KV keys: {e}"))
        })? {
            if key.contains("/ip/") {
                continue;
            }
            if let Some(record) = self.load_record(&store, &key).await? {
                records.push(record);
            }
        }

        info!(
            request_id = %request.request_id,
            record_count = records.len(),
            "assembled lease snapshot from KV"
        );

        Ok(LeaseSnapshotResponse {
            request_id: request.request_id,
            server_id: self.server_id.clone(),
            records,
            sent_at: Utc::now(),
        })
    }

    /// Sweep expired records and remove stale IP indexes.
    pub async fn gc_expired(&self) -> CoordinationResult<LeaseGcStats> {
        if !self.is_available().await {
            return Err(CoordinationError::NotConnected(
                "cannot run lease GC: NATS not connected".into(),
            ));
        }

        let store = self.leases_store().await?;
        let mut stats = LeaseGcStats::default();
        let now = Utc::now();

        let mut keys = store.keys().await.map_err(|e| {
            CoordinationError::Transport(format!("failed to list lease KV keys: {e}"))
        })?;

        let mut all_keys = Vec::new();
        while let Some(key) = keys.try_next().await.map_err(|e| {
            CoordinationError::Transport(format!("failed reading lease KV keys: {e}"))
        })? {
            all_keys.push(key);
        }

        for key in &all_keys {
            if !key.contains("/ip/") {
                continue;
            }
            if let Some(index) = self.load_index(&store, key).await? {
                match self.load_record(&store, &index.lease_key).await? {
                    Some(record) if record.state.is_active() && record.expires_at > now => {}
                    _ => {
                        let _ = self.delete_key(&store, key).await;
                        stats.orphan_indexes += 1;
                    }
                }
            }
        }

        for key in all_keys {
            if key.contains("/ip/") {
                continue;
            }
            let Some(mut record) = self.load_record(&store, &key).await? else {
                continue;
            };
            if record.state.is_active() && record.expires_at <= now {
                record.state = LeaseState::Expired;
                record.revision = record.revision.saturating_add(1);
                record.server_id = self.server_id.clone();
                record.updated_at = now;
                self.put_record(&store, &key, &record).await?;
                let ip_key = Self::ip_key(&record);
                let _ = self.delete_key(&store, &ip_key).await;
                stats.expired_records += 1;
            }
        }

        Ok(stats)
    }

    /// Publish a lease event without expecting a reply.
    ///
    /// Runtime now persists directly to KV, so this delegates to the upsert path.
    pub async fn broadcast(
        &self,
        record: &LeaseRecord,
        _channel: Channel,
    ) -> CoordinationResult<()> {
        match self.upsert_with_retry(record.clone()).await? {
            LeaseOutcome::Success(_) => Ok(()),
            LeaseOutcome::Conflict {
                expected_revision,
                actual_revision,
            } => Err(CoordinationError::RevisionConflict {
                expected: expected_revision,
                actual: actual_revision,
            }),
            LeaseOutcome::DegradedModeBlocked => Err(CoordinationError::NotConnected(
                "cannot broadcast: NATS not connected".into(),
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_lease() -> LeaseRecord {
        LeaseRecord {
            lease_id: "test-lease-001".into(),
            protocol_family: ProtocolFamily::Dhcpv4,
            subnet: "10.0.0.0/24".into(),
            ip_address: "10.0.0.50".into(),
            client_key_v4: Some("aabb".into()),
            duid: None,
            iaid: None,
            state: LeaseState::Reserved,
            expires_at: Utc::now() + chrono::Duration::hours(1),
            probation_until: None,
            server_id: "server-test".into(),
            revision: 0,
            updated_at: Utc::now(),
        }
    }

    fn test_config() -> config::NatsConfig {
        config::NatsConfig {
            servers: vec!["nats://127.0.0.1:4222".into()],
            subject_prefix: "test".into(),
            subjects: config::wire::NatsSubjects::default(),
            leases_bucket: "test_leases".into(),
            host_options_bucket: "test_host_options".into(),
            lease_gc_interval: Duration::from_secs(30),
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
            connect_timeout: None,
            connect_retry_max: 2,
            request_timeout: None,
        }
    }

    #[test]
    fn test_retry_policy_default() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, DEFAULT_MAX_RETRIES);
        assert_eq!(policy.base_delay, std::time::Duration::from_millis(50));
    }

    #[test]
    fn test_lease_outcome_variants() {
        let lease = sample_lease();
        let success = LeaseOutcome::Success(lease);
        assert!(matches!(success, LeaseOutcome::Success(_)));

        let conflict = LeaseOutcome::Conflict {
            expected_revision: 1,
            actual_revision: 3,
        };
        assert!(matches!(conflict, LeaseOutcome::Conflict { .. }));

        let blocked = LeaseOutcome::DegradedModeBlocked;
        assert!(matches!(blocked, LeaseOutcome::DegradedModeBlocked));
    }

    #[tokio::test]
    async fn test_coordinator_degraded_mode_blocks_reserve() {
        let resolver = crate::subjects::SubjectResolver::with_defaults();
        let client = NatsClient::new(test_config(), resolver);
        let coordinator = LeaseCoordinator::new(client, "test-server".into());

        let lease = sample_lease();
        let result = coordinator.reserve(lease).await;
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), LeaseOutcome::DegradedModeBlocked));
    }

    #[tokio::test]
    async fn test_coordinator_degraded_mode_blocks_lease() {
        let resolver = crate::subjects::SubjectResolver::with_defaults();
        let client = NatsClient::new(test_config(), resolver);
        let coordinator = LeaseCoordinator::new(client, "test-server".into());

        let lease = sample_lease();
        let result = coordinator.lease(lease).await;
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), LeaseOutcome::DegradedModeBlocked));
    }

    #[tokio::test]
    async fn test_coordinator_degraded_mode_blocks_release() {
        let resolver = crate::subjects::SubjectResolver::with_defaults();
        let client = NatsClient::new(test_config(), resolver);
        let coordinator = LeaseCoordinator::new(client, "test-server".into());

        let lease = sample_lease();
        let result = coordinator.release(lease).await;
        assert!(result.is_ok());
        assert!(matches!(result.unwrap(), LeaseOutcome::DegradedModeBlocked));
    }

    #[tokio::test]
    async fn test_coordinator_snapshot_not_connected() {
        let resolver = crate::subjects::SubjectResolver::with_defaults();
        let client = NatsClient::new(test_config(), resolver);
        let coordinator = LeaseCoordinator::new(client, "test-server".into());

        let result = coordinator.request_snapshot().await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            CoordinationError::NotConnected(_)
        ));
    }

    #[tokio::test]
    async fn test_coordinator_availability() {
        let resolver = crate::subjects::SubjectResolver::with_defaults();
        let client = NatsClient::new(test_config(), resolver);
        let coordinator = LeaseCoordinator::new(client, "test-server".into());

        assert!(!coordinator.is_available().await);
        assert!(coordinator.can_renew_in_degraded_mode().await);
    }

    #[test]
    fn test_lease_key_v4() {
        let key = LeaseCoordinator::lease_key(&sample_lease()).unwrap();
        assert_eq!(key, "v4/10.0.0.0_24/client/aabb");
    }
}
