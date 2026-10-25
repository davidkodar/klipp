//! Connecting an AI assistant to Klipp (#625): where the installed
//! `klipp-mcp` is, which assistants on this computer can register it,
//! whether they have, and the commands that do it.
//!
//! An assistant owns its own list of MCP servers. Where it has a command
//! for it, Connect and Disconnect run that (`claude mcp add`, `codex mcp
//! add`, `gemini mcp add`, and their `remove`), which change Klipp's entry
//! and nothing else. Where it has none (Zed, Cursor), Klipp's entry is
//! set in the assistant's settings file (#2649): the file backed up
//! first, edited as text at Klipp's entry alone (`jsonc_edit`), so the
//! person's other settings, order and comments stay, and replaced whole
//! in one move. Either only when the person asks. Reading whether Klipp
//! is registered reads the assistant's settings and changes nothing.
//!
//! Each assistant here is configured where and how its own documentation
//! says, for Linux:
//! - Gemini CLI: `gemini mcp add -s user`, kept in `~/.gemini/settings.json`
//!   (`mcpServers`), <https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md>;
//! - Zed: `context_servers` in `~/.config/zed/settings.json`, a file with
//!   comments, <https://zed.dev/docs/ai/mcp>;
//! - Cursor: `mcpServers` in `~/.cursor/mcp.json`, <https://cursor.com/docs/context/mcp>.

use std::path::{Path, PathBuf};

/// The name Klipp is registered under in every assistant.
pub const SERVER_NAME: &str = "klipp";

/// An assistant Klipp can register itself with by the assistant's own
/// command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    Codex,
    GeminiCli,
    Zed,
    Cursor,
}

impl Client {
    pub const ALL: [Client; 5] = [Client::ClaudeCode, Client::Codex, Client::GeminiCli, Client::Zed, Client::Cursor];

    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::Codex => "Codex",
            Client::GeminiCli => "Gemini CLI",
            Client::Zed => "Zed",
            Client::Cursor => "Cursor",
        }
    }

    /// The assistant's command-line program.
    pub fn program(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude",
            Client::Codex => "codex",
            Client::GeminiCli => "gemini",
            Client::Zed => "zed",
            Client::Cursor => "cursor",
        }
    }

    /// Whether Klipp is set in the assistant's settings file rather than
    /// by a command of the assistant's.
    pub fn by_file(self) -> bool {
        matches!(self, Client::Zed | Client::Cursor)
    }

    /// The settings file that keeps the assistant's servers, read for
    /// whether Klipp is there and, for [`Client::by_file`], written.
    pub fn settings_file(self) -> Option<PathBuf> {
        match self {
            Client::ClaudeCode => dirs::home_dir().map(|h| h.join(".claude.json")),
            Client::Codex => dirs::home_dir().map(|h| h.join(".codex").join("config.toml")),
            Client::GeminiCli => dirs::home_dir().map(|h| h.join(".gemini").join("settings.json")),
            Client::Zed => dirs::config_dir().map(|c| c.join("zed").join("settings.json")),
            Client::Cursor => dirs::home_dir().map(|h| h.join(".cursor").join("mcp.json")),
        }
    }

    /// The object in the settings file that holds the servers, by name.
    pub fn section(self) -> &'static str {
        match self {
            Client::Zed => "context_servers",
            _ => "mcpServers",
        }
    }

    /// Klipp's entry in the settings file, running `klipp_mcp`.
    pub fn entry(self, klipp_mcp: &Path) -> serde_json::Value {
        let command = klipp_mcp.to_string_lossy();
        match self {
            Client::Zed => serde_json::json!({ "command": command, "args": [], "env": {} }),
            Client::Cursor => serde_json::json!({ "type": "stdio", "command": command, "args": [] }),
            _ => serde_json::json!({ "command": command, "args": [] }),
        }
    }

    /// Whether the assistant is on this computer: its program on `PATH`,
    /// or, for an editor that may not put one there, its settings folder.
    pub fn installed(self) -> bool {
        let program = find_on_path(self.program()).is_some();
        match self {
            // Some distributions name Zed's program `zeditor`.
            Client::Zed => program || find_on_path("zeditor").is_some() || self.settings_file().and_then(|f| f.parent().map(Path::is_dir)).unwrap_or(false),
            Client::Cursor => program || self.settings_file().and_then(|f| f.parent().map(Path::is_dir)).unwrap_or(false),
            _ => program,
        }
    }

    /// The arguments that register `klipp_mcp` for every project of the
    /// person's: Claude Code's user scope, Codex's own settings.
    pub fn connect_args(self, klipp_mcp: &Path) -> Vec<String> {
        let path = klipp_mcp.to_string_lossy().to_string();
        match self {
            Client::ClaudeCode => vec!["mcp".into(), "add".into(), "--scope".into(), "user".into(), SERVER_NAME.into(), "--".into(), path],
            Client::Codex => vec!["mcp".into(), "add".into(), SERVER_NAME.into(), "--".into(), path],
            Client::GeminiCli => vec!["mcp".into(), "add".into(), "-s".into(), "user".into(), SERVER_NAME.into(), path],
            // Set in their settings files, by no command.
            Client::Zed | Client::Cursor => Vec::new(),
        }
    }

    /// The arguments that remove Klipp's entry, and only it.
    pub fn disconnect_args(self) -> Vec<String> {
        match self {
            Client::ClaudeCode => vec!["mcp".into(), "remove".into(), "--scope".into(), "user".into(), SERVER_NAME.into()],
            Client::Codex => vec!["mcp".into(), "remove".into(), SERVER_NAME.into()],
            Client::GeminiCli => vec!["mcp".into(), "remove".into(), "-s".into(), "user".into(), SERVER_NAME.into()],
            Client::Zed | Client::Cursor => Vec::new(),
        }
    }

    /// The command a person can type instead, with the path quoted.
    pub fn connect_command(self, klipp_mcp: &Path) -> String {
        let mut words = vec![self.program().to_string()];
        words.extend(self.connect_args(klipp_mcp).into_iter().map(|w| shell_quote(&w)));
        words.join(" ")
    }
}

