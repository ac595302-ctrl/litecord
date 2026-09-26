//! Native visual QA of shell docking and an optional horizontal server strip.
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_layout::{Destination, LayoutNode, Placement};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = LitecordApp::builder(LitecordConfig::default())
        .in_memory()
        .start()
        .await?;
    let view = app.layout_profiles_view()?;
    let mut profile = view
        .profiles
        .active()
        .ok_or("missing active profile")?
        .clone();
    profile.shell.dock("nav", "outlet", Placement::Top)?;
    profile.shell.dock("account", "outlet", Placement::Bottom)?;
    let tree = profile
        .destinations
        .get_mut(&Destination::Servers)
        .ok_or("missing Servers")?;
    tree.insert_panel(
        LayoutNode::panel("servers_optional", "server_list", Placement::Top),
        "main",
        Placement::Top,
    )?;
    app.save_layout_profile(profile, &view.storage_token)?;
    let options = litecord_ui::WindowOptions {
        destination: Some(Destination::Servers),
        #[cfg(feature = "screenshots")]
        screenshot: std::env::args_os().nth(1).map(std::path::PathBuf::from),
        ..Default::default()
    };
    let result =
        litecord_ui::run_with_options(app.clone(), tokio::runtime::Handle::current(), options);
    app.shutdown().await;
    result?;
    Ok(())
}
