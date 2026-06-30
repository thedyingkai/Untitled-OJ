use anyhow::Result;
use clap::Parser;
use eframe::egui;
use orchestrator_core::{
    ActionRequest, OperationWorkbenchContext, OperationWorkbenchSession, OperationWorkbenchView,
    OrchestratorActionConsole, OrchestratorView, OrchestratorViewPage, endpoint_hosts,
    merge_operation_workbench_session_into_view,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "ojos-orchestrator-gui")]
#[command(about = "OJOS Orchestrator native GUI")]
#[command(version)]
struct Cli {
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
}

struct GuiApp {
    page: OrchestratorViewPage,
    console: OrchestratorActionConsole,
    context: OperationWorkbenchContext,
    session: OperationWorkbenchSession,
    view: OrchestratorView,
    last_error: Option<String>,
}

impl GuiApp {
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
            view,
            last_error: None,
        })
    }

    fn refresh(&mut self) {
        match (self.console.context(), self.console.view()) {
            (Ok(context), Ok(view)) => {
                let action = self.session.workbench.selected_action.clone();
                match context
                    .build_session_from_request(&self.session.workbench.request)
                    .or_else(|_| context.build_session(&action))
                    .or_else(|_| context.build_session("release.install"))
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
                self.set_session(session);
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
                self.set_session(session);
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
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
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                return;
            }
        }
        self.apply_session();
    }

    fn run_endpoint_action(&mut self, action: &str, endpoint: Option<String>) {
        match endpoint {
            Some(endpoint) => self.run_action_with_fields(action, [("endpoint", endpoint)]),
            None => self.run_action(action),
        }
    }

    fn run_link_action(&mut self, action: &str, source: Option<String>, target: Option<String>) {
        match (source, target) {
            (Some(source), Some(target)) => self.run_action_with_fields(
                action,
                [("source_endpoint", source), ("target_endpoint", target)],
            ),
            _ => self.run_action(action),
        }
    }

    fn run_operation_logs_view(&mut self, operation_id: Option<String>) {
        match operation_id {
            Some(operation_id) => {
                self.run_action_with_fields("log.query", [("operation_id", operation_id)])
            }
            None => self.run_action("log.query"),
        }
    }

    fn run_release_action(&mut self, action: &str, service_name: Option<String>) {
        match service_name {
            Some(service_name) => {
                self.run_action_with_fields(action, [("service_id", service_name)])
            }
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
                self.refresh();
                self.last_error = Some(message);
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
            }
        }
    }

    fn set_session(&mut self, session: OperationWorkbenchSession) {
        merge_operation_workbench_session_into_view(&mut self.view, &session);
        self.session = session;
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("OJOS Orchestrator");
                ui.label("Service / Template / Endpoint / Link / Operation / Topology / LogView / DiagnosticReport");
                if ui.button("Refresh").clicked() {
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
                ui.heading("Objects");
                for page in OrchestratorViewPage::all() {
                    if ui
                        .selectable_label(self.page == *page, page.title())
                        .clicked()
                    {
                        self.page = *page;
                    }
                }
                ui.separator();
                ui.label("GUI and TUI use the same orchestrator/core view.");
                ui.label("Web Shell and Gateway are managed Services.");
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.page {
                OrchestratorViewPage::Overview => draw_overview(ui, &self.view),
                OrchestratorViewPage::Services => draw_services(ui, self),
                OrchestratorViewPage::Templates => draw_templates(ui, self),
                OrchestratorViewPage::Endpoints => draw_endpoints(ui, self),
                OrchestratorViewPage::Links => draw_links(ui, self),
                OrchestratorViewPage::Operations => draw_operations(ui, self),
                OrchestratorViewPage::Topology => draw_topology(ui, &self.view),
                OrchestratorViewPage::Logs => draw_logs(ui, &self.view),
                OrchestratorViewPage::Diagnostics => draw_diagnostics(ui, self),
            });
        });
    }
}

