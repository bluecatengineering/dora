//! # dora
//!
#![warn(
    missing_debug_implementations,
    missing_docs,
    missing_copy_implementations,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![allow(clippy::cognitive_complexity)]
#![deny(rustdoc::broken_intra_doc_links)]
#![doc(test(
    no_crate_inject,
    attr(deny(warnings, rust_2018_idioms), allow(dead_code, unused_variables))
))]
pub use anyhow;
pub use async_trait::async_trait;
pub use chrono;
pub use chrono_tz;
pub use dhcproto;
pub use pnet;
pub use tokio;
pub use tokio_stream;
pub use tracing;
pub use trust_dns_proto;
pub use unix_udp_sock;

pub use crate::server::Server;

pub mod config;
pub mod env;
pub mod handler;
pub mod metrics;
pub mod prelude;
pub mod server;

/// Register a plugin with the server
pub trait Register<T> {
    /// add plugin to one of the server's plugin lists in the implementation of
    /// this method
    fn register(self, srv: &mut Server<T>);
}
