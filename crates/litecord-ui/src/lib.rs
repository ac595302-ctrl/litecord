//! Native presentation. All data and writes go through `LitecordApp`.
mod attention;
mod bridge;
#[cfg(all(feature = "browser-login", target_os = "windows"))]
mod browser_login;
mod context_ui;
mod friends_ui;
mod home_ui;
mod kit;
mod layout_editor;
mod local_screens;
mod memory_ui;
mod messages_ui;
mod omni_ui;
mod overlays;
mod ph;
#[cfg(test)]
mod render_tests;
mod screens;
mod social_screens;
mod tasks_ui;
#[cfg(test)]
mod tests;
mod theme;
mod workspace;

use litecord_app::LitecordApp;
pub use litecord_layout::Destination;

#[derive(Debug, Default, Clone)]
pub struct WindowOptions {
    pub size: Option<[f32; 2]>,
    pub destination: Option<litecord_layout::Destination>,
    /// Start with the Omni panel open.
    pub omni_open: bool,
    #[cfg(feature = "screenshots")]
    pub screenshot: Option<std::path::PathBuf>,
}

/// Run on the main OS thread while the caller's Tokio runtime remains alive.
pub fn run(app: LitecordApp, runtime: tokio::runtime::Handle) -> Result<(), eframe::Error> {
    run_with_options(app, runtime, WindowOptions::default())
}

pub fn run_with_options(
    app: LitecordApp,
    runtime: tokio::runtime::Handle,
    options: WindowOptions,
) -> Result<(), eframe::Error> {
    eframe::run_native(
        "Litecord",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size(options.size.unwrap_or([1586.0, 992.0]))
                .with_min_inner_size([640.0, 480.0]),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            // Loads Discord CDN avatars (fetched once, then cached in memory).
            egui_extras::install_image_loaders(&cc.egui_ctx);
            let mut workspace = workspace::Workspace::new(app, runtime, cc.egui_ctx.clone());
            if let Some(destination) = options.destination {
                workspace.navigate(destination);
            }
            workspace.omni_open = options.omni_open;
            #[cfg(feature = "screenshots")]
            {
                workspace.screenshot_path = options.screenshot;
            }
            Ok(Box::new(workspace))
        }),
    )
}

/// A small window explaining why Litecord could not start.
///
/// When the app is opened from Finder or Explorer there is no terminal, so
/// an error printed to stderr looked like the app silently doing nothing.
pub fn show_startup_error(
    message: &str,
    log_path: Option<&std::path::Path>,
) -> Result<(), eframe::Error> {
    let message = message.to_owned();
    let log = log_path.map(|p| p.display().to_string());
    eframe::run_native(
        "Litecord",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([560.0, 260.0])
                .with_resizable(false),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(StartupError { message, log }))
        }),
    )
}

struct StartupError {
    message: String,
    log: Option<String>,
}

impl eframe::App for StartupError {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        use eframe::egui;
        egui::Frame::central_panel(ui.style()).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.add_space(8.0);
            ui.heading("Litecord couldn't start");
            ui.add_space(10.0);
            ui.label(egui::RichText::new(&self.message).color(theme::WARNING));
            if let Some(log) = &self.log {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(format!("Details are in {log}"))
                        .color(theme::MUTED)
                        .small(),
                );
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button("Copy message").clicked() {
                    ui.ctx().copy_text(self.message.clone());
                }
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}
