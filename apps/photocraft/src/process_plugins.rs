//! Process plug-ins on the desktop: where they are installed and where they keep their data.
//!
//! Plug-ins live in `<settings dir>/process-plugins/<folder>`, keep their data in
//! `<settings dir>/process-plugins-data/<plugin id>` and log to `<settings dir>/logs/plugins`.
//! The UI starts them on its first frame (`Services::process_plugins`).

use photocraft_process_plugin_host::{Config, Manager};
use photocraft_process_plugin_protocol::host::ProcessPluginHost;

/// The plug-in manager, or `None` when there is no settings directory to install plug-ins in.
pub fn manager() -> Option<Box<dyn ProcessPluginHost>> {
    let settings = crate::app_dirs::config_dir()?;
    let config = Config::new(
        settings.join("process-plugins"),
        settings.join("process-plugins-data"),
        settings.join("logs").join("plugins"),
        photocraft_engine::build_info::VERSION,
        "en",
    );
    Some(Box::new(Manager::new(config)))
}
