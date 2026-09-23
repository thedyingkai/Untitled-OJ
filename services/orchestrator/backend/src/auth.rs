//! 控制面门禁：编排器内部令牌与节点安装令牌。

use crate::http::{ApiRequest, StatusError};
use anyhow::Result;
use orchestrator_legacy::V1Role;
use std::fmt;

pub(crate) const ORCHESTRATOR_INTERNAL_TOKEN_HEADER: &str = "x-ojos-orchestrator-token";

/// Authenticated identity passed to v1 handlers. A `Principal` is constructed
/// only by a server-side verifier; request identity and role headers are never
/// considered identity evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    id: String,
    role: V1Role,
    source: PrincipalSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalSource {
    DesktopSession,
    InternalToken,
    Oidc,
    EphemeralDev,
}

impl Principal {
    pub fn verified(id: impl Into<String>, role: V1Role, source: PrincipalSource) -> Self {
        Self {
            id: id.into(),
            role,
            source,
        }
    }

    pub fn desktop_admin() -> Self {
        Self::verified(
            "local-admin",
            V1Role::Admin,
            PrincipalSource::DesktopSession,
        )
    }

    pub fn internal_admin() -> Self {
        Self::verified(
            "internal-admin",
            V1Role::Admin,
            PrincipalSource::InternalToken,
        )
    }

    pub fn ephemeral_dev() -> Self {
        Self::verified(
            "ephemeral-dev",
            V1Role::Admin,
            PrincipalSource::EphemeralDev,
        )
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn role(&self) -> V1Role {
        self.role
    }

    pub fn source(&self) -> PrincipalSource {
        self.source
    }

    pub fn allows(&self, required: V1Role) -> bool {
        self.role >= required
    }
}

/// Boundary for the remote OIDC browser and TUI flow verifier.
/// Implementations must validate signature, issuer, audience, expiry and the
/// flow-specific state/nonce before returning a principal. The v1 server does
/// not parse claims or accept role headers itself.
pub trait OidcPrincipalVerifier: Send + Sync {
    fn verify_bearer(
        &self,
        authorization_header: Option<&str>,
    ) -> std::result::Result<Option<Principal>, PrincipalVerificationError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalVerificationError {
    detail: String,
}

impl PrincipalVerificationError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for PrincipalVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for PrincipalVerificationError {}

/// Resolves identity from authenticated server context. `desktop_principal`
/// is supplied only after the HttpOnly session and CSRF checks succeed.
pub(crate) fn resolve_principal(
    request: &ApiRequest,
    desktop_principal: Option<&Principal>,
    expected_internal_token: Option<&str>,
    ephemeral_dev: bool,
    oidc_verifier: Option<&dyn OidcPrincipalVerifier>,
) -> std::result::Result<Option<Principal>, PrincipalVerificationError> {
    if let Some(principal) = desktop_principal {
        return Ok(Some(principal.clone()));
    }

    if let Some(expected) = expected_internal_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && request
            .headers
            .get(ORCHESTRATOR_INTERNAL_TOKEN_HEADER)
            .map(String::as_str)
            .map(str::trim)
            == Some(expected)
    {
        return Ok(Some(Principal::internal_admin()));
    }

    if let Some(verifier) = oidc_verifier
        && let Some(principal) =
            verifier.verify_bearer(request.headers.get("authorization").map(String::as_str))?
    {
        if principal.source() != PrincipalSource::Oidc {
            return Err(PrincipalVerificationError::new(
                "OIDC verifier returned a principal with an invalid source",
            ));
        }
        return Ok(Some(principal));
    }

    if ephemeral_dev {
        return Ok(Some(Principal::ephemeral_dev()));
    }

    Ok(None)
}

/// 配置了内部令牌后，除 `GET /health` 以外的所有 API 请求都必须携带令牌：只读 GET
/// 同样会泄露拓扑、端点与节点清单，因此不再豁免。静态资源与 SPA 入口不经过这里
/// （`dispatch_request` 先把它们交给静态层），浏览器仍能正常打开页面。
/// `GET /health` 保持开放，Prometheus 抓取与 compose healthcheck 不带令牌。
pub(crate) fn requires_internal_token(method: &str, segments: &[&str]) -> bool {
    !(method == "GET" && matches!(segments, ["health"]))
}

/// 强制控制面的编排器内部令牌。未配置令牌时 fail-open（开发与运维演练不带令牌跑
/// daemon）；一旦设置 `ORCHESTRATOR_INTERNAL_TOKEN` 就 fail-closed，生产由
/// secret-check 强制要求。网关已按 `x-ojos-orchestrator-token` 发送该令牌。
pub(crate) fn internal_token_check(
    method: &str,
    segments: &[&str],
    header_token: Option<&str>,
    expected: Option<&str>,
) -> Result<()> {
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    if !requires_internal_token(method, segments) {
        return Ok(());
    }
    match header_token.map(str::trim) {
        Some(actual) if actual == expected => Ok(()),
        _ => {
            Err(StatusError::new(401, "orchestrator control-plane request is unauthorized").into())
        }
    }
}

pub fn configured_internal_token() -> Option<String> {
    std::env::var("ORCHESTRATOR_INTERNAL_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(feature = "legacy-0_2")]
pub(crate) fn require_node_token(request: &ApiRequest) -> Result<()> {
    let token = std::env::var("ORCHESTRATOR_NODE_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(token) = token else {
        return Ok(());
    };
    let expected = format!("Bearer {token}");
    let actual = request
        .headers
        .get("authorization")
        .map(String::as_str)
        .unwrap_or("");
    if actual == expected {
        Ok(())
    } else {
        Err(StatusError::new(401, "node install request is unauthorized").into())
    }
}

/// 节点一旦允许真实运行驱动，安装入口就不能继续沿用开发环境的 fail-open 规则。
/// 此时节点令牌和控制面令牌必须同时配置，并且请求必须同时匹配两者。驱动总开关
/// 关闭时仍保留原来的 metadata-only 兼容行为。
#[cfg(feature = "legacy-0_2")]
pub(crate) fn require_node_install_credentials(
    request: &ApiRequest,
    expected_internal_token: Option<&str>,
) -> Result<()> {
    if !node_driver_execution_enabled() {
        return require_node_token(request);
    }

    let expected_internal_token = expected_internal_token
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StatusError::new(
                401,
                "node driver execution requires ORCHESTRATOR_INTERNAL_TOKEN",
            )
        })?;
    let actual_internal_token = request
        .headers
        .get(ORCHESTRATOR_INTERNAL_TOKEN_HEADER)
        .map(String::as_str)
        .map(str::trim);
    if actual_internal_token != Some(expected_internal_token) {
        return Err(StatusError::new(
            401,
            "node driver execution request has an invalid orchestrator control-plane token",
        )
        .into());
    }

    let node_token = std::env::var("ORCHESTRATOR_NODE_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StatusError::new(
                401,
                "node driver execution requires ORCHESTRATOR_NODE_TOKEN",
            )
        })?;
    let expected_node_token = format!("Bearer {node_token}");
    let actual_node_token = request
        .headers
        .get("authorization")
        .map(String::as_str)
        .unwrap_or("");
    if actual_node_token != expected_node_token {
        return Err(StatusError::new(
            401,
            "node driver execution request has an invalid node token",
        )
        .into());
    }
    Ok(())
}

#[cfg(feature = "legacy-0_2")]
fn node_driver_execution_enabled() -> bool {
    std::env::var("ORCHESTRATOR_NODE_EXECUTE_SERVICE_DRIVER")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}
