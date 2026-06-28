use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use service_installer_core::{
    ServiceManifest, ServicePlan, ServicePlanAction, ServicePlanKind, ServiceState,
    validate_endpoint_id, validate_service_manifest, validate_service_manifest_file,
    validate_service_set_file,
};
use sqlx::{Column, PgPool, Row, postgres::PgPoolOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const RUNTIME_TOKEN_HEADER: &str = "x-ojos-runtime-token";
const DEFAULT_LOCK_TTL_SECONDS: u64 = 300;

#[derive(Clone)]
struct AppState {
    db: PgPool,
    repo_root: PathBuf,
    internal_token: String,
    lock_ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ErrorBody {
    error: ErrorInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ErrorInfo {
    code: String,
    message: String,
    severity: String,
    details: Value,
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    code: &'static str,
    msg: String,
    details: Value,
}

impl AppError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "RUNTIME_INTERNAL_UNAUTHORIZED",
            msg: "missing or invalid runtime internal token".to_string(),
            details: json!({}),
        }
    }

    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "FORBIDDEN",
            msg: msg.into(),
            details: json!({}),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "INTERNAL_ERROR",
            msg: msg.into(),
            details: json!({}),
        }
    }

    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "SERVICE_NOT_FOUND",
            msg: msg.into(),
            details: json!({}),
        }
    }

    fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "OPERATION_LOCK_HELD",
            msg: msg.into(),
            details: json!({}),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorInfo {
                    code: self.code.to_string(),
                    message: self.msg,
                    severity: "error".to_string(),
                    details: self.details,
                },
            }),
        )
            .into_response()
    }
}

