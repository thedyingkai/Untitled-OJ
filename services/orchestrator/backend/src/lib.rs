//! 可复用的 OJOS 编排器 HTTP 服务。
//!
//! daemon CLI 与桌面管理器共用这一入口：CLI 可以持续运行，桌面管理器则持有
//! [`EmbeddedServerHandle`] 并在窗口关闭时显式停止服务。

mod agent_api;
mod api_v1;
mod artifact_store;
mod audit;
mod auth;
mod build_identity;
mod catalog_registry;
mod compatibility;
mod deployment_api;
mod desktop_session;
mod durable;
mod http;
mod market_api;
mod node_api;
mod node_identity;
mod observability;
mod oidc;
mod oidc_web;
mod operation_api;
mod routes;
mod server;
mod static_site;
mod store_v1_api;
#[cfg(test)]
mod test_env;
mod topology_api;
mod topology_provider;
mod topology_worker;
mod ui_layout;

// 保持内部模块既有的 crate 根路径可用。
pub(crate) use http::*;

pub use auth::{
    OidcPrincipalVerifier, Principal, PrincipalSource, PrincipalVerificationError,
    configured_internal_token,
};
pub use server::{
    EmbeddedServerHandle, EmbeddedServerOptions, EmbeddedServerShutdown, EmbeddedStorage,
    start_embedded_server, start_embedded_server_with_console,
};
