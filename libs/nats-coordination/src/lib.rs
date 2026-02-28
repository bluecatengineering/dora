//! # nats-coordination
//!
//! Reusable coordination crate for nats-mode DHCP lease and host-option
//! operations backed by NATS.
//!
//! This library provides:
//! - **Typed models** matching the AsyncAPI contract for lease records,
//!   host-option lookups, snapshots, and coordination events.
//! - **Subject resolver** with configurable templates, defaults, and
//!   contract-version awareness.
//! - **Connection manager** with optional auth/encryption mode support
//!   and connection state observability.
//! - **Lease coordination client** with reserve/lease/release/probate/snapshot
//!   operations.
//! - **Host-option lookup client** backed by JetStream KV.
//!
//! ## Design Principles
//!
//! - Small, testable APIs that avoid leaking NATS transport details into plugins.
//! - No hard-coded subject strings in runtime paths.
//! - Transport/security mode support is flexible and not mandatory.
//! - All message structures aligned with the versioned AsyncAPI contract.

pub mod client;
pub mod error;
pub mod host_options;
pub mod lease;
pub mod models;
pub mod subjects;

// Re-export key types for convenient access
pub use client::{ConnectionState, NatsClient};
pub use error::{CoordinationError, CoordinationResult};
pub use host_options::HostOptionClient;
pub use lease::{LeaseCoordinator, LeaseOutcome, RetryPolicy};
pub use models::{
    CoordinationEvent, CoordinationEventType, HostOptionOutcome, LeaseRecord, LeaseSnapshotRequest,
    LeaseSnapshotResponse, LeaseState, ProtocolFamily,
};
pub use subjects::{Channel, SubjectResolver};
