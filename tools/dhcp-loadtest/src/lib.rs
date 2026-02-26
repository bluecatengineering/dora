pub mod config;
pub mod engine;
pub mod identity;
pub mod protocols;
pub mod report;
pub mod transport;
pub mod validation;

pub use config::{Cli, LoadTestConfig, ProtocolSelection};
pub use report::LoadTestReport;

pub async fn run_load_test(config: LoadTestConfig) -> anyhow::Result<LoadTestReport> {
    engine::run(config).await
}
