#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use ojos_orchestrator_desktop::{
    Cli, DESKTOP_SMOKE_FAILURE_PATH, DESKTOP_SMOKE_SUCCESS_PATH, DesktopAgentHandle, LaunchConfig,
    desktop_smoke_duration_ms, desktop_smoke_mode, desktop_smoke_script_for,
    discover_external_authorization_origin, initialization_script, navigation_allowed,
    resolve_embedded_paths, resolve_launch_config, same_origin, unavailable_desktop_agent,
};
use orchestrator_backend::{
    EmbeddedServerHandle, EmbeddedServerOptions, EmbeddedStorage, start_embedded_server,
};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::webview::{NewWindowResponse, PageLoadEvent};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use url::Url;

fn main() -> anyhow::Result<()> {
    let _install_guard = ojos_orchestrator_installer::acquire_runtime_install_guard()?;
    let config = resolve_launch_config(Cli::parse())?;
    run_tauri(config)
}

struct LaunchTarget {
    url: Url,
    bootstrap_secret: Option<String>,
    server: Option<EmbeddedServerHandle>,
    agent: Option<DesktopAgentHandle>,
}

struct EmbeddedRuntimeState {
    server: Option<EmbeddedServerHandle>,
    agent: Option<DesktopAgentHandle>,
}

struct ServerState(Mutex<EmbeddedRuntimeState>);

impl ServerState {
    fn shutdown(&self) -> Result<()> {
        let (agent, server) = {
            let mut state = self
                .0
                .lock()
                .map_err(|_| anyhow!("desktop server state lock poisoned"))?;
            (state.agent.take(), state.server.take())
        };
        if let Some(agent) = agent {
            let result = agent.shutdown_and_join(Duration::from_secs(30));
            if !result.graceful {
                eprintln!("desktop local agent shutdown degraded: {}", result.detail);
            }
        }
        if let Some(server) = server {
            server.shutdown()?;
            server.join()?;
        }
        Ok(())
    }
}

fn start_launch_target(config: LaunchConfig, resource_dir: &Path) -> Result<LaunchTarget> {
    match config {
        LaunchConfig::Embedded {
            repo_root,
            web_root,
            data_dir,
            bootstrap_secret,
        } => {
            let paths =
                resolve_embedded_paths(repo_root.as_deref(), web_root.as_deref(), resource_dir)?;
            let server = start_embedded_server(EmbeddedServerOptions {
                repo_root: paths.repo_root,
                web_root: paths.web_root,
                artifact_root: data_dir.join("artifacts"),
                bind_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                internal_token: None,
                desktop_bootstrap_secret: Some(bootstrap_secret.clone()),
                desktop_agent_secret: None,
                storage: EmbeddedStorage::Sqlite {
                    database_path: data_dir.join("orchestrator.db"),
                },
            })?;
            let url = Url::parse(&format!("http://{}/", server.local_addr()))
                .context("construct embedded control-plane URL")?;
            let agent = Some(unavailable_desktop_agent());
            Ok(LaunchTarget {
                url,
                bootstrap_secret: Some(bootstrap_secret),
                server: Some(server),
                agent,
            })
        }
        LaunchConfig::External { url } => Ok(LaunchTarget {
            url,
            bootstrap_secret: None,
            server: None,
            agent: None,
        }),
    }
}

