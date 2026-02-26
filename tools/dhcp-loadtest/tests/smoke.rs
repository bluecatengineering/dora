use clap::Parser;

use dhcp_loadtest::{Cli, LoadTestConfig, run_load_test};

#[tokio::test]
async fn dry_run_smoke() {
    let cli = Cli::try_parse_from([
        "dhcp-loadtest",
        "--iface",
        "lo",
        "--clients",
        "10",
        "--protocol",
        "both",
        "--dry-run",
    ])
    .expect("cli parse");

    let config = LoadTestConfig::try_from(cli).expect("config parse");
    let report = run_load_test(config).await.expect("dry run report");

    assert!(report.passed);
    assert!(report.dry_run);
    assert_eq!(report.clients.len(), 10);
}
