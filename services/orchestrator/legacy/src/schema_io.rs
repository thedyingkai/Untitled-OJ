use crate::{
    ActionFormSchema, FormFieldSchema, OrchestratorError, Result, SharedSchemas,
    validate_action_catalog,
};
use serde::Deserialize;
use serde_yaml::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

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
    let schemas = SharedSchemas {
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
    };
    validate_action_catalog(&schemas)?;
    Ok(schemas)
}

fn read_schema_file(schemas_dir: &Path, name: &str) -> Result<Value> {
    let path = schemas_dir.join(name);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        OrchestratorError::Dependency(format!("cannot read schema {name}: {}", error.kind()))
    })?;
    let value: Value = serde_yaml::from_str(&text)?;
    assert_schema_header(name, &value)?;
    Ok(value)
}

fn assert_schema_header(name: &str, value: &Value) -> Result<()> {
    let schema_version = value
        .get("schema_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| OrchestratorError::Dependency(format!("{name} missing schema_version")))?;
    if schema_version != 1 {
        return Err(OrchestratorError::Dependency(format!(
            "{name} schema_version must be 1"
        )));
    }
    if value.get("product").and_then(Value::as_str).unwrap_or("") != "OJOS Orchestrator" {
        return Err(OrchestratorError::Dependency(format!(
            "{name} product must be OJOS Orchestrator"
        )));
    }
    Ok(())
}

fn string_sequence(value: &Value, path: &[&str]) -> Result<Vec<String>> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment).ok_or_else(|| {
            OrchestratorError::Dependency(format!("schema missing {}", path.join(".")))
        })?;
    }
    current
        .as_sequence()
        .ok_or_else(|| {
            OrchestratorError::Dependency(format!("schema {} must be a list", path.join(".")))
        })?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_string).ok_or_else(|| {
                OrchestratorError::Dependency(format!(
                    "schema {} must contain strings",
                    path.join(".")
                ))
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct RawForms {
    forms: BTreeMap<String, RawActionForm>,
}

#[derive(Debug, Deserialize)]
struct RawActionForm {
    #[serde(default)]
    fields: Vec<RawFormField>,
}

#[derive(Debug, Deserialize)]
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
        .map(|(action, form)| ActionFormSchema {
            action,
            fields: form
                .fields
                .into_iter()
                .map(|field| FormFieldSchema {
                    name: field.name,
                    field_type: field.field_type,
                    required: field.required,
                    values: field.values,
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    forms.sort_by(|left, right| left.action.cmp(&right.action));
    Ok(forms)
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
            "{left_name} and {right_name} must cover the same actions"
        )));
    }
    Ok(())
}
