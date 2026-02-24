#![allow(clippy::cognitive_complexity)]
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};

mod startup_health;

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
use ip_manager::{IpManager, memory::MemoryStore, sqlite::SqliteDb};
use leases::Leases;
use message_type::MsgType;
use nats_host_options::HostOptionSync;
use nats_leases::{NatsBackend, NatsLeases, NatsV6Leases};
use startup_health::{verify_background_task_running, verify_startup_subsystems};
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
            info!(?database_url, "using database at path");
            info!("starting in standalone mode (SQLite backend)");
            start_standalone(config, dhcp_cfg, database_url).await
        }
        config::wire::BackendMode::Nats => {
            info!("starting in nats mode (NATS coordination, no local SQLite)");
            start_nats(config, dhcp_cfg).await
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
    Leases::new(Arc::clone(&dhcp_cfg), Arc::clone(&ip_mgr)).register(&mut v4);

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

    let token = CancellationToken::new();
    let api_sender = api.sender();
    let mut api_guard = api.start(token.clone());

    let mut v4_task = tokio::spawn(v4.start(shutdown_signal(token.clone())));
    let mut v6_task = v6.map(|v6| tokio::spawn(v6.start(shutdown_signal(token.clone()))));

    // Keep health BAD until all startup-critical tasks are confirmed running.
    if let Err(err) =
        verify_startup_subsystems(&mut api_guard, &mut v4_task, v6_task.as_mut(), "standalone")
            .await
    {
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }

    debug!("changing health to good after startup checks passed");
    api_sender
        .send(Health::Good)
        .await
        .context("error occurred in changing health status to Good")?;

    let server_result = match v6_task {
        Some(v6_task) => tokio::try_join!(flatten(v4_task), flatten(v6_task)).map(|_| ()),
        None => flatten(v4_task).await.map(|_| ()),
    };

    // Propagate server errors if any
    if let Err(err) = server_result {
        // Set health to bad since server failed
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }
    if let Err(err) = api_guard.await {
        error!(?err, "error waiting for web server API");
    }
    Ok(())
}

