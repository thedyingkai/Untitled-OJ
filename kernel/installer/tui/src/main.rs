use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs, Wrap};
use service_installer_core::{
    ServiceManifest, ServiceSet, expand_set, validate_endpoint_id, validate_service_manifest_file,
    validate_service_set_file,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "ojos-installer-tui")]
#[command(about = "OJOS Root Installer / Runtime Manager 原生 TUI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
}

#[derive(Clone)]
struct ServiceRow {
    id: String,
    name: String,
    version: String,
    kind: String,
    endpoint: String,
    runtime: String,
    ui: String,
    path: String,
}

#[derive(Clone)]
struct SetRow {
    id: String,
    name: String,
    services: String,
    links: String,
    scope: String,
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Page {
    Overview,
    Services,
    Sets,
    Topology,
    Health,
    Help,
}

impl Page {
    fn all() -> [Page; 6] {
        [
            Page::Overview,
            Page::Services,
            Page::Sets,
            Page::Topology,
            Page::Health,
            Page::Help,
        ]
    }

    fn title(self) -> &'static str {
        match self {
            Page::Overview => "总览",
            Page::Services => "Service",
            Page::Sets => "Set",
            Page::Topology => "Topology",
            Page::Health => "Health",
            Page::Help => "帮助",
        }
    }
}

struct App {
    repo_root: PathBuf,
    page: Page,
    services: Vec<ServiceRow>,
    sets: Vec<SetRow>,
    warnings: Vec<String>,
}

impl App {
    fn new(repo_root: PathBuf) -> Result<Self> {
        let mut app = Self {
            repo_root,
            page: Page::Overview,
            services: Vec::new(),
            sets: Vec::new(),
            warnings: Vec::new(),
        };
        app.refresh()?;
        Ok(app)
    }

    fn refresh(&mut self) -> Result<()> {
        self.warnings.clear();
        self.services = load_services(&self.repo_root, &mut self.warnings)?;
        self.sets = load_sets(&self.repo_root, &mut self.warnings)?;
        Ok(())
    }

    fn set_page(&mut self, page: Page) {
        self.page = page;
    }

    fn next_page(&mut self) {
        let pages = Page::all();
        let current = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        self.page = pages[(current + 1) % pages.len()];
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
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Tab => app.next_page(),
                    KeyCode::Char('r') => app.refresh()?,
                    KeyCode::Char('1') => app.set_page(Page::Overview),
                    KeyCode::Char('2') => app.set_page(Page::Services),
                    KeyCode::Char('3') => app.set_page(Page::Sets),
                    KeyCode::Char('4') => app.set_page(Page::Topology),
                    KeyCode::Char('5') => app.set_page(Page::Health),
                    KeyCode::Char('6') => app.set_page(Page::Help),
                    _ => {}
                }
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
        Page::Overview => draw_overview(frame, app, layout[2]),
        Page::Services => draw_services(frame, app, layout[2]),
        Page::Sets => draw_sets(frame, app, layout[2]),
        Page::Topology => draw_topology(frame, app, layout[2]),
        Page::Health => draw_health(frame, app, layout[2]),
        Page::Help => draw_help(frame, layout[2]),
    }
    draw_footer(frame, layout[3]);
}

fn draw_header(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let text = vec![Line::from(vec![
        Span::styled(
            " OJOS Root Installer / Runtime Manager ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            "Service-first 控制面，Web Shell 仅为可热插拔业务 Service",
            Style::default().fg(Color::Gray),
        ),
    ])];
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
        area,
    );
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
                    .title("导航 1-6 / Tab"),
            ),
        area,
    );
}

fn draw_overview(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let ui_services = app
        .services
        .iter()
        .filter(|service| service.ui == "enabled")
        .count();
    let endpoint_count = app.services.len();
    let link_count: usize = app
        .sets
        .iter()
        .map(|set| set.links.parse::<usize>().unwrap_or(0))
        .sum();
    let lines = vec![
        Line::from(format!("Service 数量: {}", app.services.len())),
        Line::from(format!("Set 数量: {}", app.sets.len())),
        Line::from(format!("Endpoint 声明: {}", endpoint_count)),
        Line::from(format!("默认 Link 声明: {}", link_count)),
        Line::from(format!("启用 UI 的 Service: {}", ui_services)),
        Line::from(""),
        Line::from("Root Device 维护全局 Service、Set、Endpoint、Link、Device 与 Topology 状态。"),
        Line::from(
            "Non-root Device 只运行 node-agent 和后端 Service，不运行 Web Shell 或 Root Installer GUI。",
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Service-first 总览"),
        ),
        area,
    );
}

