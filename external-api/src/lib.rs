//! # Healthcheck & API
//!
//! This crate provides http api's for healthcheck, diagnostics, and metrics
//! It exposes the following endpoints:
//!
//! /health
//! /ping
#![warn(
    missing_debug_implementations,
    missing_docs,
    rust_2018_idioms,
    unreachable_pub,
    non_snake_case,
    non_upper_case_globals
)]
#![deny(rustdoc::broken_intra_doc_links)]
#![allow(clippy::cognitive_complexity, clippy::too_many_arguments)]

use std::{net::SocketAddr, sync::Arc, time::Duration};

use anyhow::Result;
use axum::{Router, extract::Extension, routing};

use ip_manager::{IpManager, Storage};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, trace};

pub use crate::models::{Health, State};
use config::DhcpConfig;

/// The task runner for the [`ExternalApi`]
///
/// [`ExternalAPI`]: crate::ExternalApi
#[derive(Debug)]
pub struct ExternalApiGuard {
    task_handle: JoinHandle<()>,
}

impl Drop for ExternalApiGuard {
    fn drop(&mut self) {
        trace!("ExternalApiRunner drop called");
        self.task_handle.abort();
    }
}

/// Listens to relevant channels to gather information about
/// the running system and reports this data in an HTTP API
#[derive(Debug)]
pub struct ExternalApi<S> {
    tx: mpsc::Sender<Health>,
    rx: mpsc::Receiver<Health>,
    addr: SocketAddr,
    state: State,
    ip_mgr: Arc<IpManager<S>>,
    cfg: Arc<DhcpConfig>,
}

impl<S: Storage> ExternalApi<S> {
    /// Create a new ExternalApi instance
    pub fn new(addr: SocketAddr, cfg: Arc<DhcpConfig>, ip_mgr: Arc<IpManager<S>>) -> Self {
        trace!("starting external api");
        let (tx, rx) = mpsc::channel(10);
        let state = models::blank_health();
        Self {
            tx,
            rx,
            addr,
            state,
            ip_mgr,
            cfg,
        }
    }

    /// clone the health sender channel
    pub fn sender(&self) -> mpsc::Sender<Health> {
        self.tx.clone()
    }

    /// Set the health
    pub async fn set_health(&self, health: Health) {
        *self.state.lock() = health;
    }

    /// Listen to Health changes over the channel
    async fn listen_status(&mut self) -> Result<()> {
        while let Some(health) = self.rx.recv().await {
            let mut guard = self.state.lock();
            if *guard != health {
                *guard = health;
            }
        }
        info!("listen health exited-- nothing listening");
        Ok(())
    }

    /// serve the HTTP external api
    async fn run(
        addr: SocketAddr,
        state: State,
        cfg: Arc<DhcpConfig>,
        ip_mgr: Arc<IpManager<S>>,
        token: CancellationToken,
    ) -> Result<()> {
        const TIMEOUT: u64 = 30;
        use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};
        // Provides:
        // /health
        // /ping
        // /metrics
        // /metrics-text
        // /leases
        let service = Router::new()
            .route("/health", routing::get(handlers::ok))
            .route("/ping", routing::get(handlers::ping))
            .route("/metrics", routing::get(handlers::metrics))
            .route("/metrics-text", routing::get(handlers::metrics_text))
            .route("/v1/leases", routing::get(handlers::leases::<S>))
            .route("/config", routing::get(handlers::config))
            .layer(TraceLayer::new_for_http())
            .layer(TimeoutLayer::new(Duration::from_secs(TIMEOUT)))
            .layer(Extension(state))
            .layer(Extension(ip_mgr))
            .layer(Extension(cfg));

        let tcp = TcpListener::bind(&addr).await?;
        tracing::debug!(%addr, "external API listening");

        axum::serve(tcp, service)
            .with_graceful_shutdown(async move {
                token.cancelled().await;
            })
            .await?;
        Ok(())
    }

    /// Kick off the HTTP service and start listening on all channels for
    /// changes
    pub fn start(mut self, token: CancellationToken) -> JoinHandle<()> {
        let state = self.state.clone();
        let addr = self.addr;
        let ip_mgr = self.ip_mgr.clone();
        let cfg = self.cfg.clone();
        // if tx is not cloned, health listen will never update since ExternalApi is owner

        tokio::spawn(async move {
            // `run` will exit when cancel token completes
            tokio::select! {
                r = ExternalApi::run(addr, state, cfg, ip_mgr, token) => {
                    if let Err(err) = r {
                        error!(?err, "external api task returned error")
                    }
                    // exiting
                }
                _ = self.listen_status() => {}
            }
        })
    }

    /// Start the `ExternalApiRunner`
    pub fn serve(self, token: CancellationToken) -> ExternalApiGuard {
        ExternalApiGuard {
            task_handle: self.start(token),
        }
    }
}

mod handlers {

    use std::{collections::HashMap, sync::Arc, time::UNIX_EPOCH};

