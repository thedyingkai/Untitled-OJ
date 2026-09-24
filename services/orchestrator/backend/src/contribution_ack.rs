//! Authenticated observations from authoritative Contribution consumers.
//!
//! Consumers never choose the expected revision. They echo the obligations
//! embedded in the exact snapshot they applied; this module re-compiles that
//! snapshot and advances only the receipt bound to the authenticated target.

use crate::auth::{Principal, PrincipalSource};
use crate::contribution_snapshot::{
    ContributionProjectionAcknowledgementV1, ContributionProjectionExpectedStateV1,
    active_contribution_snapshot,
};
use crate::durable::DurableStore;
use crate::http::{ApiRequest, ApiResponse};
use orchestrator_core::{
    ContributionActivationStateV1, ProjectionReceiptStateV1, ProjectionTargetV1,
};
use orchestrator_storage::ContributionRepository;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const CONTRIBUTION_ACK_SCHEMA_VERSION: &str = "ojos.dev/contribution-projection-ack/v1";
pub(crate) const CONTRIBUTION_ACK_TOKEN_HEADER: &str = "x-ojos-contribution-ack-token";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ContributionProjectionAckRequestV1 {
    schema_version: String,
    target: ProjectionTargetV1,
    scope_id: String,
    snapshot_digest: String,
    acknowledgements: Vec<ContributionProjectionAcknowledgementV1>,
}

pub(crate) fn response(
    storage: Option<&DurableStore>,
    request: &ApiRequest,
    principal: &Principal,
    request_id: &str,
) -> ApiResponse {
    if principal.source() != PrincipalSource::InternalToken {
        return problem(
            403,
            "CONTRIBUTION_ACK_INTERNAL_IDENTITY_REQUIRED",
            "Contribution projection observations require the verified internal control-plane identity",
            request_id,
        );
    }
    let Some(storage) = storage else {
        return problem(
            503,
            "CONTRIBUTION_STORAGE_UNAVAILABLE",
            "durable storage is required to record Contribution projection observations",
            request_id,
        );
    };
    let ack: ContributionProjectionAckRequestV1 = match serde_json::from_str(&request.body) {
        Ok(ack) => ack,
        Err(error) => {
            return problem(
                400,
                "INVALID_CONTRIBUTION_ACK",
                format!("decode Contribution projection acknowledgement: {error}"),
                request_id,
            );
        }
    };
    if ack.schema_version != CONTRIBUTION_ACK_SCHEMA_VERSION {
        return problem(
            400,
            "INVALID_CONTRIBUTION_ACK",
            "unsupported Contribution projection acknowledgement schema",
            request_id,
        );
    }
    if ack.scope_id != "default" {
        return problem(
            400,
            "INVALID_CONTRIBUTION_ACK",
            "only the default Contribution scope is currently published",
            request_id,
        );
    }
    if !matches!(
        ack.target,
        ProjectionTargetV1::Gateway | ProjectionTargetV1::Auth
    ) {
        return problem(
            403,
            "CONTRIBUTION_ACK_TARGET_FORBIDDEN",
            "only the authoritative Gateway and Auth consumers may acknowledge this endpoint",
            request_id,
        );
    }
    let expected_verifier = match configured_target_verifier(ack.target) {
        Some(verifier) => verifier,
        None => {
            return problem(
                503,
                "CONTRIBUTION_ACK_CREDENTIAL_UNAVAILABLE",
                format!(
                    "the {} Contribution acknowledgement credential is not configured",
                    ack.target.as_str()
                ),
                request_id,
            );
        }
    };
    let presented_token = request
        .headers
        .get(CONTRIBUTION_ACK_TOKEN_HEADER)
        .map(String::as_str)
        .map(str::trim)
        .unwrap_or_default();
    let presented_verifier = token_verifier(presented_token);
    if !constant_time_eq(presented_verifier.as_bytes(), expected_verifier.as_bytes()) {
        return problem(
            401,
            "CONTRIBUTION_ACK_UNAUTHORIZED",
            "the target-bound Contribution acknowledgement credential is invalid",
            request_id,
        );
    }

    let snapshot = match active_contribution_snapshot(storage, &ack.scope_id) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return problem(
                503,
                "CONTRIBUTION_SNAPSHOT_UNAVAILABLE",
                error.to_string(),
                request_id,
            );
        }
    };
    let current_digest = snapshot
        .get("digest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if ack.snapshot_digest != current_digest {
        return problem(
            409,
            "STALE_CONTRIBUTION_SNAPSHOT",
            "the observed snapshot is no longer the current Contribution projection",
            request_id,
        );
    }
    let expected_acknowledgements = match snapshot
        .get("acknowledgements")
        .cloned()
        .map(serde_json::from_value::<Vec<ContributionProjectionAcknowledgementV1>>)
        .transpose()
    {
        Ok(Some(value)) => value,
        Ok(None) => Vec::new(),
        Err(error) => {
            return problem(
                503,
                "CONTRIBUTION_SNAPSHOT_INVALID",
                format!("decode server-generated acknowledgement obligations: {error}"),
                request_id,
            );
        }
    };
    if ack.acknowledgements != expected_acknowledgements {
        return problem(
            409,
            "CONTRIBUTION_ACK_OBLIGATION_MISMATCH",
            "the acknowledgement obligations do not exactly match the observed snapshot",
            request_id,
        );
    }
    let mut identities = BTreeSet::new();
    if ack
        .acknowledgements
        .iter()
        .any(|item| !identities.insert(item.activation_id.as_str()))
    {
        return problem(
            400,
            "INVALID_CONTRIBUTION_ACK",
            "duplicate Contribution activation acknowledgement",
            request_id,
        );
    }

    for obligation in &ack.acknowledgements {
        if let Err(error) = acknowledge_one(storage, ack.target, &ack.snapshot_digest, obligation) {
            let (status, code) = if error.starts_with("stale:") {
                (409, "STALE_CONTRIBUTION_ACK")
            } else {
                (503, "CONTRIBUTION_ACK_STORAGE_FAILED")
            };
            return problem(status, code, error, request_id);
        }
    }

    crate::api_v1::envelope(
        200,
        serde_json::json!({
            "schema_version": CONTRIBUTION_ACK_SCHEMA_VERSION,
            "target": ack.target,
            "scope_id": ack.scope_id,
            "snapshot_digest": ack.snapshot_digest,
            "accepted": true,
        }),
        request_id.to_string(),
    )
}

