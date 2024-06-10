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

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Result, bail};
use axum::{Router, extract::Extension, routing};

use ip_manager::{IpManager, Storage};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
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
    ) -> Result<()> {
        let tcp = TcpListener::bind(&addr).await?;
        // Provides:
        // /health
        // /ping
        // /metrics
        // /metrics-text
        // /leases
        let app = Router::new()
            .route("/health", routing::get(handlers::ok))
            .route("/ping", routing::get(handlers::ping))
            .route("/metrics", routing::get(handlers::metrics))
            .route("/metrics-text", routing::get(handlers::metrics_text))
            .route("/leases", routing::get(handlers::leases::<S>))
            .route("/config", routing::get(handlers::config))
            .layer(Extension(state))
            .layer(Extension(ip_mgr))
            .layer(Extension(cfg));

        tracing::debug!("external API listening on {}", addr);

        axum::serve(tcp, app).await?;
        bail!("external API returned-- should not happen")
    }

    /// Kick off the HTTP service and start listening on all channels for
    /// changes
    pub fn start(mut self) -> JoinHandle<()> {
        let state = self.state.clone();
        let addr = self.addr;
        let ip_mgr = self.ip_mgr.clone();
        let cfg = self.cfg.clone();
        // if tx is not cloned, health listen will never update since ExternalApi is owner

        tokio::spawn(async move {
            if let Err(err) = tokio::try_join!(
                ExternalApi::run(addr, state, cfg, ip_mgr),
                self.listen_status()
            ) {
                error!(?err, "health task returning, this should not happen")
            }
        })
    }

    /// Start the `ExternalApiRunner`
    pub fn serve(self) -> ExternalApiGuard {
        ExternalApiGuard {
            task_handle: self.start(),
        }
    }
}

mod handlers {

    use std::sync::Arc;

    use anyhow::Context;
    use axum::{
        body::Body,
        extract::Extension,
        http::header,
        http::{Response, StatusCode},
        response::IntoResponse,
    };
    use config::DhcpConfig;
    use dora_core::metrics::{START_TIME, UPTIME};
    use ip_manager::{IpManager, Storage};
    use prometheus::{Encoder, ProtobufEncoder, TextEncoder};
    use tracing::error;

    use crate::models::{Health, ServerResult, State};

    pub(crate) async fn ok(Extension(state): Extension<State>) -> ServerResult<impl IntoResponse> {
        Ok(match *state.lock() {
            Health::Good => StatusCode::OK,
            Health::Bad => StatusCode::INTERNAL_SERVER_ERROR,
        })
    }

    pub(crate) async fn leases<S: Storage>(
        Extension(cfg): Extension<Arc<DhcpConfig>>,
        Extension(ip_mgr): Extension<Arc<IpManager<S>>>,
    ) -> ServerResult<impl IntoResponse> {
        let leases = ip_mgr.select_all().await;
        todo!();
        Ok(())
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
        Ok(serde_json::to_string_pretty(&cfg)?)
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
    use axum::response::IntoResponse;
    use parking_lot::Mutex;
    use serde::{Deserialize, Serialize};
    use std::{fmt, sync::Arc};

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
        let _handle = api.serve();
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
        let _handle = api.serve();
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
