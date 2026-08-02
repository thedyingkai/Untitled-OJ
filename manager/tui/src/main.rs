use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use orchestrator_core::{
    ActionRequest, OperationWorkbenchContext, OperationWorkbenchSession, OperationWorkbenchView,
    OrchestratorActionConsole, OrchestratorView, OrchestratorViewPage, default_console_request,
    endpoint_hosts, merge_operation_workbench_session_into_view, parse_endpoint_id,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ojos-orchestrator-tui")]
#[command(about = "OJOS Orchestrator 原生 TUI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
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

/// 商店索引里的一个模块条目。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StoreModuleRow {
    id: String,
    name: String,
    description: String,
    kind: String,
    tags: String,
    repo: String,
    source_url: String,
    checksum: String,
}

impl StoreModuleRow {
    fn source(&self) -> &str {
        if self.source_url.trim().is_empty() {
            self.repo.as_str()
        } else {
            self.source_url.as_str()
        }
    }
}

/// 「导入并安装」表单：复用 action 表单的字段光标 + 逐键输入模式。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StoreInstallForm {
    module_id: String,
    source_url: String,
    checksum: String,
    field: usize,
}

/// 商店页状态。索引只读本地文件；远程索引留给 daemon / Web UI。
#[derive(Debug, Clone, Default)]
struct StorePane {
    index_url: String,
    modules: Vec<StoreModuleRow>,
    message: String,
    selected: usize,
    form: Option<StoreInstallForm>,
}

impl StorePane {
    fn load(repo_root: &Path) -> Self {
        let mut pane = Self::default();
        pane.reload(repo_root);
        pane
    }

    fn reload(&mut self, repo_root: &Path) {
        self.index_url = configured_store_index_url();
        self.form = None;
        self.modules = Vec::new();
        if store_index_is_remote(&self.index_url) {
            self.selected = 0;
            self.message = "请通过 daemon/Web UI 使用远程索引".to_string();
            return;
        }
        match load_store_index(repo_root, &self.index_url) {
            Ok(modules) => {
                self.modules = modules;
                self.message = String::new();
            }
            Err(err) => {
                self.message = format!("读取商店索引失败: {err}");
            }
        }
        if self.selected >= self.modules.len() {
            self.selected = 0;
        }
    }
}

struct App {
    page: TuiPage,
    repo_root: PathBuf,
    console: OrchestratorActionConsole,
    context: OperationWorkbenchContext,
    session: OperationWorkbenchSession,
    selected_field_index: usize,
    selected_endpoint_index: usize,
    selected_release_index: usize,
    execute_service_driver: bool,
    store: StorePane,
    view: OrchestratorView,
    last_message: String,
}

impl App {
    fn new(repo_root: PathBuf) -> Result<Self> {
        let console = OrchestratorActionConsole::load(repo_root.clone())?;
        Self::from_console(console, repo_root)
    }

    #[cfg(test)]
    fn new_memory(repo_root: PathBuf) -> Result<Self> {
        let console = OrchestratorActionConsole::load_with_database_url(repo_root.clone(), None)?;
        Self::from_console(console, repo_root)
    }

    fn from_console(console: OrchestratorActionConsole, repo_root: PathBuf) -> Result<Self> {
        let context = console.context()?;
        let session = context.build_session("release.install")?;
        let view = console.view()?;
        let selected_release_index = view
            .release_registry
            .iter()
            .position(|record| record.record_type == "release")
            .unwrap_or(0);
        let store = StorePane::load(&repo_root);
        Ok(Self {
            page: TuiPage::Core(OrchestratorViewPage::Overview),
            repo_root,
            console,
            context,
            session,
            selected_field_index: 0,
            selected_endpoint_index: 0,
            selected_release_index,
            execute_service_driver: false,
            store,
            view,
            last_message: String::new(),
        })
    }

