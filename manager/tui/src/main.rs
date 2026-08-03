use anyhow::Result;
use clap::Parser;
mod api_client;
mod device_auth;
mod remote;
mod worker;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use orchestrator_legacy::{
    ActionRequest, DeploymentViewRow, OperationViewRow, OperationWorkbenchContext,
    OperationWorkbenchSession, OperationWorkbenchView, OrchestratorActionConsole, OrchestratorView,
    OrchestratorViewPage, endpoint_hosts, merge_operation_workbench_session_into_view,
    parse_endpoint_id,
};
use orchestrator_manager::{
    GithubReleaseView, InstalledServiceView, StoreCatalog, StoreIndexView, StoreInstallRequest,
    StoreModuleView, StoreStatusView, installed_services_from_deployments,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use worker::{CoreSnapshot, ManagerEvent, ManagerTask, ManagerWorker, WorkPurpose};

#[derive(Parser)]
#[command(name = "ojos-orchestrator-tui")]
#[command(about = "OJOS Orchestrator 原生 TUI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,

    /// Connect to a running control plane through its stable `/api/v1` API.
    /// If omitted, `OJOS_ORCHESTRATOR_URL` is used before compatibility/local mode.
    #[arg(long)]
    api_url: Option<String>,

    /// OIDC issuer used for Device Authorization Grant discovery.
    #[arg(long)]
    oidc_issuer: Option<String>,

    /// Public OIDC client registered for the TUI Device Authorization Grant.
    #[arg(long)]
    oidc_client_id: Option<String>,

    /// Space-delimited scopes requested by the TUI.
    #[arg(long)]
    oidc_scope: Option<String>,

    /// Optional OAuth audience/resource hint requested from the provider.
    #[arg(long)]
    oidc_audience: Option<String>,

    /// Execute one remote TUI command and print its v1 result as JSON.
    #[arg(long)]
    command: Option<String>,

    /// Explicitly run the deprecated in-process compatibility console.
    #[arg(long)]
    legacy_local: bool,
}

/// TUI 页签：core 的 9 个正式对象页 + TUI 独有的插件商店页。
///
/// 商店页只是 Web UI 商店的终端形态，不是新的 core 对象，
/// 所以 `OrchestratorViewPage` 保持不变，由这里包一层。
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum TuiPage {
    Core(OrchestratorViewPage),
    Store,
}

impl TuiPage {
    fn all() -> Vec<Self> {
        let mut pages = OrchestratorViewPage::all()
            .iter()
            .map(|page| Self::Core(*page))
            .collect::<Vec<_>>();
        pages.push(Self::Store);
        pages
    }

    fn title(self) -> &'static str {
        match self {
            Self::Core(page) => page.title(),
            Self::Store => "商店 Store",
        }
    }

    fn key(self) -> Option<char> {
        match self {
            Self::Core(page) => page.key(),
            Self::Store => Some('0'),
        }
    }
}

/// 「导入并安装」表单。字段顺序：仓库、包地址、校验和、目标主机、驱动、外部运行。
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreInstallForm {
    module_id: String,
    repo: String,
    source_url: String,
    checksum: String,
    host_ip: String,
    execute_service_driver: bool,
    external_service_running: bool,
    field: usize,
}

