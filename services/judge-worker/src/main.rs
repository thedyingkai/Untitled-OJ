mod cgroup;
mod checker;
mod config;
mod health;
mod judge;
mod problem_package;
mod result;
mod sandbox;
mod service_context;
mod telemetry;
mod worker_link;

use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::info;

use crate::config::LanguagesConfig;
use crate::worker_link::run_worker_link;

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return health::check_ready().await;
    }

    let telemetry = telemetry::init_tracing().context("initialize worker tracing failed")?;

    let languages_path =
        std::env::var("LANGUAGES_CONFIG").unwrap_or_else(|_| "config/languages.yaml".to_string());

    let languages = Arc::new(
        LanguagesConfig::load(&languages_path)
            .await
            .context("load languages config failed")?,
    );

    info!(
        %languages_path,
        "judge-worker starting in worker-link mode"
    );

    let health_state = Arc::new(health::HealthState::default());
    let health_server = tokio::spawn(health::serve(health_state.clone()));
    let result = run_worker_link(languages, health_state).await;
    health_server.abort();
    telemetry.shutdown();
    result
}
