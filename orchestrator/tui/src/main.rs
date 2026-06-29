use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use orchestrator_core::{
    OperationWorkbenchContext, OperationWorkbenchSession, OperationWorkbenchView, OrchestratorView,
    OrchestratorViewPage, endpoint_hosts, load_operation_workbench_context, load_orchestrator_view,
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
    repo_root: PathBuf,
    page: OrchestratorViewPage,
    context: OperationWorkbenchContext,
    session: OperationWorkbenchSession,
    selected_field_index: usize,
    view: OrchestratorView,
    last_message: String,
}

impl App {
    fn new(repo_root: PathBuf) -> Result<Self> {
        let context = load_operation_workbench_context(&repo_root)?;
        let session = context.build_session("service.install")?;
        let view = load_orchestrator_view(&repo_root)?;
        Ok(Self {
            repo_root,
            page: OrchestratorViewPage::Overview,
            context,
            session,
            selected_field_index: 0,
            view,
            last_message: String::new(),
        })
    }

    fn refresh(&mut self) -> Result<()> {
        let context = load_operation_workbench_context(&self.repo_root)?;
        let action = self.session.workbench.selected_action.clone();
        let session = context
            .build_session_from_request(&self.session.workbench.request)
            .or_else(|_| context.build_session(&action))
            .or_else(|_| context.build_session("service.install"))?;
        self.context = context;
        self.session = session;
        self.view = load_orchestrator_view(&self.repo_root)?;
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
                self.session = session;
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
                self.session = session;
                self.last_message = format!("已更新字段 {}", field.name);
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn confirm_session(&mut self) {
        match self.context.confirm(&self.session) {
            Ok(session) => {
                self.session = session;
                self.last_message = "Operation 已确认".to_string();
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn apply_session(&mut self) {
        match self.context.apply(&self.session) {
            Ok(session) => {
                self.session = session;
                self.last_message = "Operation 已执行".to_string();
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }

    fn rollback_session(&mut self) {
        match self.context.rollback(&self.session) {
            Ok(session) => {
                self.session = session;
                self.last_message = "Operation 已回滚".to_string();
            }
            Err(err) => {
                self.last_message = err.to_string();
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_root = fs::canonicalize(&cli.repo_root).unwrap_or(cli.repo_root);
    let app = App::new(repo_root)?;
    run(app)
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
                KeyCode::Char('c') => app.confirm_session(),
                KeyCode::Char('a') => app.apply_session(),
                KeyCode::Char('u') => app.rollback_session(),
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
        OrchestratorViewPage::Sets => draw_sets(frame, app, layout[2]),
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
            "Service、Set、Endpoint、Link、Operation、Topology、LogView、DiagnosticReport",
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
        Line::from(format!("Set: {}", app.view.sets.len())),
        Line::from(format!("Endpoint: {}", app.view.endpoints.len())),
        Line::from(format!("Link: {}", app.view.links.len())),
        Line::from(format!("Operation Action: {}", app.view.operations.len())),
        Line::from(format!("LogView Source: {}", app.view.logs.len())),
        Line::from(format!("DiagnosticReport: {}", app.view.diagnostics.len())),
        Line::from(""),
        Line::from("Endpoint 使用 IP:Port 作为运行时唯一身份。"),
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
        area,
    );
}

fn draw_sets(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let rows = app.view.sets.iter().map(|set| {
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
            Row::new(vec!["ID", "名称", "Service", "Link", "范围"])
                .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("Set")),
        area,
    );
}

fn draw_endpoints(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
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
                .title("Endpoint = IP:Port"),
        ),
        area,
    );
}

fn draw_links(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
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
        area,
    );
}

fn draw_operations(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(10), Constraint::Min(8)])
        .split(area);
    draw_operation_workbench(frame, app, chunks[0]);

    let rows = app.view.operations.iter().map(|operation| {
        Row::new(vec![
            Cell::from(operation.action.clone()),
            Cell::from(operation.target.clone()),
            Cell::from(operation.risk.clone()),
            Cell::from(operation.mode.clone()),
            Cell::from(operation.plan_required.clone()),
            Cell::from(operation.fields.clone()),
            Cell::from(operation.preview_target.clone()),
            Cell::from(operation.preview_confirmation.clone()),
            Cell::from(operation.summary.clone()),
            Cell::from(operation.preview_steps.clone()),
        ])
    });
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(28),
                Constraint::Length(16),
                Constraint::Length(6),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Length(20),
                Constraint::Length(18),
                Constraint::Length(8),
                Constraint::Length(18),
                Constraint::Min(24),
            ],
        )
        .header(
            Row::new(vec![
                "Action",
                "对象",
                "风险",
                "模式",
                "Plan",
                "字段",
                "预览目标",
                "确认",
                "摘要",
                "预览步骤",
            ])
            .style(Style::default().fg(Color::Yellow)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Operation Action Registry"),
        ),
        chunks[1],
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
            "Topology 由 Service、Set、Endpoint、Link、Operation、LogView、DiagnosticReport 组成。",
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
                Constraint::Min(16),
            ],
        )
        .header(
            Row::new(vec!["Source", "Service", "Endpoint", "位置"])
                .style(Style::default().fg(Color::Yellow)),
        )
        .block(Block::default().borders(Borders::ALL).title("LogView")),
        area,
    );
}

fn draw_diagnostics(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
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
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(
            "q/Esc 退出  r 刷新  Tab/1-9 切页  n/p 选择 action  f 字段  v 改值  c 确认  a 执行  u 回滚",
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
                "Set",
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
        let app = App::new(repo_root()).expect("TUI app should load orchestrator/core view");
        assert!(!app.view.services.is_empty());
        assert!(!app.view.sets.is_empty());
        assert!(!app.view.endpoints.is_empty());
        assert!(!app.view.links.is_empty());
        assert!(!app.view.operations.is_empty());
        assert!(
            {
                let workbench = OperationWorkbenchView::from_session(&app.session);
                workbench.selected_action == "service.install"
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
        assert!(!app.view.logs.is_empty());
        assert!(!app.view.diagnostics.is_empty());
    }

    #[test]
    fn tui_workbench_uses_core_session_for_action_field_and_apply() {
        let mut app = App::new(repo_root()).expect("TUI app should load");
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

        app = App::new(repo_root()).expect("TUI app should reload");
        app.confirm_session();
        app.apply_session();
        assert_eq!(app.session.result_status, "SUCCEEDED");
        app.rollback_session();
        assert_eq!(app.session.result_status, "ROLLED_BACK");
    }
}
