use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct LanguagesConfig {
    pub languages: HashMap<String, LanguageConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LanguageConfig {
    pub source_file: String,
    pub exe_file: String,
    pub compile: CompileConfig,
    pub run: RunConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompileConfig {
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunConfig {
    pub command: String,
    pub args: Vec<String>,
}

impl LanguagesConfig {
    pub async fn load(path: &str) -> Result<Self> {
        let content = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read languages config failed: {}", path))?;

        let cfg: LanguagesConfig =
            serde_yaml::from_str(&content).context("parse languages.yaml failed")?;

        Ok(cfg)
    }

    pub fn get(&self, language: &str) -> Option<&LanguageConfig> {
        self.languages.get(language)
    }
}

pub fn render_arg(arg: &str, source: &Path, exe: &Path, workdir: &Path) -> String {
    arg.replace("{source}", &source.to_string_lossy())
        .replace("{exe}", &exe.to_string_lossy())
        .replace("{workdir}", &workdir.to_string_lossy())
}
