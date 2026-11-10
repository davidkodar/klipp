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
        assert_eq!(super::mcp_command("io.github.davidkodar.Klipp").join(" "), "flatpak run --command=klipp-mcp io.github.davidkodar.Klipp");
    }
}