/// Whether, and how, an assistant has Klipp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// The assistant's program is not on this computer.
    NotInstalled,
    NotConnected,
    /// Registered for every project, running this command.
    Connected(String),
    /// Registered only for these projects (Claude Code's local or project
    /// scope, a checkout's `.mcp.json`): an assistant started elsewhere
    /// does not have it.
    ProjectsOnly(Vec<PathBuf>),
    /// Its settings could not be read.
    Unknown(String),
}

/// `klipp-mcp` for this Klipp: the one installed beside the running
/// executable, named by the command on `PATH` when that is the same file
/// (so the setting outlives an upgrade's new directory), else by its own
/// path; else whatever `klipp-mcp` is on `PATH`.
pub fn klipp_mcp_path() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.canonicalize().ok()).and_then(|exe| exe.parent().map(|d| d.join("klipp-mcp")));
    choose_klipp_mcp(beside.as_deref().filter(|p| p.is_file()), find_on_path("klipp-mcp").as_deref())
}

/// [`klipp_mcp_path`]'s choice, given the one beside the app and the one on
/// `PATH`.
pub fn choose_klipp_mcp(beside: Option<&Path>, on_path: Option<&Path>) -> Option<PathBuf> {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    match (beside, on_path) {
        (Some(b), Some(p)) if same(b, p) => Some(p.to_path_buf()),
        (Some(b), _) => Some(b.to_path_buf()),
        (None, Some(p)) => Some(p.to_path_buf()),
        (None, None) => None,
    }
}

