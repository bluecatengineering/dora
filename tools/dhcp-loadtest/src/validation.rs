use std::collections::{BTreeSet, HashMap};

use crate::config::LoadTestConfig;
use crate::report::{ClientResult, ValidationCheck, ValidationSummary};

pub fn run_validations(clients: &[ClientResult], config: &LoadTestConfig) -> ValidationSummary {
    let mut checks = Vec::new();

    if config.protocol.includes_v4() {
        let total = clients.iter().filter(|c| c.v4.is_some()).count();
        let success = clients
            .iter()
            .filter_map(|c| c.v4.as_ref())
            .filter(|r| r.success)
            .count();
        checks.push(ValidationCheck {
            name: "v4_allocation_correctness".to_string(),
            passed: success == total,
            details: format!("{success}/{total} clients completed DHCPv4 allocation"),
        });

        let duplicates = duplicate_v4_leases(clients);
        checks.push(ValidationCheck {
            name: "v4_no_duplicate_active_leases".to_string(),
            passed: duplicates.is_empty(),
            details: if duplicates.is_empty() {
                "No duplicate IPv4 leases detected".to_string()
            } else {
                format!("Duplicate IPv4 leases: {duplicates}")
            },
        });
    }

    if config.protocol.includes_v6() {
        let total = clients.iter().filter(|c| c.v6.is_some()).count();
        let success = clients
            .iter()
            .filter_map(|c| c.v6.as_ref())
            .filter(|r| r.success)
            .count();
        checks.push(ValidationCheck {
            name: "v6_allocation_correctness".to_string(),
            passed: success == total,
            details: format!("{success}/{total} clients completed DHCPv6 allocation"),
        });

        let duplicates = duplicate_v6_leases(clients);
        checks.push(ValidationCheck {
            name: "v6_no_duplicate_active_leases".to_string(),
            passed: duplicates.is_empty(),
            details: if duplicates.is_empty() {
                "No duplicate IPv6 leases detected".to_string()
            } else {
                format!("Duplicate IPv6 leases: {duplicates}")
            },
        });

        let iaid_conflicts = iaid_isolation_conflicts(clients);
        checks.push(ValidationCheck {
            name: "v6_iaid_isolation".to_string(),
            passed: iaid_conflicts.is_empty(),
            details: if iaid_conflicts.is_empty() {
                "No DUID/IAID lease overlap conflicts detected".to_string()
            } else {
                format!("DUID IAID overlap conflicts: {iaid_conflicts}")
            },
        });
    }

    if config.renew {
        let renew_mismatch = renewal_mismatches(clients);
        checks.push(ValidationCheck {
            name: "renewal_consistency".to_string(),
            passed: config.allow_renew_reassign || renew_mismatch.is_empty(),
            details: if renew_mismatch.is_empty() {
                "All renewals kept the same lease".to_string()
            } else if config.allow_renew_reassign {
                format!("Renew lease changed but allowed by policy: {renew_mismatch}")
            } else {
                format!("Renewal mismatches: {renew_mismatch}")
            },
        });
    }

    let total_runs = clients
        .iter()
        .map(|client| usize::from(client.v4.is_some()) + usize::from(client.v6.is_some()))
        .sum::<usize>();
    let total_errors = clients
        .iter()
        .map(|client| {
            client.v4.as_ref().map_or(0, |v4| v4.errors.len())
                + client.v6.as_ref().map_or(0, |v6| v6.errors.len())
        })
        .sum::<usize>();

    let error_rate = if total_runs == 0 {
        0.0
    } else {
        total_errors as f64 / total_runs as f64
    };
    checks.push(ValidationCheck {
        name: "timeout_error_rate".to_string(),
        passed: error_rate <= config.max_error_rate,
        details: format!(
            "error rate {:.4} (threshold {:.4})",
            error_rate, config.max_error_rate
        ),
    });

    let passed = checks.iter().all(|check| check.passed);
    ValidationSummary { passed, checks }
}

fn duplicate_v4_leases(clients: &[ClientResult]) -> String {
    let mut by_ip: HashMap<&str, Vec<usize>> = HashMap::new();
    for client in clients {
        if let Some(v4) = &client.v4
            && v4.success
            && let Some(ip) = v4.leased_ip.as_deref()
        {
            by_ip.entry(ip).or_default().push(client.client_index);
        }
    }

    format_duplicates(by_ip)
}