fn draw_services(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let rows = app.services.iter().map(|service| {
        Row::new(vec![
            Cell::from(service.id.clone()),
            Cell::from(service.name.clone()),
            Cell::from(service.version.clone()),
            Cell::from(service.kind.clone()),
            Cell::from(service.endpoint.clone()),
            Cell::from(service.runtime.clone()),
            Cell::from(service.ui.clone()),
            Cell::from(service.path.clone()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(22),
            Constraint::Length(18),
            Constraint::Length(10),
            Constraint::Length(16),
            Constraint::Length(18),
            Constraint::Length(14),
            Constraint::Length(9),
            Constraint::Min(16),
        ],
    )
    .header(
        Row::new(vec![
            "ID", "名称", "版本", "类型", "Endpoint", "Runtime", "UI", "路径",
        ])
        .style(Style::default().fg(Color::Yellow)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Service Registry"),
    );
    frame.render_widget(table, area);
}

fn draw_sets(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let rows = app.sets.iter().map(|set| {
        Row::new(vec![
            Cell::from(set.id.clone()),
            Cell::from(set.name.clone()),
            Cell::from(set.services.clone()),
            Cell::from(set.links.clone()),
            Cell::from(set.scope.clone()),
        ])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(24),
            Constraint::Length(24),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Min(16),
        ],
    )
    .header(
        Row::new(vec!["ID", "名称", "Service", "Link", "范围"])
            .style(Style::default().fg(Color::Yellow)),
    )
    .block(Block::default().borders(Borders::ALL).title("预设 Set"));
    frame.render_widget(table, area);
}

fn draw_topology(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mut lines = vec![
        Line::from("Device"),
        Line::from("  - root: Root Installer / Runtime Manager"),
        Line::from(""),
        Line::from("Endpoint"),
    ];
    for service in &app.services {
        lines.push(Line::from(format!(
            "  - {} -> {}",
            service.id, service.endpoint
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Link"));
    for set in load_raw_sets(&app.repo_root, &mut Vec::new()).unwrap_or_default() {
        for link in expand_set(&set).default_links {
            lines.push(Line::from(format!(
                "  - {} -> {} ({})",
                link.from,
                link.to,
                empty_to_default(&link.protocol, "runtime")
            )));
        }
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Topology 只读视角"),
        ),
        area,
    );
}

fn draw_health(frame: &mut ratatui::Frame<'_>, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if app.warnings.is_empty() {
        lines.push(Line::from("service.yaml 与 set.yaml 基础校验通过。"));
    } else {
        lines.push(Line::from("发现以下校验问题:"));
        for warning in &app.warnings {
            lines.push(Line::from(format!("  - {}", warning)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(
        "TUI 不执行危险 apply，不显示 secret/token/password，不直接控制 Non-root Device。",
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("Health")),
        area,
    );
}

fn draw_help(frame: &mut ratatui::Frame<'_>, area: Rect) {
    let lines = vec![
        Line::from("快捷键"),
        Line::from("  1-6 / Tab: 切换页面"),
        Line::from("  r: 重新读取 services/ 与 sets/"),
        Line::from("  q / Esc: 退出"),
        Line::from(""),
        Line::from("边界"),
        Line::from("  Service 是最小安装、运行、启停、热插拔和连接单位。"),
        Line::from(
            "  Endpoint 使用 IP:Port 标识；Link 使用 source endpoint -> target endpoint 标识。",
        ),
        Line::from("  Web Shell 是 Root 侧可热插拔 Service，不是 Installer 或 Runtime Manager。"),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("帮助")),
        area,
    );
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new("q 退出    r 刷新    Tab 切换    全局变更请使用 ojosctl 生成受控 plan")
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn load_services(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<ServiceRow>> {
    let mut rows = Vec::new();
    let services_dir = repo_root.join("services");
    if !services_dir.is_dir() {
        warnings.push("services/ 目录不存在".to_string());
        return Ok(rows);
    }
    for entry in fs::read_dir(&services_dir).context("读取 services/ 失败")? {
        let entry = entry.context("读取 Service 目录失败")?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let rel = Path::new("services")
            .join(entry.file_name())
            .join("service.yaml");
        if !repo_root.join(&rel).is_file() {
            continue;
        }
        match validate_service_manifest_file(repo_root, &rel) {
            Ok(manifest) => rows.push(service_row(manifest, &rel)),
            Err(err) => warnings.push(format!("{}: {}", rel.display(), err)),
        }
    }
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(rows)
}

fn service_row(manifest: ServiceManifest, rel: &Path) -> ServiceRow {
    let endpoint = format!("0.0.0.0:{}", manifest.endpoint.default_port);
    let endpoint = if validate_endpoint_id(&endpoint).is_ok() {
        endpoint
    } else {
        format!("invalid:{}", manifest.endpoint.default_port)
    };
    ServiceRow {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        kind: manifest.kind,
        endpoint,
        runtime: format!("{:?}", manifest.runtime.mode),
        ui: if manifest.ui.enabled {
            "enabled".to_string()
        } else {
            "disabled".to_string()
        },
        path: slash_path(rel),
    }
}

fn load_sets(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<SetRow>> {
    let sets = load_raw_sets(repo_root, warnings)?;
    let mut rows = sets
        .into_iter()
        .map(|set| {
            let expanded = expand_set(&set);
            SetRow {
                id: set.id,
                name: set.name,
                services: expanded.services.len().to_string(),
                links: expanded.default_links.len().to_string(),
                scope: if expanded.non_root_only {
                    "non-root only".to_string()
                } else {
                    "root allowed".to_string()
                },
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(rows)
}

fn load_raw_sets(repo_root: &Path, warnings: &mut Vec<String>) -> Result<Vec<ServiceSet>> {
    let mut sets = Vec::new();
    let sets_dir = repo_root.join("sets");
    if !sets_dir.is_dir() {
        warnings.push("sets/ 目录不存在".to_string());
        return Ok(sets);
    }
    for entry in fs::read_dir(&sets_dir).context("读取 sets/ 失败")? {
        let entry = entry.context("读取 Set 文件失败")?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("yaml") {
            continue;
        }
        let rel = Path::new("sets").join(entry.file_name());
        match validate_service_set_file(repo_root, &rel) {
            Ok(set) => sets.push(set),
            Err(err) => warnings.push(format!("{}: {}", rel.display(), err)),
        }
    }
    Ok(sets)
}

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn empty_to_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.trim().is_empty() {
        default
    } else {
        value
    }
}
