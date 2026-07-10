//! Light/dark theme toggle, implemented **as a plugin** (invariant #3).
//!
//! The theme itself is app render state (colors, truecolor detection, user overrides), so the
//! plugin doesn't own it — it just expresses the intent through [`Host::toggle_theme`], and the
//! app flips its theme on the next drain. A one-liner, but it moves `view.toggleTheme` off the
//! app's stringly-matched `exec_id` arm onto the same command path as every other feature.

use editor_plugin::{Contributions, Host, Plugin};

pub struct ThemePlugin;

impl Plugin for ThemePlugin {
    fn id(&self) -> &str {
        "theme"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("view.toggleTheme", "View: Toggle Light/Dark Theme")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id == "view.toggleTheme" {
            host.toggle_theme();
            return true;
        }
        false
    }
}