    fn refresh(&mut self) -> Result<()> {
        let context = self.console.context()?;
        let action = self.session.workbench.selected_action.clone();
        let session = context
            .build_session_from_request(&self.session.workbench.request)
            .or_else(|_| context.build_session(&action))
            .or_else(|_| context.build_session("release.install"))?;
        self.context = context;
        self.session = session;
        self.view = self.console.view()?;
        if self.selected_release_index >= self.view.release_registry.len() {
            self.selected_release_index = self
                .view
                .release_registry
                .iter()
                .position(|record| record.record_type == "release")
                .unwrap_or(0);
        }
        self.last_message = "已刷新".to_string();
        Ok(())
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

    /// 表格行选择：商店页选模块，Service 页选 Release，Endpoint 页选 Endpoint。
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
                let len = self.view.release_registry.len();
                self.selected_release_index = shift_index(self.selected_release_index, len, delta);
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

    fn confirm_session(&mut self) {
        self.dispatch_current("operation.confirm");
    }

    fn apply_session(&mut self) {
        let action = self.session.workbench.selected_action.clone();
        self.dispatch_current(&action);
    }

    fn rollback_session(&mut self) {
        self.dispatch_current("operation.rollback");
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

    fn run_endpoint_action(&mut self, action: &str) {
        if let Some(endpoint) = self.selected_endpoint_id() {
            self.run_action_with_fields(action, [("endpoint", endpoint)]);
        } else {
            self.run_action(action);
        }
    }

    /// 主机启停：`host.start` / `host.stop` 需要 host_ip + confirm=true；
    /// 另外只有带上 `execute_service_driver=true` 时 dispatcher 才会真的执行驱动。
    fn run_host_action(&mut self, action: &str) {
        let Some(host_ip) = self.selected_host_ip() else {
            self.last_message = "没有可用于主机启停的 Endpoint".to_string();
            return;
        };
        if self.execute_service_driver {
            self.run_action_with_fields(
                action,
                [
                    ("host_ip", host_ip),
                    ("confirm", "true".to_string()),
                    ("execute_service_driver", "true".to_string()),
                ],
            );
        } else {
            self.run_action_with_fields(
                action,
                [("host_ip", host_ip), ("confirm", "true".to_string())],
            );
        }
    }

    fn run_link_action(&mut self, action: &str) {
        if let Some(link) = self.view.links.first().cloned() {
            self.run_action_with_fields(
                action,
                [("source_endpoint", link.from), ("target_endpoint", link.to)],
            );
        } else {
            self.run_action(action);
        }
    }

    fn run_operation_logs_view(&mut self) {
        let operation_id = self
            .view
            .operations
            .iter()
            .find(|operation| operation.status != "CATALOG")
            .map(|operation| operation.operation_id.clone())
            .unwrap_or_else(|| self.session.current_operation.operation_id.clone());
        self.run_action_with_fields("log.query", [("operation_id", operation_id)]);
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

    /// 某个模块 id 是否已经作为 Service 存在于当前视图里。
    fn installed_version(&self, service_id: &str) -> Option<String> {
        self.view
            .services
            .iter()
            .find(|service| service.id == service_id)
            .map(|service| service.version.clone())
    }

    fn reload_store_index(&mut self) {
        let repo_root = self.repo_root.clone();
        self.store.reload(&repo_root);
        if self.store.message.is_empty() {
            self.last_message = format!("商店索引已刷新，共 {} 个模块", self.store.modules.len());
        } else {
            self.last_message = self.store.message.clone();
        }
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
            form.source_url = module.source_url.clone();
            form.checksum = module.checksum.clone();
        }
        self.store.form = Some(form);
        self.page = TuiPage::Store;
        self.last_message = "Tab 切换字段  Enter 导入并安装  Esc 取消".to_string();
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
            KeyCode::Enter => self.submit_store_form(),
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
                if let Some(form) = self.store.form.as_mut() {
                    form.field = usize::from(form.field == 0);
                }
            }
            KeyCode::Backspace => {
                if let Some(form) = self.store.form.as_mut() {
                    if form.field == 0 {
                        form.source_url.pop();
                    } else {
                        form.checksum.pop();
                    }
                }
            }
            KeyCode::Char(value) => {
                if let Some(form) = self.store.form.as_mut() {
                    if form.field == 0 {
                        form.source_url.push(value);
                    } else {
                        form.checksum.push(value);
                    }
                }
            }
            _ => {}
        }
    }

