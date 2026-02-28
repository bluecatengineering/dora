use clap::Parser;

use dhcp_loadtest::{Cli, LoadTestConfig, run_load_test};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let output_json = cli.json;

    let config = match LoadTestConfig::try_from(cli) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("configuration error: {err:#}");
            std::process::exit(2);
        }
    };

    match run_load_test(config).await {
        Ok(report) => {
            if output_json {
                match serde_json::to_string_pretty(&report) {
                    Ok(json) => println!("{json}"),
                    Err(err) => {
                        eprintln!("failed to serialize report: {err:#}");
                        std::process::exit(2);
                    }
                }
            } else {
                println!("{}", report.human_summary());
            }

            if report.passed {
                std::process::exit(0);
            }

            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("run failed: {err:#}");
            std::process::exit(1);
        }
    }
}