impl Default for StoreInstallForm {
    fn default() -> Self {
        Self {
            module_id: String::new(),
            repo: String::new(),
            source_url: String::new(),
            checksum: String::new(),
            host_ip: "127.0.0.1".to_string(),
            execute_service_driver: false,
            external_service_running: false,
            field: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct StorePane {
    status: StoreStatusView,
    index_url: String,
    cached: bool,
    modules: Vec<StoreModuleView>,
    installed: BTreeMap<String, InstalledServiceView>,
    message: String,
    selected: usize,
    form: Option<StoreInstallForm>,
    releases: Vec<GithubReleaseView>,
    selected_release: usize,
    selected_asset: usize,
    uninstall_driver_authorized: bool,
}

impl StorePane {
    fn new(status: StoreStatusView, installed: BTreeMap<String, InstalledServiceView>) -> Self {
        Self {
            index_url: status.index_url.clone(),
            status,
            cached: false,
            modules: Vec::new(),
            installed,
            message: "正在后台加载商店索引…".to_string(),
            selected: 0,
            form: None,
            releases: Vec::new(),
            selected_release: 0,
            selected_asset: 0,
            uninstall_driver_authorized: false,
        }
    }

    fn apply_index(&mut self, index: StoreIndexView) -> Result<()> {
        self.modules = index.modules()?;
        self.index_url = index.index_url;
        self.cached = index.cached;
        self.installed = index.installed;
        self.message.clear();
        if self.selected >= self.modules.len() {
            self.selected = 0;
        }
        Ok(())
    }

    fn selected_asset_url(&self) -> Option<&str> {
        self.releases
            .get(self.selected_release)
            .and_then(|release| release.assets.get(self.selected_asset))
            .map(|asset| asset.browser_download_url.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputEditor {
    WorkbenchField { name: String, value: String },
    OperationFilter { value: String },
}

struct App {
    page: TuiPage,
    console: Arc<Mutex<OrchestratorActionConsole>>,
    worker: ManagerWorker,
    context: OperationWorkbenchContext,
    session: OperationWorkbenchSession,
    selected_field_index: usize,
    selected_endpoint_index: usize,
    selected_deployment_index: usize,
    selected_link_index: usize,
    selected_operation_index: usize,
    selected_release_index: usize,
    operation_filter: String,
    selected_log_operation_id: String,
    input_editor: Option<InputEditor>,
    execute_service_driver: bool,
    store: StorePane,
    view: OrchestratorView,
    last_message: String,
    last_live_refresh: Instant,
}

impl App {
    fn new(repo_root: PathBuf) -> Result<Self> {
        let console = OrchestratorActionConsole::load(repo_root.clone())?;
        Self::from_console(console, repo_root)
    }

    #[cfg(test)]
    fn new_memory(repo_root: PathBuf) -> Result<Self> {
        let console = OrchestratorActionConsole::load_with_database_url(repo_root.clone(), None)?;
        let mut app = Self::from_console(console, repo_root)?;
        app.wait_for_worker();
        Ok(app)
    }

    fn from_console(console: OrchestratorActionConsole, repo_root: PathBuf) -> Result<Self> {
        let context = console.context()?;
        let session = context.build_session("release.install")?;
        let view = console.view()?;
        let installed = installed_services_from_deployments(view.deployments.clone())?;
        let selected_release_index = view
            .release_registry
            .iter()
            .position(|record| record.record_type == "release")
            .unwrap_or(0);
        let catalog = Arc::new(StoreCatalog::new());
        let status = catalog.status(&console);
        let store = StorePane::new(status, installed);
        let console = Arc::new(Mutex::new(console));
        let worker = ManagerWorker::spawn(console.clone(), catalog.clone(), repo_root.clone());
        let mut app = Self {
            page: TuiPage::Core(OrchestratorViewPage::Overview),
            console,
            worker,
            context,
            session,
            selected_field_index: 0,
            selected_endpoint_index: 0,
            selected_deployment_index: 0,
            selected_link_index: 0,
            selected_operation_index: 0,
            selected_release_index,
            operation_filter: String::new(),
            selected_log_operation_id: String::new(),
            input_editor: None,
            execute_service_driver: false,
            store,
            view,
            last_message: String::new(),
            last_live_refresh: Instant::now(),
        };
        app.queue_store_reload(false);
        Ok(app)
    }

    fn apply_core_state(
        &mut self,
        context: OperationWorkbenchContext,
        view: OrchestratorView,
        installed: BTreeMap<String, InstalledServiceView>,
    ) -> Result<()> {
        let selected_deployment = self
            .selected_deployment()
            .map(|row| (row.service_id.clone(), row.host_ip.clone()));
        let selected_link = self
            .selected_link()
            .map(|row| (row.from.clone(), row.to.clone()));
        let selected_operation_id = self
            .selected_operation()
            .map(|row| row.operation_id.clone());
        let action = self.session.workbench.selected_action.clone();
        let session = context
            .build_session_from_request(&self.session.workbench.request)
            .or_else(|_| context.build_session(&action))
            .or_else(|_| context.build_session("release.install"))?;
        self.context = context;
        self.session = session;
        self.view = view;
        self.store.installed = installed;
        if self.selected_release_index >= self.view.release_registry.len() {
            self.selected_release_index = self
                .view
                .release_registry
                .iter()
                .position(|record| record.record_type == "release")
                .unwrap_or(0);
        }
        self.selected_endpoint_index =
            bounded_index(self.selected_endpoint_index, self.view.endpoints.len());
        self.selected_deployment_index = selected_deployment
            .and_then(|(service_id, host_ip)| {
                self.view
                    .deployments
                    .iter()
                    .position(|row| row.service_id == service_id && row.host_ip == host_ip)
            })
            .unwrap_or_else(|| {
                bounded_index(self.selected_deployment_index, self.view.deployments.len())
            });
        self.selected_link_index = selected_link
            .and_then(|(from, to)| {
                self.view
                    .links
                    .iter()
                    .position(|row| row.from == from && row.to == to)
            })
            .unwrap_or_else(|| bounded_index(self.selected_link_index, self.view.links.len()));
        self.selected_operation_index = selected_operation_id
            .and_then(|operation_id| {
                self.view
                    .operations
                    .iter()
                    .position(|row| row.operation_id == operation_id)
            })
            .unwrap_or_else(|| {
                bounded_index(self.selected_operation_index, self.view.operations.len())
            });
        Ok(())
    }

    fn apply_snapshot(&mut self, snapshot: CoreSnapshot) {
        if let Err(err) = self.apply_core_state(snapshot.context, snapshot.view, snapshot.installed)
        {
            self.last_message = err.to_string();
        }
    }

    fn submit_task(&mut self, task: ManagerTask, message: impl Into<String>) -> bool {
        if self.worker.is_busy() {
            self.last_message = "已有后台管理任务正在执行，请稍候".to_string();
            return false;
        }
        match self.worker.submit(task) {
            Ok(()) => {
                self.last_message = message.into();
                true
            }
            Err(err) => {
                self.last_message = err;
                false
            }
        }
    }

    fn queue_refresh(&mut self) {
        self.submit_task(ManagerTask::Refresh, "正在后台刷新…");
    }

    fn queue_store_reload(&mut self, refresh: bool) {
        if self.submit_task(
            ManagerTask::LoadStoreIndex { refresh },
            "正在后台加载商店索引…",
        ) {
            self.store.message = "正在后台加载商店索引…".to_string();
        }
    }

    fn queue_github_releases(&mut self, repo: String) {
        if repo.trim().is_empty() {
            self.last_message = "请先填写 GitHub 仓库 owner/name".to_string();
            return;
        }
        self.submit_task(
            ManagerTask::LoadGithubReleases { repo },
            "正在后台获取 GitHub Releases…",
        );
    }

    fn drain_worker_events(&mut self) {
        while let Some(event) = self.worker.try_next() {
            self.handle_worker_event(event);
        }
    }

    fn handle_worker_event(&mut self, event: ManagerEvent) {
        match event {
            ManagerEvent::Refreshed(Ok(snapshot)) => {
                self.apply_snapshot(*snapshot);
                self.last_message = "已刷新".to_string();
            }
            ManagerEvent::Refreshed(Err(err)) => self.last_message = err,
            ManagerEvent::StoreIndexLoaded(Ok(index)) => {
                let count = index
                    .index
                    .get("modules")
                    .and_then(|value| value.as_array())
                    .map(Vec::len)
                    .unwrap_or_default();
                match self.store.apply_index(*index) {
                    Ok(()) => self.last_message = format!("商店索引已加载，共 {count} 个模块"),
                    Err(err) => {
                        self.store.message = err.to_string();
                        self.last_message = self.store.message.clone();
                    }
                }
            }
            ManagerEvent::StoreIndexLoaded(Err(err)) => {
                self.store.message = format!("读取商店索引失败: {err}");
                self.last_message = self.store.message.clone();
            }
            ManagerEvent::GithubReleasesLoaded(Ok(list)) => {
                let list = *list;
                self.store.releases = list.releases;
                self.store.selected_release = 0;
                self.store.selected_asset = 0;
                if let Some(url) = self.store.selected_asset_url().map(str::to_string)
                    && let Some(form) = self.store.form.as_mut()
                {
                    form.source_url = url;
                }
                self.last_message = format!(
                    "已加载 {} 的 {} 个 GitHub Release；PageUp/PageDown 切换资产",
                    list.repo,
                    self.store.releases.len()
                );
            }
            ManagerEvent::GithubReleasesLoaded(Err(err)) => {
                self.store.releases.clear();
                self.last_message = format!("获取 GitHub Release 失败: {err}");
            }
            ManagerEvent::Installed(Ok(completion)) => {
                let completion = *completion;
                let service_id = completion.result.service_id.clone();
                let result = completion.result.action_result;
                self.apply_snapshot(completion.snapshot);
                self.store.form = None;
                self.last_message = format!(
                    "已安装 {service_id} · {} {}: {}",
                    result.capability_status.label(),
                    result.status,
                    result.message
                );
            }
            ManagerEvent::Installed(Err(err)) => {
                self.last_message = format!("安装失败: {err}");
            }
            ManagerEvent::Dispatched {
                purpose,
                completion: Ok(completion),
            } => {
                let completion = *completion;
                let result = completion.result;
                self.apply_snapshot(completion.snapshot);
                if purpose == WorkPurpose::StoreUninstall {
                    self.store.uninstall_driver_authorized = false;
                }
                self.last_message = format!(
                    "{} {}: {}",
                    result.capability_status.label(),
                    result.status,
                    result.message
                );
            }
            ManagerEvent::Dispatched {
                purpose,
                completion: Err(err),
            } => {
                if purpose == WorkPurpose::StoreUninstall {
                    self.store.uninstall_driver_authorized = false;
                }
                self.last_message = err;
            }
        }
    }

    fn tick(&mut self) {
        self.drain_worker_events();
        let live = self.selected_operation().is_some_and(operation_is_live);
        if live
            && self.last_live_refresh.elapsed() >= Duration::from_secs(1)
            && !self.worker.is_busy()
        {
            self.last_live_refresh = Instant::now();
            let _ = self.worker.submit(ManagerTask::Refresh);
        }
    }

    #[cfg(test)]
    fn wait_for_worker(&mut self) {
        while self.worker.is_busy() {
            let Some(event) = self.worker.recv() else {
                break;
            };
            self.handle_worker_event(event);
        }
    }

    fn set_page(&mut self, page: TuiPage) {
        self.page = page;
    }

    fn next_page(&mut self) {
        let pages = TuiPage::all();
        let current = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        self.page = pages[(current + 1) % pages.len()];
    }

    /// 表格行选择始终作用于当前页正在显示的对象。
    fn move_selection(&mut self, delta: isize) {
        match self.page {
            TuiPage::Store => {
                let len = self.store.modules.len();
                self.store.selected = shift_index(self.store.selected, len, delta);
            }
            TuiPage::Core(OrchestratorViewPage::Endpoints) => {
                let len = self.view.endpoints.len();
                let index = shift_index(self.selected_endpoint_index, len, delta);
                self.selected_endpoint_index = index;
            }
            TuiPage::Core(OrchestratorViewPage::Services) => {
                let len = self.view.deployments.len();
                self.selected_deployment_index =
                    shift_index(self.selected_deployment_index, len, delta);
            }
            TuiPage::Core(OrchestratorViewPage::Links) => {
                self.selected_link_index =
                    shift_index(self.selected_link_index, self.view.links.len(), delta);
            }
            TuiPage::Core(OrchestratorViewPage::Operations) => {
                let indices = self.filtered_operation_indices();
                let current = indices
                    .iter()
                    .position(|index| *index == self.selected_operation_index)
                    .unwrap_or(0);
                let next = shift_index(current, indices.len(), delta);
                if let Some(index) = indices.get(next) {
                    self.selected_operation_index = *index;
                }
            }
            _ => {
                self.last_message = "当前页没有可选择的表格行".to_string();
            }
        }
    }

    /// `execute_service_driver` 是 dispatcher 在 `dispatch_planned_action` 里读的字段：
    /// 只有值为 "true" 时 OperationExecutor 才会真的调运行时驱动启停进程。
    /// 默认关闭，避免终端里误触就把服务拉起来/停掉。
    fn toggle_service_driver(&mut self) {
        self.execute_service_driver = !self.execute_service_driver;
        self.last_message = format!(
            "运行时驱动执行已{}（execute_service_driver）",
            driver_toggle_label(self.execute_service_driver)
        );
    }

    fn next_action(&mut self) {
        self.shift_action(1);
    }

    fn previous_action(&mut self) {
        self.shift_action(-1);
    }

    fn select_action(&mut self, action: &str) {
        match self.context.build_session(action) {
            Ok(session) => {
                self.set_session(session);
                self.selected_field_index = 0;
                self.last_message = format!("已选择 {action}");
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn shift_action(&mut self, delta: isize) {
        if self.view.operations.is_empty() {
            self.last_message = "没有可选择的 action".to_string();
            return;
        }
        let current = self
            .view
            .operations
            .iter()
            .position(|operation| operation.action == self.session.workbench.selected_action)
            .unwrap_or(0);
        let len = self.view.operations.len() as isize;
        let index = (current as isize + delta).rem_euclid(len) as usize;
        let action = self.view.operations[index].action.clone();
        match self.context.build_session(&action) {
            Ok(session) => {
                self.set_session(session);
                self.selected_field_index = 0;
                self.last_message = format!("已选择 {action}");
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn next_field(&mut self) {
        let fields = self.session.workbench.form_fields.len();
        if fields == 0 {
            self.last_message = "当前 action 没有表单字段".to_string();
        } else {
            self.selected_field_index = (self.selected_field_index + 1) % fields;
        }
    }

    fn cycle_selected_field(&mut self) {
        let Some(field) = self
            .session
            .workbench
            .form_fields
            .get(self.selected_field_index)
            .cloned()
        else {
            self.last_message = "当前 action 没有可编辑字段".to_string();
            return;
        };
        match self.context.cycle_field_value(&self.session, &field.name) {
            Ok(session) => {
                self.set_session(session);
                self.last_message = format!("已更新字段 {}", field.name);
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn update_field(&mut self, field: &str, value: String) {
        match self.context.update_field(&self.session, field, value) {
            Ok(session) => {
                self.set_session(session);
                self.last_message = format!("已更新字段 {field}");
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn begin_field_edit(&mut self) {
        let Some(field) = self
            .session
            .workbench
            .form_fields
            .get(self.selected_field_index)
        else {
            self.last_message = "当前 action 没有可编辑字段".to_string();
            return;
        };
        let value = self
            .session
            .workbench
            .request
            .fields
            .get(&field.name)
            .cloned()
            .unwrap_or_default();
        self.input_editor = Some(InputEditor::WorkbenchField {
            name: field.name.clone(),
            value,
        });
        self.last_message = "正在编辑字段：Enter 保存，Esc 取消".to_string();
    }

    fn begin_operation_filter_edit(&mut self) {
        self.input_editor = Some(InputEditor::OperationFilter {
            value: self.operation_filter.clone(),
        });
        self.last_message = "输入操作筛选条件：Enter 保存，Esc 取消".to_string();
    }

    fn handle_input_editor_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => {
                self.input_editor = None;
                self.last_message = "已取消编辑".to_string();
            }
            KeyCode::Enter => {
                let Some(editor) = self.input_editor.take() else {
                    return;
                };
                match editor {
                    InputEditor::WorkbenchField { name, value } => self.update_field(&name, value),
                    InputEditor::OperationFilter { value } => {
                        self.operation_filter = value;
                        if let Some(index) = self.filtered_operation_indices().first().copied() {
                            self.selected_operation_index = index;
                        }
                        self.last_message = "操作筛选已更新".to_string();
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(editor) = self.input_editor.as_mut() {
                    match editor {
                        InputEditor::WorkbenchField { value, .. }
                        | InputEditor::OperationFilter { value } => {
                            value.pop();
                        }
                    }
                }
            }
            KeyCode::Char(character) => {
                if let Some(editor) = self.input_editor.as_mut() {
                    match editor {
                        InputEditor::WorkbenchField { value, .. }
                        | InputEditor::OperationFilter { value } => value.push(character),
                    }
                }
            }
            _ => {}
        }
    }

    fn confirm_session(&mut self) {
        if self.page == TuiPage::Core(OrchestratorViewPage::Operations) {
            self.run_selected_operation_action("operation.confirm");
        } else {
            self.dispatch_current("operation.confirm");
        }
    }

    fn apply_session(&mut self) {
        if self.page == TuiPage::Core(OrchestratorViewPage::Operations) {
            self.run_selected_operation_action("operation.apply");
            return;
        }
        let action = self.session.workbench.selected_action.clone();
        self.dispatch_current(&action);
    }

    fn rollback_session(&mut self) {
        if self.page == TuiPage::Core(OrchestratorViewPage::Operations) {
            self.run_selected_operation_action("operation.rollback");
        } else {
            self.dispatch_current("operation.rollback");
        }
    }

    fn run_action(&mut self, action: &str) {
        self.run_action_with_fields(action, []);
    }

    fn run_action_with_fields<const N: usize>(
        &mut self,
        action: &str,
        fields: [(&str, String); N],
    ) {
        self.select_action(action);
        let mut request = self.session.workbench.request.clone();
        for (field, value) in fields {
            request.fields.insert(field.to_string(), value);
        }
        match self.context.build_session_from_request(&request) {
            Ok(session) => {
                self.set_session(session);
                self.last_message.clear();
            }
            Err(err) => {
                self.last_message = err.to_string();
                return;
            }
        }
        self.apply_session();
    }

    /// 对象快捷键只带当前对象的字段，不继承 workbench 预览里的示例路径或版本。
    fn run_action_with_exact_fields(
        &mut self,
        action: &str,
        fields: impl IntoIterator<Item = (String, String)>,
    ) {
        self.select_action(action);
        let mut request = self.session.workbench.request.clone();
        request.fields.clear();
        request.fields.extend(fields);
        match self.context.build_session_from_request(&request) {
            Ok(session) => {
                self.set_session(session);
                self.last_message.clear();
            }
            Err(err) => {
                self.last_message = err.to_string();
                return;
            }
        }
        self.apply_session();
    }

    /// 选中的 Endpoint 行；索引越界时退回第一行，保持旧行为。
    fn selected_endpoint_id(&self) -> Option<String> {
        self.view
            .endpoints
            .get(self.selected_endpoint_index)
            .or_else(|| self.view.endpoints.first())
            .map(|endpoint| endpoint.endpoint.clone())
    }

    /// 选中 Endpoint 所属的主机 IP（Endpoint = ip:port:service-name）。
    fn selected_host_ip(&self) -> Option<String> {
        let endpoint = self.selected_endpoint_id()?;
        parse_endpoint_id(&endpoint)
            .ok()
            .map(|identity| identity.host.to_string())
    }

    fn selected_deployment(&self) -> Option<&DeploymentViewRow> {
        self.view
            .deployments
            .get(self.selected_deployment_index)
            .or_else(|| self.view.deployments.first())
    }

    fn selected_deployment_host_ip(&self) -> Option<String> {
        self.selected_deployment()
            .map(|deployment| deployment.host_ip.clone())
            .filter(|host| !host.trim().is_empty())
    }

    fn selected_link(&self) -> Option<&orchestrator_legacy::LinkViewRow> {
        self.view
            .links
            .get(self.selected_link_index)
            .or_else(|| self.view.links.first())
    }

    fn filtered_operation_indices(&self) -> Vec<usize> {
        let keyword = self.operation_filter.trim().to_ascii_lowercase();
        self.view
            .operations
            .iter()
            .enumerate()
            .filter(|(_, operation)| {
                keyword.is_empty()
                    || operation
                        .operation_id
                        .to_ascii_lowercase()
                        .contains(&keyword)
                    || operation.action.to_ascii_lowercase().contains(&keyword)
                    || operation.target.to_ascii_lowercase().contains(&keyword)
                    || operation.status.to_ascii_lowercase().contains(&keyword)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_operation(&self) -> Option<&OperationViewRow> {
        let operation = self.view.operations.get(self.selected_operation_index);
        if operation.is_some_and(|operation| {
            self.filtered_operation_indices()
                .contains(&self.selected_operation_index)
                && !operation.operation_id.trim().is_empty()
        }) {
            operation
        } else {
            self.filtered_operation_indices()
                .first()
                .and_then(|index| self.view.operations.get(*index))
                .filter(|operation| !operation.operation_id.trim().is_empty())
        }
    }

    fn run_selected_operation_action(&mut self, action: &str) {
        match self.selected_operation_action_request(action) {
            Ok(request) => self.dispatch_request(request),
            Err(err) => self.last_message = err.to_string(),
        }
    }

    fn selected_operation_action_request(&self, action: &str) -> Result<ActionRequest> {
        let operation = self
            .selected_operation()
            .ok_or_else(|| anyhow::anyhow!("请先选择一条持久化 Operation 记录"))?;
        let allowed = match action {
            "operation.confirm" => operation.status == "PLANNED" && operation.requires_confirmation,
            "operation.apply" => {
                operation.status == "AWAITING_CONFIRMATION"
                    || (operation.status == "PLANNED" && !operation.requires_confirmation)
            }
            "operation.rollback" => {
                matches!(operation.status.as_str(), "SUCCEEDED" | "FAILED")
                    && operation.rollback_available
            }
            _ => false,
        };
        if !allowed {
            anyhow::bail!("{action} 不适用于当前状态 {}", operation.status);
        }
        if matches!(action, "operation.apply" | "operation.rollback")
            && operation_driver_required(operation)
            && !self.execute_service_driver
        {
            anyhow::bail!("请先按 w 授权执行运行时驱动");
        }
        let mut request = ActionRequest::new(
            format!("{}-tui", action.replace('.', "-")),
            action,
            [
                ("operation_id".to_string(), operation.operation_id.clone()),
                ("confirm".to_string(), "true".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        if matches!(action, "operation.apply" | "operation.rollback") && self.execute_service_driver
        {
            request
                .fields
                .insert("execute_service_driver".to_string(), "true".to_string());
        }
        Ok(request)
    }

    fn run_endpoint_action(&mut self, action: &str) {
        if let Some(endpoint) = self.selected_endpoint_id() {
            self.run_action_with_exact_fields(action, [("endpoint".to_string(), endpoint)]);
        } else {
            self.run_action(action);
        }
    }

    /// 主机启停：`host.start` / `host.stop` 需要 host_ip + confirm=true；
    /// 另外只有带上 `execute_service_driver=true` 时 dispatcher 才会真的执行驱动。
    fn run_host_action(&mut self, action: &str) {
        if !self.execute_service_driver {
            self.last_message = "主机启停需要先按 w 授权执行运行时驱动".to_string();
            return;
        }
        match self.host_action_fields() {
            Ok(fields) => self.run_action_with_exact_fields(action, fields),
            Err(err) => self.last_message = err.to_string(),
        }
    }

    fn host_action_fields(&self) -> Result<Vec<(String, String)>> {
        let host_ip = self
            .selected_deployment_host_ip()
            .ok_or_else(|| anyhow::anyhow!("没有可用于主机启停的部署记录"))?;
        Ok(vec![
            ("host_ip".to_string(), host_ip),
            ("confirm".to_string(), "true".to_string()),
            ("execute_service_driver".to_string(), "true".to_string()),
        ])
    }

    fn run_service_action(&mut self, action: &str) {
        if !self.execute_service_driver {
            self.last_message = "服务生命周期操作需要先按 w 授权执行运行时驱动".to_string();
            return;
        }
        match self.service_action_fields() {
            Ok(fields) => self.run_action_with_exact_fields(action, fields),
            Err(err) => self.last_message = err.to_string(),
        }
    }

    fn service_action_fields(&self) -> Result<Vec<(String, String)>> {
        let deployment = self
            .selected_deployment()
            .ok_or_else(|| anyhow::anyhow!("请先选择一条部署记录"))?;
        if deployment.endpoint.trim().is_empty() {
            anyhow::bail!(
                "{}@{} 没有登记 Endpoint，无法精确执行生命周期动作",
                deployment.service_id,
                deployment.host_ip
            );
        }
        Ok(vec![
            ("service_id".to_string(), deployment.service_id.clone()),
            ("host_ip".to_string(), deployment.host_ip.clone()),
            ("endpoint".to_string(), deployment.endpoint.clone()),
            ("version".to_string(), deployment.version.clone()),
            ("confirm".to_string(), "true".to_string()),
            ("execute_service_driver".to_string(), "true".to_string()),
        ])
    }

    fn run_link_action(&mut self, action: &str) {
        match self.link_action_fields() {
            Some(fields) => self.run_action_with_exact_fields(action, fields),
            None => self.run_action(action),
        }
    }

    fn link_action_fields(&self) -> Option<Vec<(String, String)>> {
        let link = self.selected_link()?;
        Some(vec![
            ("source_endpoint".to_string(), link.from.clone()),
            ("target_endpoint".to_string(), link.to.clone()),
        ])
    }

    fn run_operation_logs_view(&mut self) {
        let Some(operation_id) = self
            .selected_operation()
            .map(|operation| operation.operation_id.clone())
        else {
            self.last_message = "请先选择一条 Operation 记录".to_string();
            return;
        };
        self.selected_log_operation_id = operation_id.clone();
        self.page = TuiPage::Core(OrchestratorViewPage::Logs);
        self.last_message = format!("正在查看 Operation {operation_id} 的日志");
    }

    fn selected_release_target(&self) -> Option<(String, String)> {
        self.view
            .release_registry
            .get(self.selected_release_index)
            .filter(|record| record.record_type == "release")
            .map(|record| (record.service_name.clone(), record.version.clone()))
    }

    fn release_action_fields(&self, action: &str) -> Option<Vec<(String, String)>> {
        let (service_id, version) = self.selected_release_target()?;
        let mut fields = vec![("service_id".to_string(), service_id)];
        if matches!(action, "release.install" | "release.delete") && !version.is_empty() {
            fields.push(("version".to_string(), version));
        }
        Some(fields)
    }

    fn run_release_action(&mut self, action: &str) {
        if action == "release.rollback" && !self.execute_service_driver {
            self.last_message = format!("{action} 需要先按 w 授权执行运行时驱动");
            return;
        }
        match self.release_action_fields(action) {
            Some(fields) => self.run_action_with_exact_fields(action, fields),
            None => {
                self.last_message = "请先在 Service 页用上下键选择一条 Release 记录".to_string();
            }
        }
    }

    /// 已安装状态来自 HostService/Deployment 投影，不能把仓库 manifest 当成已部署实例。
    fn installed_version(&self, service_id: &str) -> Option<String> {
        self.store
            .installed
            .get(service_id)
            .map(|service| service.version.clone())
    }

    fn reload_store_index(&mut self) {
        self.queue_store_reload(true);
    }

    /// 打开「导入并安装」表单。`use_selected_module` 为 true 时用索引里选中的模块预填。
    fn open_store_form(&mut self, use_selected_module: bool) {
        let mut form = StoreInstallForm::default();
        if use_selected_module {
            let Some(module) = self.store.modules.get(self.store.selected).cloned() else {
                self.last_message = "商店索引为空，按 M 手动填写来源".to_string();
                return;
            };
            form.module_id = module.id.clone();
            form.repo = module.repo.clone();
            form.source_url = module.source_url.clone();
            form.checksum = module.checksum.clone();
        }
        let repo = form.repo.clone();
        self.store.form = Some(form);
        self.store.releases.clear();
        self.store.selected_release = 0;
        self.store.selected_asset = 0;
        self.page = TuiPage::Store;
        self.last_message =
            "Tab 切换字段，仓库字段按 Enter 获取 Releases，其余字段按 Enter 安装".to_string();
        if !repo.trim().is_empty() {
            self.queue_github_releases(repo);
        }
    }

    /// 表单输入模式：所有按键先交给表单，避免与全局快捷键冲突。
    fn handle_store_form_key(&mut self, code: KeyCode) {
        if self.store.form.is_none() {
            return;
        }
        match code {
            KeyCode::Esc => {
                self.store.form = None;
                self.last_message = "已取消导入安装".to_string();
            }
            KeyCode::Enter => {
                let repo = self
                    .store
                    .form
                    .as_ref()
                    .filter(|form| form.field == 0)
                    .map(|form| form.repo.trim().to_string())
                    .filter(|repo| !repo.is_empty());
                if let Some(repo) = repo {
                    self.queue_github_releases(repo);
                } else {
                    self.submit_store_form();
                }
            }
            KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
                if let Some(form) = self.store.form.as_mut() {
                    form.field = (form.field + 1) % 6;
                }
            }
            KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
                if let Some(form) = self.store.form.as_mut() {
                    form.field = (form.field + 5) % 6;
                }
            }
            KeyCode::PageUp => self.cycle_store_asset(-1),
            KeyCode::PageDown => self.cycle_store_asset(1),
            KeyCode::Char(' ') => {
                if let Some(form) = self.store.form.as_mut() {
                    match form.field {
                        4 => {
                            form.execute_service_driver = !form.execute_service_driver;
                            if form.execute_service_driver {
                                form.external_service_running = false;
                            }
                        }
                        5 => {
                            form.external_service_running = !form.external_service_running;
                            if form.external_service_running {
                                form.execute_service_driver = false;
                            }
                        }
                        _ => match form.field {
                            0 => form.repo.push(' '),
                            1 => form.source_url.push(' '),
                            2 => form.checksum.push(' '),
                            3 => form.host_ip.push(' '),
                            _ => {}
                        },
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = self.store.form.as_mut() {
                    match form.field {
                        0 => {
                            form.repo.pop();
                        }
                        1 => {
                            form.source_url.pop();
                        }
                        2 => {
                            form.checksum.pop();
                        }
                        3 => {
                            form.host_ip.pop();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char(value) => {
                if let Some(form) = self.store.form.as_mut() {
                    match form.field {
                        0 => form.repo.push(value),
                        1 => form.source_url.push(value),
                        2 => form.checksum.push(value),
                        3 => form.host_ip.push(value),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn cycle_store_asset(&mut self, delta: isize) {
        let assets = self
            .store
            .releases
            .iter()
            .enumerate()
            .flat_map(|(release_index, release)| {
                release
                    .assets
                    .iter()
                    .enumerate()
                    .map(move |(asset_index, _)| (release_index, asset_index))
            })
            .collect::<Vec<_>>();
        if assets.is_empty() {
            self.last_message = "当前没有可选择的 GitHub Release 资产".to_string();
            return;
        }
        let current = assets
            .iter()
            .position(|(release, asset)| {
                *release == self.store.selected_release && *asset == self.store.selected_asset
            })
            .unwrap_or(0);
        let (release, asset) = assets[shift_index(current, assets.len(), delta)];
        self.store.selected_release = release;
        self.store.selected_asset = asset;
        let source = self.store.selected_asset_url().map(str::to_string);
        if let Some(form) = self.store.form.as_mut()
            && let Some(source) = source
        {
            form.source_url = source;
        }
    }

    fn submit_store_form(&mut self) {
        let Some(form) = self.store.form.clone() else {
            return;
        };
        let request = match self.store_install_request(&form) {
            Ok(request) => request,
            Err(err) => {
                self.last_message = err.to_string();
                return;
            }
        };
        self.submit_task(ManagerTask::Install(request), "正在后台导入并安装…");
    }

    fn store_install_request(&self, form: &StoreInstallForm) -> Result<StoreInstallRequest> {
        let source_url = form.source_url.trim().to_string();
        if source_url.is_empty() {
            anyhow::bail!("请先填写 source_url");
        }
        if self.store.status.require_release_checksum && form.checksum.trim().is_empty() {
            anyhow::bail!("当前配置强制校验 release 包，请填写 sha256 校验和");
        }
        if form.execute_service_driver && form.external_service_running {
            anyhow::bail!("运行时驱动与“外部服务已在运行”不能同时启用");
        }
        Ok(StoreInstallRequest {
            source_url,
            checksum: form.checksum.trim().to_string(),
            host_ip: if form.host_ip.trim().is_empty() {
                "127.0.0.1".to_string()
            } else {
                form.host_ip.trim().to_string()
            },
            execute_service_driver: form.execute_service_driver,
            external_service_running: form.external_service_running,
            ..StoreInstallRequest::default()
        })
    }

    fn toggle_uninstall_driver(&mut self) {
        self.store.uninstall_driver_authorized = !self.store.uninstall_driver_authorized;
        self.last_message = format!(
            "卸载运行时驱动授权已{}",
            driver_toggle_label(self.store.uninstall_driver_authorized)
        );
    }

    fn uninstall_selected_store_module(&mut self) {
        let (service_id, request) = match self.store_uninstall_request() {
            Ok(request) => request,
            Err(err) => {
                self.last_message = err.to_string();
                return;
            }
        };
        if !self.submit_task(
            ManagerTask::Dispatch {
                request,
                purpose: WorkPurpose::StoreUninstall,
            },
            format!("正在后台卸载 {service_id}…"),
        ) {
            self.store.uninstall_driver_authorized = false;
        }
    }

    fn store_uninstall_request(&self) -> Result<(String, ActionRequest)> {
        let module = self
            .store
            .modules
            .get(self.store.selected)
            .ok_or_else(|| anyhow::anyhow!("请先选择一个商店模块"))?;
        let service_id = module.id.clone();
        if !self.store.installed.contains_key(&service_id) {
            anyhow::bail!("{service_id} 尚未部署");
        }
        if !self.store.uninstall_driver_authorized {
            anyhow::bail!("请先按 t 授权执行卸载运行时驱动");
        }
        let request = ActionRequest::new(
            "service-delete-tui",
            "service.delete",
            [
                ("service_id".to_string(), service_id.clone()),
                ("confirm".to_string(), "true".to_string()),
                ("execute_service_driver".to_string(), "true".to_string()),
            ]
            .into_iter()
            .collect(),
        );
        Ok((service_id, request))
    }

    fn dispatch_current(&mut self, action: &str) {
        let request = self.current_dispatch_request(action);
        self.dispatch_request(request);
    }

    fn current_dispatch_request(&self, action: &str) -> ActionRequest {
        let mut request = if action == "operation.confirm"
            || action == "operation.apply"
            || action == "operation.rollback"
        {
            ActionRequest::new(
                format!("{}-console", action.replace('.', "-")),
                action,
                [
                    (
                        "operation_id".to_string(),
                        self.session.current_operation.operation_id.clone(),
                    ),
                    ("confirm".to_string(), "true".to_string()),
                ]
                .into_iter()
                .collect(),
            )
        } else {
            let mut request = self.session.workbench.request.clone();
            request.operation_id = self.session.current_operation.operation_id.clone();
            if self.session.workbench.preview.requires_confirmation {
                request
                    .fields
                    .insert("confirm".to_string(), "true".to_string());
            }
            request
        };
        if self.execute_service_driver
            && matches!(
                action,
                "operation.apply"
                    | "operation.rollback"
                    | "release.install"
                    | "release.rollback"
                    | "host.start"
                    | "host.stop"
                    | "service.start"
                    | "service.stop"
                    | "service.restart"
                    | "service.delete"
            )
        {
            request
                .fields
                .insert("execute_service_driver".to_string(), "true".to_string());
        }
        request
    }

    /// 统一的 action 派发出口：core dispatcher 执行 + 刷新视图 + 回显能力状态。
    fn dispatch_request(&mut self, request: ActionRequest) {
        if self.worker.is_busy() {
            self.last_message = "已有后台管理任务正在执行，请稍候".to_string();
            return;
        }
        if request.field("execute_service_driver") == Some("true") {
            self.submit_task(
                ManagerTask::Dispatch {
                    request,
                    purpose: WorkPurpose::RuntimeAction,
                },
                "运行时动作正在后台执行…",
            );
            return;
        }
        let outcome = (|| -> Result<_> {
            let mut console = self
                .console
                .lock()
                .map_err(|_| anyhow::anyhow!("TUI 编排器状态锁已损坏"))?;
            let result = console.dispatch(request)?;
            let context = console.context()?;
            let view = console.view()?;
            let installed = installed_services_from_deployments(view.deployments.clone())?;
            Ok((result, context, view, installed))
        })();
        match outcome {
            Ok((result, context, view, installed)) => {
                let message = format!(
                    "{} {}: {}",
                    result.capability_status.label(),
                    result.status,
                    result.message
                );
                if let Err(err) = self.apply_core_state(context, view, installed) {
                    self.last_message = err.to_string();
                } else {
                    self.last_message = message;
                }
            }
            Err(err) => self.last_message = err.to_string(),
        }
    }

    fn set_session(&mut self, session: OperationWorkbenchSession) {
        merge_operation_workbench_session_into_view(&mut self.view, &session);
        self.session = session;
    }
}

fn shift_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = len as isize;
    (current as isize + delta).rem_euclid(len) as usize
}

fn bounded_index(current: usize, len: usize) -> usize {
    if len == 0 || current >= len {
        0
    } else {
        current
    }
}

fn runtime_driver_action(action: &str) -> bool {
    matches!(
        action,
        "release.install"
            | "release.rollback"
            | "host.start"
            | "host.stop"
            | "service.start"
            | "service.stop"
            | "service.restart"
            | "service.delete"
            | "service.enable"
            | "service.disable"
    )
}

fn operation_driver_required(operation: &OperationViewRow) -> bool {
    if !runtime_driver_action(&operation.action) {
        return false;
    }
    operation.action != "release.install" || operation.driver_authorized
}

fn operation_is_live(operation: &OperationViewRow) -> bool {
    matches!(
        operation.status.as_str(),
        "RUNNING" | "PLANNED" | "AWAITING_CONFIRMATION"
    )
}

fn driver_toggle_label(enabled: bool) -> &'static str {
    match enabled {
        true => "开启",
        false => "关闭",
    }
}

fn field_marker(selected: bool) -> &'static str {
    match selected {
        true => ">",
        false => " ",
    }
}

fn main() -> Result<()> {
    configure_utf8_console()?;
    let cli = Cli::parse();
    let api_url = cli
        .api_url
        .or_else(|| std::env::var("OJOS_ORCHESTRATOR_URL").ok());
    if let Some(api_url) = api_url {
        let mut config = api_client::ApiClientConfig::new(api_url)?;
        let issuer = cli
            .oidc_issuer
            .or_else(|| std::env::var("OJOS_TUI_OIDC_ISSUER").ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "remote TUI requires --oidc-issuer or OJOS_TUI_OIDC_ISSUER; bearer-token fallback is disabled"
                )
            })?;
        let client_id = cli
            .oidc_client_id
            .or_else(|| std::env::var("OJOS_TUI_OIDC_CLIENT_ID").ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("remote TUI requires --oidc-client-id or OJOS_TUI_OIDC_CLIENT_ID")
            })?;
        let mut oidc = device_auth::DeviceFlowConfig::new(issuer, client_id)?;
        oidc.scope = cli
            .oidc_scope
            .or_else(|| std::env::var("OJOS_TUI_OIDC_SCOPE").ok())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "openid profile".to_string());
        oidc.audience = cli
            .oidc_audience
            .or_else(|| std::env::var("OJOS_TUI_OIDC_AUDIENCE").ok())
            .filter(|value| !value.trim().is_empty());
        let access_token = device_auth::authenticate(&oidc, |prompt| {
            eprintln!("OIDC device authorization required.");
            eprintln!("Verification URI: {}", prompt.verification_uri);
            eprintln!("User code: {}", prompt.user_code);
            if let Some(uri) = prompt.verification_uri_complete.as_deref() {
                eprintln!("Direct verification URI: {uri}");
            }
            eprintln!(
                "Waiting for authorization (expires in {} seconds)...",
                prompt.expires_in.as_secs()
            );
        })?;
        config.bearer_token = Some(access_token);
        let client = api_client::ApiClient::connect(config);
        if let Some(command) = cli.command {
            println!(
                "{}",
                serde_json::to_string_pretty(&remote::execute_once(&client, &command)?)?
            );
            return Ok(());
        }
        return remote::run_remote(client);
    }
    if cli.command.is_some() {
        anyhow::bail!("--command requires --api-url or OJOS_ORCHESTRATOR_URL");
    }
    if !cli.legacy_local {
        anyhow::bail!(
            "the v1 TUI requires --api-url or OJOS_ORCHESTRATOR_URL; use --legacy-local only for the deprecated 0.2 compatibility console"
        );
    }
    let repo_root = fs::canonicalize(&cli.repo_root).unwrap_or(cli.repo_root);
    let app = App::new(repo_root)?;
    run(app)
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

fn run(mut app: App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        app.tick();
        terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if app.input_editor.is_some() {
                app.handle_input_editor_key(key.code);
                continue;
            }
            // 商店表单处于输入模式时按键全部归表单，避免和全局快捷键抢按键。
            if app.store.form.is_some() {
                app.handle_store_form_key(key.code);
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                KeyCode::Tab => app.next_page(),
                KeyCode::Char('r') => app.queue_refresh(),
                KeyCode::Char('n') => app.next_action(),
                KeyCode::Char('p') => app.previous_action(),
                KeyCode::Char('f') => app.next_field(),
                KeyCode::Char('v') => app.cycle_selected_field(),
                KeyCode::Char('V') => {
                    if let Some(field) = app
                        .session
                        .workbench
                        .form_fields
                        .get(app.selected_field_index)
                        .cloned()
                    {
                        app.update_field(&field.name, String::new());
                    }
                }
                KeyCode::Enter => app.begin_field_edit(),
                KeyCode::Char('/') => {
                    if app.page == TuiPage::Core(OrchestratorViewPage::Operations) {
                        app.begin_operation_filter_edit();
                    }
                }
                KeyCode::Char('c') => app.confirm_session(),
                KeyCode::Char('a') => app.apply_session(),
                KeyCode::Char('u') => app.rollback_session(),
                KeyCode::Char('i') => app.run_release_action("release.install"),
                KeyCode::Char('R') => app.run_release_action("release.create"),
                KeyCode::Char('U') => app.run_release_action("release.update"),
                KeyCode::Char('Y') => app.run_release_action("release.delete"),
                KeyCode::Char('B') => app.run_release_action("release.rollback"),
                KeyCode::Char('z') => app.run_release_action("release.validate"),
                KeyCode::Char('e') => app.run_action("endpoint.create"),
                KeyCode::Char('E') => app.run_endpoint_action("endpoint.update"),
                KeyCode::Char('x') => app.run_endpoint_action("endpoint.delete"),
                KeyCode::Char('h') => app.run_endpoint_action("endpoint.health.check"),
                KeyCode::Char('l') => app.run_action("link.create"),
                KeyCode::Char('L') => app.run_link_action("link.update"),
                KeyCode::Char('X') => app.run_link_action("link.delete"),
                KeyCode::Char('k') => app.run_link_action("link.enable"),
                KeyCode::Char('K') => app.run_link_action("link.disable"),
                KeyCode::Char('H') => app.run_link_action("link.health.check"),
                KeyCode::Char('o') => app.run_operation_logs_view(),
                KeyCode::Char('d') => app.run_action("diagnostic.create"),
                KeyCode::Char('D') => app.run_action_with_fields(
                    "diagnostic.export",
                    [("format", "markdown".to_string())],
                ),
                KeyCode::Up => app.move_selection(-1),
                KeyCode::Down => app.move_selection(1),
                KeyCode::Char('w') => app.toggle_service_driver(),
                KeyCode::Char('s') => app.run_host_action("host.start"),
                KeyCode::Char('S') => app.run_host_action("host.stop"),
                KeyCode::F(5) => app.run_service_action("service.start"),
                KeyCode::F(6) => app.run_service_action("service.stop"),
                KeyCode::F(7) => app.run_service_action("service.restart"),
                KeyCode::Char('g') => app.reload_store_index(),
                KeyCode::Char('m') => app.open_store_form(true),
                KeyCode::Char('M') => app.open_store_form(false),
                KeyCode::Char('t') => {
                    if app.page == TuiPage::Store {
                        app.toggle_uninstall_driver();
                    }
                }
                KeyCode::Delete => {
                    if app.page == TuiPage::Store {
                        app.uninstall_selected_store_module();
                    }
                }
                KeyCode::Char(value) => {
                    if let Some(page) = TuiPage::all()
                        .into_iter()
                        .find(|page| page.key() == Some(value))
                    {
                        app.set_page(page);
                    }
                }
                _ => {}
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(frame.area());

    draw_header(frame, app, layout[0]);
    draw_tabs(frame, app, layout[1]);
    match app.page {
        TuiPage::Core(page) => draw_core_page(frame, app, layout[2], page),
        TuiPage::Store => draw_store(frame, app, layout[2]),
    }
    draw_footer(frame, app, layout[3]);
}

fn draw_core_page(
    frame: &mut ratatui::Frame<'_>,
    app: &App,
    area: Rect,
    page: OrchestratorViewPage,
) {
    match page {
        OrchestratorViewPage::Overview => draw_overview(frame, app, area),
        OrchestratorViewPage::Services => draw_services(frame, app, area),
        OrchestratorViewPage::Templates => draw_templates(frame, app, area),
        OrchestratorViewPage::Endpoints => draw_endpoints(frame, app, area),
        OrchestratorViewPage::Links => draw_links(frame, app, area),
        OrchestratorViewPage::Operations => draw_operations(frame, app, area),
        OrchestratorViewPage::Topology => draw_topology(frame, app, area),
        OrchestratorViewPage::Logs => draw_logs(frame, app, area),
        OrchestratorViewPage::Diagnostics => draw_diagnostics(frame, app, area),
    }
}

fn draw_header(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let text = vec![Line::from(vec![
        Span::styled(
            " OJOS Orchestrator ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "Service、Template、Endpoint、Link、Operation、Topology、LogView、DiagnosticReport",
            Style::default().fg(Color::Gray),
        ),
        Span::raw("  "),
        Span::styled("Store", Style::default().fg(Color::Cyan)),
        Span::raw("  │  "),
        Span::styled(
            if app.last_message.is_empty() {
                "就绪"
            } else {
                app.last_message.as_str()
            },
            Style::default().fg(if app.worker.is_busy() {
                Color::Yellow
            } else {
                Color::White
            }),
        ),
    ])];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_tabs(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let titles = TuiPage::all()
        .iter()
        .map(|page| Line::from(page.title()))
        .collect::<Vec<_>>();
    let selected = TuiPage::all()
        .iter()
        .position(|page| *page == app.page)
        .unwrap_or(0);
    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("导航 1-9 / 0 / Tab"),
            ),
        area,
    );
}

fn draw_overview(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let lines = vec![
        Line::from(format!("Action: {}", app.view.schemas.action_count())),
        Line::from(format!("Form: {}", app.view.schemas.form_count())),
        Line::from(format!("Deployment: {}", app.view.deployments.len())),
        Line::from(format!("Service Manifest: {}", app.view.services.len())),
        Line::from(format!(
            "Release Registry: {}",
            app.view.release_registry.len()
        )),
        Line::from(format!("Template: {}", app.view.templates.len())),
        Line::from(format!("Endpoint: {}", app.view.endpoints.len())),
        Line::from(format!("Link: {}", app.view.links.len())),
        Line::from(format!("Operation Action: {}", app.view.operations.len())),
        Line::from(format!("LogView Source: {}", app.view.logs.len())),
        Line::from(format!("DiagnosticReport: {}", app.view.diagnostics.len())),
        Line::from(""),
        Line::from("Endpoint 使用 ip:port:service-name 作为运行时唯一身份。"),
        Line::from("Gateway 和 Web Shell 都只是被编排的 Service，不是 Orchestrator 控制面。"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("核心对象总览")),
        area,
    );
}

fn draw_services(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(7),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new("Service: F5 start  F6 stop  F7 restart · Host: s start  S stop · w driver")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Lifecycle Actions"),
            ),
        chunks[0],
    );
    let selected = app.selected_deployment();
    let hosts = app
        .view
        .deployments
        .iter()
        .map(|deployment| deployment.host_ip.as_str())
        .filter(|host| !host.is_empty())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("  ");
    let host_lines = vec![
        Line::from(format!(
            "选中部署: {}@{}  Endpoint: {}",
            selected.map(|row| row.service_id.as_str()).unwrap_or("—"),
            selected.map(|row| row.host_ip.as_str()).unwrap_or("—"),
            selected
                .map(|row| row.endpoint.as_str())
                .filter(|endpoint| !endpoint.is_empty())
                .unwrap_or("未登记")
        )),
        Line::from(format!(
            "全部部署主机: {hosts}  运行时驱动: {}  后台任务: {}",
            driver_toggle_label(app.execute_service_driver),
            if app.worker.is_busy() {
                "运行中"
            } else {
                "空闲"
            }
        )),
    ];
    frame.render_widget(
        Paragraph::new(host_lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Host")),
        chunks[1],
    );

    let rows = app
        .view
        .deployments
        .iter()
        .enumerate()
        .map(|(index, deployment)| {
            let style = if index == app.selected_deployment_index {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(deployment.service_id.clone()),
                Cell::from(deployment.host_ip.clone()),
                Cell::from(deployment.version.clone()),
                Cell::from(deployment.kind.clone()),
                Cell::from(deployment.runtime.clone()),
                Cell::from(deployment.status.clone()),
                Cell::from(if deployment.endpoint.is_empty() {
                    "未登记".to_string()
                } else {
                    deployment.endpoint.clone()
                }),
                Cell::from(deployment.endpoint_health.clone()),
                Cell::from(deployment.protocol.clone()),
                Cell::from(deployment.health_path.clone()),
            ])
            .style(style)
        });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(15),
                Constraint::Length(8),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(14),
                Constraint::Length(24),
                Constraint::Length(12),
                Constraint::Length(9),
                Constraint::Min(14),
            ],
        )
        .header(
            Row::new(vec![
                "Service",
                "Host",
                "Version",
                "Kind",
                "Runtime",
                "Status",
                "Endpoint",
                "Health",
                "Protocol",
                "Health Path",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("Deployments")),
        chunks[2],
    );

    let registry_rows = app
        .view
        .release_registry
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let mut style = Style::default();
            if index == app.selected_release_index {
                style = style.add_modifier(Modifier::REVERSED);
            }
            Row::new(vec![
                Cell::from(record.service_name.clone()),
                Cell::from(record.version.clone()),
                Cell::from(record.record_type.clone()),
                Cell::from(record.name.clone()),
                Cell::from(record.detail.clone()),
                Cell::from(record.source.clone()),
            ])
            .style(style)
        });
    frame.render_widget(
        Table::new(
            registry_rows,
            [
                Constraint::Length(18),
                Constraint::Length(8),
                Constraint::Length(12),
                Constraint::Length(24),
                Constraint::Min(24),
                Constraint::Length(12),
            ],
        )
        .header(
            Row::new(vec![
                "Service", "Version", "Record", "Name", "Detail", "Source",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Service Release Registry"),
        ),
        chunks[3],
    );
}

fn draw_templates(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Deployment templates are readonly").block(
            Block::default()
                .borders(Borders::ALL)
                .title("Deployment Templates"),
        ),
        chunks[0],
    );
    let rows = app.view.templates.iter().map(|set| {
        Row::new(vec![
            Cell::from(set.id.clone()),
            Cell::from(set.name.clone()),
            Cell::from(set.services.clone()),
            Cell::from(set.links.clone()),
            Cell::from(set.scope.clone()),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(22),
                Constraint::Length(18),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Min(16),
            ],
        )
        .header(
            Row::new(vec!["ID", "Name", "Service", "Link", "Scope"])
                .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("Template")),
        chunks[1],
    );
}

fn draw_endpoints(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(4),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new("Endpoint Actions: e create  E update  x delete  h health check").block(
            Block::default()
                .borders(Borders::ALL)
                .title("Endpoint Actions"),
        ),
        chunks[0],
    );
    draw_host_panel(frame, app, chunks[1]);
    let mut rows = Vec::new();
    for (index, endpoint) in app.view.endpoints.iter().enumerate() {
        let mut style = Style::default();
        if index == app.selected_endpoint_index {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let row = Row::new(vec![
            Cell::from(endpoint.endpoint.clone()),
            Cell::from(endpoint.service_id.clone()),
            Cell::from(endpoint.protocol.clone()),
            Cell::from(endpoint.expose.clone()),
            Cell::from(endpoint.source.clone()),
        ]);
        rows.push(row.style(style));
    }
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(17),
                Constraint::Length(18),
                Constraint::Length(10),
                Constraint::Length(8),
                Constraint::Min(18),
            ],
        )
        .header(
            Row::new(vec!["Endpoint", "Service", "协议", "暴露", "来源"])
                .style(Style::default().fg(Color::Yellow)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Endpoint = ip:port:service-name"),
        ),
        chunks[2],
    );
}

/// 主机视图：选中 Endpoint 决定 host.start / host.stop 的目标主机。
fn draw_host_panel(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let endpoint = app.selected_endpoint_id().unwrap_or_default();
    let host_ip = app.selected_host_ip().unwrap_or_default();
    let hosts = endpoint_hosts(&app.view.endpoints).join("  ");
    let lines = vec![
        Line::from("Host Actions: s host start  S host stop  w execute_service_driver"),
        Line::from(format!("选中 Endpoint: {endpoint}    目标 Host: {host_ip}")),
        Line::from(format!(
            "全部 Host: {hosts}    运行时驱动: {}",
            driver_toggle_label(app.execute_service_driver)
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Host")),
        area,
    );
}

fn draw_links(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);
    frame.render_widget(
        Paragraph::new(
            "Link Actions: l create  L update  X delete  H health check  k enable  K disable",
        )
        .block(Block::default().borders(Borders::ALL).title("Link Actions")),
        chunks[0],
    );
    let rows = app.view.links.iter().enumerate().map(|(index, link)| {
        let style = if index == app.selected_link_index {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(link.from.clone()),
            Cell::from(link.to.clone()),
            Cell::from(link.protocol.clone()),
            Cell::from(link.auth_mode.clone()),
            Cell::from(link.scope.clone()),
            Cell::from(link.enabled.clone()),
            Cell::from(link.source.clone()),
        ])
        .style(style)
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(18),
                Constraint::Length(10),
                Constraint::Length(14),
                Constraint::Length(16),
                Constraint::Length(10),
                Constraint::Min(18),
            ],
        )
        .header(
            Row::new(vec![
                "Source", "Target", "协议", "认证", "范围", "启停", "来源",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("Link")),
        chunks[1],
    );
}

fn draw_operations(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(10),
            Constraint::Min(8),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(format!(
            "c confirm  a apply  u rollback  o logs  / filter [{}]  w driver [{}]",
            if app.operation_filter.is_empty() {
                "all"
            } else {
                app.operation_filter.as_str()
            },
            driver_toggle_label(app.execute_service_driver)
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Operation Actions"),
        ),
        chunks[0],
    );
    draw_operation_workbench(frame, app, chunks[1]);

    let rows = app.filtered_operation_indices().into_iter().map(|index| {
        let operation = &app.view.operations[index];
        let style = if index == app.selected_operation_index {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(operation.operation_id.clone()),
            Cell::from(operation.action.clone()),
            Cell::from(operation.target.clone()),
            Cell::from(operation.status.clone()),
            Cell::from(operation.risk.clone()),
            Cell::from(operation.log_count.to_string()),
            Cell::from(operation.updated_at.clone()),
        ])
        .style(style)
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(24),
                Constraint::Length(28),
                Constraint::Length(22),
                Constraint::Length(16),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Min(18),
            ],
        )
        .header(
            Row::new(vec![
                "Operation",
                "Action",
                "目标",
                "状态",
                "风险",
                "日志",
                "Updated",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Operation Records"),
        ),
        chunks[2],
    );
}

fn draw_operation_workbench(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let workbench = OperationWorkbenchView::from_session(&app.session);
    let selected_field = workbench
        .editable_fields
        .get(app.selected_field_index)
        .map(|field| field.name.as_str())
        .unwrap_or("无");
    let selected = app.selected_operation();
    let mut lines = vec![
        Line::from(format!(
            "选中 Operation: {}  Action: {}  Status: {}",
            selected
                .map(|operation| operation.operation_id.as_str())
                .unwrap_or("—"),
            selected
                .map(|operation| operation.action.as_str())
                .unwrap_or("—"),
            selected
                .map(|operation| operation.status.as_str())
                .unwrap_or("—")
        )),
        Line::from(format!(
            "目标: {}  摘要: {}",
            selected
                .map(|operation| operation.target.as_str())
                .unwrap_or("—"),
            selected
                .map(|operation| operation.summary.as_str())
                .filter(|summary| !summary.is_empty())
                .unwrap_or("—")
        )),
        Line::from(format!(
            "需确认: {}  可回滚: {}  驱动已记录授权: {}  错误: {}",
            selected.is_some_and(|operation| operation.requires_confirmation),
            selected.is_some_and(|operation| operation.rollback_available),
            selected.is_some_and(|operation| operation.driver_authorized),
            selected
                .map(|operation| operation.error.as_str())
                .filter(|error| !error.is_empty())
                .unwrap_or("—")
        )),
        Line::from(format!(
            "Action 工作台: {}  当前字段: {selected_field}  Enter 输入任意文本，v 循环值",
            workbench.selected_action
        )),
        Line::from(format!(
            "表单: {}",
            workbench
                .editable_fields
                .iter()
                .enumerate()
                .map(|(index, field)| format!(
                    "{}{}{}={}",
                    if index == app.selected_field_index {
                        "["
                    } else {
                        ""
                    },
                    field.name,
                    if index == app.selected_field_index {
                        "]"
                    } else {
                        ""
                    },
                    field.value
                ))
                .collect::<Vec<_>>()
                .join("  ")
        )),
    ];
    if let Some(operation) = selected {
        let operation_logs = app
            .view
            .logs
            .iter()
            .filter(|log| log.operation_id == operation.operation_id)
            .rev()
            .take(3)
            .collect::<Vec<_>>();
        for log in operation_logs.into_iter().rev() {
            lines.push(Line::from(format!("[{}] {}", log.level, log.message)));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Operation 工作台"),
        ),
        area,
    );
}

fn draw_topology(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mut lines = vec![
        Line::from(
            "Topology 由 Service、Endpoint、Link、Operation、LogView、DiagnosticReport 组成；Template 只用于本地预览。",
        ),
        Line::from(""),
        Line::from("Endpoint host 分组:"),
    ];
    for host in endpoint_hosts(&app.view.endpoints) {
        lines.push(Line::from(format!("  - {}", host)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Link:"));
    for link in &app.view.links {
        lines.push(Line::from(format!(
            "  - {} -> {} ({})",
            link.from, link.to, link.protocol
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Topology")),
        area,
    );
}

fn draw_logs(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);
    let filter = if app.selected_log_operation_id.is_empty() {
        "全部日志".to_string()
    } else {
        format!(
            "Operation {}（运行中每秒自动刷新）",
            app.selected_log_operation_id
        )
    };
    frame.render_widget(
        Paragraph::new(filter).block(Block::default().borders(Borders::ALL).title("日志筛选")),
        chunks[0],
    );
    let rows = app
        .view
        .logs
        .iter()
        .filter(|log| {
            app.selected_log_operation_id.is_empty()
                || log.operation_id == app.selected_log_operation_id
        })
        .map(|log| {
            Row::new(vec![
                Cell::from(log.source_id.clone()),
                Cell::from(log.service_id.clone()),
                Cell::from(log.endpoint.clone()),
                Cell::from(log.operation_id.clone()),
                Cell::from(log.level.clone()),
                Cell::from(log.message.clone()),
                Cell::from(log.path.clone()),
            ])
        });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(24),
                Constraint::Length(18),
                Constraint::Length(17),
                Constraint::Length(24),
                Constraint::Length(8),
                Constraint::Min(24),
                Constraint::Min(16),
            ],
        )
        .header(
            Row::new(vec![
                "Source",
                "Service",
                "Endpoint",
                "Operation",
                "级别",
                "消息",
                "位置",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("LogView")),
        chunks[1],
    );
}

fn draw_diagnostics(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Diagnostics: d run  D export markdown")
            .block(Block::default().borders(Borders::ALL).title("Diagnostics")),
        chunks[0],
    );
    let rows = app.view.diagnostics.iter().map(|diagnostic| {
        Row::new(vec![
            Cell::from(diagnostic.target.clone()),
            Cell::from(diagnostic.status.clone()),
            Cell::from(diagnostic.summary.clone()),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(24),
                Constraint::Length(12),
                Constraint::Min(30),
            ],
        )
        .header(Row::new(vec!["目标", "状态", "摘要"]).style(Style::default().fg(Color::Yellow)))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("DiagnosticReport"),
        ),
        chunks[1],
    );
}

/// 插件商店页：索引来自 `OJOS_STORE_INDEX_URL`（默认仓库内 store/index.json）。
/// 只读本地索引文件；远程索引由 daemon / Web UI 负责拉取。
fn draw_store(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Min(5),
            Constraint::Length(12),
        ])
        .split(area);
    let header = vec![
        Line::from("Store: m install  M manual  g reload  t authorize uninstall  Delete uninstall"),
        Line::from("Up/Down 选择模块；表单内 Tab 切字段，PageUp/PageDown 切 GitHub 资产"),
        Line::from(format!(
            "索引 {}  模块 {}  cache={}  后台任务={}",
            app.store.index_url,
            app.store.modules.len(),
            app.store.cached,
            if app.worker.is_busy() {
                "运行中"
            } else {
                "空闲"
            }
        )),
        Line::from(format!(
            "包加载={}  GitHub Token={}  校验和={}  卸载授权={}",
            app.store.status.package_load_enabled,
            app.store.status.github_token_configured,
            if app.store.status.require_release_checksum {
                "强制"
            } else {
                "可选"
            },
            driver_toggle_label(app.store.uninstall_driver_authorized)
        )),
    ];
    frame.render_widget(
        Paragraph::new(header)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("插件商店")),
        chunks[0],
    );

    let mut rows = Vec::new();
    for (index, module) in app.store.modules.iter().enumerate() {
        let mut style = Style::default();
        if index == app.store.selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let row = Row::new(vec![
            Cell::from(module.id.clone()),
            Cell::from(module.name.clone()),
            Cell::from(module.kind.clone()),
            Cell::from(module.tags.join(",")),
            Cell::from(module.source().to_string()),
            Cell::from(store_installed_label(app, &module.id)),
        ]);
        rows.push(row.style(style));
    }
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(18),
                Constraint::Length(15),
                Constraint::Length(18),
                Constraint::Min(20),
                Constraint::Length(14),
            ],
        )
        .header(
            Row::new(vec!["ID", "名称", "类型", "标签", "来源", "安装状态"])
                .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("商店模块")),
        chunks[1],
    );

    frame.render_widget(
        Paragraph::new(store_detail_lines(app))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("模块详情")),
        chunks[2],
    );
}

fn store_installed_label(app: &App, service_id: &str) -> String {
    match app.installed_version(service_id) {
        Some(version) => format!("已安装 v{version}"),
        None => "未安装".to_string(),
    }
}

fn store_detail_lines(app: &App) -> Vec<Line<'static>> {
    if let Some(form) = &app.store.form {
        return store_form_lines(form);
    }
    let Some(module) = app.store.modules.get(app.store.selected) else {
        let mut lines = vec![
            Line::from("索引里没有模块。"),
            Line::from("按 M 手动填写 release 包来源后导入安装。"),
        ];
        if !app.store.message.is_empty() {
            lines.push(Line::from(app.store.message.clone()));
        }
        return lines;
    };
    let mut lines = vec![
        Line::from(format!("{} ({})", module.name, module.id)),
        Line::from(module.description.clone()),
        Line::from(format!(
            "类型 {}    标签 {}",
            module.kind,
            module.tags.join(",")
        )),
        Line::from(format!("来源 {}", module.source())),
        Line::from(format!("状态 {}", store_installed_label(app, &module.id))),
        Line::from("m 用该来源安装；远程索引、GitHub Release 与直链均由共享管理层处理。"),
    ];
    if let Some(installed) = app.store.installed.get(&module.id) {
        lines.push(Line::from(format!(
            "部署 {} 个：{}",
            installed.deployments.len(),
            installed
                .deployments
                .iter()
                .map(|deployment| format!(
                    "{}@{} {}",
                    deployment.version, deployment.host_ip, deployment.status
                ))
                .collect::<Vec<_>>()
                .join("；")
        )));
    }
    if !app.store.message.is_empty() {
        lines.push(Line::from(app.store.message.clone()));
    }
    lines
}

fn store_form_lines(form: &StoreInstallForm) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("导入并安装：仓库字段 Enter 获取 Releases；其余字段 Enter 提交；Esc 取消"),
        Line::from(format!(
            "{} repo       {}",
            field_marker(form.field == 0),
            form.repo
        )),
        Line::from(format!(
            "{} source_url {}",
            field_marker(form.field == 1),
            form.source_url
        )),
        Line::from(format!(
            "{} checksum   {}",
            field_marker(form.field == 2),
            form.checksum
        )),
        Line::from(format!(
            "{} host_ip    {}",
            field_marker(form.field == 3),
            form.host_ip
        )),
        Line::from(format!(
            "{} execute_service_driver={}   {} external_service_running={}",
            field_marker(form.field == 4),
            form.execute_service_driver,
            field_marker(form.field == 5),
            form.external_service_running
        )),
        Line::from("布尔字段按 Space 切换且互斥；source_url 支持本地包、直链和 GitHub 资产。"),
    ];
    if !form.module_id.is_empty() {
        lines.push(Line::from(format!("来自索引模块 {}", form.module_id)));
    }
    lines
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let text = match &app.input_editor {
        Some(InputEditor::WorkbenchField { name, value }) => {
            format!("编辑字段 {name}> {value}_  · Enter 保存 / Esc 取消")
        }
        Some(InputEditor::OperationFilter { value }) => {
            format!("操作筛选> {value}_  · Enter 保存 / Esc 取消")
        }
        None => "q/Esc quit  r refresh  Tab/1-9 pages  Enter edit field  / operation filter  F5/F6/F7 service start-stop-restart  s/S host start-stop  w driver  e/E/x/h endpoint  l/L/X/H/k/K link  c/a/u/o operation  0 store  m/M install  g index  t+Delete uninstall  Up/Down select".to_string(),
    };
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::ReleaseRegistryViewRow;
    use std::collections::BTreeMap;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf()
    }

    fn tui_source() -> String {
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
            .expect("TUI source should be readable as UTF-8")
    }

    fn deployment(service_id: &str, host_ip: &str, endpoint: &str) -> DeploymentViewRow {
        DeploymentViewRow {
            service_id: service_id.to_string(),
            name: service_id.to_string(),
            version: "1.0.0".to_string(),
            kind: "backend-api".to_string(),
            runtime: "local-process".to_string(),
            host_ip: host_ip.to_string(),
            status: "RUNNING".to_string(),
            endpoint: endpoint.to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            endpoint_health: "healthy".to_string(),
            reachable: true,
            endpoint_count: usize::from(!endpoint.is_empty()),
            endpoints: (!endpoint.is_empty())
                .then(|| endpoint.to_string())
                .into_iter()
                .collect(),
        }
    }

    fn operation(id: &str, action: &str, status: &str) -> OperationViewRow {
        OperationViewRow {
            operation_id: id.to_string(),
            action: action.to_string(),
            target: "Service demo".to_string(),
            status: status.to_string(),
            risk: "MEDIUM".to_string(),
            plan_required: String::new(),
            mode: "store".to_string(),
            requires_confirmation: false,
            rollback_available: false,
            driver_authorized: false,
            fields: String::new(),
            preview_target: "demo".to_string(),
            preview_steps: String::new(),
            preview_confirmation: String::new(),
            result: String::new(),
            error: String::new(),
            log_count: 0,
            summary: "test operation".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn tui_pages_cover_the_same_core_objects_as_web() {
        let titles = OrchestratorViewPage::all()
            .iter()
            .map(|page| page.title())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec![
                "总览",
                "Service",
                "Template",
                "Endpoint",
                "Link",
                "Operation",
                "Topology",
                "LogView",
                "DiagnosticReport",
            ]
        );
    }

    #[test]
    fn tui_loads_shared_orchestrator_view_from_core() {
        let app = App::new_memory(repo_root()).expect("TUI app should load orchestrator/core view");
        assert!(!app.view.services.is_empty());
        assert!(
            app.view.templates.is_empty(),
            "console store view must not expose deployment templates as formal objects"
        );
        assert!(!app.view.endpoints.is_empty());
        assert!(!app.view.links.is_empty());
        assert!(app.view.schemas.action_count() > 0);
        assert!(
            {
                let workbench = OperationWorkbenchView::from_session(&app.session);
                workbench.selected_action == "release.install"
                    && workbench.fields.contains("service_id*")
                    && workbench.editable_fields.iter().any(|field| {
                        field.name == "service_id" && field.required && field.value == "gateway"
                    })
                    && workbench.current_status == "Planned"
                    && workbench.result_status.is_empty()
                    && workbench.log_count == 0
                    && !workbench.preview_steps.is_empty()
            },
            "TUI should load the same shared operation workbench as Web"
        );
        assert!(app.view.logs.is_empty());
        assert!(app.view.diagnostics.is_empty());
    }

    #[test]
    fn tui_exposes_dispatcher_backed_actions() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.next_action();
        assert_ne!(app.session.workbench.selected_action, "");
        app.selected_field_index = app
            .session
            .workbench
            .form_fields
            .iter()
            .position(|field| field.name == "service_id")
            .unwrap_or(0);
        app.cycle_selected_field();
        assert!(app.session.workbench.request.field("service_id").is_some());

        app = App::new_memory(repo_root()).expect("TUI app should reload");
        app.select_action("endpoint.create");
        app.update_field("endpoint", "127.0.0.1:19002:gateway".to_string());
        app.update_field("service_id", "gateway".to_string());
        app.update_field("protocol", "http".to_string());
        app.apply_session();
        let endpoint_operation_id = app.session.current_operation.operation_id.clone();
        assert!(
            app.view
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19002:gateway"),
            "TUI action console should write Endpoint into Store-backed view"
        );
        let expected_log_source = format!("operation:{endpoint_operation_id}");
        app.run_action_with_fields("log.query", [("operation_id", endpoint_operation_id)]);
        assert!(
            app.view
                .logs
                .iter()
                .any(|log| log.source_id == expected_log_source),
            "TUI LogView should expose log.query as the LogView layer"
        );
        // service.enable/disable 仍未接通真实执行链，必须继续显式回显 UNSUPPORTED。
        app.select_action("service.enable");
        app.apply_session();
        assert!(
            app.last_message.contains("UNSUPPORTED"),
            "TUI must not report unsupported lifecycle actions as success: {}",
            app.last_message
        );

        // service.start 已接入 core 的 runtime pipeline：这里没有可用驱动，
        // 结果必须是 RUNTIME_PIPELINE + FAILED，而不是 UNSUPPORTED，也不是假成功。
        app.select_action("service.start");
        app.apply_session();
        assert!(
            app.last_message.contains("RUNTIME_PIPELINE"),
            "service.start 必须经 core dispatcher 的 runtime pipeline: {}",
            app.last_message
        );
        assert!(
            app.last_message.contains("FAILED"),
            "驱动未开启真实执行时 service.start 必须失败而不是假成功: {}",
            app.last_message
        );
    }

    #[test]
    fn tui_endpoint_action_menu_exists() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("TUI source should be readable as UTF-8");
        assert!(source.contains("Endpoint Actions: e create  E update  x delete  h health check"));

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.run_action_with_fields(
            "endpoint.create",
            [
                ("endpoint", "127.0.0.1:19301:gateway".to_string()),
                ("service_id", "gateway".to_string()),
                ("protocol", "http".to_string()),
            ],
        );
        assert!(
            app.view
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19301:gateway")
        );
        app.run_endpoint_action("endpoint.health.check");
        assert!(app.last_message.contains("REAL"));
    }

    #[test]
    fn tui_link_action_menu_exists() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("TUI source should be readable as UTF-8");
        assert!(source.contains("Link Actions: l create  L update  X delete  H health check"));

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        for (endpoint, service_id) in [
            ("127.0.0.1:19310:gateway", "gateway"),
            ("127.0.0.1:19311:auth-service", "auth-service"),
        ] {
            app.run_action_with_fields(
                "endpoint.create",
                [
                    ("endpoint", endpoint.to_string()),
                    ("service_id", service_id.to_string()),
                    ("protocol", "http".to_string()),
                ],
            );
        }
        app.run_action_with_fields(
            "link.create",
            [
                ("source_endpoint", "127.0.0.1:19310:gateway".to_string()),
                (
                    "target_endpoint",
                    "127.0.0.1:19311:auth-service".to_string(),
                ),
            ],
        );
        assert!(
            app.view
                .links
                .iter()
                .any(|link| link.from == "127.0.0.1:19310:gateway"
                    && link.to == "127.0.0.1:19311:auth-service")
        );
        app.run_link_action("link.health.check");
        assert!(app.last_message.contains("REAL"));

        for (action, expected) in [("link.disable", "disabled"), ("link.enable", "enabled")] {
            app.run_action_with_fields(
                action,
                [
                    ("source_endpoint", "127.0.0.1:19310:gateway".to_string()),
                    (
                        "target_endpoint",
                        "127.0.0.1:19311:auth-service".to_string(),
                    ),
                ],
            );
            assert!(
                app.view
                    .links
                    .iter()
                    .any(|link| link.from == "127.0.0.1:19310:gateway"
                        && link.to == "127.0.0.1:19311:auth-service"
                        && link.enabled == expected),
                "{action} 之后 TUI Link 视图应显示 {expected}"
            );
        }
    }

    #[test]
    fn tui_set_templates_are_readonly() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("TUI source should be readable as UTF-8");
        assert!(source.contains("Deployment templates are readonly"));

        let repo_view =
            orchestrator_legacy::load_orchestrator_view_with_database_url(&repo_root(), None)
                .expect("repo view");
        assert!(!repo_view.templates.is_empty());

        let app = App::new_memory(repo_root()).expect("TUI app should load");
        assert!(app.view.templates.is_empty());
        assert!(
            app.view
                .operations
                .iter()
                .all(|operation| !operation.action.starts_with("set."))
        );
    }

    #[test]
    fn tui_diagnostics_action_exists() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("TUI source should be readable as UTF-8");
        assert!(source.contains("Diagnostics: d run  D export markdown"));

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.run_action("diagnostic.create");
        assert!(
            app.view
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.target.contains("Topology"))
        );
        app.run_action_with_fields("diagnostic.export", [("format", "markdown".to_string())]);
        assert!(app.last_message.contains("STORE_BACKED"));
    }

    #[test]
    fn tui_action_feedback_shows_capability_status() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        // host.create 目前仍是 Unsupported，用它守住能力状态回显这条链路。
        app.run_action("host.create");
        assert!(
            app.last_message.contains("UNSUPPORTED"),
            "{}",
            app.last_message
        );

        // 已接通的动作要回显各自真实的能力状态，不能一律标成 UNSUPPORTED。
        app.run_action("service.stop");
        assert!(
            app.last_message.contains("RUNTIME_PIPELINE"),
            "{}",
            app.last_message
        );
    }

    #[test]
    fn tui_source_keeps_utf8_chinese_text_without_mojibake() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("TUI source should be readable as UTF-8");
        assert!(
            source.contains("原生 TUI")
                && source.contains("核心对象总览")
                && source.contains("q/Esc quit")
                && source.contains("Endpoint Actions"),
            "TUI user-facing text must remain readable UTF-8 Chinese"
        );
        assert_no_mojibake(&source);
    }

    #[test]
    fn tui_adds_store_tab_on_top_of_core_pages() {
        let pages = TuiPage::all();
        assert_eq!(pages.len(), OrchestratorViewPage::all().len() + 1);
        let titles = pages.iter().map(|page| page.title()).collect::<Vec<_>>();
        assert_eq!(titles.last().copied(), Some("商店 Store"));
        assert_eq!(TuiPage::Store.key(), Some('0'));
        assert_eq!(
            TuiPage::Core(OrchestratorViewPage::Overview).title(),
            OrchestratorViewPage::Overview.title()
        );

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        assert_eq!(app.page, TuiPage::Core(OrchestratorViewPage::Overview));
        for _ in 0..pages.len() {
            app.next_page();
        }
        assert_eq!(app.page, TuiPage::Core(OrchestratorViewPage::Overview));
        app.set_page(TuiPage::Store);
        assert_eq!(app.page, TuiPage::Store);
    }

    #[test]
    fn tui_key_bindings_do_not_collide() {
        let source = tui_source();
        let handlers = source
            .rsplit_once("#[cfg(test)]")
            .map(|(head, _)| head.to_string())
            .expect("TUI source should contain a test module");
        let mut keys = Vec::new();
        for fragment in handlers.split("KeyCode::Char('").skip(1) {
            let Some((literal, _)) = fragment.split_once('\'') else {
                continue;
            };
            if literal.chars().count() != 1 {
                continue;
            }
            let Some(key) = literal.chars().next() else {
                continue;
            };
            keys.push(key);
        }
        assert!(keys.contains(&'k'), "既有 link.enable 按键必须保留");
        assert!(keys.contains(&'K'), "既有 link.disable 按键必须保留");
        for key in ['w', 's', 'S', 'g', 'm', 'M', 't', '/'] {
            assert!(keys.contains(&key), "新增按键 {key} 未接入事件处理");
        }
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(keys.len(), unique.len(), "action 快捷键不能重复: {keys:?}");
        for page in TuiPage::all() {
            let Some(key) = page.key() else {
                continue;
            };
            assert!(!keys.contains(&key), "页签键 {key} 与 action 快捷键冲突");
        }
    }

    #[test]
    fn tui_store_page_lists_local_index_modules() {
        let source = tui_source();
        assert!(source.contains("Store: m install  M manual  g reload"));

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        assert_eq!(app.store.index_url, "store/index.json");
        assert!(
            !app.store.modules.is_empty(),
            "TUI 应能读出仓库内商店索引: {}",
            app.store.message
        );
        assert!(
            app.store
                .modules
                .iter()
                .any(|module| module.id == "gateway")
        );
        assert!(app.installed_version("gateway").is_none());
        assert!(
            app.store.installed.is_empty(),
            "仓库 manifest 不能被误标为已经部署"
        );
        assert!(app.installed_version("not-installed-module").is_none());

        app.reload_store_index();
        assert!(app.worker.is_busy(), "远程/本地索引刷新都应离开渲染线程");
        app.run_action("host.create");
        assert!(
            app.last_message.contains("后台管理任务正在执行"),
            "输入处理不能等待后台任务持有的 console 锁"
        );
        app.wait_for_worker();
        assert!(!app.worker.is_busy());
        assert!(app.store.message.is_empty());
    }

    #[test]
    fn tui_store_form_collects_source_url_and_checksum() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.open_store_form(false);
        assert_eq!(app.page, TuiPage::Store);
        app.handle_store_form_key(KeyCode::Tab);
        for value in ['s', 'v', 'c'] {
            app.handle_store_form_key(KeyCode::Char(value));
        }
        app.handle_store_form_key(KeyCode::Backspace);
        app.handle_store_form_key(KeyCode::Tab);
        app.handle_store_form_key(KeyCode::Char('a'));
        app.handle_store_form_key(KeyCode::Tab);
        app.handle_store_form_key(KeyCode::Tab);
        app.handle_store_form_key(KeyCode::Char(' '));
        app.handle_store_form_key(KeyCode::Tab);
        app.handle_store_form_key(KeyCode::Char(' '));
        let form = app.store.form.clone().expect("store form should stay open");
        assert_eq!(form.source_url, "sv");
        assert_eq!(form.checksum, "a");
        assert!(!form.execute_service_driver);
        assert!(form.external_service_running);
        app.handle_store_form_key(KeyCode::Esc);
        assert!(app.store.form.is_none());

        app.open_store_form(false);
        app.submit_store_form();
        assert_eq!(app.last_message, "请先填写 source_url");
        assert!(
            app.store.form.is_some(),
            "source_url 为空时表单应保持打开等待补填"
        );
    }

    #[test]
    fn tui_store_install_request_matches_web_options_and_rejects_conflicts() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.store.selected = app
            .store
            .modules
            .iter()
            .position(|module| module.id == "gateway")
            .expect("store index should contain gateway");
        app.open_store_form(true);
        let form = app.store.form.clone().expect("store form should open");
        assert_eq!(form.module_id, "gateway");
        assert_eq!(form.source_url, "services/gateway");
        let mut configured = form.clone();
        configured.host_ip = "10.0.0.8".to_string();
        configured.external_service_running = true;
        let request = app
            .store_install_request(&configured)
            .expect("valid Store request");
        assert_eq!(request.source_url, "services/gateway");
        assert_eq!(request.host_ip, "10.0.0.8");
        assert!(request.external_service_running);
        assert!(!request.execute_service_driver);

        configured.execute_service_driver = true;
        assert!(app.store_install_request(&configured).is_err());
    }

    #[test]
    fn tui_service_and_host_lifecycle_target_selected_deployment_exactly() {
        let source = tui_source();
        assert!(source.contains("Service: F5 start  F6 stop  F7 restart"));

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.view.deployments = vec![
            deployment("gateway", "10.0.0.1", "10.0.0.1:8080:gateway"),
            deployment("auth-service", "10.0.0.2", "10.0.0.2:8081:auth-service"),
        ];
        app.set_page(TuiPage::Core(OrchestratorViewPage::Services));
        app.move_selection(1);
        assert_eq!(app.selected_deployment_index, 1);
        let service = app
            .service_action_fields()
            .expect("selected service action fields")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            service.get("service_id").map(String::as_str),
            Some("auth-service")
        );
        assert_eq!(service.get("host_ip").map(String::as_str), Some("10.0.0.2"));
        assert_eq!(
            service.get("endpoint").map(String::as_str),
            Some("10.0.0.2:8081:auth-service")
        );
        assert_eq!(service.get("version").map(String::as_str), Some("1.0.0"));
        assert_eq!(
            service.get("execute_service_driver").map(String::as_str),
            Some("true")
        );

        let host = app
            .host_action_fields()
            .expect("selected host fields")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(host.get("host_ip").map(String::as_str), Some("10.0.0.2"));
        assert_eq!(host.get("confirm").map(String::as_str), Some("true"));
    }

    #[test]
    fn tui_link_actions_target_the_selected_row_instead_of_the_first() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.view.links = vec![
            orchestrator_legacy::LinkViewRow {
                from: "127.0.0.1:8000:first".to_string(),
                to: "127.0.0.1:8001:first-target".to_string(),
                protocol: "http".to_string(),
                auth_mode: "none".to_string(),
                scope: "local".to_string(),
                enabled: "enabled".to_string(),
                source: "store".to_string(),
            },
            orchestrator_legacy::LinkViewRow {
                from: "10.0.0.2:9000:selected".to_string(),
                to: "10.0.0.3:9001:selected-target".to_string(),
                protocol: "grpc".to_string(),
                auth_mode: "token".to_string(),
                scope: "cluster".to_string(),
                enabled: "enabled".to_string(),
                source: "store".to_string(),
            },
        ];
        app.page = TuiPage::Core(OrchestratorViewPage::Links);
        app.move_selection(1);
        let fields = app
            .link_action_fields()
            .expect("selected link fields")
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            fields.get("source_endpoint").map(String::as_str),
            Some("10.0.0.2:9000:selected")
        );
        assert_eq!(
            fields.get("target_endpoint").map(String::as_str),
            Some("10.0.0.3:9001:selected-target")
        );
    }

    #[test]
    fn tui_operation_actions_use_selected_record_and_web_gating() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        let mut confirm = operation("op-confirm", "endpoint.create", "PLANNED");
        confirm.requires_confirmation = true;
        let apply = operation("op-apply", "service.stop", "AWAITING_CONFIRMATION");
        app.view.operations = vec![confirm, apply];
        app.page = TuiPage::Core(OrchestratorViewPage::Operations);
        app.selected_operation_index = 0;

        let request = app
            .selected_operation_action_request("operation.confirm")
            .expect("confirm should be allowed");
        assert_eq!(request.field("operation_id"), Some("op-confirm"));
        assert!(
            app.selected_operation_action_request("operation.apply")
                .is_err(),
            "confirmation-required PLANNED operation cannot be applied directly"
        );

        app.selected_operation_index = 1;
        assert!(
            app.selected_operation_action_request("operation.apply")
                .is_err(),
            "runtime action requires an explicit driver authorization"
        );
        app.execute_service_driver = true;
        let request = app
            .selected_operation_action_request("operation.apply")
            .expect("authorized apply should be allowed");
        assert_eq!(request.field("operation_id"), Some("op-apply"));
        assert_eq!(request.field("execute_service_driver"), Some("true"));

        app.view.operations[1].status = "SUCCEEDED".to_string();
        app.view.operations[1].rollback_available = true;
        let rollback = app
            .selected_operation_action_request("operation.rollback")
            .expect("available rollback should be allowed");
        assert_eq!(rollback.field("operation_id"), Some("op-apply"));

        app.view.logs = vec![
            orchestrator_legacy::LogViewRow {
                source_id: "operation:op-confirm".to_string(),
                service_id: String::new(),
                endpoint: String::new(),
                operation_id: "op-confirm".to_string(),
                level: "info".to_string(),
                message: "first".to_string(),
                path: "operation".to_string(),
            },
            orchestrator_legacy::LogViewRow {
                source_id: "operation:op-apply".to_string(),
                service_id: String::new(),
                endpoint: String::new(),
                operation_id: "op-apply".to_string(),
                level: "info".to_string(),
                message: "selected".to_string(),
                path: "operation".to_string(),
            },
        ];
        app.run_operation_logs_view();
        assert_eq!(app.selected_log_operation_id, "op-apply");
        assert_eq!(app.page, TuiPage::Core(OrchestratorViewPage::Logs));
    }

    #[test]
    fn tui_text_editor_accepts_arbitrary_field_values_and_operation_filters() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.select_action("endpoint.create");
        app.selected_field_index = app
            .session
            .workbench
            .form_fields
            .iter()
            .position(|field| field.name == "protocol")
            .expect("protocol field");
        app.update_field("protocol", String::new());
        app.begin_field_edit();
        for character in "custom+proto".chars() {
            app.handle_input_editor_key(KeyCode::Char(character));
        }
        app.handle_input_editor_key(KeyCode::Enter);
        assert_eq!(
            app.session.workbench.request.field("protocol"),
            Some("custom+proto")
        );

        app.view.operations = vec![
            operation("op-start", "service.start", "SUCCEEDED"),
            operation("op-stop", "service.stop", "FAILED"),
        ];
        app.begin_operation_filter_edit();
        for character in "stop".chars() {
            app.handle_input_editor_key(KeyCode::Char(character));
        }
        app.handle_input_editor_key(KeyCode::Enter);
        assert_eq!(app.filtered_operation_indices(), vec![1]);
        assert_eq!(app.selected_operation_index, 1);
    }