/// `name` on `PATH`, as a path.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Whether `client` has Klipp registered, read from its own settings
/// without running it or changing anything.
pub fn registration(client: Client) -> Registration {
    if !client.installed() {
        return Registration::NotInstalled;
    }
    match client {
        Client::ClaudeCode => {
            let Some(home) = dirs::home_dir() else { return Registration::Unknown("no home directory".into()) };
            match std::fs::read_to_string(home.join(".claude.json")) {
                Ok(text) => claude_registration(&text, &|project| std::fs::read_to_string(project.join(".mcp.json")).ok()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registration::NotConnected,
                Err(e) => Registration::Unknown(e.to_string()),
            }
        }
        Client::Codex => match run(client, &["mcp", "list", "--json"]) {
            Ok(text) => codex_registration(&text),
            Err(e) => Registration::Unknown(e),
        },
        Client::GeminiCli | Client::Zed | Client::Cursor => {
            let Some(file) = client.settings_file() else { return Registration::Unknown("no home directory".into()) };
            match std::fs::read_to_string(&file) {
                Ok(text) => file_registration(&text, client.section()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registration::NotConnected,
                Err(e) => Registration::Unknown(e.to_string()),
            }
        }
    }
}

/// Klipp in a settings file that keeps its servers in `section`.
pub fn file_registration(settings: &str, section: &str) -> Registration {
    match crate::jsonc_edit::parse(settings) {
        Ok(v) => match v.get(section).and_then(|s| s.get(SERVER_NAME)) {
            Some(server) => Registration::Connected(server.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string()),
            None => Registration::NotConnected,
        },
        Err(e) => Registration::Unknown(e),
    }
}

/// Write the settings file at `file` as `edit` makes it from its text:
/// nothing when `edit` gives `None`; else a copy of the file as it was
/// beside it first, then the new text written beside it and moved into
/// place, so the file is never half written. A file that is a link is
/// written where it points, the link left as it is.
pub fn write_settings(file: &Path, edit: impl FnOnce(&str) -> Result<Option<String>, String>) -> Result<Option<PathBuf>, String> {
    let real = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let before = match std::fs::read_to_string(&real) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("could not read {}: {e}", real.display())),
    };
    let Some(after) = edit(before.as_deref().unwrap_or(""))? else { return Ok(None) };
    let dir = real.parent().ok_or("the settings file has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    let name = real.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "settings".into());
    let backup = before.as_ref().map(|text| {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let at = dir.join(format!("{name}.before-klipp-{secs}"));
        std::fs::write(&at, text).map(|()| at).map_err(|e| format!("could not back up {}: {e}", real.display()))
    }).transpose()?;
    let tmp = dir.join(format!(".{name}.klipp-{}.tmp", std::process::id()));
    std::fs::write(&tmp, after).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &real) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("could not replace {}: {e}", real.display()));
    }
    Ok(backup)
}

/// Klipp in Claude Code's settings (`~/.claude.json`): the user scope's
/// servers, where Connect puts it, first. Failing that, the projects
/// Claude Code knows that have it for themselves: in their own entry (the
/// local scope), or in the project's `.mcp.json` unless turned off there,
/// read by `mcp_json`.
pub fn claude_registration(settings: &str, mcp_json: &dyn Fn(&Path) -> Option<String>) -> Registration {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(settings) else {
        return Registration::Unknown("Claude Code's settings could not be read".into());
    };
    if let Some(server) = v.get("mcpServers").and_then(|s| s.get(SERVER_NAME)) {
        return Registration::Connected(server.get("command").and_then(|c| c.as_str()).unwrap_or("").to_string());
    }
    let mut projects: Vec<PathBuf> = v
        .get("projects")
        .and_then(|p| p.as_object())
        .into_iter()
        .flatten()
        .filter(|(path, entry)| {
            let local = entry.get("mcpServers").and_then(|s| s.get(SERVER_NAME)).is_some();
            let disabled = entry.get("disabledMcpjsonServers").and_then(|d| d.as_array()).is_some_and(|d| d.iter().any(|n| n.as_str() == Some(SERVER_NAME)));
            let shared = !disabled
                && mcp_json(Path::new(path))
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .is_some_and(|j| j.get("mcpServers").and_then(|s| s.get(SERVER_NAME)).is_some());
            local || shared
        })
        .map(|(path, _)| PathBuf::from(path))
        .collect();
    projects.sort();
    if projects.is_empty() {
        Registration::NotConnected
    } else {
        Registration::ProjectsOnly(projects)
    }
}

/// Klipp in `codex mcp list --json`.
pub fn codex_registration(listing: &str) -> Registration {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(listing) else {
        return Registration::Unknown("Codex's list of servers could not be read".into());
    };
    let found = v.as_array().into_iter().flatten().find(|s| s.get("name").and_then(|n| n.as_str()) == Some(SERVER_NAME));
    match found {
        Some(server) => Registration::Connected(server.pointer("/transport/command").and_then(|c| c.as_str()).unwrap_or("").to_string()),
        None => Registration::NotConnected,
    }
}

