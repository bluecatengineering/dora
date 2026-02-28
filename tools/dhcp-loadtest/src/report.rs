use serde::{Deserialize, Serialize};

use crate::config::{LoadTestConfig, ProtocolSelection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Timeout,
    MalformedResponse,
    UnexpectedMessageType,
    LeaseConflict,
    RenewalMismatch,
    Operational,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub category: ErrorCategory,
    pub phase: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct V4ClientResult {
    pub success: bool,
    pub offered_ip: Option<String>,
    pub leased_ip: Option<String>,
    pub boot_file: Option<String>,
    pub next_server: Option<String>,
    pub renew_ip: Option<String>,
    pub released: bool,
    pub offer_latency_ms: Option<u128>,
    pub ack_latency_ms: Option<u128>,
    pub renew_latency_ms: Option<u128>,
    pub errors: Vec<ErrorRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct V6ClientResult {
    pub success: bool,
    pub advertised_ip: Option<String>,
    pub leased_ip: Option<String>,
    pub renew_ip: Option<String>,
    pub released: bool,
    pub advertise_latency_ms: Option<u128>,
    pub reply_latency_ms: Option<u128>,
    pub renew_latency_ms: Option<u128>,
    pub errors: Vec<ErrorRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientResult {
    pub client_index: usize,
    pub mac: String,
    pub duid: String,
    pub iaid: u32,
    pub v4: Option<V4ClientResult>,
    pub v6: Option<V6ClientResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCheck {
    pub name: String,
    pub passed: bool,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationSummary {
    pub passed: bool,
    pub checks: Vec<ValidationCheck>,
}

impl ValidationSummary {
    pub fn dry_run() -> Self {
        Self {
            passed: true,
            checks: vec![ValidationCheck {
                name: "dry_run".to_string(),
                passed: true,
                details: "No packets sent; config and identity generation only.".to_string(),
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfigSnapshot {
    pub iface: String,
    pub iface_index: u32,
    pub protocol: ProtocolSelection,
    pub clients: usize,
    pub concurrency: usize,
    pub ramp_per_sec: usize,
    pub timeout_ms: u64,
    pub retries: usize,
    pub renew: bool,
    pub release: bool,
    pub dry_run: bool,
    pub max_error_rate: f64,
}

impl From<&LoadTestConfig> for RunConfigSnapshot {
    fn from(config: &LoadTestConfig) -> Self {
        Self {
            iface: config.iface.clone(),
            iface_index: config.iface_index,
            protocol: config.protocol,
            clients: config.clients,
            concurrency: config.concurrency,
            ramp_per_sec: config.ramp_per_sec,
            timeout_ms: config.timeout_ms,
            retries: config.retries,
            renew: config.renew,
            release: config.release,
            dry_run: config.dry_run,
            max_error_rate: config.max_error_rate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Totals {
    pub planned_clients: usize,
    pub completed_clients: usize,
    pub v4_success: usize,
    pub v4_failures: usize,
    pub v6_success: usize,
    pub v6_failures: usize,
    pub total_errors: usize,
    pub timeout_errors: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStats {
    pub duration_ms: u128,
    pub throughput_per_sec: f64,
    pub latency_p50_ms: Option<u128>,
    pub latency_p95_ms: Option<u128>,
    pub latency_p99_ms: Option<u128>,
    pub error_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadTestReport {
    pub config: RunConfigSnapshot,
    pub dry_run: bool,
    pub passed: bool,
    pub totals: Totals,
    pub stats: RuntimeStats,
    pub validation: ValidationSummary,
    pub clients: Vec<ClientResult>,
}

impl LoadTestReport {
    pub fn human_summary(&self) -> String {
        let mut output = String::new();
        output.push_str("DHCP load test report\n");
        output.push_str(&format!(
            "status: {}\n",
            if self.passed { "PASS" } else { "FAIL" }
        ));
        output.push_str(&format!(
            "mode: protocol={:?}, iface={} (ifindex {})\n",
            self.config.protocol, self.config.iface, self.config.iface_index
        ));
        output.push_str(&format!(
            "clients: planned={}, completed={}\n",
            self.totals.planned_clients, self.totals.completed_clients
        ));
        output.push_str(&format!(
            "v4: success={}, failures={} | v6: success={}, failures={}\n",
            self.totals.v4_success,
            self.totals.v4_failures,
            self.totals.v6_success,
            self.totals.v6_failures
        ));
        output.push_str(&format!(
            "errors: total={}, timeout={} (rate {:.2}%)\n",
            self.totals.total_errors,
            self.totals.timeout_errors,
            self.stats.error_rate * 100.0
        ));
        output.push_str(&format!(
            "timing: duration={}ms throughput={:.2}/s p50={:?}ms p95={:?}ms p99={:?}ms\n",
            self.stats.duration_ms,
            self.stats.throughput_per_sec,
            self.stats.latency_p50_ms,
            self.stats.latency_p95_ms,
            self.stats.latency_p99_ms
        ));

        for check in &self.validation.checks {
            output.push_str(&format!(
                "check [{}] {}: {}\n",
                if check.passed { "PASS" } else { "FAIL" },
                check.name,
                check.details
            ));
        }
        output
    }
}
