use crate::http::ApiResponse;

const LEGACY_SUNSET: &str = "Thu, 31 Dec 2026 23:59:59 GMT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LegacyApiMode {
    Deprecated02,
    Gone10,
}

impl LegacyApiMode {
    pub(crate) fn configured() -> anyhow::Result<Self> {
        match std::env::var("ORCHESTRATOR_LEGACY_API_MODE") {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "0.2" | "deprecated" if cfg!(any(feature = "legacy-0_2", test)) => {
                    Ok(Self::Deprecated02)
                }
                "0.2" | "deprecated" => {
                    anyhow::bail!("this 1.0 build does not contain the 0.2 compatibility routes")
                }
                "1.0" | "gone" => Ok(Self::Gone10),
                _ => {
                    anyhow::bail!("ORCHESTRATOR_LEGACY_API_MODE must be 0.2/deprecated or 1.0/gone")
                }
            },
            Err(std::env::VarError::NotPresent) if cfg!(feature = "legacy-0_2") => {
                Ok(Self::Deprecated02)
            }
            // The historical route tests remain golden fixtures for the 0.2
            // compatibility build. Production binaries do not compile with
            // `cfg(test)` and therefore default to the 1.0 Gone behavior.
            Err(std::env::VarError::NotPresent) if cfg!(test) => Ok(Self::Deprecated02),
            Err(std::env::VarError::NotPresent) => Ok(Self::Gone10),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn decorate(self, response: ApiResponse) -> ApiResponse {
        match self {
            Self::Deprecated02 => response
                .with_header("Deprecation", "true")
                .with_header("Sunset", LEGACY_SUNSET)
                .with_header("Link", "</api/v1>; rel=\"successor-version\""),
            Self::Gone10 => response,
        }
    }
}

pub(crate) fn is_legacy_api_path(path: &str) -> bool {
    if path == "/api/v1" || path.starts_with("/api/v1/") {
        return false;
    }
    if path == "/ui/layout" {
        return true;
    }
    let first = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    matches!(
        first,
        "health"
            | "services"
            | "deployments"
            | "nodes"
            | "releases"
            | "release-registry"
            | "templates"
            | "sets"
            | "endpoints"
            | "links"
            | "operations"
            | "topology"
            | "diagnostics"
            | "actions"
            | "internal"
            | "store"
            | "api"
    )
}

pub(crate) fn gone_response(path: &str, request_id: &str) -> ApiResponse {
    let successor = successor_path(path);
    ApiResponse::problem(
        410,
        "LEGACY_API_GONE",
        format!("the unversioned Orchestrator API was removed in 1.0; use {successor}"),
        request_id,
        None,
    )
    .with_header("Link", format!("<{successor}>; rel=\"successor-version\""))
}

fn successor_path(path: &str) -> &'static str {
    if path == "/api/node/services/install" {
        return "/api/v1/store/releases:install";
    }
    let first = path
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or_default();
    match first {
        "health" => "/api/v1/healthz/ready",
        "store" | "releases" | "release-registry" => "/api/v1/store/packages",
        "nodes" | "api" => "/api/v1/nodes",
        "deployments" | "services" => "/api/v1/deployments",
        "operations" | "actions" => "/api/v1/operations",
        "topology" | "endpoints" | "links" | "sets" | "templates" => "/api/v1/topologies",
        "diagnostics" => "/api/v1/diagnostics",
        "ui" => "/api/v1/ui/layout",
        _ => "/api/v1/capabilities",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_unversioned_control_plane_paths_are_legacy() {
        assert!(is_legacy_api_path("/store/index"));
        assert!(is_legacy_api_path("/api/node/services/install"));
        assert!(is_legacy_api_path("/operations/op-1/apply"));
        assert!(is_legacy_api_path("/ui/layout"));
        assert!(!is_legacy_api_path("/api/v1/operations/op-1:apply"));
        assert!(!is_legacy_api_path("/api/v1/ui/layout"));
        assert!(!is_legacy_api_path("/assets/app.js"));
        assert!(!is_legacy_api_path("/"));
    }

    #[test]
    fn compatibility_build_decorates_every_legacy_response() {
        let response = LegacyApiMode::Deprecated02.decorate(ApiResponse::ok(json!({})));
        assert_eq!(
            response.headers.get("Deprecation").map(String::as_str),
            Some("true")
        );
        assert!(response.headers.contains_key("Sunset"));
        assert!(response.headers["Link"].contains("successor-version"));
    }

    #[test]
    fn ga_returns_problem_410_with_a_specific_successor() {
        let response = gone_response("/api/node/services/install", "req-gone");
        assert_eq!(response.status, 410);
        assert_eq!(response.body["code"], "LEGACY_API_GONE");
        assert!(
            response.body["detail"]
                .as_str()
                .unwrap()
                .contains("/api/v1/store/releases:install")
        );
    }
}