/// Register `klipp_mcp` with `client`, replacing an entry of Klipp's that
/// runs another command (an older build, a checkout's). Nothing else of
/// the assistant's changes; already registered as asked, nothing does.
pub fn connect(client: Client, klipp_mcp: &Path) -> Result<(), String> {
    let wanted = klipp_mcp.to_string_lossy().to_string();
    match registration(client) {
        Registration::NotInstalled => return Err(format!("{} is not installed on this computer", client.name())),
        Registration::Connected(cmd) if cmd == wanted => return Ok(()),
        // A settings file's entry is replaced where it is.
        Registration::Connected(_) if !client.by_file() => disconnect(client)?,
        _ => {}
    }
    if client.by_file() {
        let file = client.settings_file().ok_or("no home directory")?;
        let entry = client.entry(klipp_mcp);
        return write_settings(&file, |text| crate::jsonc_edit::set(text, client.section(), SERVER_NAME, &entry).map(Some)).map(|_| ());
    }
    let args = client.connect_args(klipp_mcp);
    run(client, &args.iter().map(String::as_str).collect::<Vec<_>>()).map(|_| ())
}

/// Remove Klipp's entry from `client`, and nothing else.
pub fn disconnect(client: Client) -> Result<(), String> {
    if matches!(registration(client), Registration::NotConnected) {
        return Ok(());
    }
    if client.by_file() {
        let file = client.settings_file().ok_or("no home directory")?;
        return write_settings(&file, |text| crate::jsonc_edit::remove(text, client.section(), SERVER_NAME)).map(|_| ());
    }
    let args = client.disconnect_args();
    run(client, &args.iter().map(String::as_str).collect::<Vec<_>>()).map(|_| ())
}

/// Run the assistant's program from the home directory — not this
/// checkout's, whose project settings it would read as well — and its
/// output, or what went wrong in a sentence.
fn run(client: Client, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new(client.program());
    cmd.args(args).stdin(std::process::Stdio::null());
    if let Some(home) = dirs::home_dir() {
        cmd.current_dir(home);
    }
    let out = cmd.output().map_err(|e| format!("could not run {}: {e}", client.program()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let said = String::from_utf8_lossy(&out.stderr);
        let said = said.trim();
        Err(if said.is_empty() { format!("{} {} failed", client.program(), args.join(" ")) } else { said.lines().last().unwrap_or(said).to_string() })
    }
}

/// Run `klipp_mcp --check`: the read-only status an assistant asks for
/// first, through the binary it runs. Klipp's summary when it answered,
/// else what to do.
pub fn check(klipp_mcp: &Path) -> Result<String, String> {
    let out = std::process::Command::new(klipp_mcp)
        .arg("--check")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", klipp_mcp.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if said.is_empty() { "klipp-mcp did not answer".into() } else { said })
    }
}

