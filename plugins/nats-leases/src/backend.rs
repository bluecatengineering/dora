//! Abstract lease backend interface for NATS-backed DHCPv4 lease operations.
//!
//! This module defines `LeaseBackend`, consumed by the NATS DHCPv4
//! plugin so it can isolate lease-flow logic from coordination/storage logic.

use std::{net::IpAddr, time::SystemTime};

use async_trait::async_trait;
use config::v4::{NetRange, Network};

/// Result type for lease backend operations.
pub type BackendResult<T> = Result<T, BackendError>;

/// Error type for lease backend operations, abstracting over different storage backends.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The requested IP address is already in use or assigned.
    #[error("address in use: {0}")]
    AddrInUse(IpAddr),

    /// No available address in the requested range.
    #[error("no address available in range")]
    RangeExhausted,

    /// The address is not reserved or the client ID does not match.
    #[error("address unreserved or client mismatch")]
    Unreserved,

    /// NATS coordination is unavailable; new allocations are blocked.
    #[error("coordination unavailable: new allocations blocked")]
    CoordinationUnavailable,

    /// A lease conflict was detected across concurrent allocators.
    #[error("lease conflict: {0}")]
    Conflict(String),

    /// Internal/storage error.
    #[error("internal error: {0}")]
    Internal(String),
}

/// Information about a released lease.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub ip: IpAddr,
    pub client_id: Option<Vec<u8>>,
    pub subnet: IpAddr,
}

/// Abstract lease backend interface for NATS DHCPv4 operations.
///
/// This trait is implemented by `NatsBackend` and is used by the
/// NATS DHCPv4 plugin to route storage and coordination operations.
#[async_trait]
pub trait LeaseBackend: Send + Sync + std::fmt::Debug + 'static {
    /// Try to reserve a specific IP for a client.
    /// Used during DISCOVER when the client requests a specific address.
    async fn try_ip(
        &self,
        ip: IpAddr,
        subnet: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        network: &Network,
        state: Option<ip_manager::IpState>,
    ) -> BackendResult<()>;

    /// Reserve the first available IP in a range.
    /// Used during DISCOVER when no specific address is requested.
    async fn reserve_first(
        &self,
        range: &NetRange,
        network: &Network,
        client_id: &[u8],
        expires_at: SystemTime,
        state: Option<ip_manager::IpState>,
    ) -> BackendResult<IpAddr>;

    /// Transition a reserved IP to leased state.
    /// Used during REQUEST to confirm a lease.
    async fn try_lease(
        &self,
        ip: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        network: &Network,
    ) -> BackendResult<()>;

    /// Release a lease for the given IP/client pair.
    /// Used during RELEASE.
    async fn release_ip(&self, ip: IpAddr, client_id: &[u8]) -> BackendResult<Option<ReleaseInfo>>;

    /// Mark an IP as probated (declined).
    /// Used during DECLINE.
    async fn probate_ip(
        &self,
        ip: IpAddr,
        client_id: &[u8],
        expires_at: SystemTime,
        subnet: IpAddr,
    ) -> BackendResult<()>;

    /// Check if coordination is available for new allocations.
    fn is_coordination_available(&self) -> bool;

    /// Check if a client has a known active lease (for degraded-mode renewals).
    /// Returns the IP address of the active lease, or None.
    async fn lookup_active_lease(&self, client_id: &[u8]) -> BackendResult<Option<IpAddr>>;

    /// Trigger post-outage reconciliation (snapshot refresh and conflict cleanup).
    async fn reconcile(&self) -> BackendResult<()>;

    /// Select all leases (for external API compatibility).
    async fn select_all(&self) -> BackendResult<Vec<ip_manager::State>>;

    /// Get a specific lease by IP (for external API compatibility).
    async fn get(&self, ip: IpAddr) -> BackendResult<Option<ip_manager::State>>;
}