/// Start the server in nats mode with NATS coordination.
async fn start_nats(config: cli::Config, dhcp_cfg: Arc<DhcpConfig>) -> Result<()> {
    let nats_config = dhcp_cfg
        .nats()
        .ok_or_else(|| anyhow!("nats mode requires nats configuration"))?
        .clone();

    let server_id = config.effective_instance_id().to_string();
    info!(?server_id, "nats server identity");

    // Build NATS coordination components
    let subject_resolver = nats_coordination::SubjectResolver::new(
        nats_config.subjects.clone(),
        nats_config.contract_version.clone(),
    )
    .map_err(|e| anyhow!("subject resolver error: {e}"))?;

    let nats_client = nats_coordination::NatsClient::new(nats_config.clone(), subject_resolver);

    // Connect to NATS
    info!("connecting to NATS for nats coordination");
    nats_client
        .connect()
        .await
        .map_err(|e| anyhow!("NATS connection failed: {e}"))?;
    info!("NATS connection established for nats mode");

    // Create lease coordinator
    let lease_coordinator =
        nats_coordination::LeaseCoordinator::new(nats_client.clone(), server_id.clone());
    let gc_coordinator = lease_coordinator.clone();

    // Create local in-memory IpManager for address selection and ping checks.
    // NATS mode avoids local SQLite persistence and uses JetStream for coordination state.
    debug!("starting in-memory lease cache for nats mode");
    let ip_mgr = Arc::new(IpManager::new(MemoryStore::new())?);

    // Clone coordinator/server_id for v6 before moving into v4 NATS backend
    let v6_lease_coordinator = lease_coordinator.clone();
    let v6_server_id = server_id.clone();

    // Create NATS backend
    let nats_backend = NatsBackend::new(Arc::clone(&ip_mgr), lease_coordinator, server_id);

    // Get coordination availability flag for background monitor before moving backend
    let coordination_available = nats_backend.coordination_available();

    if let Err(err) = nats_leases::LeaseBackend::reconcile(&nats_backend).await {
        warn!(?err, "nats backend initial reconcile failed");
    }

    // Mark coordination as available after initial reconcile
    coordination_available.store(true, std::sync::atomic::Ordering::Relaxed);

    let backend: Arc<dyn nats_leases::LeaseBackend> = Arc::new(nats_backend);

    // Create host-option lookup client for response enrichment
    let host_option_client = nats_coordination::HostOptionClient::new(nats_client.clone());

    // Start external API (uses local IpManager for /leases endpoint)
    let api = ExternalApi::new(
        config.external_api,
        Arc::clone(&dhcp_cfg),
        Arc::clone(&ip_mgr),
    );

    // Start v4 server with NATS leases plugin and host-option sync
    debug!("starting v4 server (nats)");
    let mut v4: Server<v4::Message> =
        Server::new(config.clone(), dhcp_cfg.v4().interfaces().to_owned())?;
    debug!("starting v4 plugins (nats)");

    MsgType::new(Arc::clone(&dhcp_cfg))?.register(&mut v4);
    StaticAddr::new(Arc::clone(&dhcp_cfg))?.register(&mut v4);
    NatsLeases::new(Arc::clone(&dhcp_cfg), backend).register(&mut v4);
    HostOptionSync::new(host_option_client.clone()).register(&mut v4);

    let v6 = if dhcp_cfg.has_v6() {
        info!("starting v6 server (nats)");
        let mut v6: Server<v6::Message> =
            Server::new(config.clone(), dhcp_cfg.v6().interfaces().to_owned())?;
        info!("starting v6 plugins (nats)");
        MsgType::new(Arc::clone(&dhcp_cfg))?.register(&mut v6);
        // Register stateful v6 lease plugin for nats mode
        NatsV6Leases::new(Arc::clone(&dhcp_cfg), v6_lease_coordinator, v6_server_id)
            .register(&mut v6);
        HostOptionSync::new(host_option_client.clone()).register(&mut v6);
        Some(v6)
    } else {
        None
    };

    let token = CancellationToken::new();
    let mut gc_task =
        spawn_lease_gc_task(gc_coordinator, nats_config.lease_gc_interval, token.clone());

    // Spawn background task to monitor NATS connection state and update coordination availability flag
    let mut coordination_monitor = spawn_coordination_monitor_task(
        nats_client.clone(),
        coordination_available,
        nats_config.coordination_state_poll_interval,
        token.clone(),
    );

    let api_sender = api.sender();
    let mut api_guard = api.start(token.clone());

    let mut v4_task = tokio::spawn(v4.start(shutdown_signal(token.clone())));
    let mut v6_task = v6.map(|v6| tokio::spawn(v6.start(shutdown_signal(token.clone()))));

    // Keep health BAD until all startup-critical tasks are confirmed running.
    if let Err(err) =
        verify_startup_subsystems(&mut api_guard, &mut v4_task, v6_task.as_mut(), "nats").await
    {
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }
    if let Err(err) = verify_background_task_running("nats lease GC", &mut gc_task).await {
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }
    if let Err(err) =
        verify_background_task_running("nats coordination monitor", &mut coordination_monitor).await
    {
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }
    if let Err(err) = nats_client
        .startup_write_selftest()
        .await
        .map_err(|e| anyhow!("nats startup write selftest failed: {e}"))
    {
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }

    debug!("changing health to good after startup checks and write selftest passed");
    api_sender
        .send(Health::Good)
        .await
        .context("error occurred in changing health status to Good")?;

    let server_result = match v6_task {
        Some(v6_task) => tokio::try_join!(flatten(v4_task), flatten(v6_task)).map(|_| ()),
        None => flatten(v4_task).await.map(|_| ()),
    };

    // Propagate server errors if any
    if let Err(err) = server_result {
        // Set health to bad since server failed
        let _ = api_sender.send(Health::Bad).await;
        token.cancel();
        return Err(err);
    }
    if let Err(err) = api_guard.await {
        error!(?err, "error waiting for web server API");
    }
    if let Err(err) = gc_task.await {
        error!(?err, "error waiting for lease GC task");
    }
    if let Err(err) = coordination_monitor.await {
        error!(?err, "error waiting for coordination monitor task");
    }
    Ok(())
}

fn spawn_lease_gc_task(
    coordinator: nats_coordination::LeaseCoordinator,
    interval: std::time::Duration,
    token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    debug!("nats lease GC task stopping");
                    return;
                }
                _ = ticker.tick() => {
                    match coordinator.gc_expired().await {
                        Ok(stats) => {
                            nats_leases::metrics::CLUSTER_GC_SWEEPS.inc();
                            nats_leases::metrics::CLUSTER_GC_EXPIRED.inc_by(stats.expired_records);
                            nats_leases::metrics::CLUSTER_GC_ORPHANED_INDEXES.inc_by(stats.orphan_indexes);
                            debug!(expired = stats.expired_records, orphaned = stats.orphan_indexes, "nats lease GC sweep completed");
                        }
                        Err(err) => {
                            nats_leases::metrics::CLUSTER_GC_ERRORS.inc();
                            warn!(?err, "nats lease GC sweep failed");
                        }
                    }
                }
            }
        }
    })
}

fn spawn_coordination_monitor_task(
    nats_client: nats_coordination::NatsClient,
    coordination_available: std::sync::Arc<std::sync::atomic::AtomicBool>,
    poll_interval: std::time::Duration,
    token: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(poll_interval);
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    debug!("coordination monitor task stopping");
                    return;
                }
                _ = ticker.tick() => {
                    let is_connected = nats_client.is_connected().await;
                    let was_available = coordination_available.load(std::sync::atomic::Ordering::Relaxed);

                    if is_connected != was_available {
                        coordination_available.store(is_connected, std::sync::atomic::Ordering::Relaxed);

                        if is_connected {
                            info!("NATS connection restored - coordination available");
                            nats_leases::metrics::CLUSTER_COORDINATION_STATE.set(1);
                        } else {
                            warn!("NATS connection lost - coordination unavailable");
                            nats_leases::metrics::CLUSTER_COORDINATION_STATE.set(0);
                        }
                    }
                }
            }
        }
    })
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
