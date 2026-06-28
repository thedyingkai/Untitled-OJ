use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use module_installer_core::{
    InstalledModule, Manifest, ModuleState, Plan, RegistrySnapshot, ServiceDecl, WorkerDecl,
    disable_plan, enable_plan, install_plan, uninstall_plan, validate_manifest_file,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap};
use serde::Deserialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_OPERATION_LOG: &str = ".tmp/agent/runtime-operations.jsonl";

#[derive(Parser)]
#[command(name = "ojos-installer-tui")]
#[command(about = "OJOS 原生安装器 TUI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
    #[arg(long, default_value = DEFAULT_OPERATION_LOG)]
    operation_log: PathBuf,
}

#[derive(Clone)]
struct ModuleRow {
    module_id: String,
    name: String,
    version: String,
    kind: String,
    status: String,
    manifest_path: String,
}

#[derive(Clone)]
struct RuntimeRow {
    service_id: String,
    module_id: String,
    kind: String,
    lifecycle: String,
    runtime: String,
    state: String,
    health: String,
    blocked_by: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct OperationRow {
    operation_id: String,
    module_id: String,
    service_id: String,
    action: String,
    status: String,
    #[serde(default)]
    actor_username: String,
    updated_at: String,
    #[serde(default)]
    error_message: String,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Page {
    Home,
    Modules,
    Runtime,
    Operations,
    Topology,
    Plans,
    Help,
}

impl Page {
    fn all() -> [Page; 7] {
        [
            Page::Home,
            Page::Modules,
            Page::Runtime,
            Page::Operations,
            Page::Topology,
            Page::Plans,
            Page::Help,
        ]
    }

    fn title(self) -> &'static str {
        match self {
            Page::Home => "总览",
            Page::Modules => "模块",
            Page::Runtime => "Runtime",
            Page::Operations => "操作日志",
            Page::Topology => "拓扑",
            Page::Plans => "计划",
            Page::Help => "帮助",
        }
    }
}

struct App {
    repo_root: PathBuf,
    operation_log: PathBuf,
    page: Page,
    selected: usize,
    filter: String,
    filter_mode: bool,
    modules: Vec<ModuleRow>,
    runtime: Vec<RuntimeRow>,
    operations: Vec<OperationRow>,
    warnings: Vec<String>,
    plan_title: String,
    plan_lines: Vec<String>,
    last_refresh: DateTime<Utc>,
}

impl App {
    fn new(repo_root: PathBuf, operation_log: PathBuf) -> Result<Self> {
        let mut app = Self {
            repo_root,
            operation_log,
            page: Page::Home,
            selected: 0,
            filter: String::new(),
            filter_mode: false,
            modules: Vec::new(),
            runtime: Vec::new(),
            operations: Vec::new(),
            warnings: Vec::new(),
            plan_title: "尚未生成计划".to_string(),
            plan_lines: vec![
                "模块页选择条目后按 i/e/d/u 生成安装、启用、禁用、卸载 dry-run 计划。".to_string(),
                "Runtime 页选择服务后按 s/t/x 生成 start/stop/restart 计划。".to_string(),
                "TUI 默认不执行危险 apply；runtime apply 请导出计划后使用 ojosctl apply-plan --confirm。".to_string(),
            ],
            last_refresh: Utc::now(),
        };
        app.refresh()?;
        Ok(app)
    }

    fn refresh(&mut self) -> Result<()> {
        self.warnings.clear();
        self.modules = load_modules(&self.repo_root, &mut self.warnings)?;
        self.runtime = load_runtime(&self.repo_root, &mut self.warnings)?;
        self.operations = load_operations(&self.operation_log, &mut self.warnings)?;
        self.last_refresh = Utc::now();
        self.selected = self.selected.min(self.visible_len().saturating_sub(1));
        Ok(())
    }

    fn visible_len(&self) -> usize {
        match self.page {
            Page::Modules => self.filtered_modules().len(),
            Page::Runtime => self.filtered_runtime().len(),
            Page::Operations => self.filtered_operations().len(),
            _ => 1,
        }
    }