fn run_tauri(config: LaunchConfig) -> Result<()> {
    let smoke_mode = desktop_smoke_mode();
    let smoke_duration_ms = desktop_smoke_duration_ms()?;
    let startup_signal = Arc::new(Mutex::new(startup_signal_from_environment()?));

    let app = tauri::Builder::default()
        .manage(ServerState(Mutex::new(EmbeddedRuntimeState {
            server: None,
            agent: None,
        })))
        .setup(move |app| {
            let resource_dir = app
                .path()
                .resource_dir()
                .context("resolve installed Desktop resource directory")?;
            let mut target = start_launch_target(config, &resource_dir)?;
            let target_url = target.url.clone();
            let allowed_origin = target_url.clone();
            let smoke_origin = target_url.clone();
            let startup_signal = Arc::clone(&startup_signal);
            let embedded = target.server.is_some();
            if smoke_mode && !embedded {
                return Err(anyhow!(
                    "OJOS_DESKTOP_SMOKE validates the embedded Desktop control plane only"
                )
                .into());
            }
            let authorization_origin = if embedded {
                None
            } else {
                discover_external_authorization_origin(&target_url)?
            };
            let init_script =
                initialization_script(&target_url, target.bootstrap_secret.as_deref(), embedded)?;
            WebviewWindowBuilder::new(
                app,
                "orchestrator",
                WebviewUrl::External(target_url.clone()),
            )
            .title("OJOS Orchestrator")
            .inner_size(1280.0, 820.0)
            .min_inner_size(900.0, 620.0)
            .center()
            .disable_drag_drop_handler()
            .initialization_script(init_script.clone())
            .on_navigation(move |url| {
                navigation_allowed(url, &allowed_origin, authorization_origin.as_ref())
            })
            .on_new_window(|_url, _features| NewWindowResponse::Deny)
            .on_page_load(move |window, payload| {
                if payload.event() != PageLoadEvent::Finished
                    || !same_origin(payload.url(), &smoke_origin)
                {
                    return;
                }
                if payload.url().path() == "/" {
                    let signal = match startup_signal.lock() {
                        Ok(mut signal) => signal.take(),
                        Err(_) => {
                            eprintln!("Desktop startup readiness state lock poisoned");
                            window.app_handle().exit(1);
                            return;
                        }
                    };
                    if let Some(signal) = signal
                        && let Err(error) = publish_startup_ready(&signal)
                    {
                        eprintln!("Desktop could not acknowledge startup readiness: {error}");
                        window.app_handle().exit(1);
                        return;
                    }
                }
                if !smoke_mode {
                    return;
                }
                match payload.url().path() {
                    DESKTOP_SMOKE_SUCCESS_PATH => {
                        window.app_handle().exit(0);
                    }
                    DESKTOP_SMOKE_FAILURE_PATH => {
                        let detail = payload
                            .url()
                            .query_pairs()
                            .find_map(|(key, value)| {
                                (key == "detail").then_some(value.into_owned())
                            })
                            .unwrap_or_else(|| "unknown browser-side failure".to_string());
                        eprintln!("Desktop startup smoke failed: {detail}");
                        window.app_handle().exit(1);
                    }
                    "/" => {
                        if let Err(error) = window.eval(desktop_smoke_script_for(smoke_duration_ms))
                        {
                            eprintln!("Desktop startup smoke could not run: {error}");
                            window.app_handle().exit(1);
                        }
                    }
                    path => {
                        eprintln!("Desktop startup smoke reached an unexpected path: {path}");
                        window.app_handle().exit(1);
                    }
                }
            })
            .build()?;
            let state = app.state::<ServerState>();
            let mut state = state
                .0
                .lock()
                .map_err(|_| anyhow!("desktop server state lock poisoned"))?;
            state.server = target.server.take();
            state.agent = target.agent.take();
            Ok(())
        })
        .build(tauri::generate_context!())?;

    app.run(|app_handle, event| {
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            let state = app_handle.state::<ServerState>();
            if let Err(err) = state.shutdown() {
                eprintln!("desktop embedded server shutdown failed: {err}");
            }
        }
    });
    Ok(())
}

#[derive(Debug)]
struct StartupSignal {
    path: PathBuf,
    token: String,
}

fn startup_signal_from_environment() -> Result<Option<StartupSignal>> {
    let path = std::env::var_os("OJOS_DESKTOP_READY_FILE").map(PathBuf::from);
    let token = std::env::var("OJOS_DESKTOP_READY_TOKEN").ok();
    match (path, token) {
        (None, None) => Ok(None),
        (Some(path), Some(token)) if !token.trim().is_empty() => {
            Ok(Some(StartupSignal { path, token }))
        }
        _ => anyhow::bail!(
            "OJOS_DESKTOP_READY_FILE and OJOS_DESKTOP_READY_TOKEN must be provided together"
        ),
    }
}

fn publish_startup_ready(signal: &StartupSignal) -> Result<()> {
    let parent = signal
        .path
        .parent()
        .ok_or_else(|| anyhow!("Desktop readiness file has no parent directory"))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create Desktop readiness directory {}", parent.display()))?;
    let temporary = signal
        .path
        .with_extension(format!("ready-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .with_context(|| format!("create Desktop readiness file {}", temporary.display()))?;
    file.write_all(signal.token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &signal.path)
        .with_context(|| format!("publish Desktop readiness file {}", signal.path.display()))?;
    Ok(())
}

#[cfg(test)]
mod startup_signal_tests {
    use super::*;

    #[test]
    fn startup_readiness_is_published_atomically_with_the_exact_token() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ready");
        let signal = StartupSignal {
            path: path.clone(),
            token: "launch-token".to_string(),
        };
        publish_startup_ready(&signal).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "launch-token\n");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
