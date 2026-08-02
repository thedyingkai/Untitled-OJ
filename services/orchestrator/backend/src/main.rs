//! OJOS Orchestrator daemon 入口。
//!
//! 模块边界：
//! - `http`   —— HTTP 报文解析/写出与状态码错误载体，不认识业务路由；
//! - `auth`   —— 控制面令牌门禁；
//! - `routes` —— URL 到 core 动作的翻译层；
//! - `server` —— 监听、工作线程池与请求分发；
//! - `market_api` / `static_site` / `ui_layout` —— 插件商店、静态托管与画布布局。

use anyhow::Result;
use clap::Parser;
use orchestrator_core::OrchestratorActionConsole;
use std::fs;
use std::path::PathBuf;

mod auth;
mod http;
mod market_api;
mod routes;
mod server;
mod static_site;
#[cfg(test)]
mod test_env;
mod ui_layout;

/// 保持 `crate::ApiRequest` / `crate::ApiResponse` / `crate::query_value` 等历史路径可用，
/// 拆分模块后 market_api 等既有模块无需改 use。
pub(crate) use http::*;

#[derive(Parser)]
#[command(name = "ojos-orchestrator-daemon")]
#[command(about = "OJOS Orchestrator HTTP API 入口（含 Web UI 托管）")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    #[arg(long, default_value = "127.0.0.1:8090")]
    bind: String,

    /// Web UI 构建产物目录；默认 <repo_root>/manager/web/dist
    #[arg(long)]
    web_root: Option<PathBuf>,
}

fn main() -> Result<()> {
    configure_utf8_console()?;
    let cli = Cli::parse();
    let repo_root = fs::canonicalize(&cli.repo_root).unwrap_or(cli.repo_root);
    let web_root = cli
        .web_root
        .clone()
        .map(|path| fs::canonicalize(&path).unwrap_or(path))
        .unwrap_or_else(|| repo_root.join("manager").join("web").join("dist"));
    let console = OrchestratorActionConsole::load(repo_root.clone())?;
    server::serve(cli.bind, console, repo_root, web_root)
}

fn configure_utf8_console() -> Result<()> {
    #[cfg(windows)]
    {
        const CP_UTF8: u32 = 65001;
        let output_ok = unsafe { SetConsoleOutputCP(CP_UTF8) } != 0;
        let input_ok = unsafe { SetConsoleCP(CP_UTF8) } != 0;
        if !output_ok || !input_ok {
            anyhow::bail!("无法将 Windows 控制台输入/输出编码设置为 UTF-8");
        }
    }
    Ok(())
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleOutputCP(code_page_id: u32) -> i32;
    fn SetConsoleCP(code_page_id: u32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 边界术语门禁：拆模块后扫描 daemon 的全部源码，而不再只看 main.rs。
    #[test]
    fn daemon_source_avoids_forbidden_boundary_terms() {
        let source_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden = forbidden_boundary_terms();
        let mut checked = 0_usize;
        for entry in fs::read_dir(&source_dir).expect("daemon src dir") {
            let path = entry.expect("daemon src entry").path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("daemon source");
            checked += 1;
            for term in &forbidden {
                assert!(
                    !source.contains(term.as_str()),
                    "daemon source {} must not contain forbidden term {term}",
                    path.display()
                );
            }
        }
        assert!(
            checked >= 7,
            "expected every daemon module to be scanned, saw {checked}"
        );
    }

    fn forbidden_boundary_terms() -> Vec<String> {
        [
            ["Ma", "chine"].concat(),
            ["De", "vice"].concat(),
            ["Service", "Installation"].concat(),
            ["Service", "Package"].concat(),
            ["Root", "Runtime", "Manager"].concat(),
            ["Root ", "Runtime ", "Manager"].concat(),
            ["oj", "os", "ctl"].concat(),
            ["shared", "-", "ui"].concat(),
            ["kernel", "/", "installer"].concat(),
            ["Runtime ", "Manager"].concat(),
            ["Module", "-first"].concat(),
            ["module", "-first"].concat(),
            ["Installer", "-first"].concat(),
            ["installer", "-first"].concat(),
        ]
        .to_vec()
    }
}
