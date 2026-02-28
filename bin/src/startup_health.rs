use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use dora_core::tokio::{self, task::JoinHandle};

const STARTUP_STABILIZATION_WINDOW: Duration = Duration::from_millis(300);

pub async fn verify_startup_subsystems(
    api_task: &mut JoinHandle<()>,
    v4_task: &mut JoinHandle<Result<(), anyhow::Error>>,
    v6_task: Option<&mut JoinHandle<Result<(), anyhow::Error>>>,
    mode: &str,
) -> Result<()> {
    verify_background_task_running("external API", api_task)
        .await
        .with_context(|| format!("{mode} startup check failed"))?;
    verify_server_task_running("dhcpv4 server", v4_task)
        .await
        .with_context(|| format!("{mode} startup check failed"))?;
    if let Some(v6_task) = v6_task {
        verify_server_task_running("dhcpv6 server", v6_task)
            .await
            .with_context(|| format!("{mode} startup check failed"))?;
    }
    Ok(())
}

pub async fn verify_server_task_running(
    name: &str,
    task: &mut JoinHandle<Result<(), anyhow::Error>>,
) -> Result<()> {
    match tokio::time::timeout(STARTUP_STABILIZATION_WINDOW, task).await {
        Err(_) => Ok(()),
        Ok(join_res) => match join_res {
            Ok(Ok(())) => Err(anyhow!("{name} exited during startup stabilization window")),
            Ok(Err(err)) => Err(anyhow!("{name} failed during startup: {err}")),
            Err(err) => Err(anyhow!("{name} panicked during startup: {err}")),
        },
    }
}

pub async fn verify_background_task_running(name: &str, task: &mut JoinHandle<()>) -> Result<()> {
    match tokio::time::timeout(STARTUP_STABILIZATION_WINDOW, task).await {
        Err(_) => Ok(()),
        Ok(join_res) => match join_res {
            Ok(()) => Err(anyhow!("{name} exited during startup stabilization window")),
            Err(err) => Err(anyhow!("{name} panicked during startup: {err}")),
        },
    }
}