fn draw_overview(ui: &mut egui::Ui, view: &OrchestratorView) {
    ui.heading("Core Object Overview");
    egui::Grid::new("overview_grid")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            metric(ui, "Action", view.schemas.action_count());
            metric(ui, "Form", view.schemas.form_count());
            metric(ui, "Service", view.services.len());
            metric(ui, "Release Registry", view.release_registry.len());
            metric(ui, "Template", view.templates.len());
            metric(ui, "Endpoint", view.endpoints.len());
            metric(ui, "Link", view.links.len());
            metric(ui, "Operation", view.operations.len());
            metric(ui, "LogView", view.logs.len());
            metric(ui, "DiagnosticReport", view.diagnostics.len());
            metric(ui, "Warning", view.warnings.len());
        });
    ui.separator();
    ui.label("Endpoint uses ip:port:service-name as the runtime identity.");
    ui.label("Topology mutations go through planned and confirmed Operations.");
}

fn metric(ui: &mut egui::Ui, label: &str, value: usize) {
    ui.label(label);
    ui.monospace(value.to_string());
    ui.end_row();
}

fn draw_services(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Service");
    egui::Grid::new("services_grid")
        .striped(true)
        .show(ui, |ui| {
            header(
                ui,
                &[
                    "ID", "Name", "Version", "Kind", "Endpoint", "Runtime", "UI", "Health",
                ],
            );
            for service in &app.view.services {
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
    ui.separator();
    let selected_release_service = app
        .view
        .release_registry
        .iter()
        .find(|record| record.record_type == "release")
        .map(|record| record.service_name.clone())
        .or_else(|| app.view.services.first().map(|service| service.id.clone()));
    ui.horizontal(|ui| {
        ui.strong("Release Actions");
        if ui.button("Create").clicked() {
            app.run_release_action("release.create", selected_release_service.clone());
        }
        if ui.button("Update").clicked() {
            app.run_release_action("release.update", selected_release_service.clone());
        }
        if ui.button("Install").clicked() {
            app.run_release_action("release.install", selected_release_service.clone());
        }
        if ui.button("Delete").clicked() {
            app.run_release_action("release.delete", selected_release_service.clone());
        }
        if ui.button("Rollback").clicked() {
            app.run_release_action("release.rollback", selected_release_service.clone());
        }
        if ui.button("Validate").clicked() {
            app.run_release_action("release.validate", selected_release_service.clone());
        }
    });
    ui.separator();
    ui.heading("Service Release Registry");
    egui::Grid::new("release_registry_grid")
        .striped(true)
        .show(ui, |ui| {
            header(
                ui,
                &["Service", "Version", "Record", "Name", "Detail", "Source"],
            );
            for record in &app.view.release_registry {
                ui.monospace(&record.service_name);
                ui.label(&record.version);
                ui.label(&record.record_type);
                ui.monospace(&record.name);
                ui.label(&record.detail);
                ui.label(&record.source);
                ui.end_row();
            }
        });
}

fn draw_templates(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Deployment Templates");
    ui.label("Service-name[*] is a derived endpoint query; local templates are read-only.");
    ui.separator();
    egui::Grid::new("sets_grid").striped(true).show(ui, |ui| {
        header(ui, &["ID", "Name", "Service", "Link", "Scope"]);
        for set in &app.view.templates {
            ui.monospace(&set.id);
            ui.label(&set.name);
            ui.label(&set.services);
            ui.label(&set.links);
            ui.label(&set.scope);
            ui.end_row();
        }
    });
}

fn draw_endpoints(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Endpoint = ip:port:service-name");
    let selected_endpoint = app
        .view
        .endpoints
        .first()
        .map(|endpoint| endpoint.endpoint.clone());
    ui.horizontal(|ui| {
        ui.strong("Endpoint Actions");
        if ui.button("Create Endpoint").clicked() {
            app.run_action("endpoint.create");
        }
        if ui.button("Update Endpoint").clicked() {
            app.run_endpoint_action("endpoint.update", selected_endpoint.clone());
        }
        if ui.button("Delete Endpoint").clicked() {
            app.run_endpoint_action("endpoint.delete", selected_endpoint.clone());
        }
        if ui.button("Check Endpoint Health").clicked() {
            app.run_endpoint_action("endpoint.health.check", selected_endpoint.clone());
        }
    });
    ui.separator();
    egui::Grid::new("endpoints_grid")
        .striped(true)
        .show(ui, |ui| {
            header(ui, &["Endpoint", "Service", "Protocol", "Expose", "Source"]);
            for endpoint in &app.view.endpoints {
                ui.monospace(&endpoint.endpoint);
                ui.label(&endpoint.service_id);
                ui.label(&endpoint.protocol);
                ui.label(&endpoint.expose);
                ui.label(&endpoint.source);
                ui.end_row();
            }
        });
}

fn draw_links(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Link");
    let selected_link = app
        .view
        .links
        .first()
        .map(|link| (link.from.clone(), link.to.clone()));
    let source = selected_link.as_ref().map(|(source, _)| source.clone());
    let target = selected_link.as_ref().map(|(_, target)| target.clone());
    ui.horizontal(|ui| {
        ui.strong("Link Actions");
        if ui.button("Create Link").clicked() {
            app.run_action("link.create");
        }
        if ui.button("Update Link").clicked() {
            app.run_link_action("link.update", source.clone(), target.clone());
        }
        if ui.button("Delete Link").clicked() {
            app.run_link_action("link.delete", source.clone(), target.clone());
        }
        if ui.button("Check Link Health").clicked() {
            app.run_link_action("link.health.check", source.clone(), target.clone());
        }
    });
    ui.separator();
    egui::Grid::new("links_grid").striped(true).show(ui, |ui| {
        header(
            ui,
            &[
                "Source",
                "Target",
                "Protocol",
                "Auth",
                "Scope",
                "Source Type",
            ],
        );
        for link in &app.view.links {
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
    ui.heading("Operation Workbench");
    let selected_operation = app
        .view
        .operations
        .iter()
        .find(|operation| operation.status != "CATALOG")
        .map(|operation| operation.operation_id.clone())
        .or_else(|| Some(app.session.current_operation.operation_id.clone()));
    ui.horizontal(|ui| {
        ui.strong("Operation Actions");
        if ui.button("Confirm").clicked() {
            app.confirm_session();
        }
        if ui.button("Apply").clicked() {
            app.apply_session();
        }
        if ui.button("Rollback").clicked() {
            app.rollback_session();
        }
        if ui.button("View Logs").clicked() {
            app.run_operation_logs_view(selected_operation.clone());
        }
    });
    ui.separator();
    let workbench = OperationWorkbenchView::from_session(&app.session);
    draw_operation_workbench(ui, app, &workbench);
    ui.heading("Operation Action Registry");
    egui::Grid::new("operations_grid")
        .striped(true)
        .show(ui, |ui| {
            header(
                ui,
                &[
                    "Select",
                    "Operation",
                    "Action",
                    "Target",
                    "Status",
                    "Risk",
                    "Mode",
                    "Plan",
                    "Fields",
                    "Preview Target",
                    "Preview Steps",
                    "Confirm",
                    "Result",
                    "Error",
                    "Logs",
                    "Summary",
                    "Created",
                    "Updated",
                ],
            );
            for operation in app.view.operations.clone() {
                if ui.button("Use").clicked() {
                    app.select_action(&operation.action);
                }
                ui.monospace(&operation.operation_id);
                ui.monospace(&operation.action);
                ui.label(&operation.target);
                ui.label(&operation.status);
                ui.label(&operation.risk);
                ui.label(&operation.mode);
                ui.label(&operation.plan_required);
                ui.label(&operation.fields);
                ui.label(&operation.preview_target);
                ui.label(&operation.preview_steps);
                ui.label(&operation.preview_confirmation);
                ui.label(&operation.result);
                ui.label(&operation.error);
                ui.label(operation.log_count.to_string());
                ui.label(&operation.summary);
                ui.label(&operation.created_at);
                ui.label(&operation.updated_at);
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
            ui.strong("Current Action");
            ui.monospace(&workbench.selected_action);
            ui.end_row();
            ui.strong("Operation");
            ui.monospace(&workbench.operation_id);
            ui.end_row();
            ui.strong("Target");
            ui.label(&workbench.target);
            ui.end_row();
            ui.strong("Fields");
            ui.label(&workbench.fields);
            ui.end_row();
            ui.strong("Current Status");
            ui.label(&workbench.current_status);
            ui.end_row();
            ui.strong("Result");
            ui.label(if workbench.result_status.is_empty() {
                "Pending"
            } else {
                &workbench.result_status
            });
            ui.end_row();
            ui.strong("Logs");
            ui.label(workbench.log_count.to_string());
            ui.end_row();
            ui.strong("Preview Steps");
            ui.label(&workbench.preview_steps);
            ui.end_row();
            ui.strong("Requires Confirmation");
            ui.label(&workbench.requires_confirmation);
            ui.end_row();
            ui.strong("Can Apply");
            ui.label(&workbench.can_apply);
            ui.end_row();
            ui.strong("Can Rollback");
            ui.label(&workbench.rollback);
            ui.end_row();
            ui.strong("Warnings");
            ui.label(&workbench.warnings);
            ui.end_row();
        });
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("Confirm").clicked() {
            app.confirm_session();
        }
        if ui.button("Apply Action").clicked() {
            app.apply_session();
        }
        if ui.button("Rollback").clicked() {
            app.rollback_session();
        }
    });
    ui.separator();
    ui.strong("Form Fields");
    let fields = workbench.editable_fields.clone();
    egui::Grid::new("operation_workbench_fields_grid")
        .striped(true)
        .show(ui, |ui| {
            header(ui, &["Field", "Type", "Required", "Current Value"]);
            for field in fields {
                ui.monospace(&field.name);
                ui.label(&field.field_type);
                ui.label(if field.required { "yes" } else { "no" });
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
    ui.label("Topology is derived from Service, Endpoint, and Link state.");
    ui.separator();
    ui.label("Endpoint host/IP groups");
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
        header(
            ui,
            &[
                "Source",
                "Service",
                "Endpoint",
                "Operation",
                "Level",
                "Message",
                "Path",
            ],
        );
        for log in &view.logs {
            ui.label(&log.source_id);
            ui.label(&log.service_id);
            ui.monospace(&log.endpoint);
            ui.monospace(&log.operation_id);
            ui.label(&log.level);
            ui.label(&log.message);
            ui.label(&log.path);
            ui.end_row();
        }
    });
}

fn draw_diagnostics(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("DiagnosticReport");
    ui.horizontal(|ui| {
        ui.strong("Diagnostics");
        if ui.button("Create DiagnosticReport").clicked() {
            app.run_action("diagnostic.create");
        }
        if ui.button("Export JSON").clicked() {
            app.run_action_with_fields("diagnostic.export", [("format", "json".to_string())]);
        }
        if ui.button("Export Markdown").clicked() {
            app.run_action_with_fields("diagnostic.export", [("format", "markdown".to_string())]);
        }
    });
    ui.separator();
    egui::Grid::new("diagnostics_grid")
        .striped(true)
        .show(ui, |ui| {
            header(ui, &["Target", "Status", "Summary"]);
            for diagnostic in &app.view.diagnostics {
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
    configure_utf8_console()?;
    let cli = Cli::parse();
    let repo_root = fs::canonicalize(&cli.repo_root).unwrap_or(cli.repo_root);
    let app = GuiApp::new(repo_root)?;
    let gui_font = load_required_gui_font()?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        "OJOS Orchestrator",
        options,
        Box::new(move |cc| {
            cc.egui_ctx
                .set_fonts(gui_font_definitions(gui_font.clone()));
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

#[derive(Clone, Debug)]
struct LoadedGuiFont {
    name: String,
    bytes: Vec<u8>,
}

fn load_required_gui_font() -> Result<LoadedGuiFont> {
    for path in gui_font_candidates() {
        if path.is_file() {
            return Ok(LoadedGuiFont {
                name: path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("ojos-cjk-font")
                    .to_string(),
                bytes: fs::read(&path)?,
            });
        }
    }
    anyhow::bail!("no usable GUI font found; install Noto Sans CJK or a system CJK font")
}

fn gui_font_definitions(font: LoadedGuiFont) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let font_key = format!("ojos-cjk-{}", font.name);
    fonts.font_data.insert(
        font_key.clone(),
        Arc::new(egui::FontData::from_owned(font.bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, font_key.clone());
    }
    fonts
}

fn gui_font_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("OJOS_ORCHESTRATOR_GUI_FONT") {
        paths.push(PathBuf::from(path));
    }
    paths.extend([
        PathBuf::from(r"C:\Windows\Fonts\NotoSansSC-VF.ttf"),
        PathBuf::from(r"C:\Windows\Fonts\msyh.ttc"),
        PathBuf::from(r"C:\Windows\Fonts\simhei.ttf"),
        PathBuf::from(r"C:\Windows\Fonts\simsun.ttc"),
        PathBuf::from("/System/Library/Fonts/PingFang.ttc"),
        PathBuf::from("/System/Library/Fonts/STHeiti Light.ttc"),
        PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc"),
        PathBuf::from("/usr/share/fonts/truetype/noto/NotoSansSC-Regular.otf"),
        PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"),
    ]);
    paths
}

fn configure_utf8_console() -> Result<()> {
    #[cfg(windows)]
    {
        const CP_UTF8: u32 = 65001;
        let output_ok = unsafe { SetConsoleOutputCP(CP_UTF8) } != 0;
        let input_ok = unsafe { SetConsoleCP(CP_UTF8) } != 0;
        if !output_ok || !input_ok {
            anyhow::bail!("failed to configure Windows console UTF-8");
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

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|path| path.parent())
            .expect("repo root")
            .to_path_buf()
    }

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
    fn gui_loads_shared_operation_workbench_from_core() {
        let app =
            GuiApp::new_memory(repo_root()).expect("GUI app should load orchestrator/core view");
        assert!(
            app.view.templates.is_empty(),
            "console store view must not expose deployment templates as formal objects"
        );
        let workbench = OperationWorkbenchView::from_session(&app.session);
        assert_eq!(workbench.selected_action, "release.install");
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
    fn gui_exposes_dispatcher_backed_actions() {
        let mut app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        app.select_action("endpoint.create");
        app.update_field("endpoint", "127.0.0.1:19001:gateway".to_string());
        app.update_field("service_id", "gateway".to_string());
        app.update_field("protocol", "http".to_string());
        assert_eq!(
            app.session.workbench.request.field("endpoint"),
            Some("127.0.0.1:19001:gateway")
        );
        app.apply_session();
        let endpoint_operation_id = app.session.current_operation.operation_id.clone();
        assert!(
            app.view
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19001:gateway"),
            "GUI action console should write Endpoint into Store-backed view"
        );
        let expected_log_source = format!("operation:{endpoint_operation_id}");
        app.run_action_with_fields("log.query", [("operation_id", endpoint_operation_id)]);
        assert!(
            app.view
                .logs
                .iter()
                .any(|log| log.source_id == expected_log_source),
            "GUI action console should expose log.query as the LogView layer"
        );

        app.select_action("service.start");
        app.apply_session();
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("UNSUPPORTED")),
            "GUI must not report unsupported lifecycle actions as success"
        );
    }

    #[test]
    fn gui_endpoint_actions_are_directly_available() {
        let mut app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        app.run_action_with_fields(
            "endpoint.create",
            [
                ("endpoint", "127.0.0.1:19201:gateway".to_string()),
                ("service_id", "gateway".to_string()),
                ("protocol", "http".to_string()),
            ],
        );
        assert!(
            app.view
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19201:gateway")
        );
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("STORE_BACKED"))
        );

        app.run_endpoint_action(
            "endpoint.health.check",
            Some("127.0.0.1:19201:gateway".to_string()),
        );
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("REAL"))
        );

        app.run_endpoint_action(
            "endpoint.update",
            Some("127.0.0.1:19201:gateway".to_string()),
        );
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("STORE_BACKED"))
        );

        app.run_endpoint_action(
            "endpoint.delete",
            Some("127.0.0.1:19201:gateway".to_string()),
        );
        assert!(
            !app.view
                .endpoints
                .iter()
                .any(|endpoint| endpoint.endpoint == "127.0.0.1:19201:gateway")
        );
    }

    #[test]
    fn gui_release_actions_are_directly_available() {
        let mut app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        app.run_release_action("release.create", Some("gateway".to_string()));
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("STORE_BACKED"))
        );

        app.run_release_action("release.update", Some("gateway".to_string()));
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("STORE_BACKED"))
        );

        app.run_release_action("release.install", Some("gateway".to_string()));
        assert!(
            app.view
                .release_registry
                .iter()
                .any(|row| row.service_name == "gateway" && row.record_type == "route")
        );

        app.run_release_action("release.validate", Some("gateway".to_string()));
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("READONLY"))
        );
    }

    #[test]
    fn gui_link_actions_are_directly_available() {
        let mut app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        for (endpoint, service_id) in [
            ("127.0.0.1:19210:gateway", "gateway"),
            ("127.0.0.1:19211:auth-service", "auth-service"),
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
                ("source_endpoint", "127.0.0.1:19210:gateway".to_string()),
                (
                    "target_endpoint",
                    "127.0.0.1:19211:auth-service".to_string(),
                ),
            ],
        );
        assert!(app.view.links.iter().any(|link| {
            link.from == "127.0.0.1:19210:gateway" && link.to == "127.0.0.1:19211:auth-service"
        }));

        app.run_link_action(
            "link.health.check",
            Some("127.0.0.1:19210:gateway".to_string()),
            Some("127.0.0.1:19211:auth-service".to_string()),
        );
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("REAL"))
        );

        app.run_link_action(
            "link.update",
            Some("127.0.0.1:19210:gateway".to_string()),
            Some("127.0.0.1:19211:auth-service".to_string()),
        );
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("STORE_BACKED"))
        );

        app.run_link_action(
            "link.delete",
            Some("127.0.0.1:19210:gateway".to_string()),
            Some("127.0.0.1:19211:auth-service".to_string()),
        );
        assert!(!app.view.links.iter().any(|link| {
            link.from == "127.0.0.1:19210:gateway" && link.to == "127.0.0.1:19211:auth-service"
        }));
    }

    #[test]
    fn gui_set_templates_are_readonly() {
        let repo_view = orchestrator_core::load_orchestrator_view(&repo_root()).expect("repo view");
        assert!(!repo_view.templates.is_empty());
        let app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        assert!(app.view.templates.is_empty());
        assert!(
            app.view
                .operations
                .iter()
                .all(|operation| !operation.action.starts_with("set."))
        );
    }

    #[test]
    fn gui_diagnostics_export_is_directly_available() {
        let mut app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        app.run_action("diagnostic.create");
        assert!(
            app.view
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.target.contains("Topology"))
        );

        app.run_action_with_fields("diagnostic.export", [("format", "markdown".to_string())]);
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("STORE_BACKED"))
        );
    }

    #[test]
    fn gui_action_feedback_shows_capability_status() {
        let mut app = GuiApp::new_memory(repo_root()).expect("GUI app should load");
        app.run_action("service.start");
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|message| message.contains("UNSUPPORTED"))
        );
    }

    #[test]
    fn gui_fonts_force_required_cjk_font_for_all_text_styles() {
        let fonts = gui_font_definitions(LoadedGuiFont {
            name: "test-cjk.ttf".to_string(),
            bytes: vec![0],
        });
        let proportional = fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .expect("proportional family");
        let monospace = fonts
            .families
            .get(&egui::FontFamily::Monospace)
            .expect("monospace family");
        assert_eq!(
            proportional.first(),
            Some(&"ojos-cjk-test-cjk.ttf".to_string())
        );
        assert_eq!(
            monospace.first(),
            Some(&"ojos-cjk-test-cjk.ttf".to_string())
        );
        assert!(fonts.font_data.contains_key("ojos-cjk-test-cjk.ttf"));
    }

    #[test]
    fn gui_font_candidates_cover_windows_cjk_fonts() {
        let candidates = gui_font_candidates();
        let names = candidates
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .collect::<Vec<_>>();
        assert!(
            names.contains(&"NotoSansSC-VF.ttf")
                && names.contains(&"msyh.ttc")
                && names.contains(&"simhei.ttf")
                && names.contains(&"simsun.ttc")
        );
    }

    #[test]
    fn gui_source_keeps_utf8_chinese_text_without_mojibake() {
        let source =
            std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
                .expect("GUI source should be readable as UTF-8");
        assert!(
            source.contains("OJOS Orchestrator")
                && source.contains("Core Object Overview")
                && source.contains("Endpoint uses ip:port:service-name as the runtime identity."),
            "GUI user-facing text must remain readable UTF-8/ASCII"
        );
        assert_no_mojibake(&source);
    }

    fn assert_no_mojibake(source: &str) {
        for marker in mojibake_markers() {
            assert!(
                !source.contains(&marker),
                "GUI source contains mojibake marker {marker}"
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