    fn submit_store_form(&mut self) {
        let Some(form) = self.store.form.clone() else {
            return;
        };
        let source_url = form.source_url.trim().to_string();
        if source_url.is_empty() {
            self.last_message = "请先填写 source_url".to_string();
            return;
        }
        let checksum = form.checksum.trim().to_string();
        self.store.form = None;
        self.install_release_from_source(&source_url, &checksum);
    }

    /// 商店安装两步走：先 `import_external_release` 把外部 release 包注册成
    /// Service + Release 契约，再派发 `release.install` 走正式 action 通道。
    fn install_release_from_source(&mut self, source_url: &str, checksum: &str) {
        let repo_root = self.repo_root.clone();
        let expected = Some(checksum.trim()).filter(|value| !value.is_empty());
        let outcome = self
            .console
            .import_external_release(&repo_root, source_url, expected);
        let imported = match outcome {
            Ok(imported) => imported,
            Err(err) => {
                self.last_message = format!("导入 release 失败: {err}");
                return;
            }
        };
        let service_id = imported.service.id.clone();
        let mut request = match default_console_request("release.install") {
            Ok(request) => request,
            Err(err) => {
                self.last_message = format!("release.install 表单缺失: {err}");
                return;
            }
        };
        request
            .fields
            .insert("service_id".to_string(), service_id.clone());
        request
            .fields
            .insert("confirm".to_string(), "true".to_string());
        if self.execute_service_driver {
            request
                .fields
                .insert("execute_service_driver".to_string(), "true".to_string());
        }
        let prefix = match imported.replaced_existing {
            true => format!("已更新 {service_id}"),
            false => format!("已导入 {service_id}"),
        };
        self.dispatch_request(request);
        let message = format!("{prefix} · {}", self.last_message);
        self.last_message = message;
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
        match self.console.dispatch(request) {
            Ok(result) => {
                let message = format!(
                    "{} {}: {}",
                    result.capability_status.label(),
                    result.status,
                    result.message
                );
                if let Err(err) = self.refresh() {
                    self.last_message = err.to_string();
                } else {
                    self.last_message = message;
                }
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
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

const DEFAULT_STORE_INDEX_PATH: &str = "store/index.json";

/// 与 daemon 的 `market_api` 同名开关；TUI 只支持本地相对路径形态。
fn configured_store_index_url() -> String {
    std::env::var("OJOS_STORE_INDEX_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_STORE_INDEX_PATH.to_string())
}

fn store_index_is_remote(index_url: &str) -> bool {
    let lowered = index_url.trim().to_ascii_lowercase();
    lowered.starts_with("http://") || lowered.starts_with("https://")
}

fn store_index_path(repo_root: &Path, index_url: &str) -> Result<PathBuf> {
    let trimmed = index_url.trim();
    let relative = trimmed
        .strip_prefix("file://")
        .unwrap_or(trimmed)
        .trim_start_matches('/');
    let path = Path::new(relative);
    let escapes = path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    });
    if path.is_absolute() || escapes {
        anyhow::bail!("商店索引路径必须位于仓库内: {trimmed}");
    }
    Ok(repo_root.join(path))
}

fn load_store_index(repo_root: &Path, index_url: &str) -> Result<Vec<StoreModuleRow>> {
    let path = store_index_path(repo_root, index_url)?;
    let Ok(text) = fs::read_to_string(&path) else {
        anyhow::bail!("无法读取 {}", path.display());
    };
    store_modules_from_index(&text)
}

fn store_modules_from_index(text: &str) -> Result<Vec<StoreModuleRow>> {
    let document = JsonParser::parse(text)?;
    let Some(entries) = document.get("modules").and_then(Json::as_array) else {
        anyhow::bail!("索引缺少 modules 数组");
    };
    let mut rows = Vec::new();
    for entry in entries {
        let id = entry.string_field("id");
        if id.trim().is_empty() {
            continue;
        }
        let mut name = entry.string_field("name");
        if name.trim().is_empty() {
            name = id.clone();
        }
        rows.push(StoreModuleRow {
            id,
            name,
            description: entry.string_field("description"),
            kind: entry.string_field("kind"),
            tags: entry.string_list("tags").join(","),
            repo: entry.string_field("repo"),
            source_url: entry.string_field("source_url"),
            checksum: entry.string_field("checksum"),
        });
    }
    Ok(rows)
}

/// 最小 JSON 数据模型。TUI 的依赖被限制为 anyhow/clap/orchestrator-core/ratatui/crossterm，
/// 拿不到 serde_json，所以商店索引在这里自带一个只读解析器。
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries
                .iter()
                .find(|(name, _)| name.as_str() == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    fn string_field(&self, key: &str) -> String {
        self.get(key)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn string_list(&self, key: &str) -> Vec<String> {
        let Some(items) = self.get(key).and_then(Json::as_array) else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_string)
            .collect()
    }
}

struct JsonParser {
    chars: Vec<char>,
    index: usize,
}

impl JsonParser {
    fn parse(text: &str) -> Result<Json> {
        let mut parser = Self {
            chars: text.chars().collect(),
            index: 0,
        };
        let value = parser.parse_value()?;
        parser.skip_whitespace();
        if parser.index != parser.chars.len() {
            anyhow::bail!("JSON 尾部有多余内容");
        }
        Ok(value)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let value = self.peek();
        if value.is_some() {
            self.index += 1;
        }
        value
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.index += 1;
        }
    }

    fn expect(&mut self, expected: char) -> Result<()> {
        if self.bump() == Some(expected) {
            return Ok(());
        }
        anyhow::bail!("JSON 缺少 {expected}");
    }

    fn parse_value(&mut self) -> Result<Json> {
        self.skip_whitespace();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => self.parse_string().map(Json::String),
            Some('t') => self.parse_literal("true").map(|()| Json::Bool(true)),
            Some('f') => self.parse_literal("false").map(|()| Json::Bool(false)),
            Some('n') => self.parse_literal("null").map(|()| Json::Null),
            Some(_) => self.parse_number(),
            None => anyhow::bail!("JSON 意外结束"),
        }
    }

    fn parse_literal(&mut self, literal: &str) -> Result<()> {
        for expected in literal.chars() {
            self.expect(expected)?;
        }
        Ok(())
    }

    fn parse_number(&mut self) -> Result<Json> {
        let start = self.index;
        while let Some(value) = self.peek() {
            if !value.is_ascii_digit() && !matches!(value, '-' | '+' | '.' | 'e' | 'E') {
                break;
            }
            self.index += 1;
        }
        let text = self.chars[start..self.index].iter().collect::<String>();
        let Ok(number) = text.parse::<f64>() else {
            anyhow::bail!("JSON 数字无法解析");
        };
        Ok(Json::Number(number))
    }

    fn parse_string(&mut self) -> Result<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            let Some(value) = self.bump() else {
                anyhow::bail!("JSON 字符串未闭合");
            };
            if value == '"' {
                break;
            }
            if value != '\\' {
                out.push(value);
                continue;
            }
            let Some(escape) = self.bump() else {
                anyhow::bail!("JSON 转义未结束");
            };
            match escape {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => out.push(self.parse_unicode_escape()?),
                _ => anyhow::bail!("JSON 不支持的转义"),
            }
        }
        Ok(out)
    }

    fn parse_unicode_escape(&mut self) -> Result<char> {
        let high = self.parse_hex4()?;
        if !(0xd800..0xdc00).contains(&high) {
            let Some(value) = char::from_u32(high) else {
                anyhow::bail!("JSON 转义字符不合法");
            };
            return Ok(value);
        }
        self.expect('\\')?;
        self.expect('u')?;
        let low = self.parse_hex4()?;
        if !(0xdc00..0xe000).contains(&low) {
            anyhow::bail!("JSON 代理对不合法");
        }
        let code = 0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00);
        let Some(value) = char::from_u32(code) else {
            anyhow::bail!("JSON 代理对不合法");
        };
        Ok(value)
    }

    fn parse_hex4(&mut self) -> Result<u32> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(digit) = self.bump().and_then(|item| item.to_digit(16)) else {
                anyhow::bail!("JSON 转义需要 4 位十六进制");
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_array(&mut self) -> Result<Json> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(']') {
            self.index += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_whitespace();
            match self.bump() {
                Some(',') => continue,
                Some(']') => break,
                _ => anyhow::bail!("JSON 数组缺少 , 或 ]"),
            }
        }
        Ok(Json::Array(items))
    }

    fn parse_object(&mut self) -> Result<Json> {
        self.expect('{')?;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some('}') {
            self.index += 1;
            return Ok(Json::Object(entries));
        }
        loop {
            self.skip_whitespace();
            let key = self.parse_string()?;
            self.skip_whitespace();
            self.expect(':')?;
            let value = self.parse_value()?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                _ => anyhow::bail!("JSON 对象缺少 , 或 }}"),
            }
        }
        Ok(Json::Object(entries))
    }
}

