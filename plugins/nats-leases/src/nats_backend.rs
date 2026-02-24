//! NATS lease backend: NATS-coordinated multi-server DHCPv4 lease operations.
//!
//! This backend enforces:
//! - Strict uniqueness: one active lease per client identity per subnet, no duplicate IPs
//! - Degraded mode: new allocations blocked on NATS loss, renewals allowed for known leases
//! - Post-outage reconciliation: snapshot refresh and conflict cleanup on reconnect
//!
//! It wraps a local `IpManager<S>` for IP selection/ping-check and the NATS
//! `LeaseCoordinator` for cluster-wide state sharing.

use std::{
    net::IpAddr,
    sync::Arc,
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

use crate::metrics;
use async_trait::async_trait;
use config::v4::{NetRange, Network};
use ip_manager::{IpManager, IpState, Storage};
use nats_coordination::{LeaseCoordinator, LeaseOutcome, LeaseRecord, LeaseState, ProtocolFamily};
use tracing::{debug, info, warn};

use crate::backend::{BackendError, BackendResult, LeaseBackend, ReleaseInfo};

/// Maximum retries for conflict resolution during NATS operations.
const MAX_CONFLICT_RETRIES: u32 = 3;

/// NATS lease backend combining local IP management with NATS coordination.
pub struct NatsBackend<S: Storage> {
    /// Local IP manager for address selection, ping checks, and local cache.
    ip_mgr: Arc<IpManager<S>>,
    /// NATS lease coordinator for cluster-wide state.
    coordinator: LeaseCoordinator,
    /// Server identity for lease records.
    server_id: String,
    /// Known active leases cached locally for degraded-mode renewal checks.
    known_leases: Arc<parking_lot::RwLock<std::collections::HashMap<Vec<u8>, KnownLease>>>,
    /// Synchronous flag for coordination availability, updated by background job.
    /// This allows sync checks without async calls.
    coordination_available: Arc<AtomicBool>,
}

/// A locally cached record of a known active lease for degraded-mode support.
#[derive(Debug, Clone)]
struct KnownLease {
    ip: IpAddr,
    expires_at: SystemTime,
}

impl<S: Storage> std::fmt::Debug for NatsBackend<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsBackend")
            .field("server_id", &self.server_id)
            .finish()
    }
}

impl<S: Storage> NatsBackend<S> {
    pub fn new(
        ip_mgr: Arc<IpManager<S>>,
        coordinator: LeaseCoordinator,
        server_id: String,
    ) -> Self {
        Self {
            ip_mgr,
            coordinator,
            server_id,
            known_leases: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            coordination_available: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Get access to the underlying IpManager (for external API compatibility).
    pub fn ip_mgr(&self) -> &Arc<IpManager<S>> {
        &self.ip_mgr
    }

    /// Get the coordination availability flag for background updates.
    pub fn coordination_available(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.coordination_available)
    }

    /// Record a known active lease in the local cache.
    fn record_known_lease(&self, client_id: &[u8], ip: IpAddr, expires_at: SystemTime) {
        self.known_leases
            .write()
            .insert(client_id.to_vec(), KnownLease { ip, expires_at });
    }

    /// Remove a known lease from the local cache.
    fn remove_known_lease(&self, client_id: &[u8]) {
        self.known_leases.write().remove(client_id);
    }

    /// Look up a known active lease in the local cache.
    fn get_known_lease(&self, client_id: &[u8]) -> Option<KnownLease> {
        let leases = self.known_leases.read();
        leases.get(client_id).and_then(|lease| {
            if lease.expires_at > SystemTime::now() {
                Some(lease.clone())
            } else {
                None
            }
        })
    }

    /// Create a LeaseRecord for NATS coordination from local parameters.
    fn make_lease_record(
        &self,
        ip: IpAddr,
        subnet: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        state: LeaseState,
    ) -> LeaseRecord {
        use chrono::{DateTime, Utc};
        let now = Utc::now();
        let expires_chrono: DateTime<Utc> = expires_at.into();
        LeaseRecord {
            lease_id: uuid::Uuid::new_v4().to_string(),
            protocol_family: ProtocolFamily::Dhcpv4,
            subnet: format!("{}", subnet),
            ip_address: format!("{}", ip),
            client_key_v4: Some(hex::encode(client_id)),
            duid: None,
            iaid: None,
            state,
            expires_at: expires_chrono,
            probation_until: None,
            server_id: self.server_id.clone(),
            revision: 0,
            updated_at: now,
        }
    }

    /// Handle a LeaseOutcome from the coordinator, mapping to BackendResult.
    fn handle_outcome(
        &self,
        outcome: LeaseOutcome,
        client_id: &[u8],
        ip: IpAddr,
        expires_at: SystemTime,
    ) -> BackendResult<()> {
        match outcome {
            LeaseOutcome::Success(record) => {
                debug!(
                    ip = %record.ip_address,
                    state = %record.state,
                    revision = record.revision,
                    "lease coordinated successfully"
                );
                self.record_known_lease(client_id, ip, expires_at);
                Ok(())
            }
            LeaseOutcome::Conflict {
                expected_revision,
                actual_revision,
            } => {
                metrics::CLUSTER_CONFLICTS_DETECTED.inc();
                warn!(
                    expected = expected_revision,
                    actual = actual_revision,
                    "lease conflict could not be resolved within retry budget"
                );
                Err(BackendError::Conflict(format!(
                    "revision conflict: expected {expected_revision}, found {actual_revision}"
                )))
            }
            LeaseOutcome::DegradedModeBlocked => {
                metrics::CLUSTER_ALLOCATIONS_BLOCKED.inc();
                info!(
                    mode = "nats",
                    "new allocation blocked: NATS coordination unavailable"
                );
                Err(BackendError::CoordinationUnavailable)
            }
        }
    }

    async fn rollback_local_allocation(&self, ip: IpAddr, client_id: &[u8], reason: &str) {
        match self.ip_mgr.release_ip(ip, client_id).await {
            Ok(Some(_)) => {
                debug!(?ip, ?client_id, reason, "rolled back local allocation");
            }
            Ok(None) => {
                debug!(?ip, ?client_id, reason, "no local allocation to roll back");
            }
            Err(err) => {
                warn!(
                    ?err,
                    ?ip,
                    ?client_id,
                    reason,
                    "failed to roll back local allocation"
                );
            }
        }
    }
}

/// Map IpError to BackendError.
fn map_ip_error<E: std::error::Error + Send + Sync + 'static>(
    err: ip_manager::IpError<E>,
) -> BackendError {
    match err {
        ip_manager::IpError::AddrInUse(ip) => BackendError::AddrInUse(ip),
        ip_manager::IpError::Unreserved => BackendError::Unreserved,
        ip_manager::IpError::RangeError { .. } => BackendError::RangeExhausted,
        ip_manager::IpError::MaxAttempts { .. } => BackendError::RangeExhausted,
        other => BackendError::Internal(other.to_string()),
    }
}

