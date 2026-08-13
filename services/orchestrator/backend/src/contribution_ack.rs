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
use orchestrator_legacy::{
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contribution_snapshot::CONTRIBUTION_SNAPSHOT_SCHEMA_VERSION;
    use crate::http::ApiRequest;
    use crate::test_env::TestEnv;
    use orchestrator_storage::{SqliteOptions, SqliteOrchestratorStore};
    use std::collections::BTreeMap;

    #[test]
    fn ack_schema_is_distinct_from_snapshot_schema() {
        assert_eq!(
            CONTRIBUTION_ACK_SCHEMA_VERSION,
            "ojos.dev/contribution-projection-ack/v1"
        );
        assert_ne!(
            CONTRIBUTION_ACK_SCHEMA_VERSION,
            CONTRIBUTION_SNAPSHOT_SCHEMA_VERSION
        );
    }

    #[test]
    fn token_verifier_is_canonical_and_constant_time_comparable() {
        let verifier = token_verifier("gateway-only-secret");
        assert_eq!(verifier.len(), 71);
        assert!(verifier.starts_with("sha256:"));
        assert!(constant_time_eq(verifier.as_bytes(), verifier.as_bytes()));
        assert!(!constant_time_eq(
            verifier.as_bytes(),
            token_verifier("auth-only-secret").as_bytes()
        ));
    }

    fn sqlite() -> (tempfile::TempDir, DurableStore) {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteOrchestratorStore::open_with_options(
            directory.path().join("ack.db"),
            SqliteOptions {
                acquire_instance_lock: false,
                ..SqliteOptions::default()
            },
        )
        .unwrap();
        (directory, DurableStore::Sqlite(store))
    }

    fn request(token: Option<&str>, digest: &str) -> ApiRequest {
        let mut headers = BTreeMap::new();
        if let Some(token) = token {
            headers.insert(CONTRIBUTION_ACK_TOKEN_HEADER.to_string(), token.to_string());
        }
        ApiRequest {
            method: "POST".to_string(),
            path: "/api/v1/contributions/projections:ack".to_string(),
            headers,
            body: serde_json::json!({
                "schema_version": CONTRIBUTION_ACK_SCHEMA_VERSION,
                "target": "GATEWAY",
                "scope_id": "default",
                "snapshot_digest": digest,
                "acknowledgements": [],
            })
            .to_string(),
        }
    }

    #[test]
    fn acknowledgement_credentials_fail_closed_and_reject_stale_snapshots() {
        let mut environment = TestEnv::lock();
        environment.remove("ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256");
        let (_directory, storage) = sqlite();
        let snapshot = active_contribution_snapshot(&storage, "default").unwrap();
        let digest = snapshot["digest"].as_str().unwrap();

        let forbidden = response(
            Some(&storage),
            &request(Some("gateway-secret"), digest),
            &Principal::desktop_admin(),
            "forbidden",
        );
        assert_eq!(forbidden.status, 403);

        let unavailable = response(
            Some(&storage),
            &request(Some("gateway-secret"), digest),
            &Principal::internal_admin(),
            "unavailable",
        );
        assert_eq!(unavailable.status, 503);

        environment.set(
            "ORCHESTRATOR_CONTRIBUTION_GATEWAY_ACK_TOKEN_SHA256",
            &token_verifier("gateway-secret"),
        );
        let unauthorized = response(
            Some(&storage),
            &request(Some("forged-secret"), digest),
            &Principal::internal_admin(),
            "unauthorized",
        );
        assert_eq!(unauthorized.status, 401);

        let stale = response(
            Some(&storage),
            &request(
                Some("gateway-secret"),
                &format!("sha256:{}", "f".repeat(64)),
            ),
            &Principal::internal_admin(),
            "stale",
        );
        assert_eq!(stale.status, 409);

        let accepted = response(
            Some(&storage),
            &request(Some("gateway-secret"), digest),
            &Principal::internal_admin(),
            "accepted",
        );
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.body["data"]["accepted"], true);

        let mut wire = Vec::new();
        crate::http::write_v1_response(&mut wire, accepted).unwrap();
        let wire = String::from_utf8(wire).unwrap();
        let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
        let root = body.as_object().expect("v1 acknowledgement envelope");
        assert_eq!(root.len(), 2);
        assert!(root.contains_key("data"));
        assert!(root.contains_key("meta"));
        assert!(root.get("status").is_none());
    }
}
