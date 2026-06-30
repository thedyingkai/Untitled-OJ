use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use orchestrator_core::{
    ActionRequest, OperationWorkbenchContext, OperationWorkbenchSession, OperationWorkbenchView,
    OrchestratorActionConsole, OrchestratorView, OrchestratorViewPage, endpoint_hosts,
    merge_operation_workbench_session_into_view,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ojos-orchestrator-tui")]
#[command(about = "OJOS Orchestrator 原生 TUI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
}

struct App {
    page: OrchestratorViewPage,
    console: OrchestratorActionConsole,
    context: OperationWorkbenchContext,
    session: OperationWorkbenchSession,
    selected_field_index: usize,
    view: OrchestratorView,
    last_message: String,
}

impl App {
    fn new(repo_root: PathBuf) -> Result<Self> {
        let console = OrchestratorActionConsole::load(repo_root)?;
        Self::from_console(console)
    }

    #[cfg(test)]
    fn new_memory(repo_root: PathBuf) -> Result<Self> {
        Self::new(repo_root)
    }

    fn from_console(console: OrchestratorActionConsole) -> Result<Self> {
        let context = console.context()?;
        let session = context.build_session("release.install")?;
        let view = console.view()?;
        Ok(Self {
            page: OrchestratorViewPage::Overview,
            console,
            context,
            session,
            selected_field_index: 0,
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
        self.last_message = "已刷新".to_string();
        Ok(())
    }

    fn set_page(&mut self, page: OrchestratorViewPage) {
        self.page = page;
    }

    fn next_page(&mut self) {
        let pages = OrchestratorViewPage::all();
        let current = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        self.page = pages[(current + 1) % pages.len()];
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

    fn run_endpoint_action(&mut self, action: &str) {
        if let Some(endpoint) = self
            .view
            .endpoints
            .first()
            .map(|endpoint| endpoint.endpoint.clone())
        {
            self.run_action_with_fields(action, [("endpoint", endpoint)]);
        } else {
            self.run_action(action);
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

    fn selected_release_service(&self) -> Option<String> {
        self.view
            .release_registry
            .iter()
            .find(|record| record.record_type == "release")
            .map(|record| record.service_name.clone())
            .or_else(|| self.view.services.first().map(|service| service.id.clone()))
    }

    fn run_release_action(&mut self, action: &str) {
        match self.selected_release_service() {
            Some(service_id) => self.run_action_with_fields(action, [("service_id", service_id)]),
            None => self.run_action(action),
        }
    }

    fn dispatch_current(&mut self, action: &str) {
        let request = if action == "operation.confirm"
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
                KeyCode::Char('H') => app.run_link_action("link.health.check"),
                KeyCode::Char('o') => app.run_operation_logs_view(),
                KeyCode::Char('d') => app.run_action("diagnostic.create"),
                KeyCode::Char('D') => app.run_action_with_fields(
                    "diagnostic.export",
                    [("format", "markdown".to_string())],
                ),
                KeyCode::Char(value) => {
                    if let Some(page) = OrchestratorViewPage::all()
                        .iter()
                        .find(|page| page.key() == Some(value))
                    {
                        app.set_page(*page);
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
            Constraint::Length(3),
        ])
        .split(frame.area());

    draw_header(frame, layout[0]);
    draw_tabs(frame, app, layout[1]);
    match app.page {
        OrchestratorViewPage::Overview => draw_overview(frame, app, layout[2]),
        OrchestratorViewPage::Services => draw_services(frame, app, layout[2]),
        OrchestratorViewPage::Templates => draw_templates(frame, app, layout[2]),
        OrchestratorViewPage::Endpoints => draw_endpoints(frame, app, layout[2]),
        OrchestratorViewPage::Links => draw_links(frame, app, layout[2]),
        OrchestratorViewPage::Operations => draw_operations(frame, app, layout[2]),
        OrchestratorViewPage::Topology => draw_topology(frame, app, layout[2]),
        OrchestratorViewPage::Logs => draw_logs(frame, app, layout[2]),
        OrchestratorViewPage::Diagnostics => draw_diagnostics(frame, app, layout[2]),
    }
    draw_footer(frame, layout[3]);
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
    ])];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn draw_tabs(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let titles = OrchestratorViewPage::all()
        .iter()
        .map(|page| Line::from(page.title()))
        .collect::<Vec<_>>();
    let selected = OrchestratorViewPage::all()
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
                    .title("导航 1-9 / Tab"),
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

    let registry_rows = app.view.release_registry.iter().map(|record| {
        Row::new(vec![
            Cell::from(record.service_name.clone()),
            Cell::from(record.version.clone()),
            Cell::from(record.record_type.clone()),
            Cell::from(record.name.clone()),
            Cell::from(record.detail.clone()),
            Cell::from(record.source.clone()),
        ])
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
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Endpoint Actions: e create  E update  x delete  h health check").block(
            Block::default()
                .borders(Borders::ALL)
                .title("Endpoint Actions"),
        ),
        chunks[0],
    );
    let rows = app.view.endpoints.iter().map(|endpoint| {
        Row::new(vec![
            Cell::from(endpoint.endpoint.clone()),
            Cell::from(endpoint.service_id.clone()),
            Cell::from(endpoint.protocol.clone()),
            Cell::from(endpoint.expose.clone()),
            Cell::from(endpoint.source.clone()),
        ])
    });
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
        chunks[1],
    );
}

fn draw_links(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Link Actions: l create  L update  X delete  H health check")
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
                Constraint::Min(18),
            ],
        )
        .header(
            Row::new(vec!["Source", "Target", "协议", "认证", "范围", "来源"])
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

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(
            "q/Esc quit  r refresh  Tab/1-9 pages  R/U/i/Y/B/z release  e/E/x/h endpoint  l/L/X/H link  c/a/u/o operation  d/D diagnostics",
        )
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf()
    }

    #[test]
    fn tui_pages_cover_the_same_core_objects_as_gui() {
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
            "TUI should load the same shared operation workbench as GUI"
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
        app.select_action("service.start");
        app.apply_session();
        assert!(
            app.last_message.contains("UNSUPPORTED"),
            "TUI must not report unsupported lifecycle actions as success"
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
    }

    #[test]
    fn tui_set_templates_are_readonly() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("TUI source should be readable as UTF-8");
        assert!(source.contains("Deployment templates are readonly"));

        let repo_view = orchestrator_core::load_orchestrator_view(&repo_root()).expect("repo view");
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
        app.run_action("service.start");
        assert!(app.last_message.contains("UNSUPPORTED"));
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