    use anyhow::Context;
    use axum::{
        body::Body,
        extract::Extension,
        http::header,
        http::{Response, StatusCode},
        response::IntoResponse,
    };
    use chrono::{DateTime, Utc};
    use config::DhcpConfig;
    use dora_core::metrics::{START_TIME, UPTIME};
    use ip_manager::{IpManager, Storage};
    use ipnet::Ipv4Net;
    use prometheus::{Encoder, ProtobufEncoder, TextEncoder};
    use tracing::{error, warn};

    use crate::models::{Health, ReserveIp, ServerResult, State};

    pub(crate) async fn ok(Extension(state): Extension<State>) -> ServerResult<impl IntoResponse> {
        Ok(match *state.lock() {
            Health::Good => StatusCode::OK,
            Health::Bad => StatusCode::INTERNAL_SERVER_ERROR,
        })
    }

    pub(crate) async fn leases<S: Storage>(
        Extension(cfg): Extension<Arc<DhcpConfig>>,
        Extension(ip_mgr): Extension<Arc<IpManager<S>>>,
    ) -> ServerResult<axum::Json<crate::models::Leases>> {
        use crate::models::{LeaseIp, LeaseNetworks, LeaseState, Leases};
        use ip_manager::State as S;

        let cfg = (*cfg).clone();
        let mut networks = ip_mgr
            .select_all()
            .await?
            .into_iter()
            .map(|lease| {
                let info = lease.as_ref();
                let ip = info.ip();
                let id = info.id().map(hex::encode);
                let secs = info.expires_at().duration_since(UNIX_EPOCH)?.as_secs();
                let network = info.network();
                let expires_at_epoch = secs;
                let expires_at_utc = DateTime::<Utc>::from_timestamp(
                    info.expires_at().duration_since(UNIX_EPOCH)?.as_secs() as i64,
                    0,
                )
                .context("failed to create UTC datetime")?
                .to_rfc3339();
                let lease_info = LeaseIp {
                    ip,
                    id: id.clone(),
                    expires_at_epoch,
                    expires_at_utc,
                };

                let netv4 = match network {
                    std::net::IpAddr::V4(ip) => ip,
                    std::net::IpAddr::V6(_) => {
                        // TODO
                        warn!("/v1/leases does not support not dynamic ipv6 at this time");
                        return Ok(None);
                    }
                };
                if let Some(net) = cfg.v4().network(netv4) {
                    Ok(match lease {
                        S::Leased(_) => Some((net, LeaseState::Leased(lease_info))),
                        S::Probated(_) => Some((net, LeaseState::Probated(lease_info))),
                        // TODO if we store reserved in db, change this
                        S::Reserved(_) => None,
                    })
                } else {
                    Err(anyhow::anyhow!(
                        "failed to find network in cfg for {lease_info:?}"
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .fold(
                HashMap::<Ipv4Net, LeaseNetworks>::new(),
                |mut map, (net, lease)| {
                    let entry = map.entry(net.full_subnet()).or_default();
                    entry.ips.push(lease);

                    map
                },
            );
        // add reserved entries from config
        // TODO if we start to store reserved in db, then delete this
        for net in cfg.v4().networks().values() {
            for reservation in net.get_reservations() {
                let entry = networks.entry(net.full_subnet()).or_default();
                entry.ips.push(LeaseState::Reserved(ReserveIp {
                    ip: reservation.ip().into(),
                    id: None,
                    condition: reservation.condition().clone(),
                }))
            }
        }

        Ok(axum::Json(Leases { networks }))
    }

    pub(crate) async fn config(
        Extension(cfg): Extension<Arc<DhcpConfig>>,
    ) -> ServerResult<impl IntoResponse> {
        // TODO: if serializing worked we could get DhcpConfig back into JSON/YAML but there's
        // a lot of logic left to make that particular transform. So just read from disk
        let path = cfg.path().context("no path specified for config")?;
        let cfg = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to find config at {}", path.display()))?;
        Ok(axum::Json(cfg))
    }

    pub(crate) async fn metrics() -> ServerResult<impl IntoResponse> {
        UPTIME.set(START_TIME.elapsed().as_secs() as i64);
        let encoder = ProtobufEncoder::new();
        let mut buf = Vec::new();
        let mf = prometheus::gather();
        let resp = Response::builder().header(header::CONTENT_TYPE, encoder.format_type());

        match encoder.encode(&mf, &mut buf) {
            Err(err) => {
                error!(?err, "error protobuf encoding prometheus metrics");
                Ok(resp
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty())?)
            }
            Ok(_) => Ok(resp.status(StatusCode::OK).body(Body::from(buf))?),
        }
    }

    pub(crate) async fn metrics_text() -> ServerResult<impl IntoResponse> {
        UPTIME.set(START_TIME.elapsed().as_secs() as i64);
        let encoder = TextEncoder::new();
        let mut buf = String::new();
        let mf = prometheus::gather();
        let resp = Response::builder().header(header::CONTENT_TYPE, encoder.format_type());

        match encoder.encode_utf8(&mf, &mut buf) {
            Err(err) => {
                error!(?err, "error text encoding prometheus metrics");
                Ok(resp
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty())?)
            }
            Ok(_) => Ok(resp.status(StatusCode::OK).body(Body::from(buf))?),
        }
    }

    pub(crate) async fn ping() -> impl IntoResponse {
        StatusCode::OK
    }
}

/// Various models for API responses
pub mod models {
    use std::{collections::HashMap, fmt, net::IpAddr, sync::Arc};

    use axum::response::IntoResponse;
    use config::wire::v4::Condition;
    use ipnet::Ipv4Net;
    use parking_lot::Mutex;
    use serde::{Deserialize, Serialize};

    /// The overall health of the system
    pub type State = Arc<Mutex<Health>>;
    /// Health is binary Good/Bad at the moment
    #[derive(Serialize, Deserialize, Debug, PartialEq, Copy, Clone, Eq)]
    #[serde(rename_all = "UPPERCASE")]
    pub enum Health {
        /// Report good health
        Good,
        /// Report bad health
        Bad,
    }

    impl fmt::Display for Health {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "{}",
                match *self {
                    Health::Good => "GOOD",
                    Health::Bad => "BAD",
                }
            )
        }
    }

    /// leases table
    #[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone, Eq)]
    pub struct Leases {
        /// map of networks
        pub networks: HashMap<Ipv4Net, LeaseNetworks>,
    }

    /// list of leases
    #[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone, Eq)]
    pub struct LeaseNetworks {
        /// list of ips in database
        pub ips: Vec<LeaseState>,
    }

    /// lease state
    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Eq)]
    #[serde(tag = "type", rename_all = "lowercase")]
    pub enum LeaseState {
        /// reserved
        Reserved(ReserveIp),
        /// leased
        Leased(LeaseIp),
        /// probated ip
        Probated(LeaseIp),
    }

    /// details about lease ip
    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Eq)]
    pub struct LeaseIp {
        /// ip
        pub ip: IpAddr,
        /// id
        pub id: Option<String>,
        /// expiry as u64
        pub expires_at_epoch: u64,
        /// expiry as string
        pub expires_at_utc: String,
    }

    /// static reservation
    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Eq)]
    pub struct ReserveIp {
        /// ip
        pub ip: IpAddr,
        /// id: will be None for now
        pub id: Option<String>,
        /// reservation condition
        #[serde(rename = "match")]
        pub condition: Condition,
    }

    pub(crate) fn blank_health() -> State {
        Arc::new(Mutex::new(Health::Bad))
    }

    // error type
    /// Make our own error that wraps `anyhow::Error`.
    #[derive(Debug)]
    pub struct ServerError(anyhow::Error);
    /// return error result
    pub type ServerResult<T> = Result<T, ServerError>;

    impl IntoResponse for ServerError {
        fn into_response(self) -> axum::response::Response {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("{}", self.0),
            )
                .into_response()
        }
    }

    impl<E> From<E> for ServerError
    where
        E: Into<anyhow::Error>,
    {
        fn from(err: E) -> Self {
            Self(err.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ip_manager::sqlite::SqliteDb;

    use super::*;
    #[tokio::test]
    async fn test_health() -> anyhow::Result<()> {
        let mgr = Arc::new(IpManager::new(SqliteDb::new("sqlite::memory:").await?)?);
        let cfg = Arc::new(DhcpConfig::default());
        let api = ExternalApi::new("0.0.0.0:8889".parse().unwrap(), cfg, mgr);
        let token = CancellationToken::new();
        let _handle = api.serve(token);
        // wait for server to come up
        tokio::time::sleep(Duration::from_secs(1)).await;
        let r = reqwest::get("http://0.0.0.0:8889/health")
            .await?
            .error_for_status();
        // initial health state will be BAD i.e. 500
        match r {
            Ok(_) => {}
            Err(err) => {
                assert_eq!(
                    err.status(),
                    Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
                );
            }
        }
        Ok(())
    }
    // very simple test for existence of metrics endpoint
    #[tokio::test]
    async fn test_metrics() -> anyhow::Result<()> {
        let mgr = Arc::new(IpManager::new(SqliteDb::new("sqlite::memory:").await?)?);
        let cfg = Arc::new(DhcpConfig::default());
        let api = ExternalApi::new("0.0.0.0:8888".parse().unwrap(), cfg, mgr);
        let token = CancellationToken::new();
        let _handle = api.serve(token);
        // wait for server to come up
        tokio::time::sleep(Duration::from_secs(1)).await;
        let bytes = reqwest::get("http://0.0.0.0:8888/metrics")
            .await?
            .error_for_status()?
            .bytes()
            .await;
        assert!(bytes.is_ok());

        Ok(())
    }
}
