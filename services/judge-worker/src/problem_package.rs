use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
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
    pub validator: ComponentRef,
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

impl ComponentConfig {
    pub fn config_string(&self, key: &str) -> Option<String> {
        let serde_yaml::Value::Mapping(map) = &self.config else {
            return None;
        };
        map.get(&serde_yaml::Value::String(key.to_string()))
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    pub fn args(&self) -> Vec<String> {
        let serde_yaml::Value::Mapping(map) = &self.config else {
            return Vec::new();
        };
        let Some(serde_yaml::Value::Sequence(args)) =
            map.get(&serde_yaml::Value::String("args".to_string()))
        else {
            return Vec::new();
        };
        args.iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect()
    }

    pub fn source(&self) -> Option<String> {
        self.config_string("source")
    }

    pub fn language(&self) -> Option<String> {
        self.config_string("language").or_else(|| {
            self.source()
                .and_then(|source| infer_language_from_source(&source))
        })
    }

    pub fn is_builtin(&self, name: &str) -> bool {
        self.component_type == "builtin" && self.name == name
    }

    pub fn is_custom(&self) -> bool {
        self.component_type == "custom"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CasesFile {
    pub cases: Vec<CaseRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupsFile {
    #[serde(default)]
    pub groups: Vec<GroupRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupRecord {
    pub group_no: i32,
    pub score: i32,

    #[serde(default)]
    pub rule: String,
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
    pub cases: Vec<CaseRecord>,
    #[allow(dead_code)]
    pub groups: Vec<GroupRecord>,
    pub runner: ComponentConfig,
    pub checker: ComponentConfig,
    pub validator: ComponentConfig,
    pub scorer: ComponentConfig,
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

    pub fn component_source_path(&self, component: &ComponentConfig) -> Result<PathBuf> {
        let source = component
            .source()
            .ok_or_else(|| anyhow!("custom component {} requires config.source", component.name))?;
        safe_join(&self.package_dir, &source)
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

    let validator: ComponentConfig =
        read_yaml(&safe_join(&package_dir, &manifest.validator.config)?)
            .await
            .context("load validator config failed")?;

    let scorer: ComponentConfig = read_yaml(&safe_join(&package_dir, &manifest.scorer.config)?)
        .await
        .context("load scorer config failed")?;

    validate_component(
        &package_dir,
        &runner,
        "runner",
        &[
            "traditional-runner",
            "interactive-runner",
            "communication-runner",
            "output-only-runner",
            "heuristic-runner",
        ],
    )
    .await?;
    validate_component(
        &package_dir,
        &checker,
        "checker",
        &[
            "default-trim-checker",
            "interactive-checker",
            "communication-checker",
            "output-only-checker",
            "heuristic-checker",
        ],
    )
    .await?;
    validate_component(
        &package_dir,
        &validator,
        "validator",
        &["default-input-validator"],
    )
    .await?;
    validate_component(
        &package_dir,
        &scorer,
        "scorer",
        &["default-sum-scorer", "heuristic-scorer"],
    )
    .await?;

    let tests_root = safe_join(&package_dir, &manifest.tests.root)?;

    let (declared_groups, groups) = validate_groups(&package_dir, &manifest).await?;

    let cases_path = safe_join(&package_dir, &manifest.tests.cases)?;
    let cases_file: CasesFile = read_yaml(&cases_path)
        .await
        .with_context(|| format!("load cases.yaml failed: {}", cases_path.display()))?;

    if cases_file.cases.is_empty() {
        return Err(anyhow!("no test cases found"));
    }

    validate_cases(&tests_root, &cases_file.cases, &declared_groups).await?;

    Ok(LoadedProblemPackage {
        package_dir,
        manifest,
        cases: cases_file.cases,
        groups,
        runner,
        checker,
        validator,
        scorer,
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

    if manifest.id <= 0 {
        return Err(anyhow!("problem id must be positive"));
    }

    if manifest.slug.trim().is_empty() {
        return Err(anyhow!("problem slug is required"));
    }

    if manifest.title.trim().is_empty() {
        return Err(anyhow!("problem title is required"));
    }

    if !matches!(
        manifest.problem_type.as_str(),
        "traditional" | "interactive" | "communication" | "output_only" | "heuristic"
    ) {
        return Err(anyhow!(
            "unsupported problem type: {}",
            manifest.problem_type
        ));
    }

    if !matches!(manifest.visibility.as_str(), "public" | "private") {
        return Err(anyhow!(
            "unsupported problem visibility: {}",
            manifest.visibility
        ));
    }

    if !matches!(
        manifest.status.as_str(),
        "draft" | "ready" | "published" | "archived"
    ) {
        return Err(anyhow!("unsupported problem status: {}", manifest.status));
    }

    if manifest.limits.default.time_ms == 0 {
        return Err(anyhow!("default time limit must be positive"));
    }

    if manifest.limits.default.memory_mb == 0 {
        return Err(anyhow!("default memory limit must be positive"));
    }

    Ok(())
}

async fn validate_component(
    package_dir: &Path,
    component: &ComponentConfig,
    kind: &str,
    builtin_names: &[&str],
) -> Result<()> {
    if !matches!(component.component_type.as_str(), "builtin" | "custom") {
        return Err(anyhow!(
            "unsupported {} type: {}",
            kind,
            component.component_type
        ));
    }

    if component.name.trim().is_empty() {
        return Err(anyhow!("{} name is required", kind));
    }

    if matches!(component.config, serde_yaml::Value::Tagged(_)) {
        return Err(anyhow!(
            "{} config uses unsupported tagged yaml value",
            kind
        ));
    }

    if component.component_type == "builtin" {
        if !builtin_names.contains(&component.name.as_str()) {
            return Err(anyhow!("unsupported {} name: {}", kind, component.name));
        }
        return Ok(());
    }

    let source = component
        .source()
        .ok_or_else(|| anyhow!("custom {} requires config.source", kind))?;
    let source_path = safe_join(package_dir, &source)?;
    let metadata = fs::metadata(&source_path)
        .await
        .with_context(|| format!("custom {} source not found: {}", kind, source))?;
    if metadata.is_dir() {
        return Err(anyhow!("custom {} source is a directory: {}", kind, source));
    }

    if component.language().is_none() {
        return Err(anyhow!(
            "custom {} requires config.language or a known source extension",
            kind
        ));
    }

    Ok(())
}

async fn validate_groups(
    package_dir: &Path,
    manifest: &ProblemManifest,
) -> Result<(HashSet<i32>, Vec<GroupRecord>)> {
    let mut declared = HashSet::new();
    let groups_path = manifest.tests.groups.trim();
    if groups_path.is_empty() {
        return Ok((declared, Vec::new()));
    }

    let groups_path = safe_join(package_dir, groups_path)?;
    let groups_file: GroupsFile = read_yaml(&groups_path)
        .await
        .with_context(|| format!("load groups.yaml failed: {}", groups_path.display()))?;

    for group in &groups_file.groups {
        if group.group_no < 0 {
            return Err(anyhow!("invalid group_no: {}", group.group_no));
        }
        if group.score < 0 {
            return Err(anyhow!(
                "group score must be non-negative: {}",
                group.group_no
            ));
        }
        if !matches!(
            group.rule.as_str(),
            "" | "sum" | "min" | "max" | "any" | "all_or_nothing"
        ) {
            return Err(anyhow!(
                "unsupported group rule for group {}: {}",
                group.group_no,
                group.rule
            ));
        }
        if !declared.insert(group.group_no) {
            return Err(anyhow!("duplicate group_no: {}", group.group_no));
        }
    }

    Ok((declared, groups_file.groups))
}

async fn validate_cases(
    tests_root: &Path,
    cases: &[CaseRecord],
    declared_groups: &HashSet<i32>,
) -> Result<()> {
    let mut seen = HashSet::new();
    for case in cases {
        if case.case_no <= 0 {
            return Err(anyhow!("invalid case_no: {}", case.case_no));
        }
        if !seen.insert(case.case_no) {
            return Err(anyhow!("duplicate case_no: {}", case.case_no));
        }
        if case.score < 0 {
            return Err(anyhow!("case score must be non-negative: {}", case.case_no));
        }
        if case.group < 0 {
            return Err(anyhow!("case group must be non-negative: {}", case.case_no));
        }
        if !declared_groups.is_empty() && !declared_groups.contains(&case.group) {
            return Err(anyhow!(
                "case {} references undeclared group {}",
                case.case_no,
                case.group
            ));
        }
        if case.sample && case.hidden {
            return Err(anyhow!(
                "case {} cannot be both sample and hidden",
                case.case_no
            ));
        }

        let input = safe_join(tests_root, &case.input)?;
        let answer = safe_join(tests_root, &case.answer)?;

        if fs::metadata(&input).await.is_err() {
            return Err(anyhow!("input file not found: {}", input.display()));
        }

        if fs::metadata(&answer).await.is_err() {
            return Err(anyhow!("answer file not found: {}", answer.display()));
        }
    }

    Ok(())
}

fn infer_language_from_source(source: &str) -> Option<String> {
    match Path::new(source)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("c") => Some("c11".to_string()),
        Some("cpp") | Some("cc") | Some("cxx") => Some("cpp17".to_string()),
        Some("java") => Some("java17".to_string()),
        Some("py") => Some("python3".to_string()),
        _ => None,
    }
}

pub fn safe_join(base: &Path, child: &str) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;

    #[tokio::test]
    async fn load_problem_package_accepts_valid_package_and_applies_case_limits() {
        let package_dir = create_test_package("traditional");

        let package = load_problem_package(package_dir.to_str().unwrap())
            .await
            .expect("valid package should load");

        assert_eq!(package.manifest.slug, "a-plus-b");
        assert_eq!(package.cases.len(), 1);
        assert_eq!(package.validator.name, "default-input-validator");

        let limit = package.limit_for("python3", &package.cases[0]);
        assert_eq!(limit.time_ms, 1500);
        assert_eq!(limit.memory_mb, 96);

        let default_limit = package.limit_for("cpp17", &package.cases[0]);
        assert_eq!(default_limit.time_ms, 1500);
        assert_eq!(default_limit.memory_mb, 96);
    }

    #[tokio::test]
    async fn load_problem_package_accepts_all_declared_problem_types() {
        for problem_type in [
            "traditional",
            "interactive",
            "communication",
            "output_only",
            "heuristic",
        ] {
            let package_dir = create_test_package(problem_type);
            load_problem_package(package_dir.to_str().unwrap())
                .await
                .unwrap_or_else(|err| panic!("{problem_type} should load: {err:#}"));
        }
    }

    #[tokio::test]
    async fn load_problem_package_accepts_custom_components() {
        let package_dir = create_test_package("interactive");
        stdfs::create_dir_all(package_dir.join("runner")).unwrap();
        stdfs::create_dir_all(package_dir.join("checker")).unwrap();
        write_file(
            &package_dir.join("runner").join("runner.py"),
            "print('runner')\n",
        );
        write_file(
            &package_dir.join("checker").join("checker.cpp"),
            "int main(){return 0;}\n",
        );
        write_file(
            &package_dir.join("runner.yaml"),
            r#"
type: custom
name: two-process-runner
config:
  language: python3
  source: runner/runner.py
  args:
    - --pipe
"#,
        );
        write_file(
            &package_dir.join("checker.yaml"),
            r#"
type: custom
name: strict-checker
config:
  source: checker/checker.cpp
"#,
        );

        let package = load_problem_package(package_dir.to_str().unwrap())
            .await
            .expect("custom component package should load");

        assert!(package.runner.is_custom());
        assert_eq!(package.runner.language().as_deref(), Some("python3"));
        assert_eq!(package.checker.language().as_deref(), Some("cpp17"));
    }

    #[tokio::test]
    async fn load_problem_package_rejects_case_path_traversal() {
        let package_dir = create_test_package("traditional");
        write_file(
            &package_dir.join("tests").join("cases.yaml"),
            r#"
cases:
  - case_no: 1
    input: ../secret.txt
    answer: 001.ans
    score: 100
    group: 0
"#,
        );

        let err = load_problem_package(package_dir.to_str().unwrap())
            .await
            .expect_err("path traversal must be rejected");

        assert!(
            err.to_string().contains("parent path is not allowed"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn load_problem_package_rejects_missing_answer() {
        let package_dir = create_test_package("traditional");
        stdfs::remove_file(package_dir.join("tests").join("001.ans")).unwrap();

        let err = load_problem_package(package_dir.to_str().unwrap())
            .await
            .expect_err("missing answer must be rejected");

        assert!(
            err.to_string().contains("answer file not found"),
            "unexpected error: {err:#}"
        );
    }

    #[tokio::test]
    async fn load_problem_package_rejects_invalid_component_config() {
        let package_dir = create_test_package("traditional");
        write_file(
            &package_dir.join("checker.yaml"),
            r#"
type: builtin
name: custom-checker
config: {}
"#,
        );

        let err = load_problem_package(package_dir.to_str().unwrap())
            .await
            .expect_err("unsupported checker must be rejected");

        assert!(
            err.to_string().contains("unsupported checker name"),
            "unexpected error: {err:#}"
        );
    }

    fn create_test_package(problem_type: &str) -> PathBuf {
        let unique = uuid::Uuid::new_v4();
        let root = std::env::temp_dir().join(format!("ojos-worker-package-test-{unique}"));
        stdfs::create_dir_all(root.join("tests")).unwrap();

        let runner = match problem_type {
            "interactive" => "interactive-runner",
            "communication" => "communication-runner",
            "output_only" => "output-only-runner",
            "heuristic" => "heuristic-runner",
            _ => "traditional-runner",
        };
        let checker = match problem_type {
            "interactive" => "interactive-checker",
            "communication" => "communication-checker",
            "output_only" => "output-only-checker",
            "heuristic" => "heuristic-checker",
            _ => "default-trim-checker",
        };
        let scorer = if problem_type == "heuristic" {
            "heuristic-scorer"
        } else {
            "default-sum-scorer"
        };

        write_file(
            &root.join("problem.yaml"),
            &format!(
                r#"
schema: ojos.problem.v1
id: 1
slug: a-plus-b
title: A+B
type: {problem_type}
visibility: public
status: ready
limits:
  default:
    time_ms: 1000
    memory_mb: 64
  languages:
    python3:
      time_ms: 2000
      memory_mb: 128
runner:
  config: runner.yaml
checker:
  config: checker.yaml
validator:
  config: validator.yaml
scorer:
  config: scorer.yaml
tests:
  root: tests
  groups: tests/groups.yaml
  cases: tests/cases.yaml
"#
            ),
        );

        write_file(
            &root.join("runner.yaml"),
            &format!(
                r#"
type: builtin
name: {runner}
config: {{}}
"#
            ),
        );
        write_file(
            &root.join("checker.yaml"),
            &format!(
                r#"
type: builtin
name: {checker}
config:
  trim_trailing_spaces: true
  ignore_trailing_blank_lines: true
"#
            ),
        );
        write_file(
            &root.join("validator.yaml"),
            r#"
type: builtin
name: default-input-validator
config: {}
"#,
        );
        write_file(
            &root.join("scorer.yaml"),
            &format!(
                r#"
type: builtin
name: {scorer}
config: {{}}
"#
            ),
        );
        write_file(
            &root.join("tests").join("groups.yaml"),
            r#"
groups:
  - group_no: 0
    score: 100
    rule: sum
"#,
        );
        write_file(
            &root.join("tests").join("cases.yaml"),
            r#"
cases:
  - case_no: 1
    input: 001.in
    answer: 001.ans
    score: 100
    group: 0
    sample: true
    hidden: false
    time_limit_ms: 1500
    memory_limit_mb: 96
"#,
        );
        write_file(&root.join("tests").join("001.in"), "1 2\n");
        write_file(&root.join("tests").join("001.ans"), "3\n");

        root
    }

    fn write_file(path: &Path, content: &str) {
        stdfs::write(path, content.trim_start()).unwrap();
    }
}
