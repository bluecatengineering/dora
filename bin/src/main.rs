#![allow(clippy::cognitive_complexity)]
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};

use config::DhcpConfig;
use dora_core::{
    Register, Server,
    config::{
        cli::{self, Parser},
        trace,
    },
    dhcproto::{v4, v6},
    tokio::{self, runtime::Builder, signal, task::JoinHandle},
    tracing::*,
};
use external_api::{ExternalApi, Health};
use ip_manager::{IpManager, sqlite::SqliteDb};
use leases::Leases;
use message_type::MsgType;
use nats_host_options::HostOptionSync;
use static_addr::StaticAddr;

#[cfg(not(target_env = "musl"))]
use jemallocator::Jemalloc;
use tokio_util::sync::CancellationToken;

#[cfg(not(target_env = "musl"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

fn main() -> Result<()> {
    // parses from cli or environment var
    let config = cli::Config::parse();
    let trace_config = trace::Config::parse(&config.dora_log)?;
    debug!(?config, ?trace_config);
    if let Err(err) = dotenv::dotenv() {
        debug!(?err, ".env file not loaded");
    }

    let mut builder = Builder::new_multi_thread();
    // configure thread name & enable IO/time
    builder.thread_name(&config.thread_name).enable_all();
    // default num threads will be num logical CPUs
    // if we have a configured value here, set it
    if let Some(num) = config.threads {
        builder.worker_threads(num);
    }
    // build the runtime
    let rt = builder.build()?;

    rt.block_on(async move {
        match dora_core::tokio::spawn(async move { start(config).await }).await {
            Err(err) => error!(?err, "failed to start server"),
            Ok(Err(err)) => error!(?err, "exited with error"),
            Ok(_) => debug!("exiting..."),
        }
    });

    Ok(())
}

async fn start(config: cli::Config) -> Result<()> {
    let database_url = config.database_url.clone();
    info!(?database_url, "using database at path");
    let dora_id = config.dora_id.clone();
    info!(?dora_id, "using id");
    // setting DORA_ID for other plugins
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("DORA_ID", &dora_id) };

    debug!("parsing DHCP config");
    let dhcp_cfg = Arc::new(DhcpConfig::parse(&config.config_path)?);

    // Determine backend mode
    let backend_mode = dhcp_cfg.backend_mode();
    info!(?backend_mode, "lease backend mode");

    match backend_mode {
        config::wire::BackendMode::Standalone => {
            info!("starting in standalone mode (SQLite backend)");
            start_standalone(config, dhcp_cfg, database_url).await
        }
        config::wire::BackendMode::Nats => {
            info!("starting in nats mode (NATS coordination)");
            start_clustered(config, dhcp_cfg, database_url).await
        }
    }
}

/// Start the server in standalone mode with SQLite backend (existing path).
async fn start_standalone(
    config: cli::Config,
    dhcp_cfg: Arc<DhcpConfig>,
    database_url: String,
) -> Result<()> {
    debug!("starting database");
    let ip_mgr = Arc::new(IpManager::new(SqliteDb::new(database_url).await?)?);
    // start external api for healthchecks
    let api = ExternalApi::new(
        config.external_api,
        Arc::clone(&dhcp_cfg),
        Arc::clone(&ip_mgr),
    );
    // start v4 server
    debug!("starting v4 server");
    let mut v4: Server<v4::Message> =
        Server::new(config.clone(), dhcp_cfg.v4().interfaces().to_owned())?;
    debug!("starting v4 plugins");

    MsgType::new(Arc::clone(&dhcp_cfg))?.register(&mut v4);
    StaticAddr::new(Arc::clone(&dhcp_cfg))?.register(&mut v4);
    Leases::with_ip_manager(Arc::clone(&dhcp_cfg), Arc::clone(&ip_mgr)).register(&mut v4);

    let v6 = if dhcp_cfg.has_v6() {
        info!("starting v6 server");
        let mut v6: Server<v6::Message> =
            Server::new(config.clone(), dhcp_cfg.v6().interfaces().to_owned())?;
        info!("starting v6 plugins");
        MsgType::new(Arc::clone(&dhcp_cfg))?.register(&mut v6);
        Some(v6)
    } else {
        None
    };

    debug!("changing health to good");
    api.sender()
        .send(Health::Good)
        .await
        .context("error occurred in changing health status to Good")?;

    let token = CancellationToken::new();
    let api_guard = api.start(token.clone());
    match v6 {
        Some(v6) => {
            tokio::try_join!(
                flatten(tokio::spawn(v4.start(shutdown_signal(token.clone())))),
                flatten(tokio::spawn(v6.start(shutdown_signal(token.clone())))),
            )?;
        }
        None => {
            tokio::spawn(v4.start(shutdown_signal(token.clone()))).await??;
        }
    };
    if let Err(err) = api_guard.await {
        error!(?err, "error waiting for web server API");
    }
    Ok(())
}

