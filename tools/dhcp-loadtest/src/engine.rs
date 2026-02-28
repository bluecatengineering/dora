use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::LoadTestConfig;
use crate::identity::{ClientIdentity, IdentityGenerator};
use crate::protocols;
use crate::report::{
    ClientResult, ErrorCategory, ErrorRecord, LoadTestReport, RunConfigSnapshot, RuntimeStats,
    Totals, V4ClientResult, V6ClientResult,
};
use crate::transport::udp_v4::UdpV4Transport;
use crate::transport::udp_v6::UdpV6Transport;
use crate::validation;

pub async fn run(config: LoadTestConfig) -> Result<LoadTestReport> {
    let started = Instant::now();
    let identity_gen = IdentityGenerator::new(config.seed);

    if config.dry_run {
        return Ok(build_dry_run_report(&config, &identity_gen));
    }

    let v4_transport = if config.protocol.includes_v4() {
        Some(Arc::new(
            UdpV4Transport::bind(Some(&config.iface)).context("bind v4 transport")?,
        ))
    } else {
        None
    };

    let v6_transport = if config.protocol.includes_v6() {
        Some(Arc::new(
            UdpV6Transport::bind(Some(&config.iface), config.iface_index)
                .context("bind v6 transport")?,
        ))
    } else {
        None
    };

    let semaphore = Arc::new(Semaphore::new(config.concurrency));
    let ramp_delay = ramp_delay(config.ramp_per_sec);

    let mut tasks = JoinSet::new();
    for client_index in 0..config.clients {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .context("acquire concurrency permit")?;

        let identity = identity_gen.identity(client_index);
        let config = config.clone();
        let v4_transport = v4_transport.clone();
        let v6_transport = v6_transport.clone();

        tasks.spawn(async move {
            let _permit = permit;
            run_single_client(client_index, identity, &config, v4_transport, v6_transport).await
        });

        if let Some(delay) = ramp_delay {
            tokio::time::sleep(delay).await;
        }
    }

    let mut clients = Vec::with_capacity(config.clients);
    while let Some(joined) = tasks.join_next().await {
        let result = joined.context("client task join failed")?;
        clients.push(result);
    }
    clients.sort_by_key(|client| client.client_index);

    let totals = compute_totals(&clients, config.clients);
    let stats = compute_stats(&clients, started.elapsed(), &totals);
    let validation = validation::run_validations(&clients, &config);
    let passed = validation.passed;

    Ok(LoadTestReport {
        config: RunConfigSnapshot::from(&config),
        dry_run: false,
        passed,
        totals,
        stats,
        validation,
        clients,
    })
}

async fn run_single_client(
    client_index: usize,
    identity: ClientIdentity,
    config: &LoadTestConfig,
    v4_transport: Option<Arc<UdpV4Transport>>,
    v6_transport: Option<Arc<UdpV6Transport>>,
) -> ClientResult {
    let mut client = ClientResult {
        client_index,
        mac: identity.mac_string(),
        duid: identity.duid_hex(),
        iaid: identity.iaid,
        v4: None,
        v6: None,
    };

    if config.protocol.includes_v4() {
        client.v4 = Some(match v4_transport {
            Some(transport) => protocols::v4::run(client_index, &identity, config, transport).await,
            None => missing_v4_transport_result(),
        });
    }

    if config.protocol.includes_v6() {
        client.v6 = Some(match v6_transport {
            Some(transport) => protocols::v6::run(client_index, &identity, config, transport).await,
            None => missing_v6_transport_result(),
        });
    }

    client
}

fn build_dry_run_report(config: &LoadTestConfig, generator: &IdentityGenerator) -> LoadTestReport {
    let clients = (0..config.clients)
        .map(|index| {
            let identity = generator.identity(index);
            ClientResult {
                client_index: index,
                mac: identity.mac_string(),
                duid: identity.duid_hex(),
                iaid: identity.iaid,
                v4: None,
                v6: None,
            }
        })
        .collect::<Vec<_>>();

    LoadTestReport {
        config: RunConfigSnapshot::from(config),
        dry_run: true,
        passed: true,
        totals: Totals {
            planned_clients: config.clients,
            completed_clients: 0,
            v4_success: 0,
            v4_failures: 0,
            v6_success: 0,
            v6_failures: 0,
            total_errors: 0,
            timeout_errors: 0,
        },
        stats: RuntimeStats {
            duration_ms: 0,
            throughput_per_sec: 0.0,
            latency_p50_ms: None,
            latency_p95_ms: None,
            latency_p99_ms: None,
            error_rate: 0.0,
        },
        validation: crate::report::ValidationSummary::dry_run(),
        clients,
    }
}

