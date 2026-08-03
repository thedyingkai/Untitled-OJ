use crate::{OrchestratorError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

pub fn ensure_shared_schemas_loaded(schemas: &SharedSchemas) -> Result<()> {
    if schemas.actions.is_empty() {
        return Err(OrchestratorError::Dependency(
            "Action Registry must not be empty".to_string(),
        ));
    }
    if schemas.actions.len() != schemas.form_actions.len() {
        return Err(OrchestratorError::Dependency(
            "Action Registry and Form Schema counts differ".to_string(),
        ));
    }
    ensure_form_fields_are_valid(schemas)?;
    crate::validate_action_catalog(schemas)?;
    Ok(())
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
                    "{} contains an empty field name",
                    form.action
                )));
            }
            if !field_names.insert(field.name.as_str()) {
                return Err(OrchestratorError::Dependency(format!(
                    "{} contains duplicate field {}",
                    form.action, field.name
                )));
            }
            if !allowed_field_types.contains(field.field_type.as_str()) {
                return Err(OrchestratorError::Dependency(format!(
                    "{} field {} uses unknown type {}",
                    form.action, field.name, field.field_type
                )));
            }
            if field.field_type == "enum" && field.values.is_empty() {
                return Err(OrchestratorError::Dependency(format!(
                    "{} field {} has no enum values",
                    form.action, field.name
                )));
            }
            if field.name == "target_type" {
                for value in &field.values {
                    if !allowed_target_types.contains(value.as_str()) {
                        return Err(OrchestratorError::Dependency(format!(
                            "{} target_type uses non-core object {}",
                            form.action, value
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}
