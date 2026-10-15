//! Identity of this MCP process, separate from the app it connects to.
use std::path::PathBuf;
use std::os::unix::ffi::OsStrExt;
use std::sync::OnceLock;
use serde_json::{json, Value};

const RECONNECT: &str = "Reconnect the Klipp MCP connection in your client and refresh its tool list. Restarting the Klipp app alone does not refresh the MCP server.";

pub(crate) fn label() -> String {
    format!("{} {}{} {}", env!("CARGO_PKG_VERSION"), env!("KLIPP_GIT_COMMIT"),
        if env!("KLIPP_GIT_DIRTY") == "true" { "-dirty" } else { "" }, env!("KLIPP_BUILD_PROFILE"))
}

/// Capture the running executable's identity before a subsequent rebuild.
struct Executable {
    path: PathBuf,
    identity: (u64, u64),
}

fn identity(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.dev(), m.ino()))
}

fn executable() -> Option<&'static Executable> {
    static EXE: OnceLock<Option<Executable>> = OnceLock::new();
    EXE.get_or_init(|| {
        let path = std::env::current_exe().ok()?;
        let id = identity(std::path::Path::new("/proc/self/exe"))?;
        // Linux appends this marker if the running inode was unlinked.
        let path = PathBuf::from(path.as_os_str().as_encoded_bytes().strip_suffix(b" (deleted)")
            .map(std::ffi::OsStr::from_bytes).unwrap_or(path.as_os_str()));
        Some(Executable { path, identity: id })
    }).as_ref()
}

pub(crate) fn build() -> Value {
    let exe = executable();
    json!({
        "version": env!("CARGO_PKG_VERSION"), "commit": env!("KLIPP_GIT_COMMIT"),
        "dirty": env!("KLIPP_GIT_DIRTY") == "true", "profile": env!("KLIPP_BUILD_PROFILE"),
        "label": label(), "pid": std::process::id(), "exe": exe.map(|e| &e.path),
        "executable_replaced": exe.map(|e| identity(&e.path) != Some(e.identity)),
    })
}

pub(crate) fn tool_names() -> &'static Vec<String> {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| super::tool_definitions_for(false).iter()
        .filter_map(|t| t["name"].as_str().map(str::to_owned)).collect())
}

/// FNV-1a is a stable change identifier, not a security checksum. Output
/// schemas vary with the negotiated protocol, so compare the base catalog.
fn fingerprint(tools: &[Value]) -> String {
    let mut base = tools.to_vec();
    for tool in &mut base {
        if let Some(o) = tool.as_object_mut() { o.remove("outputSchema"); }
    }
    let bytes = serde_json::to_vec(&base).expect("a JSON catalog serializes");
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3));
    format!("fnv1a64-{hash:016x}")
}

pub(crate) fn metadata() -> Value {
    static CATALOG: OnceLock<String> = OnceLock::new();
    json!({"mcp_build": build(), "catalog_fingerprint": CATALOG.get_or_init(|| fingerprint(&super::tool_definitions_for(false))), "tool_count": tool_names().len()})
}

fn warnings(app: &Value, server: &Value, expected: Option<&str>, actual: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    match (app["commit"].as_str(), server["commit"].as_str()) {
        (Some(a), Some(b)) if a != "unknown" && b != "unknown" && a != b => {
            warnings.push(format!("The app build ({a}) and MCP server build ({b}) differ; their capabilities may differ."));
        }
        (None | Some("unknown"), _) | (_, None | Some("unknown")) => {
            warnings.push("App/MCP build compatibility could not be verified.".into());
        }
        _ => {}
    }
    if app["dirty"] == true || server["dirty"] == true {
        warnings.push("A build contains uncommitted changes; commit IDs alone cannot establish matching capabilities.".into());
    }
    if server["executable_replaced"] == true {
        warnings.push("The MCP executable on disk was replaced or removed after this process started.".into());
    }
    if expected.is_some_and(|e| e != actual) {
        warnings.push("The client's expected tool catalog differs from this MCP server's catalog.".into());
    }
    warnings
}

pub(crate) fn attach(result: &mut Value, expected: Option<&str>) {
    let meta = metadata();
    let warnings = warnings(&result["build"], &meta["mcp_build"], expected, meta["catalog_fingerprint"].as_str().unwrap());
    result["mcp"] = meta;
    result["mcp"]["warnings"] = json!(warnings);
    result["mcp"]["reconnect"] = json!(RECONNECT);
    let mut text = result["text"].as_str().unwrap_or("").to_string();
    text.push_str(&format!("\nMCP server: {}. Catalog: {} ({} tools).", label(), result["mcp"]["catalog_fingerprint"].as_str().unwrap(), tool_names().len()));
    if !warnings.is_empty() {
        text.push_str(&format!("\nWarning: {} {RECONNECT}", warnings.join(" ")));
    }
    result["text"] = json!(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_identity_matches_both_protocols_and_detects_schema_changes() {
        let legacy = super::super::tool_definitions_for(false);
        let modern = super::super::tool_definitions_for(true);
        assert_eq!(fingerprint(&legacy), fingerprint(&modern));
        assert_eq!(metadata()["catalog_fingerprint"], fingerprint(&modern));
        let mut changed = legacy.clone();
        changed[0]["inputSchema"]["properties"]["new_field"] = json!({"type":"string"});
        assert_ne!(fingerprint(&legacy), fingerprint(&changed));
        assert_eq!(tool_names().len(), legacy.len());
    }

    #[test]
    fn mismatch_is_not_hidden_by_matching_package_versions() {
        let app = json!({"version":"0.1.0", "commit":"new"});
        let server = json!({"version":"0.1.0", "commit":"old", "executable_replaced":true});
        let notes = warnings(&app, &server, Some("cached"), "current").join(" ");
        assert!(notes.contains("capabilities may differ") && notes.contains("replaced") && notes.contains("catalog differs"));
        assert!(warnings(&app, &app, Some("same"), "same").is_empty());
        assert!(warnings(&json!({}), &server, None, "current")[0].contains("could not be verified"));
    }

    #[test]
    fn atomic_executable_replacement_is_detectable_without_timestamp_changes() {
        let path = std::env::temp_dir().join(format!("klipp-mcp-identity-{}", uuid::Uuid::new_v4()));
        let next = path.with_extension("next");
        std::fs::write(&path, b"old").unwrap();
        let running = identity(&path).unwrap();
        std::fs::write(&next, b"new").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert_ne!(identity(&path), Some(running));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(identity(&path), None);
    }

    #[test]
    fn read_only_diagnostics_preserve_app_identity_and_edit_revision() {
        let mut result = json!({"build":{"commit":"other"},"text":"Klipp is running.","revision":42,"project_identity":7});
        attach(&mut result, Some("old-catalog"));
        assert_eq!(result["build"]["commit"], "other");
        assert_eq!(result["revision"], 42);
        assert_eq!(result["project_identity"], 7);
        assert!(result["text"].as_str().unwrap().starts_with("Klipp is running."));
        assert!(result["text"].as_str().unwrap().contains(RECONNECT));
    }
}