fn ramp_delay(ramp_per_sec: usize) -> Option<Duration> {
    if ramp_per_sec == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(1.0 / ramp_per_sec as f64))
    }
}

fn missing_v4_transport_result() -> V4ClientResult {
    V4ClientResult {
        success: false,
        errors: vec![ErrorRecord {
            category: ErrorCategory::Operational,
            phase: "setup".to_string(),
            message: "v4 transport unavailable".to_string(),
        }],
        ..V4ClientResult::default()
    }
}

fn missing_v6_transport_result() -> V6ClientResult {
    V6ClientResult {
        success: false,
        errors: vec![ErrorRecord {
            category: ErrorCategory::Operational,
            phase: "setup".to_string(),
            message: "v6 transport unavailable".to_string(),
        }],
        ..V6ClientResult::default()
    }
}

fn compute_totals(clients: &[ClientResult], planned_clients: usize) -> Totals {
    let v4_success = clients
        .iter()
        .filter_map(|client| client.v4.as_ref())
        .filter(|result| result.success)
        .count();
    let v4_failures = clients
        .iter()
        .filter_map(|client| client.v4.as_ref())
        .filter(|result| !result.success)
        .count();

    let v6_success = clients
        .iter()
        .filter_map(|client| client.v6.as_ref())
        .filter(|result| result.success)
        .count();
    let v6_failures = clients
        .iter()
        .filter_map(|client| client.v6.as_ref())
        .filter(|result| !result.success)
        .count();

    let total_errors = clients
        .iter()
        .map(|client| {
            client.v4.as_ref().map_or(0, |result| result.errors.len())
                + client.v6.as_ref().map_or(0, |result| result.errors.len())
        })
        .sum();

    let timeout_errors = clients
        .iter()
        .map(|client| {
            client.v4.as_ref().map_or(0, |result| {
                result
                    .errors
                    .iter()
                    .filter(|error| error.category == ErrorCategory::Timeout)
                    .count()
            }) + client.v6.as_ref().map_or(0, |result| {
                result
                    .errors
                    .iter()
                    .filter(|error| error.category == ErrorCategory::Timeout)
                    .count()
            })
        })
        .sum();

    Totals {
        planned_clients,
        completed_clients: clients.len(),
        v4_success,
        v4_failures,
        v6_success,
        v6_failures,
        total_errors,
        timeout_errors,
    }
}

fn compute_stats(clients: &[ClientResult], duration: Duration, totals: &Totals) -> RuntimeStats {
    let duration_ms = duration.as_millis();
    let duration_secs = duration.as_secs_f64().max(1e-9);

    let total_runs = clients
        .iter()
        .map(|client| usize::from(client.v4.is_some()) + usize::from(client.v6.is_some()))
        .sum::<usize>();

    let throughput_per_sec = total_runs as f64 / duration_secs;
    let error_rate = if total_runs == 0 {
        0.0
    } else {
        totals.total_errors as f64 / total_runs as f64
    };

    let mut latencies = Vec::new();
    for client in clients {
        if let Some(v4) = &client.v4 {
            latencies.extend([v4.offer_latency_ms, v4.ack_latency_ms, v4.renew_latency_ms]);
        }
        if let Some(v6) = &client.v6 {
            latencies.extend([
                v6.advertise_latency_ms,
                v6.reply_latency_ms,
                v6.renew_latency_ms,
            ]);
        }
    }
    let mut latencies = latencies.into_iter().flatten().collect::<Vec<_>>();
    latencies.sort_unstable();

    RuntimeStats {
        duration_ms,
        throughput_per_sec,
        latency_p50_ms: percentile(&latencies, 0.50),
        latency_p95_ms: percentile(&latencies, 0.95),
        latency_p99_ms: percentile(&latencies, 0.99),
        error_rate,
    }
}

fn percentile(values: &[u128], p: f64) -> Option<u128> {
    if values.is_empty() {
        return None;
    }
    let index = ((values.len() - 1) as f64 * p).round() as usize;
    values.get(index).copied()
}