#[async_trait]
impl<S> LeaseBackend for NatsBackend<S>
where
    S: Storage + Send + Sync + 'static,
{
    async fn try_ip(
        &self,
        ip: IpAddr,
        subnet: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        network: &Network,
        state: Option<IpState>,
    ) -> BackendResult<()> {
        // Check coordination availability first
        if !self.coordinator.is_available().await {
            metrics::CLUSTER_ALLOCATIONS_BLOCKED.inc();
            metrics::CLUSTER_COORDINATION_STATE.set(0);
            info!(
                mode = "nats",
                "try_ip blocked: NATS coordination unavailable"
            );
            return Err(BackendError::CoordinationUnavailable);
        }
        metrics::CLUSTER_COORDINATION_STATE.set(1);

        // First, do local IP validation/ping check via IpManager
        self.ip_mgr
            .try_ip(ip, subnet, client_id, expires_at, network, state)
            .await
            .map_err(map_ip_error)?;

        // Then coordinate with the cluster
        let lease_state = match state {
            Some(IpState::Lease) => LeaseState::Leased,
            _ => LeaseState::Reserved,
        };
        let record = self.make_lease_record(ip, subnet, client_id, expires_at, lease_state);

        let outcome = match self.coordinator.reserve(record).await {
            Ok(outcome) => outcome,
            Err(e) => {
                self.rollback_local_allocation(ip, client_id, "coordination transport failure")
                    .await;
                return Err(BackendError::Internal(format!("coordination error: {e}")));
            }
        };

        match self.handle_outcome(outcome, client_id, ip, expires_at) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.rollback_local_allocation(ip, client_id, "coordination outcome failure")
                    .await;
                Err(err)
            }
        }
    }

    async fn reserve_first(
        &self,
        range: &NetRange,
        network: &Network,
        client_id: &[u8],
        expires_at: SystemTime,
        state: Option<IpState>,
    ) -> BackendResult<IpAddr> {
        // Check coordination availability first
        if !self.coordinator.is_available().await {
            metrics::CLUSTER_ALLOCATIONS_BLOCKED.inc();
            metrics::CLUSTER_COORDINATION_STATE.set(0);
            info!(
                mode = "nats",
                "reserve_first blocked: NATS coordination unavailable"
            );
            return Err(BackendError::CoordinationUnavailable);
        }
        metrics::CLUSTER_COORDINATION_STATE.set(1);

        // Use local IpManager to find an available IP
        let ip = self
            .ip_mgr
            .reserve_first(range, network, client_id, expires_at, state)
            .await
            .map_err(map_ip_error)?;

        // Coordinate with the cluster
        let lease_state = match state {
            Some(IpState::Lease) => LeaseState::Leased,
            _ => LeaseState::Reserved,
        };
        let record = self.make_lease_record(
            ip,
            network.subnet().into(),
            client_id,
            expires_at,
            lease_state,
        );

        // Attempt to coordinate with bounded retries for conflict resolution
        let mut attempts = 0u32;
        let mut current_record = record;
        loop {
            let outcome = match self.coordinator.reserve(current_record.clone()).await {
                Ok(outcome) => outcome,
                Err(e) => {
                    self.rollback_local_allocation(ip, client_id, "coordination transport failure")
                        .await;
                    return Err(BackendError::Internal(format!("coordination error: {e}")));
                }
            };

            match outcome {
                LeaseOutcome::Success(confirmed) => {
                    debug!(
                        ip = %confirmed.ip_address,
                        revision = confirmed.revision,
                        "lease reservation coordinated successfully"
                    );
                    self.record_known_lease(client_id, ip, expires_at);
                    metrics::CLUSTER_CONFLICTS_RESOLVED.inc();
                    return Ok(ip);
                }
                LeaseOutcome::Conflict {
                    expected_revision,
                    actual_revision,
                } => {
                    attempts += 1;
                    metrics::CLUSTER_CONFLICTS_DETECTED.inc();
                    if attempts >= MAX_CONFLICT_RETRIES {
                        warn!(
                            attempts,
                            expected = expected_revision,
                            actual = actual_revision,
                            "reservation conflict exhausted retry budget"
                        );
                        self.rollback_local_allocation(
                            ip,
                            client_id,
                            "coordination conflict exhausted retry budget",
                        )
                        .await;
                        return Err(BackendError::Conflict(format!(
                            "conflict after {attempts} retries: expected rev {expected_revision}, found {actual_revision}"
                        )));
                    }
                    debug!(
                        attempt = attempts,
                        "reservation conflict, updating revision and retrying"
                    );
                    current_record.revision = actual_revision;
                    continue;
                }
                LeaseOutcome::DegradedModeBlocked => {
                    metrics::CLUSTER_ALLOCATIONS_BLOCKED.inc();
                    self.rollback_local_allocation(
                        ip,
                        client_id,
                        "coordination unavailable after local reserve",
                    )
                    .await;
                    return Err(BackendError::CoordinationUnavailable);
                }
            }
        }
    }

    async fn try_lease(
        &self,
        ip: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        network: &Network,
    ) -> BackendResult<()> {
        // For lease confirmation (REQUEST), allow renewal of known leases in degraded mode
        if !self.coordinator.is_available().await {
            // Check if this is a renewal of a known active lease
            if let Some(known) = self.get_known_lease(client_id) {
                if known.ip == ip {
                    metrics::CLUSTER_DEGRADED_RENEWALS.inc();
                    info!(
                        ?ip,
                        mode = "nats",
                        "degraded-mode renewal allowed for known active lease"
                    );
                    // Do the local lease update only
                    self.ip_mgr
                        .try_lease(ip, client_id, expires_at, network)
                        .await
                        .map_err(map_ip_error)?;
                    self.record_known_lease(client_id, ip, expires_at);
                    return Ok(());
                }
            }
            // Not a known renewal - block
            metrics::CLUSTER_ALLOCATIONS_BLOCKED.inc();
            metrics::CLUSTER_COORDINATION_STATE.set(0);
            info!(
                mode = "nats",
                "try_lease blocked: NATS unavailable and not a known renewal"
            );
            return Err(BackendError::CoordinationUnavailable);
        }
        metrics::CLUSTER_COORDINATION_STATE.set(1);

        // Local lease transition
        self.ip_mgr
            .try_lease(ip, client_id, expires_at, network)
            .await
            .map_err(map_ip_error)?;

        // Coordinate with cluster
        let record = self.make_lease_record(
            ip,
            network.subnet().into(),
            client_id,
            expires_at,
            LeaseState::Leased,
        );

        let outcome = self
            .coordinator
            .lease(record)
            .await
            .map_err(|e| BackendError::Internal(format!("coordination error: {e}")))?;

        self.handle_outcome(outcome, client_id, ip, expires_at)
    }

    async fn release_ip(&self, ip: IpAddr, client_id: &[u8]) -> BackendResult<Option<ReleaseInfo>> {
        // Local release first
        let info = match self.ip_mgr.release_ip(ip, client_id).await {
            Ok(Some(info)) => {
                self.remove_known_lease(client_id);
                Some(ReleaseInfo {
                    ip: info.ip(),
                    client_id: info.id().map(|id| id.to_vec()),
                    subnet: info.network(),
                })
            }
            Ok(None) => None,
            Err(e) => return Err(map_ip_error(e)),
        };

        // Coordinate release with cluster (best-effort)
        if self.coordinator.is_available().await {
            let subnet = info
                .as_ref()
                .map(|released| released.subnet)
                .unwrap_or(IpAddr::from([0, 0, 0, 0]));
            let record = self.make_lease_record(
                ip,
                subnet,
                client_id,
                SystemTime::now(),
                LeaseState::Released,
            );
            if let Err(e) = self.coordinator.release(record).await {
                warn!(error = %e, "failed to coordinate lease release with cluster");
            }
        }

        Ok(info)
    }

    async fn probate_ip(
        &self,
        ip: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        subnet: IpAddr,
    ) -> BackendResult<()> {
        // Local probation
        self.ip_mgr
            .probate_ip(ip, client_id, expires_at)
            .await
            .map_err(map_ip_error)?;

        self.remove_known_lease(client_id);

        // Coordinate with cluster (best-effort)
        if self.coordinator.is_available().await {
            let record =
                self.make_lease_record(ip, subnet, client_id, expires_at, LeaseState::Probated);
            let probation_chrono: chrono::DateTime<chrono::Utc> = expires_at.into();
            if let Err(e) = self.coordinator.probate(record, probation_chrono).await {
                warn!(error = %e, "failed to coordinate lease probation with cluster");
            }
        }

        Ok(())
    }

    fn is_coordination_available(&self) -> bool {
        // Read from the atomic flag that is updated by the background connection monitor.
        // This allows synchronous checks without async calls.
        self.coordination_available.load(Ordering::Relaxed)
    }

    async fn lookup_active_lease(&self, client_id: &[u8]) -> BackendResult<Option<IpAddr>> {
        // First check local known-lease cache
        if let Some(known) = self.get_known_lease(client_id) {
            return Ok(Some(known.ip));
        }

        // Fall back to local IpManager
        match self.ip_mgr.lookup_id(client_id).await {
            Ok(ip) => {
                // Cache for degraded-mode use
                self.record_known_lease(
                    client_id,
                    ip,
                    SystemTime::now() + std::time::Duration::from_secs(3600),
                );
                Ok(Some(ip))
            }
            Err(ip_manager::IpError::Unreserved) => Ok(None),
            Err(e) => Err(map_ip_error(e)),
        }
    }

    async fn reconcile(&self) -> BackendResult<()> {
        info!(mode = "nats", "starting post-outage reconciliation");

        // Request a snapshot from the coordination channel
        let snapshot = match self.coordinator.request_snapshot().await {
            Ok(snap) => snap,
            Err(e) => {
                warn!(error = %e, "reconciliation snapshot request failed");
                return Err(BackendError::Internal(format!(
                    "snapshot request failed: {e}"
                )));
            }
        };

        let record_count = snapshot.records.len();
        info!(
            record_count,
            "received reconciliation snapshot, refreshing local state"
        );

        // Refresh known-lease cache from snapshot
        let mut reconciled = 0u64;
        {
            let mut known = self.known_leases.write();
            known.clear();

            for record in &snapshot.records {
                if record.protocol_family == ProtocolFamily::Dhcpv4 && record.state.is_active() {
                    if let Some(ref client_key) = record.client_key_v4 {
                        if let Ok(client_bytes) = hex::decode(client_key) {
                            if let Ok(ip) = record.ip_address.parse::<IpAddr>() {
                                let expires_at: SystemTime = record.expires_at.into();
                                known.insert(client_bytes, KnownLease { ip, expires_at });
                                reconciled += 1;
                            }
                        }
                    }
                }
            }
        }

        metrics::CLUSTER_RECONCILIATIONS.inc();
        metrics::CLUSTER_RECORDS_RECONCILED.inc_by(reconciled);

        info!(reconciled, total = record_count, "reconciliation completed");

        Ok(())
    }

    async fn select_all(&self) -> BackendResult<Vec<ip_manager::State>> {
        self.ip_mgr
            .select_all()
            .await
            .map_err(|e| BackendError::Internal(e.to_string()))
    }

    async fn get(&self, ip: IpAddr) -> BackendResult<Option<ip_manager::State>> {
        self.ip_mgr
            .get(ip)
            .await
            .map_err(|e| BackendError::Internal(e.to_string()))
    }
}
