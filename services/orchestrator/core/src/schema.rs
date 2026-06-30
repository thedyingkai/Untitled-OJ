use crate::{OrchestratorError, Result};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedSchemas {
    pub actions: Vec<String>,
    pub form_actions: Vec<String>,
    pub forms: Vec<ActionFormSchema>,
    pub plan_states: Vec<String>,
    pub plan_required_fields: Vec<String>,
    pub result_object_types: Vec<String>,
    pub result_required_fields: Vec<String>,
    pub error_required_fields: Vec<String>,
    pub error_redactions: Vec<String>,
}

impl SharedSchemas {
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    pub fn form_count(&self) -> usize {
        self.form_actions.len()
    }

    pub fn form_for(&self, action: &str) -> Option<&ActionFormSchema> {
        self.forms.iter().find(|form| form.action == action)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionFormSchema {
    pub action: String,
    pub fields: Vec<FormFieldSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldSchema {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub values: Vec<String>,
}

pub fn load_shared_schemas(repo_root: &Path) -> Result<SharedSchemas> {
    let schemas_dir = repo_root.join("platform/schemas/orchestrator");
    let actions_value = read_schema_file(&schemas_dir, "actions.yaml")?;
    let forms_value = read_schema_file(&schemas_dir, "forms.yaml")?;
    let plans_value = read_schema_file(&schemas_dir, "plans.yaml")?;
    let results_value = read_schema_file(&schemas_dir, "results.yaml")?;
    let errors_value = read_schema_file(&schemas_dir, "errors.yaml")?;

    let actions = string_sequence(&actions_value, &["actions"])?;
    let forms = form_schemas(&forms_value)?;
    let form_actions = forms
        .iter()
        .map(|form| form.action.clone())
        .collect::<Vec<_>>();
    ensure_same_set("actions.yaml", &actions, "forms.yaml", &form_actions)?;

    Ok(SharedSchemas {
        actions,
        form_actions,
        forms,
        plan_states: string_sequence(&plans_value, &["plan", "states"])?,
        plan_required_fields: string_sequence(&plans_value, &["plan", "required_fields"])?,
        result_object_types: string_sequence(&results_value, &["result", "changed_object_types"])?,
        result_required_fields: string_sequence(&results_value, &["result", "required_fields"])?,
        error_required_fields: string_sequence(&errors_value, &["error", "required_fields"])?,
        error_redactions: string_sequence(
            &errors_value,
            &["error", "redaction", "forbidden_plaintext"],
        )?,
    })
}

pub fn ensure_shared_schemas_loaded(schemas: &SharedSchemas) -> Result<()> {
    if schemas.actions.is_empty() {
        return Err(OrchestratorError::Dependency(
            "Action Registry 为空".to_string(),
        ));
    }
    if schemas.actions.len() != schemas.form_actions.len() {
        return Err(OrchestratorError::Dependency(
            "Action Registry 与 Form Schema 数量不一致".to_string(),
        ));
    }
    ensure_form_fields_are_valid(schemas)?;
    crate::validate_action_catalog(schemas)?;
    Ok(())
}

fn read_schema_file(schemas_dir: &Path, name: &str) -> Result<Value> {
    let text = fs::read_to_string(schemas_dir.join(name))?;
    let value: Value = serde_yaml::from_str(&text)?;
    assert_schema_header(name, &value)?;
    Ok(value)
}

fn assert_schema_header(name: &str, value: &Value) -> Result<()> {
    let schema_version = value
        .get("schema_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| OrchestratorError::Dependency(format!("{name} 缺少 schema_version")))?;
    if schema_version != 1 {
        return Err(OrchestratorError::Dependency(format!(
            "{name} schema_version 必须为 1"
        )));
    }
    let product = value.get("product").and_then(Value::as_str).unwrap_or("");
    if product != "OJOS Orchestrator" {
        return Err(OrchestratorError::Dependency(format!(
            "{name} product 必须为 OJOS Orchestrator"
        )));
    }
    Ok(())
}

fn string_sequence(value: &Value, path: &[&str]) -> Result<Vec<String>> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment).ok_or_else(|| {
            OrchestratorError::Dependency(format!("schema 缺少 {}", path.join(".")))
        })?;
    }
    let sequence = current.as_sequence().ok_or_else(|| {
        OrchestratorError::Dependency(format!("schema {} 必须是列表", path.join(".")))
    })?;
    sequence
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                OrchestratorError::Dependency(format!("schema {} 只能包含字符串", path.join(".")))
            })
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
struct RawForms {
    forms: BTreeMap<String, RawActionForm>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawActionForm {
    #[serde(default)]
    fields: Vec<RawFormField>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawFormField {
    name: String,
    #[serde(rename = "type")]
    field_type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    values: Vec<String>,
}

fn form_schemas(value: &Value) -> Result<Vec<ActionFormSchema>> {
    let raw: RawForms = serde_yaml::from_value(value.clone())?;
    let mut forms = raw
        .forms
        .into_iter()
        .map(|(action, form)| {
            let fields = form
                .fields
                .into_iter()
                .map(|field| FormFieldSchema {
                    name: field.name,
                    field_type: field.field_type,
                    required: field.required,
                    values: field.values,
                })
                .collect::<Vec<_>>();
            ActionFormSchema { action, fields }
        })
        .collect::<Vec<_>>();
    forms.sort_by(|left, right| left.action.cmp(&right.action));
    Ok(forms)
}

fn ensure_form_fields_are_valid(schemas: &SharedSchemas) -> Result<()> {
    let allowed_target_types = crate::CORE_ACTION_TARGETS
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let allowed_field_types = [
        "action",
        "boolean",
        "endpoint",
        "enum",
        "json",
        "secret_ref",
        "service_id",
        "string",
        "text",
    ]
    .into_iter()
    .collect::<HashSet<_>>();

    for form in &schemas.forms {
        let mut field_names = HashSet::new();
        for field in &form.fields {
            if field.name.trim().is_empty() {
                return Err(OrchestratorError::Dependency(format!(
                    "{} 包含空字段名",
                    form.action
                )));
            }
            if !field_names.insert(field.name.as_str()) {
                return Err(OrchestratorError::Dependency(format!(
                    "{} 包含重复字段 {}",
                    form.action, field.name
                )));
            }
            if !allowed_field_types.contains(field.field_type.as_str()) {
                return Err(OrchestratorError::Dependency(format!(
                    "{} 字段 {} 使用未知类型 {}",
                    form.action, field.name, field.field_type
                )));
            }
            if field.field_type == "enum" && field.values.is_empty() {
                return Err(OrchestratorError::Dependency(format!(
                    "{} 字段 {} 缺少 enum values",
                    form.action, field.name
                )));
            }
            if field.name == "target_type" {
                for value in &field.values {
                    if !allowed_target_types.contains(value.as_str()) {
                        return Err(OrchestratorError::Dependency(format!(
                            "{} target_type 使用非核心对象 {}",
                            form.action, value
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn ensure_same_set(
    left_name: &str,
    left: &[String],
    right_name: &str,
    right: &[String],
) -> Result<()> {
    let left_set = left.iter().collect::<HashSet<_>>();
    let right_set = right.iter().collect::<HashSet<_>>();
    if left_set != right_set {
        return Err(OrchestratorError::Dependency(format!(
            "{left_name} 与 {right_name} 覆盖的 action 不一致"
        )));
    }
    Ok(())
}