/// The settings for any assistant that reads an `mcpServers` list (most
/// do), with Klipp's entry.
pub fn json_snippet(klipp_mcp: &Path) -> String {
    let v = serde_json::json!({ "mcpServers": { SERVER_NAME: { "command": klipp_mcp.to_string_lossy(), "args": [] } } });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// The same for Codex's `config.toml`.
pub fn toml_snippet(klipp_mcp: &Path) -> String {
    let path = klipp_mcp.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!("[mcp_servers.{SERVER_NAME}]\ncommand = \"{path}\"\n")
}

/// The same for an editor that keeps a `servers` list with each entry's
/// transport named (VS Code's `mcp.json`).
pub fn servers_snippet(klipp_mcp: &Path) -> String {
    let v = serde_json::json!({ "servers": { SERVER_NAME: { "type": "stdio", "command": klipp_mcp.to_string_lossy(), "args": [] } } });
    serde_json::to_string_pretty(&v).unwrap_or_default()
}

/// The same for an assistant that keeps YAML extensions (Goose's
/// `config.yaml`).
pub fn yaml_snippet(klipp_mcp: &Path) -> String {
    let path = klipp_mcp.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!("extensions:\n  {SERVER_NAME}:\n    type: stdio\n    name: {SERVER_NAME}\n    enabled: true\n    cmd: \"{path}\"\n    args: []\n    timeout: 300\n")
}

/// A word quoted for a POSIX shell when it needs it.
fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:@".contains(c)) {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #625: Connect is the assistant's own add, for every project, and
    /// Disconnect its remove of Klipp's entry alone; the commands a person
    /// could type say the same.
    #[test]
    fn the_assistants_own_commands_change_klipps_entry_only() {
        let p = Path::new("/usr/bin/klipp-mcp");
        assert_eq!(Client::ClaudeCode.connect_args(p), ["mcp", "add", "--scope", "user", "klipp", "--", "/usr/bin/klipp-mcp"]);
        assert_eq!(Client::Codex.connect_args(p), ["mcp", "add", "klipp", "--", "/usr/bin/klipp-mcp"]);
        assert_eq!(Client::ClaudeCode.disconnect_args(), ["mcp", "remove", "--scope", "user", "klipp"]);
        assert_eq!(Client::Codex.disconnect_args(), ["mcp", "remove", "klipp"]);
        assert_eq!(Client::ClaudeCode.connect_command(Path::new("/home/a b/klipp-mcp")), "claude mcp add --scope user klipp -- '/home/a b/klipp-mcp'");
    }

    /// Whether Klipp is registered is read from what each assistant keeps:
    /// Claude Code's user-scope servers, Codex's own listing.
    #[test]
    fn a_registration_is_read_from_the_assistants_settings() {
        let none = |_: &Path| None;
        let claude = r#"{"numStartups": 3, "mcpServers": {"other": {"command": "x"}, "klipp": {"type": "stdio", "command": "/usr/bin/klipp-mcp", "args": []}}}"#;
        assert_eq!(claude_registration(claude, &none), Registration::Connected("/usr/bin/klipp-mcp".into()));
        assert_eq!(claude_registration(r#"{"mcpServers": {"other": {}}}"#, &none), Registration::NotConnected);
        assert_eq!(claude_registration("{}", &none), Registration::NotConnected);
        assert!(matches!(claude_registration("not json", &none), Registration::Unknown(_)));

        // Only for some projects: one in its own entry (the local scope),
        // one by its checkout's .mcp.json; not one that turned that off,
        // nor one whose .mcp.json has other servers only.
        let projects = r#"{"mcpServers": {}, "projects": {
            "/home/u/klipp-cold": {"mcpServers": {"klipp": {"command": "/x/klipp-mcp"}}},
            "/home/u/Klipp": {"mcpServers": {}, "disabledMcpjsonServers": []},
            "/home/u/Off": {"mcpServers": {}, "disabledMcpjsonServers": ["klipp"]},
            "/home/u/Other": {"mcpServers": {}}
        }}"#;
        let mcp_json = |p: &Path| match p.to_str() {
            Some("/home/u/Klipp") | Some("/home/u/Off") => Some(r#"{"mcpServers": {"klipp": {"command": "./target/release/klipp-mcp"}}}"#.to_string()),
            Some("/home/u/Other") => Some(r#"{"mcpServers": {"tona": {}}}"#.to_string()),
            _ => None,
        };
        assert_eq!(claude_registration(projects, &mcp_json), Registration::ProjectsOnly(vec!["/home/u/Klipp".into(), "/home/u/klipp-cold".into()]));

        let codex = r#"[{"name": "agent_bridge", "transport": {"type": "stdio", "command": "python3"}}, {"name": "klipp", "transport": {"type": "stdio", "command": "/usr/lib/klipp/klipp-mcp"}}]"#;
        assert_eq!(codex_registration(codex), Registration::Connected("/usr/lib/klipp/klipp-mcp".into()));
        assert_eq!(codex_registration("[]"), Registration::NotConnected);
    }

    /// The command named is the one on `PATH` when it is the same file as
    /// the one beside the app, else the one beside the app.
    #[test]
    fn the_binary_named_is_the_installed_command() {
        let dir = std::env::temp_dir().join(format!("klipp-mcp-path-{}", std::process::id()));
        let (lib, bin) = (dir.join("lib"), dir.join("bin"));
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let real = lib.join("klipp-mcp");
        std::fs::write(&real, b"").unwrap();
        let link = bin.join("klipp-mcp");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(choose_klipp_mcp(Some(&real), Some(&link)), Some(link.clone()), "the stable command when it is the same file");
        let other = dir.join("elsewhere-klipp-mcp");
        std::fs::write(&other, b"").unwrap();
        assert_eq!(choose_klipp_mcp(Some(&real), Some(&other)), Some(real.clone()), "this Klipp's own when PATH has another");
        assert_eq!(choose_klipp_mcp(None, Some(&other)), Some(other.clone()));
        assert_eq!(choose_klipp_mcp(None, None), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The settings for other assistants name the same command.
    #[test]
    fn the_snippets_name_the_command() {
        let p = Path::new("/usr/bin/klipp-mcp");
        let v: serde_json::Value = serde_json::from_str(&json_snippet(p)).unwrap();
        assert_eq!(v["mcpServers"]["klipp"]["command"], "/usr/bin/klipp-mcp");
        assert_eq!(toml_snippet(p), "[mcp_servers.klipp]\ncommand = \"/usr/bin/klipp-mcp\"\n");
    }

    /// #2649: Gemini CLI connects by its own command, for the user; Zed
    /// and Cursor by their settings files, each entry in the shape its
    /// documentation gives.
    #[test]
    fn each_assistant_is_set_its_own_way() {
        let p = Path::new("/usr/bin/klipp-mcp");
        assert_eq!(Client::GeminiCli.connect_args(p), ["mcp", "add", "-s", "user", "klipp", "/usr/bin/klipp-mcp"]);
        assert_eq!(Client::GeminiCli.disconnect_args(), ["mcp", "remove", "-s", "user", "klipp"]);
        assert!(Client::Zed.by_file() && Client::Cursor.by_file() && !Client::GeminiCli.by_file());
        assert_eq!(Client::Zed.section(), "context_servers");
        assert_eq!(Client::Zed.entry(p), serde_json::json!({"command": "/usr/bin/klipp-mcp", "args": [], "env": {}}));
        assert_eq!(Client::Cursor.entry(p)["type"], "stdio");
    }

    /// Registration read from a settings file: Klipp's entry in the
    /// assistant's section, comments allowed; the file left as it was.
    #[test]
    fn a_settings_files_registration_is_read() {
        let zed = "{\n  // fonts\n  \"buffer_font_size\": 15,\n  \"context_servers\": {\n    \"klipp\": {\"command\": \"/usr/bin/klipp-mcp\", \"args\": []},\n  },\n}";
        assert_eq!(file_registration(zed, "context_servers"), Registration::Connected("/usr/bin/klipp-mcp".into()));
        assert_eq!(file_registration("{\"mcpServers\": {\"other\": {}}}", "mcpServers"), Registration::NotConnected);
        assert!(matches!(file_registration("{ broken", "mcpServers"), Registration::Unknown(_)));
    }

    /// Writing a settings file: backed up first, Klipp's entry set and
    /// the rest kept, a link written through; removing leaves the rest.
    #[test]
    fn a_settings_file_is_written_safely() {
        let dir = std::env::temp_dir().join(format!("klipp-assistant-write-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dotfiles")).unwrap();
        let real = dir.join("dotfiles").join("mcp.json");
        let before = "{\n  \"mcpServers\": {\n    \"other\": {\"command\": \"x\"}\n  }\n}\n";
        std::fs::write(&real, before).unwrap();
        let link = dir.join("mcp.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let entry = Client::Cursor.entry(Path::new("/usr/bin/klipp-mcp"));
        let backup = write_settings(&link, |t| crate::jsonc_edit::set(t, "mcpServers", SERVER_NAME, &entry).map(Some)).unwrap().expect("a backup");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), before, "the backup is the file as it was");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
        let after = std::fs::read_to_string(&real).unwrap();
        assert_eq!(file_registration(&after, "mcpServers"), Registration::Connected("/usr/bin/klipp-mcp".into()));
        assert!(after.contains("\"other\""), "{after}");
        write_settings(&link, |t| crate::jsonc_edit::remove(t, "mcpServers", SERVER_NAME)).unwrap();
        let removed = std::fs::read_to_string(&real).unwrap();
        assert_eq!(file_registration(&removed, "mcpServers"), Registration::NotConnected);
        assert!(removed.contains("\"other\""));
        // Nothing to remove: nothing written, no backup.
        assert_eq!(write_settings(&link, |t| crate::jsonc_edit::remove(t, "mcpServers", SERVER_NAME)).unwrap(), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The settings for other assistants, in each shape, name the command.
    #[test]
    fn every_snippet_names_the_command() {
        let p = Path::new("/usr/bin/klipp-mcp");
        let v: serde_json::Value = serde_json::from_str(&servers_snippet(p)).unwrap();
        assert_eq!(v["servers"]["klipp"]["command"], "/usr/bin/klipp-mcp");
        assert_eq!(v["servers"]["klipp"]["type"], "stdio");
        assert!(yaml_snippet(p).contains("cmd: \"/usr/bin/klipp-mcp\""), "{}", yaml_snippet(p));
    }
}