    fn filtered_modules(&self) -> Vec<ModuleRow> {
        let needle = self.filter.to_ascii_lowercase();
        self.modules
            .iter()
            .filter(|item| {
                needle.is_empty()
                    || item.module_id.to_ascii_lowercase().contains(&needle)
                    || item.name.to_ascii_lowercase().contains(&needle)
                    || item.status.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    fn filtered_runtime(&self) -> Vec<RuntimeRow> {
        let needle = self.filter.to_ascii_lowercase();
        self.runtime
            .iter()
            .filter(|item| {
                needle.is_empty()
                    || item.service_id.to_ascii_lowercase().contains(&needle)
                    || item.module_id.to_ascii_lowercase().contains(&needle)
                    || item.state.to_ascii_lowercase().contains(&needle)
                    || item.health.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    fn filtered_operations(&self) -> Vec<OperationRow> {
        let needle = self.filter.to_ascii_lowercase();
        self.operations
            .iter()
            .filter(|item| {
                needle.is_empty()
                    || item.operation_id.to_ascii_lowercase().contains(&needle)
                    || item.service_id.to_ascii_lowercase().contains(&needle)
                    || item.status.to_ascii_lowercase().contains(&needle)
            })
            .cloned()
            .collect()
    }

    fn set_page(&mut self, page: Page) {
        self.page = page;
        self.selected = 0;
        self.filter_mode = false;
    }

    fn next_page(&mut self) {
        let pages = Page::all();
        let index = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        self.set_page(pages[(index + 1) % pages.len()]);
    }

    fn prev_page(&mut self) {
        let pages = Page::all();
        let index = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        self.set_page(pages[(index + pages.len() - 1) % pages.len()]);
    }

    fn move_down(&mut self) {
        let len = self.visible_len();
        if len > 0 {
            self.selected = (self.selected + 1).min(len - 1);
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn selected_module(&self) -> Option<ModuleRow> {
        if self.page != Page::Modules {
            return None;
        }
        self.filtered_modules().get(self.selected).cloned()
    }

    fn selected_runtime(&self) -> Option<RuntimeRow> {
        if self.page != Page::Runtime {
            return None;
        }
        self.filtered_runtime().get(self.selected).cloned()
    }

    fn set_plan(&mut self, title: impl Into<String>, lines: Vec<String>) {
        self.plan_title = title.into();
        self.plan_lines = lines;
        self.set_page(Page::Plans);
    }

    fn set_error(&mut self, title: impl Into<String>, err: anyhow::Error) {
        self.plan_title = title.into();
        self.plan_lines = vec![format!("错误: {}", redact_text(&err.to_string()))];
        self.set_page(Page::Plans);
    }

    fn module_plan(&mut self, action: &str) {
        let Some(row) = self.selected_module() else {
            self.set_plan("模块计划", vec!["请先在模块页选择一个模块。".to_string()]);
            return;
        };
        let result = match action {
            "install" => make_install_plan(&self.repo_root, &row.manifest_path),
            "enable" => make_state_plan(&self.repo_root, &row.module_id, "enable"),
            "disable" => make_state_plan(&self.repo_root, &row.module_id, "disable"),
            "uninstall" => make_state_plan(&self.repo_root, &row.module_id, "uninstall"),
            _ => unreachable!("unsupported module action"),
        };
        match result {
            Ok(lines) => self.set_plan(format!("模块 {} 计划: {}", action, row.module_id), lines),
            Err(err) => self.set_error(format!("模块 {} 计划失败", action), err),
        }
    }

    fn runtime_plan(&mut self, action: &str) {
        let Some(row) = self.selected_runtime() else {
            self.set_plan(
                "Runtime 计划",
                vec!["请先在 Runtime 页选择一个服务。".to_string()],
            );
            return;
        };
        let lines = make_runtime_plan(&row, action);
        self.set_plan(
            format!("Runtime {} 计划: {}", action, row.service_id),
            lines,
        );
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut app = App::new(cli.repo_root, cli.operation_log)?;
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_app(&mut terminal, &mut app);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(Duration::from_millis(250))? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if app.filter_mode {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => app.filter_mode = false,
                    KeyCode::Backspace => {
                        app.filter.pop();
                        app.selected = 0;
                    }
                    KeyCode::Char(ch) => {
                        app.filter.push(ch);
                        app.selected = 0;
                    }
                    _ => {}
                }
                continue;
            }
            match key.code {
                KeyCode::Char('q') => return Ok(()),
                KeyCode::Char('?') => app.set_page(Page::Help),
                KeyCode::Char('/') => app.filter_mode = true,
                KeyCode::Char('r') => app.refresh()?,
                KeyCode::Char('1') => app.set_page(Page::Home),
                KeyCode::Char('2') => app.set_page(Page::Modules),
                KeyCode::Char('3') => app.set_page(Page::Runtime),
                KeyCode::Char('4') => app.set_page(Page::Operations),
                KeyCode::Char('5') => app.set_page(Page::Topology),
                KeyCode::Char('6') => app.set_page(Page::Plans),
                KeyCode::Char('7') => app.set_page(Page::Help),
                KeyCode::Char('i') => app.module_plan("install"),
                KeyCode::Char('e') => app.module_plan("enable"),
                KeyCode::Char('d') => app.module_plan("disable"),
                KeyCode::Char('u') => app.module_plan("uninstall"),
                KeyCode::Char('s') => app.runtime_plan("start"),
                KeyCode::Char('t') => app.runtime_plan("stop"),
                KeyCode::Char('x') => app.runtime_plan("restart"),
                KeyCode::Tab => app.next_page(),
                KeyCode::BackTab => app.prev_page(),
                KeyCode::Down | KeyCode::Char('j') => app.move_down(),
                KeyCode::Up | KeyCode::Char('k') => app.move_up(),
                _ => {}
            }
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);
    draw_header(frame, app, layout[0]);
    draw_tabs(frame, app, layout[1]);
    match app.page {
        Page::Home => draw_home(frame, app, layout[2]),
        Page::Modules => draw_modules(frame, app, layout[2]),
        Page::Runtime => draw_runtime(frame, app, layout[2]),
        Page::Operations => draw_operations(frame, app, layout[2]),
        Page::Topology => draw_topology(frame, app, layout[2]),
        Page::Plans => draw_plans(frame, app, layout[2]),
        Page::Help => draw_help(frame, layout[2]),
    }
    draw_footer(frame, app, layout[3]);
}

fn draw_header(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let text = vec![Line::from(vec![
        Span::styled(
            " OJOS v0.1.0 原生安装器 ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "CLI/TUI 为正式安装入口，Web Shell 仅作管理视图",
            Style::default().fg(Color::Gray),
        ),
    ])];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
    let _ = app;
}

fn draw_tabs(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let titles = Page::all()
        .iter()
        .map(|page| Line::from(page.title()))
        .collect::<Vec<_>>();
    let selected = Page::all()
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
                    .title("导航 1-7 / Tab"),
            ),
        area,
    );
}

fn draw_home(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let enabled = app
        .modules
        .iter()
        .filter(|item| item.status == "ENABLED")
        .count();
    let blocked = app
        .runtime
        .iter()
        .filter(|item| !item.blocked_by.is_empty())
        .count();
    let text = vec![
        Line::from(format!(
            "模块总数: {}    已启用: {}",
            app.modules.len(),
            enabled
        )),
        Line::from(format!(
            "Runtime 条目: {}    受阻条目: {}",
            app.runtime.len(),
            blocked
        )),
        Line::from(format!(
            "操作日志: {}    最近刷新: {}",
            app.operations.len(),
            app.last_refresh.to_rfc3339()
        )),
        Line::from(""),
        Line::from(
            "安装、打包、验证、启用、禁用请使用 ojosctl 或本 TUI。危险 apply 默认不执行，必须通过受控确认。",
        ),
        Line::from("按 ? 查看帮助，按 / 搜索，按 r 刷新，按 q 退出。"),
        Line::from("模块页: i/e/d/u 生成计划；Runtime 页: s/t/x 生成 start/stop/restart 计划。"),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("首页状态总览")),
        area,
    );
}

fn draw_modules(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let rows = app.filtered_modules();
    let table_rows = rows.iter().enumerate().map(|(idx, item)| {
        let style = if idx == app.selected {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if item.status == "ENABLED" {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(item.module_id.clone()),
            Cell::from(item.name.clone()),
            Cell::from(item.version.clone()),
            Cell::from(item.kind.clone()),
            Cell::from(item.status.clone()),
            Cell::from(item.manifest_path.clone()),
        ])
        .style(style)
    });
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(28),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Min(24),
        ],
    )
    .header(header(["模块", "名称", "版本", "类型", "状态", "Manifest"]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("模块列表  i安装计划 e启用计划 d禁用计划 u卸载dry-run"),
    );
    frame.render_widget(table, area);
}

fn draw_runtime(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let rows = app.filtered_runtime();
    let table_rows = rows.iter().enumerate().map(|(idx, item)| {
        let style = if idx == app.selected {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if !item.blocked_by.is_empty() {
            Style::default().fg(Color::Red)
        } else if item.state == "RUNNING" || item.health == "ok" {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(item.service_id.clone()),
            Cell::from(item.module_id.clone()),
            Cell::from(item.kind.clone()),
            Cell::from(item.lifecycle.clone()),
            Cell::from(item.runtime.clone()),
            Cell::from(item.state.clone()),
            Cell::from(item.health.clone()),
            Cell::from(item.blocked_by.join("; ")),
        ])
        .style(style)
    });
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(28),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(24),
        ],
    )
    .header(header([
        "服务/Worker",
        "模块",
        "类型",
        "生命周期",
        "运行时",
        "状态",
        "健康",
        "阻断",
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Runtime Services / Workers  s start  t stop  x restart"),
    );
    frame.render_widget(table, area);
}

fn draw_operations(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let rows = app.filtered_operations();
    let table_rows = rows.iter().enumerate().map(|(idx, item)| {
        let style = if idx == app.selected {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if item.status == "SUCCEEDED" {
            Style::default().fg(Color::Green)
        } else if item.status == "FAILED" || item.status == "BLOCKED" {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        Row::new(vec![
            Cell::from(item.operation_id.clone()),
            Cell::from(item.action.clone()),
            Cell::from(item.status.clone()),
            Cell::from(item.service_id.clone()),
            Cell::from(item.module_id.clone()),
            Cell::from(item.actor_username.clone()),
            Cell::from(item.updated_at.clone()),
            Cell::from(item.error_message.clone()),
        ])
        .style(style)
    });
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(38),
            Constraint::Length(16),
            Constraint::Length(12),
            Constraint::Length(20),
            Constraint::Length(22),
            Constraint::Length(12),
            Constraint::Length(24),
            Constraint::Min(20),
        ],
    )
    .header(header([
        "Operation",
        "动作",
        "状态",
        "服务",
        "模块",
        "操作者",
        "更新时间",
        "错误",
    ]))
    .block(Block::default().borders(Borders::ALL).title("操作日志"));
    frame.render_widget(table, area);
}

fn draw_topology(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    for item in &app.runtime {
        lines.push(Line::from(format!(
            "{} -> {}:{}",
            item.module_id, item.kind, item.service_id
        )));
        if !item.blocked_by.is_empty() {
            lines.push(Line::from(format!(
                "  阻断: {}",
                item.blocked_by.join("; ")
            )));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("暂无 runtime topology 条目"));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Topology 文本视图"),
        ),
        area,
    );
}

fn draw_plans(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let lines = app
        .plan_lines
        .iter()
        .map(|line| Line::from(line.clone()))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.plan_title.clone()),
        ),
        area,
    );
}