impl From<service_installer_core::InstallerError> for AppError {
    fn from(value: service_installer_core::InstallerError) -> Self {
        let code = installer_error_code(&value);
        AppError {
            status: StatusCode::BAD_REQUEST,
            code,
            msg: value.to_string(),
            details: json!({}),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(_: sqlx::Error) -> Self {
        AppError::internal("database operation failed")
    }
}

type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ManifestReq {
    #[serde(default)]
    manifest_path: String,
    #[serde(default)]
    manifest: Option<ServiceManifest>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct SetReq {
    set_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct EndpointReq {
    endpoint: String,
    service_id: String,
    #[serde(default = "default_device_id")]
    device_id: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    health_path: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct LinkReq {
    source_endpoint: String,
    target_endpoint: String,
    protocol: String,
    #[serde(default = "default_auth_mode")]
    auth_mode: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    config_ref: String,
    #[serde(default)]
    secret_ref: String,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Envelope<T: Serialize> {
    code: i32,
    msg: String,
    data: T,
}

#[derive(Debug, Clone, Serialize)]
struct OperationItem {
    operation_id: String,
    object_type: String,
    object_id: String,
    action: String,
    status: String,
    actor_user_id: Option<i64>,
    actor_username: String,
    request: Value,
    plan: Value,
    result: Value,
    error_message: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct Actor {
    user_id: Option<i64>,
    username: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--healthcheck") {
        return healthcheck_command();
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .json()
        .init();

    let database_url = required_env("DATABASE_URL")?;
    let internal_token = required_env("ROOT_RUNTIME_MANAGER_INTERNAL_TOKEN")?;
    let repo_root = std::env::var("OJOS_REPO_ROOT").unwrap_or_else(|_| ".".to_string());
    let host = std::env::var("ROOT_RUNTIME_MANAGER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("ROOT_RUNTIME_MANAGER_PORT")
        .unwrap_or_else(|_| "8090".to_string())
        .parse()?;
    let lock_ttl_seconds = std::env::var("ROOT_RUNTIME_MANAGER_LOCK_TTL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (30..=3600).contains(value))
        .unwrap_or(DEFAULT_LOCK_TTL_SECONDS);

    let db = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await?;

    let state = Arc::new(AppState {
        db,
        repo_root: PathBuf::from(repo_root),
        internal_token,
        lock_ttl_seconds,
    });

    let app = router(state).layer(TraceLayer::new_for_http());
    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("root-runtime-manager listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

fn healthcheck_command() -> anyhow::Result<()> {
    let host = std::env::var("ROOT_RUNTIME_MANAGER_HOST")
        .ok()
        .filter(|value| value != "0.0.0.0")
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = std::env::var("ROOT_RUNTIME_MANAGER_PORT")
        .unwrap_or_else(|_| "8090".to_string())
        .parse()?;
    let addr = format!("{}:{}", host, port);
    let mut stream = std::net::TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200") {
        Ok(())
    } else {
        anyhow::bail!("root-runtime-manager healthcheck failed");
    }
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/services/discover", get(discover_services))
        .route("/internal/services/validate", post(validate_service))
        .route(
            "/internal/services/install-plan",
            post(service_install_plan_handler),
        )
        .route("/internal/services/install", post(install_service))
        .route("/internal/services/{id}/enable", post(enable_service))
        .route("/internal/services/{id}/disable", post(disable_service))
        .route("/internal/services/{id}/health", get(service_health))
        .route(
            "/internal/services/{id}/operations",
            get(service_operations),
        )
        .route("/internal/sets/expand", post(expand_set_handler))
        .route("/internal/endpoints/register", post(register_endpoint))
        .route("/internal/links/create", post(create_link))
        .route("/internal/topology", get(topology))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn discover_services(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let mut services = Vec::new();
    let services_dir = state.repo_root.join("services");
    if services_dir.exists() {
        for entry in std::fs::read_dir(services_dir)
            .map_err(|_| AppError::internal("service discovery failed"))?
        {
            let entry = entry.map_err(|_| AppError::internal("service discovery failed"))?;
            let manifest_path = PathBuf::from("services")
                .join(entry.file_name())
                .join("service.yaml");
            if state.repo_root.join(&manifest_path).exists() {
                match validate_service_manifest_file(&state.repo_root, &manifest_path) {
                    Ok(manifest) => services.push(json!({
                        "manifest_path": manifest_path.to_string_lossy().replace('\\', "/"),
                        "service_id": manifest.id,
                        "name": manifest.name,
                        "version": manifest.version,
                        "kind": manifest.kind,
                    })),
                    Err(err) => services.push(json!({
                        "manifest_path": manifest_path.to_string_lossy().replace('\\', "/"),
                        "valid": false,
                        "error": err.to_string(),
                    })),
                }
            }
        }
    }
    Ok(ok(json!({ "services": services })))
}

async fn validate_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let manifest = load_service_manifest(&state, &req)?;
    validate_service_manifest(&manifest)?;
    Ok(ok(json!({ "valid": true, "service": manifest })))
}

async fn service_install_plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<ServicePlan>>> {
    require_internal(&state, &headers)?;
    let manifest = load_service_manifest(&state, &req)?;
    validate_service_manifest(&manifest)?;
    let installed = load_service_states(&state.db).await?;
    Ok(ok(service_installer_core::service_install_plan(
        &manifest, &installed,
    )))
}

async fn install_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let actor = actor_from_headers(&headers);
    let manifest = load_service_manifest(&state, &req)?;
    validate_service_manifest(&manifest)?;
    let installed = load_service_states(&state.db).await?;
    let plan = service_installer_core::service_install_plan(&manifest, &installed);
    if req.dry_run {
        return Ok(ok(json!({ "plan": plan })));
    }
    if !plan.can_apply {
        return Err(AppError::forbidden("install plan is blocked"));
    }
    let request = serde_json::to_value(&req).unwrap_or_else(|_| json!({}));
    let service_id = manifest.id.clone();
    let manifest_for_apply = manifest.clone();
    let result = run_locked_operation(
        &state,
        &actor,
        "service",
        &service_id,
        "install",
        request,
        serde_json::to_value(&plan).unwrap_or_else(|_| json!({})),
        |tx| {
            Box::pin(async move {
                apply_install(tx, &manifest_for_apply).await?;
                Ok(json!({ "installed": true, "service_id": manifest_for_apply.id, "version": manifest_for_apply.version }))
            })
        },
    )
    .await?;
    Ok(ok(json!({ "plan": plan, "result": result })))
}

async fn enable_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    service_status_operation(state, headers, id, "enable", "ENABLED").await
}

async fn disable_service(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    service_status_operation(state, headers, id, "disable", "DISABLED").await
}

async fn service_status_operation(
    state: Arc<AppState>,
    headers: HeaderMap,
    id: String,
    action: &'static str,
    status: &'static str,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let actor = actor_from_headers(&headers);
    let exists = sqlx::query("SELECT 1 FROM service_nodes WHERE service_id = $1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .is_some();
    if !exists {
        return Err(AppError::not_found("service not found"));
    }
    let plan = ServicePlan {
        kind: if action == "enable" {
            ServicePlanKind::Enable
        } else {
            ServicePlanKind::Disable
        },
        service_id: id.clone(),
        version: String::new(),
        can_apply: true,
        actions: vec![ServicePlanAction {
            action: format!("{}_service", action),
            target: id.clone(),
            detail: format!("set service status to {}", status),
        }],
        blocked_by: vec![],
        warnings: vec![],
    };
    let plan_json = serde_json::to_value(&plan).unwrap_or_else(|_| json!({}));
    let service_id_for_apply = id.clone();
    let result = run_locked_operation(
        &state,
        &actor,
        "service",
        &id,
        action,
        json!({ "service_id": id }),
        plan_json,
        |tx| {
            Box::pin(async move {
                sqlx::query("UPDATE service_nodes SET status = $2, updated_at = NOW() WHERE service_id = $1")
                    .bind(&service_id_for_apply)
                    .bind(status)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query("UPDATE service_installations SET status = $2, updated_at = NOW(), enabled_at = CASE WHEN $2 = 'ENABLED' THEN COALESCE(enabled_at, NOW()) ELSE enabled_at END, disabled_at = CASE WHEN $2 = 'DISABLED' THEN COALESCE(disabled_at, NOW()) ELSE NULL END WHERE service_id = $1")
                    .bind(&service_id_for_apply)
                    .bind(status)
                    .execute(&mut **tx)
                    .await?;
                Ok(json!({ "service_id": service_id_for_apply, "status": status }))
            })
        },
    )
    .await?;
    Ok(ok(json!({ "plan": plan, "result": result })))
}

async fn service_health(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let status = sqlx::query("SELECT status FROM service_nodes WHERE service_id = $1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .map(|row| row.get::<String, _>("status"));
    match status {
        Some(status) => Ok(ok(
            json!({ "service_id": id, "status": "ok", "service_status": status }),
        )),
        None => Err(AppError::not_found("service not found")),
    }
}

async fn service_operations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let rows = sqlx::query(
        r#"
SELECT operation_id, object_type, object_id, action, status, actor_user_id, actor_username,
       request, plan, result, error_message, created_at::text, updated_at::text
FROM service_runtime_operations
WHERE object_type = 'service' AND object_id = $1
ORDER BY created_at DESC
LIMIT 50
"#,
    )
    .bind(&id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<OperationItem> = rows
        .into_iter()
        .map(|row| OperationItem {
            operation_id: row.get("operation_id"),
            object_type: row.get("object_type"),
            object_id: row.get("object_id"),
            action: row.get("action"),
            status: row.get("status"),
            actor_user_id: row.get("actor_user_id"),
            actor_username: row.get("actor_username"),
            request: row.get("request"),
            plan: row.get("plan"),
            result: row.get("result"),
            error_message: row.get("error_message"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect();
    Ok(ok(json!({ "operations": items })))
}

async fn expand_set_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<SetReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let set = validate_service_set_file(&state.repo_root, Path::new(req.set_path.trim()))?;
    Ok(ok(json!(service_installer_core::expand_set(&set))))
}

async fn register_endpoint(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<EndpointReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    validate_endpoint_id(&req.endpoint)?;
    let protocol = if req.protocol.trim().is_empty() {
        "http".to_string()
    } else {
        req.protocol.clone()
    };
    let plan = json!({
        "kind": "endpoint_register",
        "endpoint": req.endpoint,
        "service_id": req.service_id,
        "device_id": req.device_id,
        "can_apply": true
    });
    if req.dry_run {
        return Ok(ok(json!({ "plan": plan })));
    }
    sqlx::query(
        r#"
INSERT INTO service_endpoints(endpoint, service_id, device_id, protocol, health_path, display_name, note)
VALUES($1,$2,$3,$4,$5,$6,$7)
ON CONFLICT(endpoint) DO UPDATE SET
    service_id = EXCLUDED.service_id,
    device_id = EXCLUDED.device_id,
    protocol = EXCLUDED.protocol,
    health_path = EXCLUDED.health_path,
    display_name = EXCLUDED.display_name,
    note = EXCLUDED.note,
    updated_at = NOW()
"#,
    )
    .bind(&req.endpoint)
    .bind(&req.service_id)
    .bind(&req.device_id)
    .bind(&protocol)
    .bind(&req.health_path)
    .bind(&req.display_name)
    .bind(&req.note)
    .execute(&state.db)
    .await?;
    Ok(ok(json!({ "plan": plan, "endpoint": req.endpoint })))
}

async fn create_link(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LinkReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    validate_endpoint_id(&req.source_endpoint)?;
    validate_endpoint_id(&req.target_endpoint)?;
    let plan = json!({
        "kind": "link_create",
        "source_endpoint": req.source_endpoint,
        "target_endpoint": req.target_endpoint,
        "can_apply": true
    });
    if req.dry_run {
        return Ok(ok(json!({ "plan": plan })));
    }
    sqlx::query(
        r#"
INSERT INTO service_links(source_endpoint, target_endpoint, protocol, auth_mode, scope, config_ref, secret_ref)
VALUES($1,$2,$3,$4,$5,$6,$7)
ON CONFLICT(source_endpoint, target_endpoint) DO UPDATE SET
    protocol = EXCLUDED.protocol,
    auth_mode = EXCLUDED.auth_mode,
    scope = EXCLUDED.scope,
    config_ref = EXCLUDED.config_ref,
    secret_ref = EXCLUDED.secret_ref,
    updated_at = NOW()
"#,
    )
    .bind(&req.source_endpoint)
    .bind(&req.target_endpoint)
    .bind(&req.protocol)
    .bind(&req.auth_mode)
    .bind(&req.scope)
    .bind(&req.config_ref)
    .bind(&req.secret_ref)
    .execute(&state.db)
    .await?;
    Ok(ok(
        json!({ "plan": plan, "link": format!("{} -> {}", req.source_endpoint, req.target_endpoint) }),
    ))
}

async fn topology(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let devices = query_json_rows(
        &state.db,
        "SELECT device_id, name, kind, endpoint, health FROM devices ORDER BY device_id",
    )
    .await?;
    let services = query_json_rows(
        &state.db,
        "SELECT service_id, name, version, status, kind FROM service_nodes ORDER BY service_id",
    )
    .await?;
    let endpoints = query_json_rows(
        &state.db,
        "SELECT endpoint, service_id, device_id, protocol, health, reachable FROM service_endpoints ORDER BY endpoint",
    )
    .await?;
    let links = query_json_rows(
        &state.db,
        "SELECT source_endpoint, target_endpoint, protocol, auth_mode, scope, health, latency_ms FROM service_links ORDER BY source_endpoint, target_endpoint",
    )
    .await?;
    let sets = query_json_rows(
        &state.db,
        "SELECT set_id, name, description, non_root_only FROM service_sets ORDER BY set_id",
    )
    .await?;
    Ok(ok(json!({
        "devices": devices,
        "services": services,
        "endpoints": endpoints,
        "links": links,
        "sets": sets
    })))
}

fn load_service_manifest(state: &AppState, req: &ManifestReq) -> AppResult<ServiceManifest> {
    if let Some(manifest) = &req.manifest {
        validate_service_manifest(manifest)?;
        return Ok(manifest.clone());
    }
    let path = if req.manifest_path.trim().is_empty() {
        "services/gateway/service.yaml"
    } else {
        req.manifest_path.trim()
    };
    validate_service_manifest_file(&state.repo_root, Path::new(path)).map_err(AppError::from)
}

async fn load_service_states(db: &PgPool) -> AppResult<Vec<ServiceState>> {
    let rows = sqlx::query(
        r#"
SELECT service_id, version, status
FROM service_nodes
ORDER BY service_id
"#,
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ServiceState {
            service_id: row.get("service_id"),
            version: row.get("version"),
            enabled: row.get::<String, _>("status") == "ENABLED",
            endpoint: None,
        })
        .collect())
}

async fn apply_install(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    manifest: &ServiceManifest,
) -> Result<(), sqlx::Error> {
    let manifest_json = serde_json::to_value(manifest).unwrap_or_else(|_| json!({}));
    let set_id = default_set_for_service(&manifest.id);
    let install_status = if manifest.id == "root-runtime-manager" || manifest.runtime.root_allowed {
        "ENABLED"
    } else {
        "DISABLED"
    };
    sqlx::query(
        r#"
INSERT INTO service_sets(set_id, name, description, sort_order)
VALUES($1, $1, '', 100)
ON CONFLICT(set_id) DO NOTHING
"#,
    )
    .bind(&set_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
INSERT INTO service_nodes(service_id, set_id, name, version, status, kind, description, manifest)
VALUES($1,$2,$3,$4,$5,$6,$7,$8)
ON CONFLICT(service_id) DO UPDATE SET
    set_id = EXCLUDED.set_id,
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    kind = EXCLUDED.kind,
    description = EXCLUDED.description,
    manifest = EXCLUDED.manifest,
    updated_at = NOW()
"#,
    )
    .bind(&manifest.id)
    .bind(&set_id)
    .bind(&manifest.name)
    .bind(&manifest.version)
    .bind(install_status)
    .bind(&manifest.kind)
    .bind("")
    .bind(&manifest_json)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
INSERT INTO service_installations(service_id, name, version, status, manifest, enabled_at)
VALUES($1,$2,$3,$4,$5,CASE WHEN $4 = 'ENABLED' THEN NOW() ELSE NULL END)
ON CONFLICT(service_id) DO UPDATE SET
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    manifest = EXCLUDED.manifest,
    updated_at = NOW(),
    enabled_at = CASE WHEN EXCLUDED.status = 'ENABLED' THEN COALESCE(service_installations.enabled_at, NOW()) ELSE service_installations.enabled_at END,
    disabled_at = CASE WHEN EXCLUDED.status = 'DISABLED' THEN COALESCE(service_installations.disabled_at, NOW()) ELSE NULL END
"#,
    )
    .bind(&manifest.id)
    .bind(&manifest.name)
    .bind(&manifest.version)
    .bind(install_status)
    .bind(&manifest_json)
    .execute(&mut **tx)
    .await?;

    for link in &manifest.requires.links {
        sqlx::query(
            r#"
INSERT INTO service_edges(from_service_id, to_service_id, edge_type, version_constraint, required)
VALUES($1,$2,'requires','',true)
ON CONFLICT(from_service_id, to_service_id, edge_type) DO UPDATE SET required = EXCLUDED.required
"#,
        )
        .bind(&manifest.id)
        .bind(&link.id)
        .execute(&mut **tx)
        .await?;
    }

    for permission in &manifest.permissions {
        sqlx::query(
            r#"
INSERT INTO service_permissions(service_id, permission_key, description)
VALUES($1,$2,'')
ON CONFLICT(permission_key) DO UPDATE SET service_id = EXCLUDED.service_id
"#,
        )
        .bind(&manifest.id)
        .bind(permission)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
INSERT INTO permissions(code, service_code, name, description)
VALUES($1,$2,$1,'Service permission')
ON CONFLICT(code) DO UPDATE SET service_code = EXCLUDED.service_code
"#,
        )
        .bind(permission)
        .bind(&manifest.id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

type BoxFutureResult<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, sqlx::Error>> + Send + 'a>>;

async fn run_locked_operation<F>(
    state: &AppState,
    actor: &Actor,
    object_type: &str,
    object_id: &str,
    action: &str,
    request: Value,
    plan: Value,
    apply: F,
) -> AppResult<Value>
where
    F: for<'a> FnOnce(&'a mut sqlx::Transaction<'_, sqlx::Postgres>) -> BoxFutureResult<'a>,
{
    let operation_id = Uuid::new_v4().to_string();
    let owner = format!("root-runtime-manager:{}", operation_id);
    acquire_lock(
        &state.db,
        "root-runtime-manager-global",
        &owner,
        state.lock_ttl_seconds,
    )
    .await?;
    let mut tx = state.db.begin().await?;
    let request_json = redact_json(request.clone());
    sqlx::query(
        r#"
INSERT INTO service_runtime_operations(operation_id, object_type, object_id, action, status, actor_user_id, actor_username, request, plan, result)
VALUES($1,$2,$3,$4,'RUNNING',$5,$6,$7,$8,$9)
"#,
    )
    .bind(&operation_id)
    .bind(object_type)
    .bind(object_id)
    .bind(action)
    .bind(actor.user_id)
    .bind(&actor.username)
    .bind(&request_json)
    .bind(&plan)
    .bind(json!({}))
    .execute(&mut *tx)
    .await?;
    match apply(&mut tx).await {
        Ok(result) => {
            write_audit(
                &mut tx,
                actor,
                object_type,
                object_id,
                action,
                &operation_id,
            )
            .await?;
            sqlx::query(
                r#"
UPDATE service_runtime_operations
SET status = 'SUCCEEDED', result = $2, updated_at = NOW()
WHERE operation_id = $1
"#,
            )
            .bind(&operation_id)
            .bind(redact_json(result.clone()))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            release_lock(&state.db, "root-runtime-manager-global", &owner).await;
            Ok(json!({ "operation_id": operation_id, "result": result }))
        }
        Err(err) => {
            let _ = tx.rollback().await;
            let _ = sqlx::query(
                r#"
INSERT INTO service_runtime_operations(operation_id, object_type, object_id, action, status, actor_user_id, actor_username, request, plan, result, error_message)
VALUES($1,$2,$3,$4,'FAILED',$5,$6,$7,$8,$9,$10)
ON CONFLICT(operation_id) DO UPDATE SET
    status = 'FAILED',
    error_message = EXCLUDED.error_message,
    updated_at = NOW()
"#,
            )
            .bind(&operation_id)
            .bind(object_type)
            .bind(object_id)
            .bind(action)
            .bind(actor.user_id)
            .bind(&actor.username)
            .bind(&request_json)
            .bind(&plan)
            .bind(json!({}))
            .bind("operation failed")
            .execute(&state.db)
            .await;
            release_lock(&state.db, "root-runtime-manager-global", &owner).await;
            tracing::error!(error = %err, "service operation failed");
            Err(AppError::internal("operation failed"))
        }
    }
}

async fn acquire_lock(db: &PgPool, key: &str, owner: &str, ttl_seconds: u64) -> AppResult<()> {
    let row = sqlx::query(
        r#"
INSERT INTO service_operation_locks(lock_key, owner, expires_at)
VALUES($1,$2,NOW() + ($3::text || ' seconds')::interval)
ON CONFLICT(lock_key) DO UPDATE SET
    owner = EXCLUDED.owner,
    acquired_at = NOW(),
    expires_at = EXCLUDED.expires_at
WHERE service_operation_locks.expires_at < NOW()
RETURNING owner
"#,
    )
    .bind(key)
    .bind(owner)
    .bind(ttl_seconds as i64)
    .fetch_optional(db)
    .await?;
    if row.is_none() {
        return Err(AppError::conflict("service operation lock is held"));
    }
    Ok(())
}

async fn release_lock(db: &PgPool, key: &str, owner: &str) {
    let _ = sqlx::query("DELETE FROM service_operation_locks WHERE lock_key = $1 AND owner = $2")
        .bind(key)
        .bind(owner)
        .execute(db)
        .await;
}

async fn write_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &Actor,
    object_type: &str,
    object_id: &str,
    action: &str,
    operation_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
INSERT INTO permission_audit_logs(actor_type, actor_id, action, target_type, target_id, metadata)
VALUES('user', COALESCE($1, 0), $2, $3, 0, $4)
"#,
    )
    .bind(actor.user_id)
    .bind(format!("service.{}", action))
    .bind(object_type)
    .bind(json!({ "object_id": object_id, "operation_id": operation_id }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn query_json_rows(db: &PgPool, sql: &str) -> AppResult<Vec<Value>> {
    let rows = sqlx::query(sql).fetch_all(db).await?;
    let values = rows
        .into_iter()
        .map(|row| {
            let mut map = serde_json::Map::new();
            for column in row.columns() {
                let name = column.name();
                if let Ok(value) = row.try_get::<String, _>(name) {
                    map.insert(name.to_string(), Value::String(value));
                } else if let Ok(value) = row.try_get::<bool, _>(name) {
                    map.insert(name.to_string(), Value::Bool(value));
                } else if let Ok(value) = row.try_get::<i32, _>(name) {
                    map.insert(name.to_string(), json!(value));
                } else if let Ok(value) = row.try_get::<Option<i32>, _>(name) {
                    map.insert(name.to_string(), json!(value));
                }
            }
            Value::Object(map)
        })
        .collect();
    Ok(values)
}

fn require_internal(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let got = headers
        .get(RUNTIME_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if got.is_empty() || got != state.internal_token {
        return Err(AppError::unauthorized());
    }
    Ok(())
}

fn actor_from_headers(headers: &HeaderMap) -> Actor {
    let user_id = headers
        .get("x-user-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok());
    let username = headers
        .get("x-username")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    Actor { user_id, username }
}

fn ok<T: Serialize>(data: T) -> Json<Envelope<T>> {
    Json(Envelope {
        code: 0,
        msg: "success".to_string(),
        data,
    })
}

fn installer_error_code(err: &service_installer_core::InstallerError) -> &'static str {
    match err {
        service_installer_core::InstallerError::UnsafePath(_) => "SERVICE_PATH_ESCAPE",
        service_installer_core::InstallerError::InvalidManifest(_) => "SERVICE_MANIFEST_INVALID",
        service_installer_core::InstallerError::Dependency(_) => "DEPENDENCY_CONFLICT",
        service_installer_core::InstallerError::Blocked(_) => "OPERATION_BLOCKED",
        service_installer_core::InstallerError::Package(_) => "PACKAGE_INVALID",
        service_installer_core::InstallerError::Io(_) => "IO_ERROR",
        service_installer_core::InstallerError::Yaml(_) => "SERVICE_MANIFEST_PARSE_ERROR",
        service_installer_core::InstallerError::Json(_) => "JSON_ERROR",
        service_installer_core::InstallerError::Zip(_) => "PACKAGE_INVALID",
    }
}

fn redact_json(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let lower = key.to_ascii_lowercase();
                    let redacted = lower.contains("token")
                        || lower.contains("secret")
                        || lower.contains("password")
                        || lower == "authorization";
                    if redacted {
                        (key, Value::String("<redacted>".to_string()))
                    } else {
                        (key, redact_json(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_json).collect()),
        other => other,
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name)?;
    if value.trim().is_empty() {
        anyhow::bail!("{} is required", name);
    }
    Ok(value)
}

fn default_set_for_service(service_id: &str) -> String {
    if service_id == "judge-worker" {
        "judge-worker-node".to_string()
    } else {
        "single-node-oj".to_string()
    }
}

fn default_device_id() -> String {
    "root-local".to_string()
}

fn default_auth_mode() -> String {
    "internal".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_token_rejected() {
        let state = AppState {
            db: PgPoolOptions::new()
                .connect_lazy("postgres://postgres:postgres@localhost/ojos")
                .unwrap(),
            repo_root: PathBuf::from("."),
            internal_token: "secret".to_string(),
            lock_ttl_seconds: DEFAULT_LOCK_TTL_SECONDS,
        };
        let headers = HeaderMap::new();
        assert!(require_internal(&state, &headers).is_err());
    }

    #[test]
    fn redact_json_removes_sensitive_fields() {
        let value = redact_json(json!({
            "token": "abc",
            "nested": { "password": "pw", "ok": true },
            "authorization": "Bearer abc"
        }));
        assert_eq!(value["token"], "<redacted>");
        assert_eq!(value["nested"]["password"], "<redacted>");
        assert_eq!(value["nested"]["ok"], true);
        assert_eq!(value["authorization"], "<redacted>");
    }
}
