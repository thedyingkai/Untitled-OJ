use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use module_installer_core::{
    InstalledModule, Manifest, ModuleState, Plan, RegistrySnapshot, enable_plan, install_plan,
    rollback_plan, uninstall_plan, upgrade_plan, validate_manifest, validate_manifest_file,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const INSTALLER_TOKEN_HEADER: &str = "x-ojos-installer-token";
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
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "BAD_REQUEST",
            msg: msg.into(),
            details: json!({}),
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "INSTALLER_INTERNAL_UNAUTHORIZED",
            msg: "missing or invalid installer internal token".to_string(),
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
            code: "MODULE_NOT_FOUND",
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

impl From<module_installer_core::InstallerError> for AppError {
    fn from(value: module_installer_core::InstallerError) -> Self {
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
    manifest: Option<Manifest>,
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
    module_id: String,
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
    let internal_token = required_env("MODULE_INSTALLER_INTERNAL_TOKEN")?;
    let repo_root = std::env::var("OJOS_REPO_ROOT").unwrap_or_else(|_| ".".to_string());
    let host = std::env::var("MODULE_INSTALLER_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let port: u16 = std::env::var("MODULE_INSTALLER_PORT")
        .unwrap_or_else(|_| "8090".to_string())
        .parse()?;
    let lock_ttl_seconds = std::env::var("MODULE_INSTALLER_LOCK_TTL_SECONDS")
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
    tracing::info!("module-installer listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

fn healthcheck_command() -> anyhow::Result<()> {
    let host = std::env::var("MODULE_INSTALLER_HOST")
        .ok()
        .filter(|value| value != "0.0.0.0")
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = std::env::var("MODULE_INSTALLER_PORT")
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
        anyhow::bail!("module-installer healthcheck failed");
    }
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/modules/discover", get(discover))
        .route("/internal/modules/validate", post(validate))
        .route("/internal/modules/plan", post(plan))
        .route("/internal/modules/install", post(install))
        .route("/internal/modules/{id}/enable", post(enable))
        .route("/internal/modules/{id}/disable", post(disable))
        .route(
            "/internal/modules/{id}/upgrade-plan",
            post(upgrade_plan_handler),
        )
        .route(
            "/internal/modules/{id}/rollback-plan",
            post(rollback_plan_handler),
        )
        .route(
            "/internal/modules/{id}/uninstall-dry-run",
            post(uninstall_dry_run),
        )
        .route("/internal/modules/{id}/health", get(module_health))
        .route("/internal/modules/{id}/operations", get(operations))
        .with_state(state)
}

async fn health(State(state): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(json!({ "status": "ok" })))
}

async fn discover(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let mut modules = Vec::new();
    let modules_dir = state.repo_root.join("modules");
    if modules_dir.exists() {
        for entry in std::fs::read_dir(modules_dir)
            .map_err(|_| AppError::internal("module discovery failed"))?
        {
            let entry = entry.map_err(|_| AppError::internal("module discovery failed"))?;
            let manifest_path = PathBuf::from("modules")
                .join(entry.file_name())
                .join("module.yaml");
            if state.repo_root.join(&manifest_path).exists() {
                match validate_manifest_file(&state.repo_root, &manifest_path) {
                    Ok(manifest) => modules.push(json!({
                        "manifest_path": manifest_path.to_string_lossy().replace('\\', "/"),
                        "module_id": manifest.id,
                        "name": manifest.name,
                        "version": manifest.version,
                        "status": manifest.status,
                    })),
                    Err(err) => modules.push(json!({
                        "manifest_path": manifest_path.to_string_lossy().replace('\\', "/"),
                        "valid": false,
                        "error": err.to_string(),
                    })),
                }
            }
        }
    }
    Ok(ok(json!({ "modules": modules })))
}

async fn validate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let manifest = load_manifest(&state, &req)?;
    validate_manifest(&manifest)?;
    Ok(ok(json!({ "valid": true, "manifest": manifest })))
}

async fn plan(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<Plan>>> {
    require_internal(&state, &headers)?;
    let manifest = load_manifest(&state, &req)?;
    validate_manifest(&manifest)?;
    let snapshot = load_snapshot(&state.db).await?;
    let plan = install_plan(&manifest, &snapshot, true)?;
    Ok(ok(plan))
}

async fn install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let actor = actor_from_headers(&headers);
    let manifest = load_manifest(&state, &req)?;
    validate_manifest(&manifest)?;
    let snapshot = load_snapshot(&state.db).await?;
    let plan = install_plan(&manifest, &snapshot, req.dry_run)?;
    if req.dry_run {
        return Ok(ok(json!({ "plan": plan })));
    }
    if !plan.can_apply {
        return Err(AppError::forbidden("install plan is blocked"));
    }
    let request = serde_json::to_value(&req).unwrap_or_else(|_| json!({}));
    let module_id = manifest.id.clone();
    let manifest_for_apply = manifest.clone();
    let result = run_locked_operation(&state, &actor, &module_id, "install", request, plan.clone(), |tx| {
        Box::pin(async move {
            apply_install(tx, &manifest_for_apply).await?;
            Ok(json!({ "installed": true, "module_id": manifest_for_apply.id, "version": manifest_for_apply.version }))
        })
    })
    .await?;
    Ok(ok(json!({ "plan": plan, "result": result })))
}

async fn enable(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let actor = actor_from_headers(&headers);
    let snapshot = load_snapshot(&state.db).await?;
    let plan = enable_plan(&id, &snapshot, false)?;
    if !plan.can_apply {
        return Err(AppError::forbidden("enable plan is blocked"));
    }
    let module_id = id.clone();
    let result = run_locked_operation(&state, &actor, &id, "enable", json!({ "module_id": id }), plan.clone(), |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE module_nodes SET status = 'ENABLED', updated_at = NOW() WHERE module_id = $1")
                .bind(&module_id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE module_installations SET status = 'ENABLED', updated_at = NOW(), enabled_at = COALESCE(enabled_at, NOW()), disabled_at = NULL WHERE module_id = $1")
                .bind(&module_id)
                .execute(&mut **tx)
                .await?;
            Ok(json!({ "enabled": true, "module_id": module_id }))
        })
    })
    .await?;
    Ok(ok(json!({ "plan": plan, "result": result })))
}

