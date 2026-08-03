//! Store HTTP 适配器。索引、GitHub 与安装规则由 `orchestrator-manager` 统一实现。

use crate::{ApiRequest, ApiResponse, StatusError, query_value};
use anyhow::Result;
use orchestrator_legacy::OrchestratorActionConsole;
use orchestrator_manager::{
    StoreCatalog, StoreImportRequest, StoreInstallRequest, StoreRequestError,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::Path;

pub type StoreState = StoreCatalog;

pub fn installed_services(console: &OrchestratorActionConsole) -> Result<Value> {
    Ok(serde_json::to_value(
        orchestrator_manager::installed_services(console)?,
    )?)
}

/// 拉取（或读缓存）商店索引；不需要 console，调用方可在 console 锁外执行。
pub fn store_index_payload(
    state: &StoreState,
    repo_root: &Path,
    refresh: bool,
) -> Result<(String, bool, Value)> {
    state.load_index(repo_root, refresh)
}

pub fn github_releases_response(query: &str) -> Result<ApiResponse> {
    let repo = query_value(query, "repo")
        .map_err(|err| StatusError::new(400, err.to_string()))?
        .ok_or_else(|| {
            StoreRequestError::new(400, "query parameter repo=owner/name is required")
        })?;
    let per_page = query_value(query, "per_page")
        .map_err(|err| StatusError::new(400, err.to_string()))?
        .and_then(|value| value.parse::<u8>().ok())
        .unwrap_or(10);
    let releases = StoreCatalog::new().github_releases(&repo, per_page)?;
    Ok(ApiResponse::ok(serde_json::to_value(releases)?))
}

pub fn route_store_request(
    state: &StoreState,
    console: &mut OrchestratorActionConsole,
    repo_root: &Path,
    request: &ApiRequest,
    path: &str,
    _query: &str,
) -> Option<Result<ApiResponse>> {
    let segments = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["store", "status"]) => Some(
            serde_json::to_value(state.status(console))
                .map(ApiResponse::ok)
                .map_err(Into::into),
        ),
        ("POST", ["store", "import"]) => Some((|| {
            let input = body::<StoreImportRequest>(&request.body)?;
            let output = state.import_release(console, repo_root, input)?;
            Ok(ApiResponse::created(serde_json::to_value(output)?))
        })()),
        ("POST", ["store", "install"]) => Some((|| {
            let input = body::<StoreInstallRequest>(&request.body)?;
            let output = state.install(console, repo_root, input)?;
            Ok(ApiResponse::ok(serde_json::to_value(output)?))
        })()),
        _ => None,
    }
}

fn body<T: DeserializeOwned>(body: &str) -> Result<T> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(StoreRequestError::new(400, "request body must be a JSON object").into());
    }
    let value = serde_json::from_str::<Value>(trimmed)?;
    if !value.is_object() {
        return Err(StoreRequestError::new(400, "request body must be a JSON object").into());
    }
    serde_json::from_value(value).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .expect("workspace root")
            .to_path_buf()
    }

    #[test]
    fn repository_catalog_is_not_reported_as_installed() {
        let console = OrchestratorActionConsole::load_with_database_url(repo_root(), None).unwrap();
        assert!(!console.services().unwrap().is_empty());
        assert_eq!(installed_services(&console).unwrap(), json!({}));
    }

    #[test]
    fn github_release_route_reports_missing_repo_as_bad_request() {
        let error = github_releases_response("").expect_err("repo is required");
        let status = error.downcast_ref::<StoreRequestError>().unwrap();
        assert_eq!(status.status(), 400);
    }

    #[test]
    fn store_mutations_validate_their_targets() {
        let state = StoreState::new();
        let mut console =
            OrchestratorActionConsole::load_with_database_url(repo_root(), None).unwrap();
        for path in ["/store/import", "/store/install"] {
            let request = ApiRequest {
                method: "POST".to_string(),
                path: path.to_string(),
                headers: BTreeMap::new(),
                body: "{}".to_string(),
            };
            let error = route_store_request(&state, &mut console, &repo_root(), &request, path, "")
                .expect("known route")
                .expect_err("missing target");
            let status = error.downcast_ref::<StoreRequestError>().unwrap();
            // Package loading can be disabled before target validation; both are explicit client/policy errors.
            assert!(matches!(status.status(), 400 | 403));
        }
    }
}
