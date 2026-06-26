mod cgroup;
mod checker;
mod config;
mod judge;
mod problem_package;
mod result;
mod sandbox;
mod worker_link;

use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::info;

use crate::config::LanguagesConfig;
use crate::worker_link::run_worker_link;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().json().init();

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

    run_worker_link(languages).await
}
