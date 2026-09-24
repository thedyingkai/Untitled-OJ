//! Deterministic validation contracts; no environment reads or I/O.

use crate::RuntimeError;

pub fn validate_safe_resource_name(resource_name: &str) -> Result<(), RuntimeError> {
    let bytes = resource_name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 63
        || !bytes[0].is_ascii_lowercase()
        || !bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        || resource_name.contains("--")
    {
        return Err(RuntimeError::InvalidRuntimeContext(
            "resource_name must be a safe lowercase DNS label".to_string(),
        ));
    }
    Ok(())
}

pub fn validate_sha256_text(name: &str, value: &str) -> Result<(), RuntimeError> {
    let valid = value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidRuntimeContext(format!(
            "{name} must be sha256:<64 lowercase hex>"
        )))
    }
}