    #[test]
    fn tui_store_github_asset_selection_and_uninstall_request_are_exact() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.open_store_form(false);
        app.store.releases = vec![orchestrator_manager::GithubReleaseView {
            tag_name: "v1.2.3".to_string(),
            name: "Release".to_string(),
            prerelease: false,
            published_at: String::new(),
            html_url: "https://github.com/owner/repo/releases/v1.2.3".to_string(),
            assets: vec![
                orchestrator_manager::GithubAssetView {
                    name: "first.zip".to_string(),
                    size: 1,
                    browser_download_url: "https://example.com/first.zip".to_string(),
                    content_type: "application/zip".to_string(),
                },
                orchestrator_manager::GithubAssetView {
                    name: "second.zip".to_string(),
                    size: 2,
                    browser_download_url: "https://example.com/second.zip".to_string(),
                    content_type: "application/zip".to_string(),
                },
            ],
        }];
        app.cycle_store_asset(1);
        assert_eq!(
            app.store.form.as_ref().map(|form| form.source_url.as_str()),
            Some("https://example.com/second.zip")
        );

        let gateway = app
            .store
            .modules
            .iter()
            .position(|module| module.id == "gateway")
            .expect("gateway module");
        app.store.selected = gateway;
        app.store.installed.insert(
            "gateway".to_string(),
            InstalledServiceView {
                version: "1.0.0".to_string(),
                versions: vec!["1.0.0".to_string()],
                kind: "gateway".to_string(),
                deployments: Vec::new(),
            },
        );
        assert!(app.store_uninstall_request().is_err());
        app.store.uninstall_driver_authorized = true;
        let (service_id, request) = app
            .store_uninstall_request()
            .expect("authorized uninstall request");
        assert_eq!(service_id, "gateway");
        assert_eq!(request.action, "service.delete");
        assert_eq!(request.field("service_id"), Some("gateway"));
        assert_eq!(request.field("confirm"), Some("true"));
        assert_eq!(request.field("execute_service_driver"), Some("true"));
    }

    #[test]
    fn tui_renders_store_services_and_operations_at_compact_and_wide_sizes() {
        use ratatui::backend::TestBackend;

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.view.deployments = vec![deployment("gateway", "127.0.0.1", "127.0.0.1:8080:gateway")];
        app.view.operations = vec![operation("op-render", "service.start", "PLANNED")];
        for (width, height) in [(80, 24), (160, 48)] {
            for page in [
                TuiPage::Core(OrchestratorViewPage::Services),
                TuiPage::Core(OrchestratorViewPage::Operations),
                TuiPage::Store,
            ] {
                app.page = page;
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend).expect("test terminal");
                terminal
                    .draw(|frame| draw(frame, &app))
                    .expect("TUI should render without layout panic");
                let screen = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(screen.contains("OJOS"));
            }
        }
    }

    #[test]
    fn tui_operation_apply_and_rollback_forward_driver_authorization() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.execute_service_driver = true;

        for action in [
            "operation.apply",
            "operation.rollback",
            "release.install",
            "release.rollback",
            "host.start",
            "host.stop",
            "service.start",
            "service.stop",
            "service.restart",
            "service.delete",
        ] {
            let request = app.current_dispatch_request(action);
            assert_eq!(
                request.field("execute_service_driver"),
                Some("true"),
                "{action} must retain the explicit runtime-driver authorization"
            );
        }
        assert_eq!(
            app.current_dispatch_request("operation.confirm")
                .field("execute_service_driver"),
            None,
            "confirmation alone must not authorize process execution"
        );
        assert_eq!(
            app.current_dispatch_request("release.delete")
                .field("execute_service_driver"),
            None,
            "release.delete only removes unreferenced release records"
        );
    }

    #[test]
    fn tui_release_shortcuts_do_not_reuse_preview_paths_or_versions() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.view.release_registry.insert(
            0,
            ReleaseRegistryViewRow {
                service_name: "custom-service".to_string(),
                version: "9.4.2".to_string(),
                record_type: "release".to_string(),
                name: "local://custom-service".to_string(),
                detail: "local-process".to_string(),
                source: "store".to_string(),
            },
        );
        app.selected_release_index = 0;

        for action in [
            "release.create",
            "release.update",
            "release.validate",
            "release.rollback",
        ] {
            let fields = app
                .release_action_fields(action)
                .expect("selected release fields");
            assert_eq!(
                fields,
                vec![("service_id".to_string(), "custom-service".to_string())],
                "{action} must not inherit gateway paths or the 0.1.0 preview version"
            );
        }

        for action in ["release.install", "release.delete"] {
            let fields = app
                .release_action_fields(action)
                .expect("selected release fields")
                .into_iter()
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                fields.get("service_id").map(String::as_str),
                Some("custom-service")
            );
            assert_eq!(fields.get("version").map(String::as_str), Some("9.4.2"));
            assert!(!fields.contains_key("release_url"));
            assert!(!fields.contains_key("service_yaml_path"));
        }
    }

    #[test]
    fn tui_release_shortcuts_require_the_selected_release_row() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.view.release_registry.insert(
            0,
            ReleaseRegistryViewRow {
                service_name: "not-a-release".to_string(),
                version: "1.0.0".to_string(),
                record_type: "route".to_string(),
                name: "route record".to_string(),
                detail: String::new(),
                source: "store".to_string(),
            },
        );
        app.view.release_registry.insert(
            1,
            ReleaseRegistryViewRow {
                service_name: "selected-release".to_string(),
                version: "2.0.0".to_string(),
                record_type: "release".to_string(),
                name: "local://selected-release".to_string(),
                detail: String::new(),
                source: "store".to_string(),
            },
        );
        app.selected_release_index = 0;
        assert!(app.release_action_fields("release.delete").is_none());
        app.run_release_action("release.delete");
        assert!(app.last_message.contains("选择一条 Release 记录"));

        app.selected_release_index = 1;
        assert_eq!(app.selected_release_index, 1);
        assert!(app.release_action_fields("release.install").is_some());
    }

    #[test]
    fn tui_footer_documents_new_store_and_host_keys() {
        let source = tui_source();
        assert!(source.contains("q/Esc quit  r refresh  Tab/1-9 pages"));
        assert!(source.contains("F5/F6/F7 service start-stop-restart"));
        assert!(source.contains("t+Delete uninstall"));
        assert!(source.contains("导航 1-9 / 0 / Tab"));
    }

    fn assert_no_mojibake(source: &str) {
        for marker in mojibake_markers() {
            assert!(
                !source.contains(&marker),
                "TUI source contains mojibake marker {marker}"
            );
        }
    }

    fn mojibake_markers() -> Vec<String> {
        [
            &[0xfffd][..],
            &[0x00ef, 0x00bf, 0x00bd],
            &[0x00c3],
            &[0x00c2],
            &[0x00e2, 0x20ac],
            &[0x00e2, 0x20ac, 0x2122],
            &[0x00e2, 0x20ac, 0x0153],
            &[0x00e2, 0x20ac, 0x009d],
            &[0x00e4, 0x00b8],
            &[0x9358, 0x71ba],
            &[0x7035, 0x7845],
            &[0x935a, 0x5d87],
            &[0x9417, 0x581f],
            &[0x7eeb, 0x8bf2],
            &[0x9357, 0x5fda],
            &[0x93c6, 0x64ae],
            &[0x93c9, 0x30e6],
            &[0x7481, 0x3088],
            &[0x9418, 0x8235],
            &[0x690b, 0x5ea8],
            &[0x59af, 0x2033],
            &[0x701b, 0x6941],
            &[0x68f0, 0x52ee],
            &[0x7ead, 0xe1bf],
            &[0x7f01, 0x64b4],
            &[0x95bf, 0x6b12],
            &[0x93c3, 0x30e5],
            &[0x93bd, 0x6a3f],
            &[0x5bf0, 0x546e],
            &[0x95c7, 0x20ac],
            &[0x741b, 0x3125],
            &[0x8930, 0x64b3],
            &[0x5a11, 0x581f],
            &[0x6d63, 0x5d87],
            &[0x95ab, 0x20ac],
            &[0x9352, 0x950b],
            &[0x93b5, 0x0446],
            &[0x9365, 0x70b4],
            &[0x9286],
        ]
        .into_iter()
        .map(|codes| {
            codes
                .iter()
                .filter_map(|code| char::from_u32(*code))
                .collect::<String>()
        })
        .collect()
    }
}
