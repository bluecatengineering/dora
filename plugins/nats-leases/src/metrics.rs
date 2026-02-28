//! NATS leases plugin metrics (v4 and v6).
//!
//! Metrics are lazily initialized on first access via `lazy_static!`.
//! Each plugin owns its own counters rather than centralizing them in dora-core.
//!
//! # Note
//! Metric registration failures are fatal (using `.unwrap()`). This is intentional
//! as missing metrics could mask operational issues. If metric registration fails,
//! it indicates a configuration or programming error that should be fixed at startup.
//! In production deployments, ensure the Prometheus endpoint is properly configured
//! before starting the server.

use lazy_static::lazy_static;
use prometheus::{IntCounter, IntGauge, register_int_counter, register_int_gauge};

lazy_static! {
    // --- NATS DHCPv4 coordination metrics ---

    /// Count of new allocations blocked due to NATS unavailability (degraded mode)
    pub static ref CLUSTER_ALLOCATIONS_BLOCKED: IntCounter = register_int_counter!(
        "cluster_allocations_blocked",
        "count of new allocations blocked during NATS unavailability"
    ).unwrap();

    /// Count of renewals allowed in degraded mode (known active leases)
    pub static ref CLUSTER_DEGRADED_RENEWALS: IntCounter = register_int_counter!(
        "cluster_degraded_renewals",
        "count of renewals granted in degraded mode for known active leases"
    ).unwrap();

    /// Count of lease coordination conflicts detected across allocators
    pub static ref CLUSTER_CONFLICTS_DETECTED: IntCounter = register_int_counter!(
        "cluster_conflicts_detected",
        "count of lease coordination conflicts detected"
    ).unwrap();

    /// Count of lease coordination conflicts resolved by retry
    pub static ref CLUSTER_CONFLICTS_RESOLVED: IntCounter = register_int_counter!(
        "cluster_conflicts_resolved",
        "count of lease coordination conflicts resolved"
    ).unwrap();

    /// Count of reconciliation events completed after NATS recovery
    pub static ref CLUSTER_RECONCILIATIONS: IntCounter = register_int_counter!(
        "cluster_reconciliations",
        "count of post-outage reconciliation events completed"
    ).unwrap();

    /// Count of lease records reconciled during post-outage recovery
    pub static ref CLUSTER_RECORDS_RECONCILED: IntCounter = register_int_counter!(
        "cluster_records_reconciled",
        "count of lease records reconciled during post-outage recovery"
    ).unwrap();

    /// Count of lease GC sweep runs in nats mode.
    pub static ref CLUSTER_GC_SWEEPS: IntCounter = register_int_counter!(
        "cluster_gc_sweeps",
        "count of nats lease GC sweep runs"
    ).unwrap();

    /// Count of lease records marked expired by nats GC.
    pub static ref CLUSTER_GC_EXPIRED: IntCounter = register_int_counter!(
        "cluster_gc_expired_records",
        "count of nats lease records marked expired by GC"
    ).unwrap();

    /// Count of orphaned lease index entries deleted by nats GC.
    pub static ref CLUSTER_GC_ORPHANED_INDEXES: IntCounter = register_int_counter!(
        "cluster_gc_orphaned_indexes",
        "count of nats lease index entries deleted as orphans"
    ).unwrap();

    /// Count of nats lease GC sweep errors.
    pub static ref CLUSTER_GC_ERRORS: IntCounter = register_int_counter!(
        "cluster_gc_errors",
        "count of nats lease GC sweep errors"
    ).unwrap();

    /// Gauge: current coordination state (1=connected, 0=disconnected)
    pub static ref CLUSTER_COORDINATION_STATE: IntGauge = register_int_gauge!(
        "cluster_coordination_state",
        "current coordination state (1=connected, 0=disconnected/degraded)"
    ).unwrap();

    // --- NATS DHCPv6 coordination metrics ---

    /// Count of v6 lease allocations (Solicit/Advertise) in nats mode
    pub static ref CLUSTER_V6_ALLOCATIONS: IntCounter = register_int_counter!(
        "cluster_v6_allocations",
        "count of DHCPv6 lease allocations in nats mode"
    ).unwrap();

    /// Count of v6 lease renewals in nats mode
    pub static ref CLUSTER_V6_RENEWALS: IntCounter = register_int_counter!(
        "cluster_v6_renewals",
        "count of DHCPv6 lease renewals in nats mode"
    ).unwrap();

    /// Count of v6 lease releases in nats mode
    pub static ref CLUSTER_V6_RELEASES: IntCounter = register_int_counter!(
        "cluster_v6_releases",
        "count of DHCPv6 lease releases in nats mode"
    ).unwrap();

    /// Count of v6 lease declines in nats mode
    pub static ref CLUSTER_V6_DECLINES: IntCounter = register_int_counter!(
        "cluster_v6_declines",
        "count of DHCPv6 lease declines in nats mode"
    ).unwrap();

    /// Count of v6 new allocations blocked due to NATS unavailability (degraded mode)
    pub static ref CLUSTER_V6_ALLOCATIONS_BLOCKED: IntCounter = register_int_counter!(
        "cluster_v6_allocations_blocked",
        "count of DHCPv6 new allocations blocked during NATS unavailability"
    ).unwrap();

    /// Count of v6 renewals allowed in degraded mode (known active leases)
    pub static ref CLUSTER_V6_DEGRADED_RENEWALS: IntCounter = register_int_counter!(
        "cluster_v6_degraded_renewals",
        "count of DHCPv6 renewals granted in degraded mode for known active leases"
    ).unwrap();

    /// Count of v6 lease coordination conflicts detected
    pub static ref CLUSTER_V6_CONFLICTS: IntCounter = register_int_counter!(
        "cluster_v6_conflicts",
        "count of DHCPv6 lease coordination conflicts detected"
    ).unwrap();

    /// Count of v6 invalid lease key rejections (missing DUID/IAID)
    pub static ref CLUSTER_V6_INVALID_KEY: IntCounter = register_int_counter!(
        "cluster_v6_invalid_key",
        "count of DHCPv6 requests rejected due to missing/invalid DUID or IAID"
    ).unwrap();
}