async fn disable(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let actor = actor_from_headers(&headers);
    let snapshot = load_snapshot(&state.db).await?;
    let plan = module_installer_core::disable_plan(&id, &snapshot, false)?;
    if !plan.can_apply {
        return Err(AppError::forbidden("disable plan is blocked"));
    }
    let module_id = id.clone();
    let result = run_locked_operation(&state, &actor, &id, "disable", json!({ "module_id": id }), plan.clone(), |tx| {
        Box::pin(async move {
            sqlx::query("UPDATE module_nodes SET status = 'DISABLED', updated_at = NOW() WHERE module_id = $1")
                .bind(&module_id)
                .execute(&mut **tx)
                .await?;
            sqlx::query("UPDATE module_installations SET status = 'DISABLED', updated_at = NOW(), disabled_at = COALESCE(disabled_at, NOW()) WHERE module_id = $1")
                .bind(&module_id)
                .execute(&mut **tx)
                .await?;
            Ok(json!({ "disabled": true, "module_id": module_id }))
        })
    })
    .await?;
    Ok(ok(json!({ "plan": plan, "result": result })))
}

async fn upgrade_plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Json(req): Json<ManifestReq>,
) -> AppResult<Json<Envelope<Plan>>> {
    require_internal(&state, &headers)?;
    let new_manifest = if req.manifest.is_some() || !req.manifest_path.trim().is_empty() {
        load_manifest(&state, &req)?
    } else {
        get_installed_manifest(&state.db, &id).await?
    };
    let old_manifest = get_installed_manifest(&state.db, &id).await.ok();
    let snapshot = load_snapshot(&state.db).await?;
    let plan = upgrade_plan(old_manifest.as_ref(), &new_manifest, &snapshot, true)?;
    Ok(ok(plan))
}

