use anyhow::Result;
use clap::Parser;
use eframe::egui;
use orchestrator_core::{
    OperationWorkbenchContext, OperationWorkbenchSession, OperationWorkbenchView, OrchestratorView,
    OrchestratorViewPage, endpoint_hosts, load_operation_workbench_context, load_orchestrator_view,
};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ojos-orchestrator-gui")]
#[command(about = "OJOS Orchestrator 原生 GUI 入口")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
}

struct GuiApp {
    repo_root: PathBuf,
    page: OrchestratorViewPage,
    context: OperationWorkbenchContext,
    session: OperationWorkbenchSession,
    view: OrchestratorView,
    last_error: Option<String>,
}

impl GuiApp {
    fn new(repo_root: PathBuf) -> Result<Self> {
        let context = load_operation_workbench_context(&repo_root)?;
        let session = context.build_session("service.install")?;
        let view = load_orchestrator_view(&repo_root)?;
        Ok(Self {
            repo_root,
            page: OrchestratorViewPage::Overview,
            context,
            session,
            view,
            last_error: None,
        })
    }

    fn refresh(&mut self) {
        match (
            load_operation_workbench_context(&self.repo_root),
            load_orchestrator_view(&self.repo_root),
        ) {
            (Ok(context), Ok(view)) => {
                let action = self.session.workbench.selected_action.clone();
                match context
                    .build_session_from_request(&self.session.workbench.request)
                    .or_else(|_| context.build_session(&action))
                    .or_else(|_| context.build_session("service.install"))
                {
                    Ok(session) => {
                        self.context = context;
                        self.session = session;
                        self.view = view;
                        self.last_error = None;
                    }
                    Err(err) => {
                        self.last_error = Some(err.to_string());
                    }
                }
            }
            (Err(err), _) | (_, Err(err)) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn select_action(&mut self, action: &str) {
        match self.context.build_session(action) {
            Ok(session) => {
                self.session = session;
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn update_field(&mut self, field: &str, value: String) {
        match self.context.update_field(&self.session, field, value) {
            Ok(session) => {
                self.session = session;
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn confirm_session(&mut self) {
        match self.context.confirm(&self.session) {
            Ok(session) => {
                self.session = session;
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn apply_session(&mut self) {
        match self.context.apply(&self.session) {
            Ok(session) => {
                self.session = session;
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn rollback_session(&mut self) {
        match self.context.rollback(&self.session) {
            Ok(session) => {
                self.session = session;
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("OJOS Orchestrator");
                ui.label("Service / Set / Endpoint / Link / Operation / Topology / LogView / DiagnosticReport");
                if ui.button("刷新").clicked() {
                    self.refresh();
                }
            });
            if let Some(error) = &self.last_error {
                ui.colored_label(egui::Color32::RED, error);
            }
        });

        egui::SidePanel::left("navigation")
            .resizable(false)
            .default_width(180.0)
            .show(ctx, |ui| {
                ui.heading("对象");
                for page in OrchestratorViewPage::all() {
                    if ui
                        .selectable_label(self.page == *page, page.title())
                        .clicked()
                    {
                        self.page = *page;
                    }
                }
                ui.separator();
                ui.label("GUI 与 TUI 使用同一套 orchestrator/core 视图。");
                ui.label("Web Shell 与 Gateway 都只是被编排的 Service。");
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.page {
                OrchestratorViewPage::Overview => draw_overview(ui, &self.view),
                OrchestratorViewPage::Services => draw_services(ui, &self.view),
                OrchestratorViewPage::Sets => draw_sets(ui, &self.view),
                OrchestratorViewPage::Endpoints => draw_endpoints(ui, &self.view),
                OrchestratorViewPage::Links => draw_links(ui, &self.view),
                OrchestratorViewPage::Operations => draw_operations(ui, self),
                OrchestratorViewPage::Topology => draw_topology(ui, &self.view),
                OrchestratorViewPage::Logs => draw_logs(ui, &self.view),
                OrchestratorViewPage::Diagnostics => draw_diagnostics(ui, &self.view),
            });
        });
    }
}

fn draw_overview(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("核心对象总览");
    egui::Grid::new("overview_grid")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            metric(ui, "Action", view.schemas.action_count());
            metric(ui, "Form", view.schemas.form_count());
            metric(ui, "Service", view.services.len());
            metric(ui, "Set", view.sets.len());
            metric(ui, "Endpoint", view.endpoints.len());
            metric(ui, "Link", view.links.len());
            metric(ui, "Operation", view.operations.len());
            metric(ui, "LogView", view.logs.len());
            metric(ui, "DiagnosticReport", view.diagnostics.len());
            metric(ui, "Warning", view.warnings.len());
        });
    ui.separator();
    ui.label("Endpoint 使用 IP:Port 作为运行时唯一身份。");
    ui.label("Topology 修改必须经 Operation 计划和确认。");
}

fn metric(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.label(label);
    ui.monospace(value.to_string());
    ui.end_row();
}

fn draw_services(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("Service");
    egui::Grid::new("services_grid")
        .striped(true)
        .show(ui, |ui| {
            header(
                ui,
                &[
                    "ID", "名称", "版本", "类型", "Endpoint", "Runtime", "UI", "Health",
                ],
            );
            for service in &view.services {
                ui.monospace(&service.id);
                ui.label(&service.name);
                ui.label(&service.version);
                ui.label(&service.kind);
                ui.monospace(&service.endpoint);
                ui.label(&service.runtime);
                ui.label(&service.ui);
                ui.label(&service.health);
                ui.end_row();
            }
        });
}

fn draw_sets(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("Set");
    egui::Grid::new("sets_grid").striped(true).show(ui, |ui| {
        header(ui, &["ID", "名称", "Service", "Link", "范围"]);
        for set in &view.sets {
            ui.monospace(&set.id);
            ui.label(&set.name);
            ui.label(&set.services);
            ui.label(&set.links);
            ui.label(&set.scope);
            ui.end_row();
        }
    });
}

fn draw_endpoints(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("Endpoint = IP:Port");
    egui::Grid::new("endpoints_grid")
        .striped(true)
        .show(ui, |ui| {
            header(ui, &["Endpoint", "Service", "协议", "暴露", "来源"]);
            for endpoint in &view.endpoints {
                ui.monospace(&endpoint.endpoint);
                ui.label(&endpoint.service_id);
                ui.label(&endpoint.protocol);
                ui.label(&endpoint.expose);
                ui.label(&endpoint.source);
                ui.end_row();
            }
        });
}

fn draw_links(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("Link");
    egui::Grid::new("links_grid").striped(true).show(ui, |ui| {
        header(ui, &["Source", "Target", "协议", "认证", "范围", "来源"]);
        for link in &view.links {
            ui.label(&link.from);
            ui.label(&link.to);
            ui.label(&link.protocol);
            ui.label(&link.auth_mode);
            ui.label(&link.scope);
            ui.label(&link.source);
            ui.end_row();
        }
    });
}

fn draw_operations(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Operation 工作台");
    let workbench = OperationWorkbenchView::from_session(&app.session);
    draw_operation_workbench(ui, app, &workbench);
    ui.heading("Operation Action Registry");
    egui::Grid::new("operations_grid")
        .striped(true)
        .show(ui, |ui| {
            header(
                ui,
                &[
                    "选择",
                    "Action",
                    "对象",
                    "风险",
                    "模式",
                    "Plan",
                    "字段",
                    "预览目标",
                    "预览步骤",
                    "需确认",
                    "摘要",
                ],
            );
            for operation in app.view.operations.clone() {
                if ui.button("选用").clicked() {
                    app.select_action(&operation.action);
                }
                ui.monospace(&operation.action);
                ui.label(&operation.target);
                ui.label(&operation.risk);
                ui.label(&operation.mode);
                ui.label(&operation.plan_required);
                ui.label(&operation.fields);
                ui.label(&operation.preview_target);
                ui.label(&operation.preview_steps);
                ui.label(&operation.preview_confirmation);
                ui.label(&operation.summary);
                ui.end_row();
            }
        });
}

fn draw_operation_workbench(
    ui: &mut egui::Ui,
    app: &mut GuiApp,
    workbench: &OperationWorkbenchView,
) {
    egui::Grid::new("operation_workbench_grid")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.strong("当前 Action");
            ui.monospace(&workbench.selected_action);
            ui.end_row();
            ui.strong("Operation");
            ui.monospace(&workbench.operation_id);
            ui.end_row();
            ui.strong("目标");
            ui.label(&workbench.target);
            ui.end_row();
            ui.strong("字段");
            ui.label(&workbench.fields);
            ui.end_row();
            ui.strong("当前状态");
            ui.label(&workbench.current_status);
            ui.end_row();
            ui.strong("结果");
            ui.label(if workbench.result_status.is_empty() {
                "待执行"
            } else {
                &workbench.result_status
            });
            ui.end_row();
            ui.strong("日志");
            ui.label(workbench.log_count.to_string());
            ui.end_row();
            ui.strong("预览步骤");
            ui.label(&workbench.preview_steps);
            ui.end_row();
            ui.strong("需确认");
            ui.label(&workbench.requires_confirmation);
            ui.end_row();
            ui.strong("可执行");
            ui.label(&workbench.can_apply);
            ui.end_row();
            ui.strong("可回滚");
            ui.label(&workbench.rollback);
            ui.end_row();
            ui.strong("提示");
            ui.label(&workbench.warnings);
            ui.end_row();
        });
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("确认").clicked() {
            app.confirm_session();
        }
        if ui.button("执行").clicked() {
            app.apply_session();
        }
        if ui.button("回滚").clicked() {
            app.rollback_session();
        }
    });
    ui.separator();
    ui.strong("表单字段");
    let fields = workbench.editable_fields.clone();
    egui::Grid::new("operation_workbench_fields_grid")
        .striped(true)
        .show(ui, |ui| {
            header(ui, &["字段", "类型", "必填", "当前值"]);
            for field in fields {
                ui.monospace(&field.name);
                ui.label(&field.field_type);
                ui.label(if field.required { "是" } else { "否" });
                let mut value = field.value.clone();
                if ui.text_edit_singleline(&mut value).changed() {
                    app.update_field(&field.name, value);
                }
                ui.end_row();
            }
        });
    ui.separator();
}

fn draw_topology(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("Topology");
    ui.label("Topology 由核心对象和 Endpoint/Link 关系生成，不引入额外主机对象。");
    ui.separator();
    ui.label("Endpoint host/IP 分组");
    for host in endpoint_hosts(&view.endpoints) {
        ui.monospace(host);
    }
    ui.separator();
    ui.label("Link");
    for link in &view.links {
        ui.label(format!("{} -> {} ({})", link.from, link.to, link.protocol));
    }
}

fn draw_logs(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("LogView");
    egui::Grid::new("logs_grid").striped(true).show(ui, |ui| {
        header(ui, &["Source", "Service", "Endpoint", "位置"]);
        for log in &view.logs {
            ui.label(&log.source_id);
            ui.label(&log.service_id);
            ui.monospace(&log.endpoint);
            ui.label(&log.path);
            ui.end_row();
        }
    });
}

fn draw_diagnostics(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("DiagnosticReport");
    egui::Grid::new("diagnostics_grid")
        .striped(true)
        .show(ui, |ui| {
            header(ui, &["目标", "状态", "摘要"]);
            for diagnostic in &view.diagnostics {
                ui.label(&diagnostic.target);
                ui.label(&diagnostic.status);
                ui.label(&diagnostic.summary);
                ui.end_row();
            }
        });
}

fn header(ui: &mut egui::Ui, labels: &[&str]) {
    for label in labels {
        ui.strong(*label);
    }
    ui.end_row();
}

fn main() -> Result<()> {
    configure_utf8_console();
    let cli = Cli::parse();
    let repo_root = fs::canonicalize(&cli.repo_root).unwrap_or(cli.repo_root);
    let app = GuiApp::new(repo_root)?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "OJOS Orchestrator",
        options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

fn configure_utf8_console() {
    #[cfg(windows)]
    {
        const CP_UTF8: u32 = 65001;
        unsafe {
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }
    }
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

    #[test]
    fn gui_pages_cover_the_same_core_objects_as_tui() {
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
    fn gui_loads_shared_operation_workbench_from_core() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf();
        let app = GuiApp::new(repo_root).expect("GUI app should load orchestrator/core view");
        let workbench = OperationWorkbenchView::from_session(&app.session);
        assert_eq!(workbench.selected_action, "service.install");
        assert!(workbench.fields.contains("service_id*"));
        assert!(
            workbench
                .editable_fields
                .iter()
                .any(|field| field.name == "service_id"
                    && field.required
                    && field.value == "gateway")
        );
        assert_eq!(workbench.current_status, "Planned");
        assert!(workbench.result_status.is_empty());
        assert_eq!(workbench.log_count, 0);
        assert!(!workbench.preview_steps.is_empty());
    }

    #[test]
    fn gui_workbench_uses_core_session_for_updates_and_apply() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf();
        let mut app = GuiApp::new(repo_root).expect("GUI app should load");
        app.update_field("service_id", "problem-api".to_string());
        assert_eq!(
            app.session.workbench.request.field("service_id"),
            Some("problem-api")
        );
        assert_eq!(app.session.workbench.preview.target_id, "problem-api");
        app.confirm_session();
        app.apply_session();
        assert_eq!(app.session.result_status, "SUCCEEDED");
        app.rollback_session();
        assert_eq!(app.session.result_status, "ROLLED_BACK");
    }
}