fn duplicate_v6_leases(clients: &[ClientResult]) -> String {
    let mut by_ip: HashMap<&str, Vec<usize>> = HashMap::new();
    for client in clients {
        if let Some(v6) = &client.v6
            && v6.success
            && let Some(ip) = v6.leased_ip.as_deref()
        {
            by_ip.entry(ip).or_default().push(client.client_index);
        }
    }

    format_duplicates(by_ip)
}

fn format_duplicates(by_ip: HashMap<&str, Vec<usize>>) -> String {
    let mut rows = Vec::new();
    for (ip, mut client_ids) in by_ip {
        if client_ids.len() > 1 {
            client_ids.sort_unstable();
            rows.push(format!("{ip}=>{client_ids:?}"));
        }
    }
    rows.sort();
    rows.join(", ")
}

fn renewal_mismatches(clients: &[ClientResult]) -> String {
    let mut rows = Vec::new();

    for client in clients {
        if let Some(v4) = &client.v4
            && let (Some(initial), Some(renew)) = (v4.leased_ip.as_ref(), v4.renew_ip.as_ref())
            && initial != renew
        {
            rows.push(format!(
                "client={} v4 {} -> {}",
                client.client_index, initial, renew
            ));
        }

        if let Some(v6) = &client.v6
            && let (Some(initial), Some(renew)) = (v6.leased_ip.as_ref(), v6.renew_ip.as_ref())
            && initial != renew
        {
            rows.push(format!(
                "client={} v6 {} -> {}",
                client.client_index, initial, renew
            ));
        }
    }

    rows.sort();
    rows.join(", ")
}

fn iaid_isolation_conflicts(clients: &[ClientResult]) -> String {
    let mut by_duid_ip: HashMap<(&str, &str), BTreeSet<u32>> = HashMap::new();
    for client in clients {
        if let Some(v6) = &client.v6
            && v6.success
            && let Some(ip) = v6.leased_ip.as_deref()
        {
            by_duid_ip
                .entry((client.duid.as_str(), ip))
                .or_default()
                .insert(client.iaid);
        }
    }

    let mut conflicts = Vec::new();
    for ((duid, ip), iaids) in by_duid_ip {
        if iaids.len() > 1 {
            conflicts.push(format!("duid={duid} ip={ip} iaids={iaids:?}"));
        }
    }
    conflicts.sort();
    conflicts.join(", ")
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

    use crate::config::{LoadTestConfig, ProtocolSelection};
    use crate::report::{ClientResult, V4ClientResult, V6ClientResult};

    use super::run_validations;

    fn test_config() -> LoadTestConfig {
        LoadTestConfig {
            iface: "lo".to_string(),
            iface_index: 1,
            clients: 2,
            protocol: ProtocolSelection::Both,
            server_v4: Some("255.255.255.255:67".parse().unwrap()),
            server_v6: Some(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 1, 2),
                547,
                0,
                1,
            ))),
            concurrency: 2,
            ramp_per_sec: 2,
            timeout_ms: 100,
            retries: 0,
            renew: true,
            release: false,
            json: false,
            dry_run: false,
            seed: 1,
            max_error_rate: 1.0,
            allow_renew_reassign: false,
        }
    }

    #[test]
    fn detects_duplicate_v4_leases() {
        let cfg = test_config();

        let clients = vec![
            ClientResult {
                client_index: 0,
                mac: "02:00:00:00:00:01".to_string(),
                duid: "00030001020000000001".to_string(),
                iaid: 1,
                v4: Some(V4ClientResult {
                    success: true,
                    leased_ip: Some("192.168.2.50".to_string()),
                    ..V4ClientResult::default()
                }),
                v6: Some(V6ClientResult {
                    success: true,
                    leased_ip: Some("2001:db8:2::10".to_string()),
                    ..V6ClientResult::default()
                }),
            },
            ClientResult {
                client_index: 1,
                mac: "02:00:00:00:00:02".to_string(),
                duid: "00030001020000000002".to_string(),
                iaid: 2,
                v4: Some(V4ClientResult {
                    success: true,
                    leased_ip: Some("192.168.2.50".to_string()),
                    ..V4ClientResult::default()
                }),
                v6: Some(V6ClientResult {
                    success: true,
                    leased_ip: Some("2001:db8:2::11".to_string()),
                    ..V6ClientResult::default()
                }),
            },
        ];

        let summary = run_validations(&clients, &cfg);
        let check = summary
            .checks
            .iter()
            .find(|check| check.name == "v4_no_duplicate_active_leases")
            .expect("duplicate lease check must exist");
        assert!(!check.passed);
    }
}
