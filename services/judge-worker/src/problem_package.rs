use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone, Deserialize)]
pub struct ProblemManifest {
    pub schema: String,
    pub id: i64,
    pub slug: String,
    pub title: String,

    #[serde(rename = "type")]
    pub problem_type: String,

    pub visibility: String,
    pub status: String,
    pub limits: ProblemLimits,
    pub runner: ComponentRef,
    pub checker: ComponentRef,
    pub scorer: ComponentRef,
    pub tests: TestsRef,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProblemLimits {
    pub default: LimitConfig,

    #[serde(default)]
    pub languages: HashMap<String, LimitConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitConfig {
    pub time_ms: u64,
    pub memory_mb: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComponentRef {
    pub config: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestsRef {
    pub root: String,
    pub groups: String,
    pub cases: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ComponentConfig {
    #[serde(rename = "type")]
    pub component_type: String,
    pub name: String,

    #[serde(default)]
    pub config: serde_yaml::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CasesFile {
    pub cases: Vec<CaseRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaseRecord {
    pub case_no: i32,
    pub input: String,
    pub answer: String,
    pub score: i32,

    #[serde(default)]
    pub group: i32,

    #[serde(default)]
    pub sample: bool,

    #[serde(default)]
    pub hidden: bool,

    #[serde(default)]
    pub time_limit_ms: Option<u64>,

    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct LoadedProblemPackage {
    pub package_dir: PathBuf,
    pub manifest: ProblemManifest,
    pub runner: ComponentConfig,
    pub checker: ComponentConfig,
    pub scorer: ComponentConfig,
    pub cases: Vec<CaseRecord>,
}

impl LoadedProblemPackage {
    pub fn limit_for(&self, language: &str, case: &CaseRecord) -> LimitConfig {
        let mut limit = self
            .manifest
            .limits
            .languages
            .get(language)
            .cloned()
            .unwrap_or_else(|| self.manifest.limits.default.clone());

        if let Some(time_ms) = case.time_limit_ms {
            limit.time_ms = time_ms;
        }

        if let Some(memory_mb) = case.memory_limit_mb {
            limit.memory_mb = memory_mb;
        }

        limit
    }

    pub fn input_path(&self, case: &CaseRecord) -> Result<PathBuf> {
        let tests_root = safe_join(&self.package_dir, &self.manifest.tests.root)?;
        safe_join(&tests_root, &case.input)
    }

    pub fn answer_path(&self, case: &CaseRecord) -> Result<PathBuf> {
        let tests_root = safe_join(&self.package_dir, &self.manifest.tests.root)?;
        safe_join(&tests_root, &case.answer)
    }
}

pub async fn load_problem_package(package_dir: &str) -> Result<LoadedProblemPackage> {
    if package_dir.trim().is_empty() {
        return Err(anyhow!("empty package dir"));
    }

    let package_dir = PathBuf::from(package_dir);
    let manifest_path = package_dir.join("problem.yaml");

    let manifest_text = fs::read_to_string(&manifest_path)
        .await
        .with_context(|| format!("read problem manifest failed: {}", manifest_path.display()))?;

    let manifest: ProblemManifest =
        serde_yaml::from_str(&manifest_text).context("parse problem.yaml failed")?;

    validate_manifest(&manifest)?;

    let runner: ComponentConfig = read_yaml(&safe_join(&package_dir, &manifest.runner.config)?)
        .await
        .context("load runner config failed")?;

    let checker: ComponentConfig = read_yaml(&safe_join(&package_dir, &manifest.checker.config)?)
        .await
        .context("load checker config failed")?;

    let scorer: ComponentConfig = read_yaml(&safe_join(&package_dir, &manifest.scorer.config)?)
        .await
        .context("load scorer config failed")?;

    validate_component(&runner, "runner", "traditional-runner")?;
    validate_component(&checker, "checker", "default-trim-checker")?;
    validate_component(&scorer, "scorer", "default-sum-scorer")?;

    let tests_root = safe_join(&package_dir, &manifest.tests.root)?;

    let cases_path = safe_join(&package_dir, &manifest.tests.cases)?;
    let cases_file: CasesFile = read_yaml(&cases_path)
        .await
        .with_context(|| format!("load cases.yaml failed: {}", cases_path.display()))?;

    if cases_file.cases.is_empty() {
        return Err(anyhow!("no test cases found"));
    }

    for case in &cases_file.cases {
        if case.case_no <= 0 {
            return Err(anyhow!("invalid case_no: {}", case.case_no));
        }

        let input = safe_join(&tests_root, &case.input)?;
        let answer = safe_join(&tests_root, &case.answer)?;

        if fs::metadata(&input).await.is_err() {
            return Err(anyhow!("input file not found: {}", input.display()));
        }

        if fs::metadata(&answer).await.is_err() {
            return Err(anyhow!("answer file not found: {}", answer.display()));
        }
    }

    Ok(LoadedProblemPackage {
        package_dir,
        manifest,
        runner,
        checker,
        scorer,
        cases: cases_file.cases,
    })
}

async fn read_yaml<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let text = fs::read_to_string(path)
        .await
        .with_context(|| format!("read yaml failed: {}", path.display()))?;

    let value = serde_yaml::from_str(&text)
        .with_context(|| format!("parse yaml failed: {}", path.display()))?;

    Ok(value)
}

fn validate_manifest(manifest: &ProblemManifest) -> Result<()> {
    if manifest.schema != "ojos.problem.v1" {
        return Err(anyhow!("unsupported problem schema: {}", manifest.schema));
    }

    if manifest.problem_type != "traditional" {
        return Err(anyhow!(
            "unsupported problem type: {}",
            manifest.problem_type
        ));
    }

    if manifest.limits.default.time_ms == 0 {
        return Err(anyhow!("default time limit must be positive"));
    }

    if manifest.limits.default.memory_mb == 0 {
        return Err(anyhow!("default memory limit must be positive"));
    }

    Ok(())
}

fn validate_component(component: &ComponentConfig, kind: &str, expected_name: &str) -> Result<()> {
    if component.component_type != "builtin" {
        return Err(anyhow!(
            "unsupported {} type: {}",
            kind,
            component.component_type
        ));
    }

    if component.name != expected_name {
        return Err(anyhow!(
            "unsupported {} name: {}, expected {}",
            kind,
            component.name,
            expected_name
        ));
    }

    Ok(())
}

fn safe_join(base: &Path, child: &str) -> Result<PathBuf> {
    if child.trim().is_empty() {
        return Err(anyhow!("empty relative path"));
    }

    let child_path = Path::new(child);

    if child_path.is_absolute() {
        return Err(anyhow!("absolute path is not allowed: {}", child));
    }

    for part in child_path.components() {
        if matches!(part, std::path::Component::ParentDir) {
            return Err(anyhow!("parent path is not allowed: {}", child));
        }
    }

    Ok(base.join(child_path))
}
