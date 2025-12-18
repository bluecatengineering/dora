//! # discovery
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
use anyhow::{Context, Result};
use hickory_resolver::config::ResolverOpts;
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::{Resolver, TokioResolver, lookup::Ipv4Lookup};

/// DNS service discovery
#[derive(Debug)]
pub struct DnsServiceDiscovery {
    resolver: TokioResolver,
}

impl DnsServiceDiscovery {
    /// Create a new service
    pub fn new() -> Result<Self> {
        Ok(Self {
            resolver: Resolver::builder(TokioConnectionProvider::default())
                .context("failed to create tokio resolver")?
                .with_options(ResolverOpts::default())
                .build(),
        })
    }

    /// do a DNS lookup, returning a URL with the "http" schema
    /// ex.
    ///     lookup_http("foobar.internal", 67) -> "http://1.2.3.4:67"
    pub async fn lookup_http(&self, addr: impl AsRef<str>, port: u16) -> Result<String> {
        self.lookup("http", addr, port).await
    }

    /// do a DNS lookup, returning a URL
    /// ex.
    ///     lookup("http", "foobar.internal", 67) -> "http://1.2.3.4:67"
    pub async fn lookup(
        &self,
        schema: impl AsRef<str>,
        addr: impl AsRef<str>,
        port: u16,
    ) -> Result<String> {
        let get_first = |iter: Ipv4Lookup| {
            iter.iter()
                .next()
                .map(|addr| format!("{}://{}:{}", schema.as_ref(), addr, port))
        };
        let addrs = self.resolver.ipv4_lookup(addr.as_ref()).await?;

        get_first(addrs).context("failed to lookup addr")
    }
}
