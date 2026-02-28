#![warn(
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod metrics;
pub mod nats_backend;
pub mod v6;

pub use nats_backend::NatsBackend;
pub use v6::NatsV6Leases;

/// Concrete v4 leases plugin type for NATS mode.
///
/// This aliases the shared `leases::Leases` plugin over the NATS backend.
pub type NatsV4Leases = leases::Leases<NatsBackend<ip_manager::sqlite::SqliteDb>>;