async fn rollback_plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Plan>>> {
    require_internal(&state, &headers)?;
    let snapshot = load_snapshot(&state.db).await?;
    Ok(ok(rollback_plan(&id, &snapshot, true)?))
}

async fn uninstall_dry_run(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Plan>>> {
    require_internal(&state, &headers)?;
    let snapshot = load_snapshot(&state.db).await?;
    Ok(ok(uninstall_plan(&id, &snapshot, true)?))
}

async fn module_health(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let status = sqlx::query("SELECT status FROM module_nodes WHERE module_id = $1")
        .bind(&id)
        .fetch_optional(&state.db)
        .await?
        .map(|row| row.get::<String, _>("status"));
    match status {
        Some(status) => Ok(ok(
            json!({ "module_id": id, "status": "ok", "module_status": status }),
        )),
        None => Err(AppError::not_found("module not found")),
    }
}

async fn operations(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> AppResult<Json<Envelope<Value>>> {
    require_internal(&state, &headers)?;
    let rows = sqlx::query(
        r#"
SELECT operation_id, module_id, action, status, actor_user_id, actor_username,
       request, plan, result, error_message, created_at::text, updated_at::text
FROM module_operations
WHERE module_id = $1
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
            module_id: row.get("module_id"),
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

fn load_manifest(state: &AppState, req: &ManifestReq) -> AppResult<Manifest> {
    if let Some(manifest) = &req.manifest {
        validate_manifest(manifest)?;
        return Ok(manifest.clone());
    }
    let path = if req.manifest_path.trim().is_empty() {
        "modules/demo-module/module.yaml"
    } else {
        req.manifest_path.trim()
    };
    let manifest = validate_manifest_file(&state.repo_root, Path::new(path))?;
    Ok(manifest)
}

async fn load_snapshot(db: &PgPool) -> AppResult<RegistrySnapshot> {
    let rows = sqlx::query(
        r#"
SELECT module_id, name, version, status, kind, manifest
FROM module_nodes
ORDER BY module_id
"#,
    )
    .fetch_all(db)
    .await?;
    let modules = rows
        .into_iter()
        .map(|row| {
            let manifest_value: Value = row.get("manifest");
            let manifest = serde_json::from_value::<Manifest>(manifest_value).ok();
            InstalledModule {
                module_id: row.get("module_id"),
                name: row.get("name"),
                version: row.get("version"),
                status: parse_state(row.get::<String, _>("status")),
                kind: row.get("kind"),
                manifest,
            }
        })
        .collect();
    Ok(RegistrySnapshot { modules })
}

async fn get_installed_manifest(db: &PgPool, module_id: &str) -> AppResult<Manifest> {
    let row = sqlx::query("SELECT manifest FROM module_nodes WHERE module_id = $1")
        .bind(module_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::not_found("module not found"))?;
    let value: Value = row.get("manifest");
    serde_json::from_value(value)
        .map_err(|_| AppError::bad_request("installed manifest is not schema_version 1"))
}

fn parse_state(status: String) -> ModuleState {
    match status.as_str() {
        "ENABLED" => ModuleState::Enabled,
        "DISABLED" => ModuleState::Disabled,
        "FAILED_INSTALL" => ModuleState::FailedInstall,
        "FAILED_UPGRADE" => ModuleState::FailedUpgrade,
        "REMOVED" => ModuleState::Removed,
        _ => ModuleState::Installed,
    }
}

async fn apply_install(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    manifest: &Manifest,
) -> Result<(), sqlx::Error> {
    let manifest_json = serde_json::to_value(manifest).unwrap_or_else(|_| json!({}));
    let install_status = if manifest.kind == "kernel" || manifest.status == "builtin" {
        "ENABLED"
    } else {
        "DISABLED"
    };
    sqlx::query(
        r#"
INSERT INTO module_sets(set_id, name, description, sort_order)
VALUES($1, $1, '', 100)
ON CONFLICT(set_id) DO NOTHING
"#,
    )
    .bind(&manifest.set)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
INSERT INTO module_nodes(module_id, set_id, name, version, status, kind, description, manifest)
VALUES($1,$2,$3,$4,$5,$6,$7,$8)
ON CONFLICT(module_id) DO UPDATE SET
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
    .bind(&manifest.set)
    .bind(&manifest.name)
    .bind(&manifest.version)
    .bind(install_status)
    .bind(&manifest.kind)
    .bind(&manifest.description)
    .bind(&manifest_json)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        r#"
INSERT INTO module_installations(module_id, name, version, status, manifest, enabled_at)
VALUES($1,$2,$3,$4,$5,CASE WHEN $4 = 'ENABLED' THEN NOW() ELSE NULL END)
ON CONFLICT(module_id) DO UPDATE SET
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    manifest = EXCLUDED.manifest,
    updated_at = NOW(),
    enabled_at = CASE WHEN EXCLUDED.status = 'ENABLED' THEN COALESCE(module_installations.enabled_at, NOW()) ELSE module_installations.enabled_at END,
    disabled_at = CASE WHEN EXCLUDED.status = 'DISABLED' THEN COALESCE(module_installations.disabled_at, NOW()) ELSE NULL END
"#,
    )
    .bind(&manifest.id)
    .bind(&manifest.name)
    .bind(&manifest.version)
    .bind(install_status)
    .bind(&manifest_json)
    .execute(&mut **tx)
    .await?;

    for dep in &manifest.requires.modules {
        sqlx::query(
            r#"
INSERT INTO module_edges(from_module_id, to_module_id, edge_type, version_constraint, required)
VALUES($1,$2,'requires',$3,true)
ON CONFLICT(from_module_id, to_module_id, edge_type) DO UPDATE SET
    version_constraint = EXCLUDED.version_constraint,
    required = EXCLUDED.required
"#,
        )
        .bind(&manifest.id)
        .bind(&dep.id)
        .bind(&dep.version)
        .execute(&mut **tx)
        .await?;
    }

    for component in &manifest.provides.components {
        upsert_component(
            tx,
            &manifest.id,
            &component.id,
            &component.component_type,
            &component.status,
            &component.config,
        )
        .await?;
    }
    for health in &manifest.provides.health_checks {
        upsert_component(
            tx,
            &manifest.id,
            &health.id,
            "health_check",
            if health.optional {
                "DISABLED"
            } else {
                install_status
            },
            &json!({ "type": health.check_type, "optional": health.optional }),
        )
        .await?;
    }

    for permission in &manifest.provides.permissions {
        sqlx::query(
            r#"
INSERT INTO module_permissions(module_id, permission_key, description)
VALUES($1,$2,$3)
ON CONFLICT(permission_key) DO UPDATE SET
    module_id = EXCLUDED.module_id,
    description = EXCLUDED.description
"#,
        )
        .bind(&manifest.id)
        .bind(&permission.key)
        .bind(&permission.description)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            r#"
INSERT INTO permissions(code, module_code, name, description)
VALUES($1,$2,$1,$3)
ON CONFLICT(code) DO UPDATE SET
    module_code = EXCLUDED.module_code,
    description = EXCLUDED.description
"#,
        )
        .bind(&permission.key)
        .bind(&manifest.id)
        .bind(&permission.description)
        .execute(&mut **tx)
        .await?;
    }

    for menu in &manifest.provides.menus {
        sqlx::query(
            r#"
INSERT INTO module_menus(module_id, menu_key, title, route_path, icon, parent_key, sort_order, required_permission, enabled)
VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)
ON CONFLICT(menu_key) DO UPDATE SET
    module_id = EXCLUDED.module_id,
    title = EXCLUDED.title,
    route_path = EXCLUDED.route_path,
    icon = EXCLUDED.icon,
    parent_key = EXCLUDED.parent_key,
    sort_order = EXCLUDED.sort_order,
    required_permission = EXCLUDED.required_permission,
    enabled = EXCLUDED.enabled
"#,
        )
        .bind(&manifest.id)
        .bind(&menu.key)
        .bind(&menu.title)
        .bind(&menu.route_path)
        .bind(&menu.icon)
        .bind(&menu.parent_key)
        .bind(menu.sort_order)
        .bind(&menu.required_permission)
        .bind(menu.enabled)
        .execute(&mut **tx)
        .await?;
    }

    for route in &manifest.provides.frontend_routes {
        sqlx::query(
            r#"
INSERT INTO module_frontend_routes(module_id, route_path, route_name, component_key, required_permission, enabled)
VALUES($1,$2,$3,$4,$5,$6)
ON CONFLICT(module_id, route_path) DO UPDATE SET
    route_name = EXCLUDED.route_name,
    component_key = EXCLUDED.component_key,
    required_permission = EXCLUDED.required_permission,
    enabled = EXCLUDED.enabled
"#,
        )
        .bind(&manifest.id)
        .bind(&route.path)
        .bind(&route.name)
        .bind(&route.component_key)
        .bind(&route.required_permission)
        .bind(route.enabled)
        .execute(&mut **tx)
        .await?;
    }

    for route in &manifest.provides.gateway_routes {
        sqlx::query(
            r#"
INSERT INTO module_gateway_routes(module_id, prefix, target_service, auth_mode, enabled)
VALUES($1,$2,$3,$4,$5)
ON CONFLICT(prefix) DO UPDATE SET
    module_id = EXCLUDED.module_id,
    target_service = EXCLUDED.target_service,
    auth_mode = EXCLUDED.auth_mode,
    enabled = EXCLUDED.enabled
"#,
        )
        .bind(&manifest.id)
        .bind(&route.prefix)
        .bind(&route.target_service)
        .bind(&route.auth_mode)
        .bind(route.enabled)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

async fn upsert_component(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    module_id: &str,
    component_id: &str,
    component_type: &str,
    status: &str,
    config: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
INSERT INTO module_components(module_id, component_id, component_type, status, config)
VALUES($1,$2,$3,$4,$5)
ON CONFLICT(module_id, component_id) DO UPDATE SET
    component_type = EXCLUDED.component_type,
    status = EXCLUDED.status,
    config = EXCLUDED.config,
    updated_at = NOW()
"#,
    )
    .bind(module_id)
    .bind(component_id)
    .bind(component_type)
    .bind(if status.trim().is_empty() {
        "DISABLED"
    } else {
        status
    })
    .bind(config)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

type BoxFutureResult<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, sqlx::Error>> + Send + 'a>>;

async fn run_locked_operation<F>(
    state: &AppState,
    actor: &Actor,
    module_id: &str,
    action: &str,
    request: Value,
    plan: Plan,
    apply: F,
) -> AppResult<Value>
where
    F: for<'a> FnOnce(&'a mut sqlx::Transaction<'_, sqlx::Postgres>) -> BoxFutureResult<'a>,
{
    let operation_id = Uuid::new_v4().to_string();
    let owner = format!("module-installer:{}", operation_id);
    acquire_lock(
        &state.db,
        "module-installer-global",
        &owner,
        state.lock_ttl_seconds,
    )
    .await?;
    let mut tx = state.db.begin().await?;
    let plan_json = serde_json::to_value(&plan).unwrap_or_else(|_| json!({}));
    let request_json = redact_json(request.clone());
    let initial_result = json!({});
    sqlx::query(
        r#"
INSERT INTO module_operations(operation_id, module_id, action, status, actor_user_id, actor_username, request, plan, result)
VALUES($1,$2,$3,'RUNNING',$4,$5,$6,$7,$8)
"#,
    )
    .bind(&operation_id)
    .bind(module_id)
    .bind(action)
    .bind(actor.user_id)
    .bind(&actor.username)
    .bind(&request_json)
    .bind(&plan_json)
    .bind(&initial_result)
    .execute(&mut *tx)
    .await?;
    match apply(&mut tx).await {
        Ok(result) => {
            write_audit(&mut tx, actor, module_id, action, &operation_id).await?;
            sqlx::query(
                r#"
UPDATE module_operations
SET status = 'SUCCEEDED', result = $2, updated_at = NOW()
WHERE operation_id = $1
"#,
            )
            .bind(&operation_id)
            .bind(redact_json(result.clone()))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            release_lock(&state.db, "module-installer-global", &owner).await;
            Ok(json!({ "operation_id": operation_id, "result": result }))
        }
        Err(err) => {
            let message = "operation failed";
            let _ = tx.rollback().await;
            let _ = sqlx::query(
                r#"
INSERT INTO module_operations(operation_id, module_id, action, status, actor_user_id, actor_username, request, plan, result, error_message)
VALUES($1,$2,$3,'FAILED',$4,$5,$6,$7,$8,$9)
ON CONFLICT(operation_id) DO UPDATE SET
    status = 'FAILED',
    error_message = EXCLUDED.error_message,
    updated_at = NOW()
"#,
            )
            .bind(&operation_id)
            .bind(module_id)
            .bind(action)
            .bind(actor.user_id)
            .bind(&actor.username)
            .bind(&request_json)
            .bind(&plan_json)
            .bind(&json!({}))
            .bind(message)
            .execute(&state.db)
            .await;
            release_lock(&state.db, "module-installer-global", &owner).await;
            tracing::error!(error = %err, "module operation failed");
            Err(AppError::internal("operation failed"))
        }
    }
}

async fn acquire_lock(db: &PgPool, key: &str, owner: &str, ttl_seconds: u64) -> AppResult<()> {
    let row = sqlx::query(
        r#"
INSERT INTO module_operation_locks(lock_key, owner, expires_at)
VALUES($1,$2,NOW() + ($3::text || ' seconds')::interval)
ON CONFLICT(lock_key) DO UPDATE SET
    owner = EXCLUDED.owner,
    acquired_at = NOW(),
    expires_at = EXCLUDED.expires_at
WHERE module_operation_locks.expires_at < NOW()
RETURNING owner
"#,
    )
    .bind(key)
    .bind(owner)
    .bind(ttl_seconds as i64)
    .fetch_optional(db)
    .await?;
    if row.is_none() {
        return Err(AppError::conflict("module operation lock is held"));
    }
    Ok(())
}

async fn release_lock(db: &PgPool, key: &str, owner: &str) {
    let _ = sqlx::query("DELETE FROM module_operation_locks WHERE lock_key = $1 AND owner = $2")
        .bind(key)
        .bind(owner)
        .execute(db)
        .await;
}

async fn write_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &Actor,
    module_id: &str,
    action: &str,
    operation_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
INSERT INTO permission_audit_logs(actor_type, actor_id, action, target_type, target_id, metadata)
VALUES('user', COALESCE($1, 0), $2, 'module', 0, $3)
"#,
    )
    .bind(actor.user_id)
    .bind(format!("module.{}", action))
    .bind(json!({ "module_id": module_id, "operation_id": operation_id }))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn require_internal(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    let got = headers
        .get(INSTALLER_TOKEN_HEADER)
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

fn installer_error_code(err: &module_installer_core::InstallerError) -> &'static str {
    match err {
        module_installer_core::InstallerError::UnsafePath(_) => "MANIFEST_PATH_ESCAPE",
        module_installer_core::InstallerError::InvalidManifest(_) => "MANIFEST_INVALID",
        module_installer_core::InstallerError::Dependency(_) => "DEPENDENCY_CONFLICT",
        module_installer_core::InstallerError::Blocked(_) => "OPERATION_BLOCKED",
        module_installer_core::InstallerError::Package(_) => "PACKAGE_INVALID",
        module_installer_core::InstallerError::Io(_) => "IO_ERROR",
        module_installer_core::InstallerError::Yaml(_) => "MANIFEST_PARSE_ERROR",
        module_installer_core::InstallerError::Json(_) => "JSON_ERROR",
        module_installer_core::InstallerError::Zip(_) => "PACKAGE_INVALID",
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
    fn state_parse_accepts_enabled() {
        assert!(parse_state("ENABLED".to_string()).is_enabled());
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