/// Start the server in nats mode with NATS coordination.
async fn start_clustered(
    config: cli::Config,
    dhcp_cfg: Arc<DhcpConfig>,
    database_url: String,
) -> Result<()> {
    let cluster_config = dhcp_cfg
        .nats()
        .ok_or_else(|| anyhow!("nats mode requires nats configuration"))?
        .clone();

    let server_id = config.effective_instance_id().to_string();
    info!(?server_id, "nats server identity");

    // Build NATS coordination components
    let subject_resolver = nats_coordination::SubjectResolver::new(
        cluster_config.subjects.clone(),
        cluster_config.contract_version.clone(),
    )
    .map_err(|e| anyhow!("subject resolver error: {e}"))?;

    let nats_client = nats_coordination::NatsClient::new(cluster_config.clone(), subject_resolver);

    // Connect to NATS
    info!("connecting to NATS for coordination");
    nats_client
        .connect()
        .await
        .map_err(|e| anyhow!("NATS connection failed: {e}"))?;
    info!("NATS connection established for nats mode");

    // Create lease coordinator
    let lease_coordinator =
        nats_coordination::LeaseCoordinator::new(nats_client.clone(), server_id.clone());

    // Create local IpManager for address selection and ping checks
    debug!("starting database (local cache for nats mode)");
    let ip_mgr = Arc::new(IpManager::new(SqliteDb::new(database_url).await?)?);

    // Clone coordinator/server_id for v6 before moving into v4 backend
    let v6_lease_coordinator = lease_coordinator.clone();
    let v6_server_id = server_id.clone();

    // Create nats backend
    let nats_backend =
        nats_leases::NatsBackend::new(Arc::clone(&ip_mgr), lease_coordinator, server_id);
    let backend = Arc::new(nats_backend);

    // Create host-option lookup client for response enrichment
    let host_option_client = nats_coordination::HostOptionClient::new(nats_client.clone());

    // Start external API (uses local IpManager for /leases endpoint)
    let api = ExternalApi::new(
        config.external_api,
        Arc::clone(&dhcp_cfg),
        Arc::clone(&ip_mgr),
    );

    // Start v4 server with nats leases plugin and host-option sync
    debug!("starting v4 server (nats)");
    let mut v4: Server<v4::Message> =
        Server::new(config.clone(), dhcp_cfg.v4().interfaces().to_owned())?;
    debug!("starting v4 plugins (nats)");

    MsgType::new(Arc::clone(&dhcp_cfg))?.register(&mut v4);
    StaticAddr::new(Arc::clone(&dhcp_cfg))?.register(&mut v4);
    Leases::new(Arc::clone(&dhcp_cfg), backend).register(&mut v4);
    HostOptionSync::new(host_option_client.clone()).register(&mut v4);

    let v6 = if dhcp_cfg.has_v6() {
        info!("starting v6 server (nats)");
        let mut v6: Server<v6::Message> =
            Server::new(config.clone(), dhcp_cfg.v6().interfaces().to_owned())?;
        info!("starting v6 plugins (nats)");
        MsgType::new(Arc::clone(&dhcp_cfg))?.register(&mut v6);
        // Register stateful v6 lease plugin for nats mode
        nats_leases::NatsV6Leases::new(Arc::clone(&dhcp_cfg), v6_lease_coordinator, v6_server_id)
            .register(&mut v6);
        HostOptionSync::new(host_option_client.clone()).register(&mut v6);
        Some(v6)
    } else {
        None
    };

    debug!("changing health to good");
    api.sender()
        .send(Health::Good)
        .await
        .context("error occurred in changing health status to Good")?;

    // Update coordination state metric (owned by nats-leases plugin)
    nats_leases::metrics::CLUSTER_COORDINATION_STATE.set(1);

    let token = CancellationToken::new();
    let api_guard = api.start(token.clone());
    match v6 {
        Some(v6) => {
            tokio::try_join!(
                flatten(tokio::spawn(v4.start(shutdown_signal(token.clone())))),
                flatten(tokio::spawn(v6.start(shutdown_signal(token.clone())))),
            )?;
        }
        None => {
            tokio::spawn(v4.start(shutdown_signal(token.clone()))).await??;
        }
    };
    if let Err(err) = api_guard.await {
        error!(?err, "error waiting for web server API");
    }
    Ok(())
}

async fn flatten<T>(handle: JoinHandle<Result<T, anyhow::Error>>) -> Result<T, anyhow::Error> {
    match handle.await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err(err),
        Err(err) => Err(anyhow!(err)),
    }
}

async fn shutdown_signal(token: CancellationToken) -> Result<()> {
    let ret = signal::ctrl_c().await.map_err(|err| anyhow!(err));
    token.cancel();
    ret
}
