//! Running as a Flatpak: the sandbox's app id (`FLATPAK_ID`), which the
//! control socket's folder, the MCP command an assistant runs, the
//! updater and Klipp's own folders follow.

/// The Flatpak app id Klipp runs under, `None` outside a Flatpak.
pub fn app_id() -> Option<String> {
    app_id_from(std::env::var("FLATPAK_ID").ok().as_deref())
}

/// [`app_id`] from the variable's value: an empty one is no Flatpak.
pub fn app_id_from(var: Option<&str>) -> Option<String> {
    var.map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned)
}

/// The name Klipp's windows go by outside a Flatpak: the Wayland app id and
/// the X11 class, which `klipp.desktop` names as its `StartupWMClass`.
pub const WINDOW_APP_ID: &str = "klipp";

/// The app id Klipp's windows carry: the Flatpak's where there is one,
/// whose desktop entry and icon are named by it, else [`WINDOW_APP_ID`].
/// A desktop finds a window's entry, and its icon, by this id: the
/// Flatpak's windows said "klipp" and had no icon in the taskbar.
pub fn window_app_id() -> String {
    window_app_id_from(std::env::var("FLATPAK_ID").ok().as_deref())
}

/// [`window_app_id`] from `FLATPAK_ID`'s value.
pub fn window_app_id_from(var: Option<&str>) -> String {
    app_id_from(var).unwrap_or_else(|| WINDOW_APP_ID.to_owned())
}

/// The command an assistant on the host runs to start Klipp's MCP server
/// inside the app's sandbox, where it reaches the app's socket.
pub fn mcp_command(app_id: &str) -> Vec<String> {
    vec!["flatpak".into(), "run".into(), "--command=klipp-mcp".into(), app_id.into()]
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_app_id_is_the_variables_and_an_empty_one_is_none() {
        assert_eq!(super::app_id_from(Some("io.github.davidkodar.Klipp")).as_deref(), Some("io.github.davidkodar.Klipp"));
        assert_eq!(super::app_id_from(Some("")), None);
        assert_eq!(super::app_id_from(None), None);
        assert_eq!(super::window_app_id_from(Some("io.github.davidkodar.Klipp")), "io.github.davidkodar.Klipp", "a Flatpak's windows carry its id");
        assert_eq!(super::window_app_id_from(Some(" io.github.davidkodar.Klipp\n")), "io.github.davidkodar.Klipp");
        assert_eq!(super::window_app_id_from(Some("")), "klipp", "empty is no Flatpak");
        assert_eq!(super::window_app_id_from(Some("  ")), "klipp");
        assert_eq!(super::window_app_id_from(None), "klipp", "klipp.desktop's StartupWMClass");
        assert_eq!(super::mcp_command("io.github.davidkodar.Klipp").join(" "), "flatpak run --command=klipp-mcp io.github.davidkodar.Klipp");
    }
}