fn acknowledge_one(
    storage: &DurableStore,
    target: ProjectionTargetV1,
    snapshot_digest: &str,
    obligation: &ContributionProjectionAcknowledgementV1,
) -> Result<(), String> {
    let activation = storage
        .contribution_activation(&obligation.activation_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "stale: activation no longer exists".to_string())?;
    let required_activation_state = match obligation.expected_state {
        ContributionProjectionExpectedStateV1::Active => ContributionActivationStateV1::Committing,
        ContributionProjectionExpectedStateV1::Restored => {
            ContributionActivationStateV1::Compensating
        }
    };
    if activation.state() != required_activation_state
        || activation.service_id() != obligation.service_id
        || activation.candidate_revision_id() != obligation.candidate_revision_id
    {
        return Err("stale: activation identity or state changed".to_string());
    }
    let candidate = storage
        .contribution_revision(&obligation.candidate_revision_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "stale: candidate revision no longer exists".to_string())?;
    if candidate.generation() != obligation.candidate_generation {
        return Err("stale: candidate generation changed".to_string());
    }
    let head = storage
        .contribution_head(activation.scope_id(), activation.service_id())
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "stale: observed Contribution head no longer exists".to_string())?;
    if head.active_revision_id() != obligation.observed_revision_id
        || head.generation() != obligation.observed_generation
    {
        return Err("stale: observed Contribution head changed".to_string());
    }

    let receipts = storage
        .contribution_projection_receipts(activation.activation_id())
        .map_err(|error| error.to_string())?;
    let current = receipts
        .into_iter()
        .find(|receipt| receipt.target() == target)
        .ok_or_else(|| "stale: target receipt does not exist".to_string())?;
    let desired_state = match obligation.expected_state {
        ContributionProjectionExpectedStateV1::Active => ProjectionReceiptStateV1::Active,
        ContributionProjectionExpectedStateV1::Restored => ProjectionReceiptStateV1::Restored,
    };
    if current.state() == desired_state {
        if current.observed_generation() != Some(obligation.observed_generation) {
            return Err("stale: receipt contains a different observed generation".to_string());
        }
        if current.active_digest() == Some(snapshot_digest) {
            return Ok(());
        }
    }
    let observed = current
        .record(
            desired_state,
            Some(obligation.observed_generation),
            current.staged_digest().map(str::to_string),
            Some(snapshot_digest.to_string()),
            None,
        )
        .map_err(|error| format!("stale: invalid receipt transition: {error}"))?;
    storage
        .compare_and_swap_contribution_projection_receipt(&current, &observed)
        .map_err(|error| format!("stale: receipt compare-and-swap failed: {error}"))?;
    Ok(())
}

fn configured_target_verifier(target: ProjectionTargetV1) -> Option<String> {
    let variable = match target {
        ProjectionTargetV1::Gateway => "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256",
        ProjectionTargetV1::Auth => "ORCHESTRATOR_CONTRIBUTION_AUTH_ACK_TOKEN_SHA256",
        _ => return None,
    };
    std::env::var(variable)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| {
            value.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
}

fn token_verifier(token: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(token.as_bytes()))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn problem(
    status: u16,
    code: &'static str,
    detail: impl Into<String>,
    request_id: &str,
) -> ApiResponse {
    ApiResponse::problem(status, code, detail, request_id, None)
        .with_header("X-Request-ID", request_id)
}
