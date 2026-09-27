//! Native presentation. All data and writes go through `LitecordApp`.
mod attention;
mod bridge;
mod context_ui;
mod friends_ui;
mod home_ui;
mod kit;
mod layout_editor;
mod local_screens;
mod messages_ui;
mod omni_screen;
mod omni_signin;
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
            let mut workspace = workspace::Workspace::new(app, runtime, cc.egui_ctx.clone());
            if let Some(destination) = options.destination {
                workspace.navigate(destination);
            }
            workspace.omni_open = options.omni_open;
            #[cfg(feature = "screenshots")]
            {
                workspace.screenshot_path = options.screenshot;
                // Test hooks for capturing a Settings section, or states that
                // take longer than the default delay to appear.
                if let Ok(section) = std::env::var("LITECORD_SCREENSHOT_SECTION") {
                    workspace.settings_section = Some(section);
                }
                if let Some(secs) = std::env::var("LITECORD_SCREENSHOT_DELAY")
                    .ok()
                    .and_then(|v| v.parse().ok())
                {
                    workspace.screenshot_delay = std::time::Duration::from_secs(secs);
                }
            }
            Ok(Box::new(workspace))
        }),
    )
}