fn draw_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let text = vec![
        Line::from("快捷键"),
        Line::from("  1-7 / Tab: 切换页面"),
        Line::from("  j/k 或 上下方向键: 移动选择"),
        Line::from("  /: 搜索或过滤当前页"),
        Line::from("  r: 重新加载 manifest、runtime 和 operation log"),
        Line::from("  模块页 i/e/d/u: 生成安装、启用、禁用、卸载 dry-run 计划"),
        Line::from("  Runtime 页 s/t/x: 生成 start、stop、restart 受控计划"),
        Line::from("  q: 退出"),
        Line::from(""),
        Line::from("安全边界"),
        Line::from("  本 TUI 是原生安装器界面，不是浏览器、Electron 或 WebView。"),
        Line::from(
            "  Runtime apply 仍必须遵守 trusted compose allowlist、plan TTL、锁、超时和二次确认。",
        ),
        Line::from("  默认不执行危险操作，不显示 secret/token/password，也不展示本机绝对路径。"),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("帮助")),
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mode = if app.filter_mode {
        "搜索输入中"
    } else {
        "正常"
    };
    let warning = app.warnings.first().cloned().unwrap_or_default();
    let line = format!("模式: {} | 过滤: {} | 警告: {}", mode, app.filter, warning);
    frame.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn header<const N: usize>(items: [&str; N]) -> Row<'static> {
    Row::new(
        items
            .into_iter()
            .map(|item| Cell::from(item.to_string()))
            .collect::<Vec<_>>(),
    )
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
}

