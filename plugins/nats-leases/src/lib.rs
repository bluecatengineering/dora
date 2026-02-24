#![warn(
    missing_debug_implementations,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod backend;
pub mod metrics;
pub mod nats_backend;
pub mod v4;
pub mod v6;

pub use backend::{BackendError, LeaseBackend};
pub use nats_backend::NatsBackend;
pub use v4::NatsLeases;
pub use v6::NatsV6Leases;