fn main() -> Result<()> {
    configure_utf8_console()?;
    let cli = Cli::parse();
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
        terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
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
                KeyCode::Char('r') => app.refresh()?,
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
                KeyCode::Char('g') => app.reload_store_index(),
                KeyCode::Char('m') => app.open_store_form(true),
                KeyCode::Char('M') => app.open_store_form(false),
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

    draw_header(frame, layout[0]);
    draw_tabs(frame, app, layout[1]);
    match app.page {
        TuiPage::Core(page) => draw_core_page(frame, app, layout[2], page),
        TuiPage::Store => draw_store(frame, app, layout[2]),
    }
    draw_footer(frame, layout[3]);
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

fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect) {
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
        Line::from(format!("Service: {}", app.view.services.len())),
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
            Constraint::Length(11),
            Constraint::Min(6),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(
            "Release Actions: R create  U update  i install  Y delete  B rollback  z validate",
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Release Actions"),
        ),
        chunks[0],
    );
    let rows = app.view.services.iter().map(|service| {
        Row::new(vec![
            Cell::from(service.id.clone()),
            Cell::from(service.name.clone()),
            Cell::from(service.version.clone()),
            Cell::from(service.kind.clone()),
            Cell::from(service.endpoint.clone()),
            Cell::from(service.runtime.clone()),
            Cell::from(service.ui.clone()),
            Cell::from(service.health.clone()),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(16),
                Constraint::Length(8),
                Constraint::Length(15),
                Constraint::Length(16),
                Constraint::Length(11),
                Constraint::Length(8),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(vec![
                "ID", "名称", "版本", "类型", "Endpoint", "Runtime", "UI", "Health",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("Service")),
        chunks[1],
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
        chunks[2],
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
    let rows = app.view.links.iter().map(|link| {
        Row::new(vec![
            Cell::from(link.from.clone()),
            Cell::from(link.to.clone()),
            Cell::from(link.protocol.clone()),
            Cell::from(link.auth_mode.clone()),
            Cell::from(link.scope.clone()),
            Cell::from(link.enabled.clone()),
            Cell::from(link.source.clone()),
        ])
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
        Paragraph::new("Operation Actions: c confirm  a apply  u rollback  o logs").block(
            Block::default()
                .borders(Borders::ALL)
                .title("Operation Actions"),
        ),
        chunks[0],
    );
    draw_operation_workbench(frame, app, chunks[1]);

    let rows = app.view.operations.iter().map(|operation| {
        Row::new(vec![
            Cell::from(operation.operation_id.clone()),
            Cell::from(operation.action.clone()),
            Cell::from(operation.target.clone()),
            Cell::from(operation.status.clone()),
            Cell::from(operation.risk.clone()),
            Cell::from(operation.mode.clone()),
            Cell::from(operation.plan_required.clone()),
            Cell::from(operation.fields.clone()),
            Cell::from(operation.preview_target.clone()),
            Cell::from(operation.preview_confirmation.clone()),
            Cell::from(operation.result.clone()),
            Cell::from(operation.error.clone()),
            Cell::from(operation.log_count.to_string()),
            Cell::from(operation.summary.clone()),
            Cell::from(operation.created_at.clone()),
            Cell::from(operation.updated_at.clone()),
            Cell::from(operation.preview_steps.clone()),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(24),
                Constraint::Length(28),
                Constraint::Length(16),
                Constraint::Length(16),
                Constraint::Length(6),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Length(20),
                Constraint::Length(18),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Length(18),
                Constraint::Length(6),
                Constraint::Length(18),
                Constraint::Length(16),
                Constraint::Length(16),
                Constraint::Min(24),
            ],
        )
        .header(
            Row::new(vec![
                "Operation",
                "Action",
                "对象",
                "状态",
                "风险",
                "模式",
                "Plan",
                "字段",
                "预览目标",
                "确认",
                "结果",
                "错误",
                "日志",
                "摘要",
                "Created",
                "Updated",
                "预览步骤",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Operation Action Registry"),
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
    let lines = vec![
        Line::from(format!("Action: {}", workbench.selected_action)),
        Line::from(format!("Operation: {}", workbench.operation_id)),
        Line::from(format!("目标: {}", workbench.target)),
        Line::from(format!("字段: {}", workbench.fields)),
        Line::from(format!(
            "状态: {}    结果: {}    日志: {}",
            workbench.current_status,
            if workbench.result_status.is_empty() {
                "待执行"
            } else {
                &workbench.result_status
            },
            workbench.log_count
        )),
        Line::from(format!("预览步骤: {}", workbench.preview_steps)),
        Line::from(format!(
            "需确认: {}    可执行: {}    可回滚: {}",
            workbench.requires_confirmation, workbench.can_apply, workbench.rollback
        )),
        Line::from(format!("提示: {}", workbench.warnings)),
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
        Line::from(format!("当前字段: {selected_field}")),
    ];
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
    let rows = app.view.logs.iter().map(|log| {
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
        area,
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
            Constraint::Length(5),
            Constraint::Min(5),
            Constraint::Length(9),
        ])
        .split(area);
    let header = vec![
        Line::from("Store Actions: m install  M manual install  g reload index"),
        Line::from("Up/Down 选择模块  w 切换运行时驱动执行"),
        Line::from(format!(
            "索引 {}    模块 {}    运行时驱动 {}",
            app.store.index_url,
            app.store.modules.len(),
            driver_toggle_label(app.execute_service_driver)
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
            Cell::from(module.tags.clone()),
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
        Line::from(format!("类型 {}    标签 {}", module.kind, module.tags)),
        Line::from(format!("来源 {}", module.source())),
        Line::from(format!("状态 {}", store_installed_label(app, &module.id))),
        Line::from("m 用该来源导入并安装；M 手动填写来源。"),
    ];
    if module.source_url.trim().is_empty() {
        lines.push(Line::from(
            "该模块只给了仓库，请用 Web UI 选 Release 资产。",
        ));
    }
    if !app.store.message.is_empty() {
        lines.push(Line::from(app.store.message.clone()));
    }
    lines
}

fn store_form_lines(form: &StoreInstallForm) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("导入并安装：Tab 换字段  Enter 提交  Esc 取消"),
        Line::from(format!(
            "{} source_url {}",
            field_marker(form.field == 0),
            form.source_url
        )),
        Line::from(format!(
            "{} checksum   {}",
            field_marker(form.field == 1),
            form.checksum
        )),
        Line::from("source_url 支持仓库内相对路径与 release 包直链。"),
        Line::from("checksum 可留空，填写时形如 sha256:<hex>。"),
    ];
    if !form.module_id.is_empty() {
        lines.push(Line::from(format!("来自索引模块 {}", form.module_id)));
    }
    lines
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(
            "q/Esc quit  r refresh  Tab/1-9 pages  R/U/i/Y/B/z release  e/E/x/h endpoint  l/L/X/H link  c/a/u/o operation  d/D diagnostics  0 store page  m/M store install  g reload index  s/S host start-stop  w service driver  Up/Down select row",
        )
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::ReleaseRegistryViewRow;
    use std::collections::BTreeMap;

    const SAMPLE_STORE_INDEX: &str = r#"{"schema_version":1,"modules":[
{"id":"demo","name":"Demo","tags":["a","b"],"source_url":"services/demo"},
{"id":"only-repo","repo":"owner/name"},
{"id":"","name":"skipped"}
]}"#;

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
            orchestrator_core::load_orchestrator_view_with_database_url(&repo_root(), None)
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
        for key in ['w', 's', 'S', 'g', 'm', 'M'] {
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
    fn tui_store_index_parser_reads_modules_and_flags_remote_index() {
        let modules = store_modules_from_index(SAMPLE_STORE_INDEX).expect("index parses");
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0].id, "demo");
        assert_eq!(modules[0].name, "Demo");
        assert_eq!(modules[0].tags, "a,b");
        assert_eq!(modules[0].source(), "services/demo");
        assert_eq!(modules[1].name, "only-repo");
        assert_eq!(modules[1].source(), "owner/name");
        assert!(store_modules_from_index("{}").is_err());
        assert!(store_modules_from_index("not json").is_err());
        assert!(store_index_is_remote("https://example.com/index.json"));
        assert!(!store_index_is_remote("store/index.json"));
        let repo = PathBuf::from("repo-root");
        assert!(store_index_path(&repo, "../escape.json").is_err());
        assert!(store_index_path(&repo, "store/index.json").is_ok());
    }

    #[test]
    fn tui_store_page_lists_local_index_modules() {
        let source = tui_source();
        assert!(source.contains("Store Actions: m install  M manual install  g reload index"));

        let app = App::new_memory(repo_root()).expect("TUI app should load");
        assert!(!store_index_is_remote(&app.store.index_url));
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
        assert!(
            app.installed_version("gateway").is_some(),
            "已在 view.services 里的模块应标记为已安装"
        );
        assert!(app.installed_version("not-installed-module").is_none());
    }

    #[test]
    fn tui_store_form_collects_source_url_and_checksum() {
        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.open_store_form(false);
        assert_eq!(app.page, TuiPage::Store);
        for value in ['s', 'v', 'c'] {
            app.handle_store_form_key(KeyCode::Char(value));
        }
        app.handle_store_form_key(KeyCode::Backspace);
        app.handle_store_form_key(KeyCode::Tab);
        app.handle_store_form_key(KeyCode::Char('a'));
        let form = app.store.form.clone().expect("store form should stay open");
        assert_eq!(form.source_url, "sv");
        assert_eq!(form.checksum, "a");
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
    fn tui_store_import_and_install_dispatches_release_install() {
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

        app.submit_store_form();
        assert!(app.store.form.is_none());
        assert!(
            app.last_message.starts_with("已导入 gateway")
                || app.last_message.starts_with("已更新 gateway"),
            "商店安装应先经 import_external_release 注册契约: {}",
            app.last_message
        );
        assert!(
            app.last_message.contains("RUNTIME_PIPELINE"),
            "导入后应派发 release.install: {}",
            app.last_message
        );
    }

    #[test]
    fn tui_host_lifecycle_actions_target_selected_endpoint_host() {
        let source = tui_source();
        assert!(source.contains("Host Actions: s host start  S host stop"));

        let mut app = App::new_memory(repo_root()).expect("TUI app should load");
        app.set_page(TuiPage::Core(OrchestratorViewPage::Endpoints));
        app.move_selection(1);
        assert_eq!(app.selected_endpoint_index, 1);
        let host_ip = app.selected_host_ip().expect("selected endpoint host");
        assert!(!host_ip.is_empty());

        assert!(!app.execute_service_driver);
        app.toggle_service_driver();
        assert!(app.execute_service_driver);
        app.toggle_service_driver();
        assert!(!app.execute_service_driver);

        app.run_host_action("host.start");
        let request = app.session.workbench.request.clone();
        assert_eq!(request.action, "host.start");
        assert_eq!(request.field("host_ip"), Some(host_ip.as_str()));
        assert_eq!(request.field("confirm"), Some("true"));
        assert_eq!(request.field("execute_service_driver"), None);
        assert!(
            app.last_message.contains("RUNTIME_PIPELINE"),
            "host.start 必须经 core dispatcher 的 runtime pipeline: {}",
            app.last_message
        );

        app.run_host_action("host.stop");
        assert_eq!(app.session.workbench.request.action, "host.stop");
        assert!(
            app.last_message.contains("RUNTIME_PIPELINE"),
            "host.stop 必须经 core dispatcher 的 runtime pipeline: {}",
            app.last_message
        );
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

        app.set_page(TuiPage::Core(OrchestratorViewPage::Services));
        app.move_selection(1);
        assert_eq!(app.selected_release_index, 1);
        assert!(app.release_action_fields("release.install").is_some());
    }

    #[test]
    fn tui_footer_documents_new_store_and_host_keys() {
        let source = tui_source();
        assert!(source.contains("q/Esc quit  r refresh  Tab/1-9 pages"));
        assert!(source.contains("0 store page  m/M store install  g reload index"));
        assert!(source.contains("s/S host start-stop  w service driver  Up/Down select row"));
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