fn load_modules(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<ModuleRow>> {
    let mut out = Vec::new();
    let modules_dir = repo_root.join("modules");
    if !modules_dir.is_dir() {
        warnings.push("modules 目录不存在".to_string());
        return Ok(out);
    }
    for entry in fs::read_dir(&modules_dir).context("读取 modules 目录失败")? {
        let entry = entry.context("读取模块目录项失败")?;
        let manifest_path = PathBuf::from("modules")
            .join(entry.file_name())
            .join("module.yaml");
        if !repo_root.join(&manifest_path).is_file() {
            continue;
        }
        match validate_manifest_file(repo_root, &manifest_path) {
            Ok(manifest) => out.push(ModuleRow {
                module_id: manifest.id,
                name: manifest.name,
                version: manifest.version,
                kind: manifest.kind,
                status: manifest.status,
                manifest_path: slash_path(&manifest_path),
            }),
            Err(err) => warnings.push(format!("{} 校验失败: {}", slash_path(&manifest_path), err)),
        }
    }
    out.sort_by(|left, right| left.module_id.cmp(&right.module_id));
    Ok(out)
}

fn local_registry_snapshot(repo_root: &Path) -> Result<RegistrySnapshot> {
    let mut modules = Vec::new();
    let modules_dir = repo_root.join("modules");
    if !modules_dir.is_dir() {
        return Ok(RegistrySnapshot::default());
    }
    for entry in fs::read_dir(&modules_dir).context("读取 modules 目录失败")? {
        let entry = entry.context("读取模块目录项失败")?;
        let manifest_path = PathBuf::from("modules")
            .join(entry.file_name())
            .join("module.yaml");
        if !repo_root.join(&manifest_path).is_file() {
            continue;
        }
        let manifest = validate_manifest_file(repo_root, &manifest_path)?;
        modules.push(InstalledModule {
            module_id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            status: if manifest.status == "builtin" {
                ModuleState::Enabled
            } else {
                ModuleState::Installed
            },
            kind: manifest.kind.clone(),
            manifest: Some(manifest),
        });
    }
    Ok(RegistrySnapshot { modules })
}

fn make_install_plan(repo_root: &Path, manifest_path: &str) -> Result<Vec<String>> {
    let manifest = validate_manifest_file(repo_root, Path::new(manifest_path))?;
    let snapshot = local_registry_snapshot(repo_root)?;
    let plan = install_plan(&manifest, &snapshot, true)?;
    Ok(plan_lines(&plan))
}

fn make_state_plan(repo_root: &Path, module_id: &str, action: &str) -> Result<Vec<String>> {
    let snapshot = local_registry_snapshot(repo_root)?;
    let plan = match action {
        "enable" => enable_plan(module_id, &snapshot, true)?,
        "disable" => disable_plan(module_id, &snapshot, true)?,
        "uninstall" => uninstall_plan(module_id, &snapshot, true)?,
        _ => unreachable!("unsupported module action"),
    };
    Ok(plan_lines(&plan))
}

fn plan_lines(plan: &Plan) -> Vec<String> {
    let mut lines = vec![
        format!("类型: {:?}", plan.kind),
        format!("模块: {}", plan.module_id),
        format!("版本: {}", plan.version),
        format!("dry_run: {}", plan.dry_run),
        format!("can_apply: {}", plan.can_apply),
        format!(
            "阻断: {}",
            if plan.blocked_by.is_empty() {
                "无".to_string()
            } else {
                plan.blocked_by.join("; ")
            }
        ),
        format!("影响表: {}", plan.affected_tables.join(", ")),
        "动作:".to_string(),
    ];
    if plan.actions.is_empty() {
        lines.push("  - 无动作".to_string());
    } else {
        for action in &plan.actions {
            lines.push(format!(
                "  - {} -> {} {}",
                action.action, action.target, action.detail
            ));
        }
    }
    lines.push("".to_string());
    lines.push(
        "安全边界: TUI 只生成计划；真实 apply 必须通过受控 operator/ojosctl confirm。".to_string(),
    );
    lines
}

fn make_runtime_plan(row: &RuntimeRow, action: &str) -> Vec<String> {
    let mut blocked = row.blocked_by.clone();
    if row.lifecycle == "metadata" {
        blocked.push(format!("metadata lifecycle 不能 {}", action));
    }
    if row.runtime != "compose" {
        blocked.push(format!("unsupported runtime {}", row.runtime));
    }
    let command_action = if action == "reload" {
        "restart"
    } else {
        action
    };
    let can_apply = blocked.is_empty();
    let mut lines = vec![
        format!("服务: {}", row.service_id),
        format!("模块: {}", row.module_id),
        format!("动作: {}", action),
        format!("driver: compose"),
        format!("can_apply: {}", can_apply),
        "requires_confirmation: true".to_string(),
        format!(
            "阻断: {}",
            if blocked.is_empty() {
                "无".to_string()
            } else {
                blocked.join("; ")
            }
        ),
    ];
    if can_apply {
        lines.push(format!(
            "命令形状: docker compose --env-file .env -f deploy/compose/docker-compose.yml {} <trusted-service>",
            command_action
        ));
    } else {
        lines.push("命令形状: 已阻断，不生成 compose 命令。".to_string());
    }
    lines.push("安全边界: TUI 默认不执行 apply；请用 ojosctl runtime plan-* --out 后再 apply-plan --confirm。".to_string());
    lines
}

fn load_runtime(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<RuntimeRow>> {
    let mut out = Vec::new();
    let modules_dir = repo_root.join("modules");
    if !modules_dir.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(&modules_dir).context("读取 modules 目录失败")? {
        let entry = entry.context("读取模块目录项失败")?;
        let manifest_path = PathBuf::from("modules")
            .join(entry.file_name())
            .join("module.yaml");
        if !repo_root.join(&manifest_path).is_file() {
            continue;
        }
        let manifest = match validate_manifest_file(repo_root, &manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                warnings.push(format!("跳过无效 manifest: {}", err));
                continue;
            }
        };
        for service in &manifest.provides.services {
            out.push(runtime_from_service(&manifest, service));
        }
        for worker in &manifest.provides.workers {
            out.push(runtime_from_worker(&manifest, worker));
        }
    }
    out.sort_by(|left, right| {
        left.module_id
            .cmp(&right.module_id)
            .then(left.service_id.cmp(&right.service_id))
    });
    Ok(out)
}

fn runtime_from_service(manifest: &Manifest, service: &ServiceDecl) -> RuntimeRow {
    runtime_row(
        manifest,
        &service.id,
        if service.kind.trim().is_empty() {
            "http"
        } else {
            &service.kind
        },
        &service.lifecycle,
        &service.trusted_runtime,
        &service.compose_service,
    )
}

fn runtime_from_worker(manifest: &Manifest, worker: &WorkerDecl) -> RuntimeRow {
    runtime_row(
        manifest,
        &worker.id,
        if worker.kind.trim().is_empty() {
            "worker"
        } else {
            &worker.kind
        },
        &worker.lifecycle,
        &worker.trusted_runtime,
        &worker.compose_service,
    )
}

fn runtime_row(
    manifest: &Manifest,
    service_id: &str,
    kind: &str,
    lifecycle: &str,
    runtime: &str,
    compose_service: &str,
) -> RuntimeRow {
    let lifecycle = if lifecycle.trim().is_empty() {
        "managed"
    } else {
        lifecycle
    }
    .to_string();
    let runtime = if runtime.trim().is_empty() {
        if lifecycle == "metadata" {
            "metadata"
        } else {
            "compose"
        }
    } else {
        runtime
    }
    .to_string();
    let mut blocked_by = Vec::new();
    if lifecycle == "metadata" {
        blocked_by.push("metadata lifecycle 不能 apply".to_string());
    }
    if runtime == "compose" && compose_service.trim().is_empty() {
        blocked_by.push("缺少 compose_service".to_string());
    }
    RuntimeRow {
        service_id: service_id.to_string(),
        module_id: manifest.id.clone(),
        kind: kind.to_string(),
        lifecycle,
        runtime,
        state: "DECLARED".to_string(),
        health: "unknown".to_string(),
        blocked_by,
    }
}

fn load_operations(path: &Path, warnings: &mut Vec<String>) -> Result<Vec<OperationRow>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path).context("读取 operation log 失败")?;
    let mut out = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<OperationRow>(line) {
            Ok(row) => out.push(row),
            Err(_) => warnings.push("operation log 中存在无法解析的行".to_string()),
        }
    }
    out.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(out)
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn redact_text(value: &str) -> String {
    let mut out = value.to_string();
    for key in ["token", "secret", "password", "authorization"] {
        out = replace_case_insensitive(&out, key, "[redacted]");
    }
    out
}

fn replace_case_insensitive(value: &str, needle: &str, replacement: &str) -> String {
    let lower = value.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut result = String::new();
    let mut cursor = 0usize;
    let mut search = 0usize;
    while let Some(pos) = lower[search..].find(&needle_lower) {
        let start = search + pos;
        result.push_str(&value[cursor..start]);
        result.push_str(replacement);
        cursor = start + needle.len();
        search = cursor;
    }
    result.push_str(&value[cursor..]);
    result
}
