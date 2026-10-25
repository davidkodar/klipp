//! Release discovery is advisory. Pacman owns package verification and installation.
use crate::prefs::GeneralPrefs;
use semver::Version;
use serde::Deserialize;
use std::{
    io::Read,
    path::Path,
    process::Command,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RELEASES: &str = "https://github.com/davidkodar/klipp/releases";
const API: &str = "https://api.github.com/repos/davidkodar/klipp/releases?per_page=100";
const LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Clone, Debug, Deserialize)]
struct Asset {
    name: String,
}

/// The release package's file name for `version`, as the release
/// publishes it: Arch versions can't hold a hyphen, so 0.1.0-rc.3 is
/// packaged as 0.1.0rc.3. `klipp-update` builds the same name.
fn package_file(version: &Version) -> String {
    format!("klipp-{}-1-x86_64.pkg.tar.zst", version.to_string().replace('-', ""))
}

/// Whether the release of `version` carries its package and the sums
/// `klipp-update` checks it against.
fn has_package(releases: &[Release], version: &Version) -> bool {
    let tag = format!("v{version}");
    let file = package_file(version);
    releases.iter().filter(|r| r.tag_name == tag).any(|r| {
        r.assets.iter().any(|a| a.name == file) && r.assets.iter().any(|a| a.name == "SHA256SUMS")
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Installation {
    ArchRelease,
    ArchSource,
    Source,
}

fn installation(exe: &Path, owner: Option<&str>) -> Installation {
    if exe == Path::new("/usr/lib/klipp/klipp") {
        match owner.map(str::trim) {
            Some("klipp") => return Installation::ArchRelease,
            Some("klipp-git") => return Installation::ArchSource,
            _ => {}
        }
    }
    Installation::Source
}

fn probe_installation(exe: &Path, pacman: &Path, pacman_conf: &Path) -> (Installation, bool) {
    // Unmanaged builds and other distributions must not launch Arch commands.
    if exe != Path::new("/usr/lib/klipp/klipp") || !pacman.is_file() {
        return (Installation::Source, false);
    }
    let owner = Command::new(pacman).args(["-Qqo"]).arg(exe).output().ok();
    let owner = owner
        .as_ref()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout));
    let install = installation(exe, owner.as_deref());
    let repository = install == Installation::ArchRelease
        && pacman_conf.is_file()
        && Command::new(pacman_conf)
            .args(["--repo", "klipp", "Server"])
            .output()
            .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
    (install, repository)
}

/// Whether a build of version `current` offers preview releases when
/// the user hasn't said: a preview build (0.1.0-rc.2) is told of the
/// next preview, a stable one only of stable releases.
fn previews_by_default(current: &str) -> bool {
    Version::parse(current).is_ok_and(|v| !v.pre.is_empty())
}

/// Whether the update check offers preview releases: the user's choice,
/// else what this build offers by default.
pub(crate) fn include_previews(prefs: &GeneralPrefs) -> bool {
    prefs.preview_updates.unwrap_or_else(|| previews_by_default(klipp_core::build_info::get().version))
}

/// The switch for preview releases: shows what the check does, and
/// keeps a change as the user's choice. Whether it changed.
pub(crate) fn preview_switch(prefs: &mut GeneralPrefs, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui, &mut bool) -> egui::Response) -> bool {
    let mut on = include_previews(prefs);
    let changed = add(ui, &mut on).changed();
    if changed {
        prefs.preview_updates = Some(on);
    }
    changed
}

/// Starts the question about automatic checks is shown at before going
/// unanswered counts as no.
const ASK_STARTS: u32 = 3;

/// Whether to ask at this start whether to check automatically: not
/// when answered, or when checks are on (always the user's own choice),
/// and not past [`ASK_STARTS`] unanswered starts, which then count as no.
/// An ask counts as one of those starts.
fn ask_this_start(prefs: &mut GeneralPrefs) -> bool {
    if prefs.update_checks_answered || prefs.check_for_updates {
        return false;
    }
    if prefs.update_checks_asked >= ASK_STARTS {
        prefs.update_checks_answered = true;
        return false;
    }
    prefs.update_checks_asked += 1;
    true
}

/// The user's answer to the question, kept as their choice.
fn answer(prefs: &mut GeneralPrefs, yes: bool) {
    prefs.check_for_updates = yes;
    prefs.update_checks_answered = true;
}

fn newest(releases: &[Release], current: &str, preview: bool) -> Result<Option<Version>, String> {
    let current =
        Version::parse(current).map_err(|_| "This build has an invalid release version.")?;
    Ok(releases
        .iter()
        .filter(|r| !r.draft && (preview || !r.prerelease))
        .filter_map(|r| {
            r.tag_name
                .strip_prefix('v')
                .and_then(|s| Version::parse(s).ok())
        })
        .filter(|v| (preview || v.pre.is_empty()) && v.cmp_precedence(&current).is_gt())
        .max_by(Version::cmp_precedence))
}

/// The newest release past `current` on the channel, and whether it
/// carries a package to install.
fn fetch(current: &str, preview: bool) -> Result<Option<(Version, bool)>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build();
    let response = agent
        .get(API)
        .set("User-Agent", "Klipp-update-check")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(404, _) => {
                "Releases are unavailable. The repository may still be private.".to_owned()
            }
            ureq::Error::Status(403 | 429, _) => {
                "The release service is rate limited. Please try again later.".to_owned()
            }
            _ => "GitHub could not be reached. Check your connection.".to_owned(),
        })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the release list.")?;
    if bytes.len() as u64 > LIMIT {
        return Err("The release list exceeded the size limit.".into());
    }
    let releases: Vec<Release> = serde_json::from_slice(&bytes)
        .map_err(|_| "The release service returned an invalid response.")?;
    Ok(newest(&releases, current, preview)?.map(|v| {
        let package = has_package(&releases, &v);
        (v, package)
    }))
}

struct Check {
    release: Result<Option<Version>, String>,
    installation: Installation,
    repository: bool,
    preview: bool,
    /// The version checked as installed, when a test override set it.
    testing: Option<String>,
    /// A test override said the repository is configured: it is taken to
    /// carry previews too, so the updater shows for one.
    forced_repository: bool,
    /// The newer release carries its package, which `klipp-update` can
    /// download and install without a repository.
    package: bool,
}

impl Check {
    /// Whether the package repository carries the newer release, so the
    /// updater updates from it rather than downloading the package.
    fn in_repository(&self, version: &Version) -> bool {
        self.repository && (version.pre.is_empty() || self.forced_repository)
    }
}

/// Overrides for trying the update check against the real releases: the
/// version it checks as installed (`KLIPP_UPDATE_TEST_VERSION`) and the
/// installation it finds (`KLIPP_UPDATE_TEST_INSTALL`). Read once, at
/// start; without them nothing changes.
#[derive(Debug, Clone, Default, PartialEq)]
struct TestOverrides {
    version: Option<String>,
    install: Option<(Installation, bool)>,
}

/// A test version: a semver, with or without a leading v.
fn parse_test_version(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.strip_prefix('v').unwrap_or(value);
    Version::parse(value).ok().map(|v| v.to_string())
}

/// A test installation, and whether the package repository counts as
/// configured.
fn parse_test_install(value: &str) -> Option<(Installation, bool)> {
    match value.trim() {
        "package" => Some((Installation::ArchRelease, false)),
        "repository" => Some((Installation::ArchRelease, true)),
        "klipp-git" => Some((Installation::ArchSource, false)),
        "source" => Some((Installation::Source, false)),
        _ => None,
    }
}

fn parse_overrides(version: Option<&str>, install: Option<&str>) -> TestOverrides {
    let version = version.and_then(|v| {
        let parsed = parse_test_version(v);
        if parsed.is_none() {
            tracing::warn!("KLIPP_UPDATE_TEST_VERSION={v:?} is not a version; ignored");
        }
        parsed
    });
    let install = install.and_then(|i| {
        let parsed = parse_test_install(i);
        if parsed.is_none() {
            tracing::warn!("KLIPP_UPDATE_TEST_INSTALL={i:?} is not package, repository, klipp-git or source; ignored");
        }
        parsed
    });
    TestOverrides { version, install }
}

fn test_overrides() -> &'static TestOverrides {
    static OVERRIDES: std::sync::OnceLock<TestOverrides> = std::sync::OnceLock::new();
    OVERRIDES.get_or_init(|| {
        let var = |name| std::env::var(name).ok();
        let overrides = parse_overrides(var("KLIPP_UPDATE_TEST_VERSION").as_deref(), var("KLIPP_UPDATE_TEST_INSTALL").as_deref());
        if let Some(version) = &overrides.version {
            tracing::info!("update check: testing as installed version {version} (KLIPP_UPDATE_TEST_VERSION)");
        }
        if let Some((installation, repository)) = &overrides.install {
            tracing::info!("update check: testing as installation {installation:?}, repository {repository} (KLIPP_UPDATE_TEST_INSTALL)");
        }
        overrides
    })
}

pub(crate) struct Updates {
    /// Whether this start's question about automatic checks is decided
    /// yet, and whether it is on screen.
    asked: bool,
    asking: bool,
    /// A notice that keeps the keys was drawn this frame.
    showing: bool,
    /// The new version's notice put off until the next start: closed, or
    /// its details opened.
    later: bool,
    check_requested: bool,
    started: Instant,
    pending: Option<mpsc::Receiver<Check>>,
    result: Option<Check>,
    /// The check's thread ended without a result.
    stopped: Option<String>,
    /// The updater could not be opened.
    launch_error: Option<String>,
    /// The updater running: what to say if it ends without updating.
    launched: Option<mpsc::Receiver<String>>,
}

impl Default for Updates {
    fn default() -> Self {
        // Read and logged at start, not at the first check.
        test_overrides();
        Self {
            asked: false,
            asking: false,
            showing: false,
            later: false,
            check_requested: false,
            started: Instant::now(),
            pending: None,
            result: None,
            stopped: None,
            launch_error: None,
            launched: None,
        }
    }
}

impl Updates {
    fn start(&mut self, ctx: &egui::Context, prefs: &mut GeneralPrefs, dirty: &mut bool) {
        if self.pending.is_some() {
            return;
        }
        prefs.last_update_check = now();
        *dirty = true;
        self.result = None;
        self.stopped = None;
        self.launch_error = None;
        let preview = include_previews(prefs);
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        let overrides = test_overrides().clone();
        std::thread::spawn(move || {
            let (install, repository) = overrides.install.clone().unwrap_or_else(|| {
                let exe = std::env::current_exe().unwrap_or_default();
                probe_installation(&exe, Path::new("/usr/bin/pacman"), Path::new("/usr/bin/pacman-conf"))
            });
            let installed = overrides.version.as_deref().unwrap_or(klipp_core::build_info::get().version);
            let found = fetch(installed, preview);
            let package = matches!(found, Ok(Some((_, true))));
            let _ = tx.send(Check {
                release: found.map(|f| f.map(|(v, _)| v)),
                package,
                installation: install,
                repository,
                preview,
                testing: overrides.version,
                forced_repository: overrides.install.is_some_and(|(_, repository)| repository),
            });
            ctx.request_repaint();
        });
    }

    /// The question whether to check automatically, as a dialog in the
    /// middle of the window that keeps the keys while it is up. Closed
    /// unanswered, it is asked again at the next start ([`ask_this_start`]).
    fn ask(&mut self, ctx: &egui::Context, prefs: &mut GeneralPrefs, dirty: &mut bool) {
        let mut open = true;
        centred("Klipp Updates", "update_checks_question").open(&mut open).show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.label(egui::RichText::new("Check for new Klipp versions automatically?").strong());
            ui.add_space(4.0);
            ui.label("Klipp asks GitHub once a day; no project information is sent.");
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("No thanks").clicked() {
                    answer(prefs, false);
                    self.asking = false;
                    *dirty = true;
                }
                if ui.button("Yes, check daily").clicked() {
                    answer(prefs, true);
                    self.asking = false;
                    *dirty = true;
                }
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new("You can change this in Preferences. Close this to be asked next time.").small().weak());
        });
        // Closed unanswered: asked at the next start.
        if !open {
            self.asking = false;
        }
    }

    /// The new version's notice closed: not shown again until the next
    /// start. Dismiss is what keeps it away for good.
    fn put_off(&mut self) {
        self.later = true;
    }

    /// About opened from the new version's notice, on its result. The
    /// notice is put off too, rather than coming straight back after.
    fn show_details(&mut self, about: &mut bool) {
        *about = true;
        self.put_off();
    }

    /// Check now, from About.
    pub(crate) fn check_now(&mut self) {
        self.check_requested = true;
    }

    /// Check from elsewhere (Preferences): About opens, and checks.
    pub(crate) fn check_in_about(&mut self, about: &mut bool) {
        *about = true;
        self.check_now();
    }

    /// Whether a notice is up that keeps the keys: the question, or a new
    /// version's notice, drawn this frame.
    pub(crate) fn holds_keys(&self) -> bool {
        self.showing
    }

    /// Where the check is, for About's status row.
    pub(crate) fn status(&self) -> Status<'_> {
        if self.pending.is_some() {
            return Status::Checking;
        }
        if let Some(message) = &self.stopped {
            return Status::Failed(message);
        }
        let Some(check) = &self.result else { return Status::Idle };
        match &check.release {
            Err(message) => Status::Failed(message),
            Ok(None) => Status::Latest { previews: check.preview, testing: check.testing.as_deref() },
            Ok(Some(version)) => {
                let install = match check.installation {
                    Installation::ArchRelease if check.in_repository(version) || check.package => Install::Updater,
                    Installation::ArchRelease => Install::Download,
                    Installation::ArchSource => Install::Steps(ARCH_SOURCE_STEPS),
                    Installation::Source => Install::Steps(SOURCE_STEPS),
                };
                Status::Available { version: version.to_string(), notes: format!("{RELEASES}/tag/v{version}"), install, error: self.launch_error.as_deref(), testing: check.testing.as_deref() }
            }
        }
    }

    /// Open the package's updater: a terminal with pacman's own
    /// confirmation. Only offered for [`Install::Updater`]: from the
    /// repository when it carries the release, else given the version, whose
    /// package it downloads and verifies.
    pub(crate) fn install(&mut self) {
        self.launch_error = None;
        let Some(args) = self.updater_args() else { return };
        // Fixed, package-owned helper; no downloaded command or URL is executed.
        match Command::new("/usr/bin/klipp-update").args(args).spawn() {
            Ok(mut child) => {
                let (tx, rx) = mpsc::channel();
                self.launched = Some(rx);
                std::thread::spawn(move || {
                    // It refuses an installation that isn't the release
                    // package, and pacman can fail: say so in About.
                    if let Ok(status) = child.wait() {
                        if !status.success() {
                            let code = status.code().map_or_else(|| "stopped".to_owned(), |c| format!("exit {c}"));
                            let _ = tx.send(format!("The updater ended without updating ({code}). It updates only the release package installed with pacman."));
                        }
                    }
                });
            }
            Err(_) => self.launch_error = Some("Could not open the updater. Run sudo pacman -Syu klipp in a terminal.".into()),
        }
    }

    /// The updater's arguments for the version found: none to update from
    /// the repository, the version to download its package.
    fn updater_args(&self) -> Option<Vec<String>> {
        let check = self.result.as_ref()?;
        let Ok(Some(version)) = &check.release else { return None };
        Some(if check.in_repository(version) { Vec::new() } else { vec![version.to_string()] })
    }

    /// The question and the new version's notice, and the check itself.
    /// A new version's notice comes up only when no project is open, so a
    /// check that ends mid-edit waits rather than taking the keys, and not
    /// over About, which shows the result itself. `about` is whether About
    /// is open; the notice's "Show details…" opens it.
    pub(crate) fn show(&mut self, ctx: &egui::Context, prefs: &mut GeneralPrefs, dirty: &mut bool, project_open: bool, about: &mut bool) {
        if !std::mem::replace(&mut self.asked, true) {
            let before = (prefs.update_checks_answered, prefs.update_checks_asked);
            self.asking = ask_this_start(prefs);
            if (prefs.update_checks_answered, prefs.update_checks_asked) != before {
                *dirty = true;
            }
        }
        self.showing = false;
        if self.asking && !prefs.update_checks_answered {
            self.ask(ctx, prefs, dirty);
            self.showing = self.asking;
        }
        if std::mem::take(&mut self.check_requested) {
            self.start(ctx, prefs, dirty);
        }
        if let Some(rx) = &self.launched {
            match rx.try_recv() {
                Ok(message) => {
                    self.launch_error = Some(message);
                    self.launched = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.launched = None,
                Err(mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(500)),
            }
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(check) => {
                    self.result = Some(check);
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.stopped = Some("The update check stopped unexpectedly.".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // Re-evaluate when the channel changes; do not advertise a cached preview as stable.
        if self
            .result
            .as_ref()
            .is_some_and(|r| r.preview != include_previews(prefs))
        {
            self.result = None;
        }
        if prefs.check_for_updates
            && self.started.elapsed() >= Duration::from_secs(30)
            && (now().saturating_sub(prefs.last_update_check) >= 86400
                || prefs.last_update_check > now())
        {
            self.start(ctx, prefs, dirty);
        }
        if !prefs.check_for_updates {
            return;
        }
        ctx.request_repaint_after(Duration::from_secs(30));
        let Some(Check { release: Ok(Some(version)), .. }) = &self.result else { return };
        let version = version.to_string();
        if project_open || *about || self.later || prefs.dismissed_update.as_deref() == Some(&version) {
            return;
        }
        let (mut gone, mut open) = (false, true);
        centred("New Klipp Version", "update_notice").open(&mut open).show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.label(egui::RichText::new(format!("Klipp {version} available")).strong());
            ui.add_space(4.0);
            ui.label(format!("You have {}.", klipp_core::build_info::get().version));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Dismiss").clicked() {
                    prefs.dismissed_update = Some(version.clone());
                    *dirty = true;
                    gone = true;
                }
                if ui.button("Show details…").clicked() {
                    self.show_details(about);
                    gone = true;
                }
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Close this to be reminded at the next start.").small().weak());
        });
        if !open {
            self.put_off();
            gone = true;
        }
        self.showing |= !gone;
    }
}

/// Where the update check is, as About shows it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Status<'a> {
    /// Not checked since Klipp started.
    Idle,
    Checking,
    /// Nothing newer on the channel; `previews` when it includes them.
    /// `testing` is the version checked as installed, from a test override.
    Latest { previews: bool, testing: Option<&'a str> },
    Available { version: String, notes: String, install: Install, error: Option<&'a str>, testing: Option<&'a str> },
    Failed(&'a str),
}

/// How a newer version is put in, for the way this Klipp was installed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Install {
    /// The release package, with its repository: the package's updater.
    Updater,
    /// The release package without its repository, or a preview, which
    /// the repository doesn't carry: the release page's download.
    Download,
    /// Built from source: the steps to update it, to copy.
    Steps(Steps),
}

/// The steps that update a build from source, run in its checkout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Steps {
    pub(crate) what: &'static str,
    pub(crate) commands: &'static [&'static str],
}

/// klipp-git: a pull alone doesn't update the installed app.
pub(crate) const ARCH_SOURCE_STEPS: Steps = Steps { what: "Installed from source (klipp-git). In your checkout:", commands: &["git pull", "cd packaging && makepkg -si"] };
pub(crate) const SOURCE_STEPS: Steps = Steps { what: "Built from source. In your checkout:", commands: &["git pull", "cargo build --release -p klipp-app"] };

impl Steps {
    /// The steps as one line to paste in a terminal.
    pub(crate) fn line(&self) -> String {
        self.commands.join(" && ")
    }
}

/// A window in the middle of the screen, in front, as Klipp's other
/// dialogs are.
fn centred(title: &str, id: &str) -> egui::Window<'static> {
    egui::Window::new(title.to_owned())
        .id(egui::Id::new(id))
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn releases() -> Vec<Release> {
        serde_json::from_str(
            r#"[
            {"tag_name":"v0.9.0","draft":false,"prerelease":false},
            {"tag_name":"v0.20.0-rc.10","draft":false,"prerelease":true},
            {"tag_name":"v0.20.0-rc.2","draft":false,"prerelease":true},
            {"tag_name":"v0.10.0","draft":false,"prerelease":false},
            {"tag_name":"v1.0.0","draft":true,"prerelease":false},
            {"tag_name":"v2.0.0-beta.1","draft":false,"prerelease":false},
            {"tag_name":"garbage","draft":false,"prerelease":false}
        ]"#,
        )
        .unwrap()
    }
    #[test]
    fn versions_are_numeric_and_previews_are_opt_in() {
        assert_eq!(
            newest(&releases(), "0.9.0", false)
                .unwrap()
                .unwrap()
                .to_string(),
            "0.10.0"
        );
        assert!(newest(&releases(), "0.10.0", false).unwrap().is_none());
        let items: Vec<_> = releases()
            .into_iter()
            .filter(|r| !r.tag_name.starts_with("v2"))
            .collect();
        assert_eq!(
            newest(&items, "0.20.0-rc.2", true)
                .unwrap()
                .unwrap()
                .to_string(),
            "0.20.0-rc.10"
        );
        assert!(newest(&items, "0.20.0", true).unwrap().is_none());
    }
    /// A preview build is told of the next preview unless the user said
    /// otherwise; a stable build of stable releases only.
    #[test]
    fn a_preview_build_offers_the_next_preview_by_default() {
        assert!(previews_by_default("0.1.0-rc.2"));
        assert!(!previews_by_default("0.1.0"));
        assert!(!previews_by_default("invalid"));
        let r: Vec<Release> = serde_json::from_str(
            r#"[{"tag_name":"v0.1.0-rc.3","draft":false,"prerelease":true},
                {"tag_name":"v0.1.0-rc.2","draft":false,"prerelease":true}]"#,
        )
        .unwrap();
        let next = |current: &str| newest(&r, current, previews_by_default(current)).unwrap().map(|v| v.to_string());
        assert_eq!(next("0.1.0-rc.2").as_deref(), Some("0.1.0-rc.3"));
        assert_eq!(next("0.0.9"), None, "a stable build stays on stable releases");
    }

    /// The user's choice wins over the default either way, and the old
    /// key, always written as `false`, is not taken for one.
    #[test]
    fn an_explicit_choice_wins_and_the_old_default_is_not_one() {
        // A prefs file as written, with the key under test.
        let base = toml::to_string(&GeneralPrefs::default()).unwrap();
        let general = |key: &str| toml::from_str::<GeneralPrefs>(&format!("{key}\n{base}")).unwrap();
        assert_eq!(general("preview_updates = false").preview_updates, None);
        assert_eq!(general("include_preview_releases = false").preview_updates, Some(false));
        assert!(!include_previews(&general("include_preview_releases = false")));
        assert!(include_previews(&general("include_preview_releases = true")));
        let default = previews_by_default(klipp_core::build_info::get().version);
        assert_eq!(include_previews(&general("")), default);
        let written = toml::to_string(&GeneralPrefs { preview_updates: Some(true), ..GeneralPrefs::default() }).unwrap();
        assert!(written.contains("include_preview_releases = true") && !written.contains("preview_updates"), "{written}");
    }

    #[test]
    fn metadata_does_not_offer_a_same_version_update() {
        let r = vec![Release {
            tag_name: "v1.0.0+new".into(),
            draft: false,
            prerelease: false,
            assets: Vec::new(),
        }];
        assert!(newest(&r, "1.0.0+old", false).unwrap().is_none());
        assert!(newest(&r, "invalid", false).is_err());
    }
    #[test]
    fn only_owned_installed_binary_gets_package_updates() {
        let p = Path::new("/usr/lib/klipp/klipp");
        assert_eq!(installation(p, Some("klipp\n")), Installation::ArchRelease);
        assert_eq!(
            installation(p, Some("klipp-git\n")),
            Installation::ArchSource
        );
        assert_eq!(installation(p, None), Installation::Source);
        assert_eq!(
            installation(
                Path::new("/home/user/target/release/klipp-app"),
                Some("klipp")
            ),
            Installation::Source
        );
    }
    fn ui_text(updates: &mut Updates, prefs: &mut GeneralPrefs) -> String {
        fn append(shape: &egui::epaint::Shape, text: &mut String) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    text.push_str(&t.galley.job.text);
                    text.push('\n');
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for s in shapes {
                        append(s, text);
                    }
                }
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let mut text = String::new();
        for _ in 0..3 {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                updates.show(ctx, prefs, &mut false, false, &mut false)
            });
            text.clear();
            for shape in output.shapes {
                append(&shape.shape, &mut text);
            }
        }
        text
    }

    fn available(installation: Installation) -> Updates {
        Updates {
            result: Some(Check {
                release: Ok(Some(Version::parse("0.20.0").unwrap())),
                installation,
                repository: true,
                preview: false,
                testing: None,
                forced_repository: false,
                package: false,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn notifications_respect_disabled_checks_and_dismissal() {
        // A stable-channel result, on the stable channel.
        let mut prefs = GeneralPrefs {
            check_for_updates: true,
            preview_updates: Some(false),
            ..Default::default()
        };
        let mut updates = available(Installation::Source);
        assert!(ui_text(&mut updates, &mut prefs).contains("0.20.0 available"));
        prefs.dismissed_update = Some("0.20.0".into());
        assert!(!ui_text(&mut updates, &mut prefs).contains("available"));
        prefs.dismissed_update = None;
        prefs.check_for_updates = false;
        assert!(!ui_text(&mut updates, &mut prefs).contains("available"));
    }

    /// The package's updater is offered only to the release package with
    /// its repository, for a stable release; builds from source get their
    /// steps.
    #[test]
    fn package_action_is_not_shown_to_source_users() {
        let install = |updates: &Updates| match updates.status() {
            Status::Available { install, .. } => install,
            other => panic!("{other:?}"),
        };
        let mut updates = available(Installation::ArchRelease);
        assert_eq!(install(&updates), Install::Updater);
        updates.result.as_mut().unwrap().repository = false;
        assert_eq!(install(&updates), Install::Download);
        updates.result.as_mut().unwrap().installation = Installation::ArchSource;
        assert_eq!(install(&updates), Install::Steps(ARCH_SOURCE_STEPS));
        assert!(ARCH_SOURCE_STEPS.line().contains("makepkg -si"));
        updates.result.as_mut().unwrap().installation = Installation::Source;
        assert_eq!(install(&updates), Install::Steps(SOURCE_STEPS));
        let mut preview = available(Installation::ArchRelease);
        preview.result.as_mut().unwrap().release = Ok(Some(Version::parse("0.20.0-rc.1").unwrap()));
        assert_eq!(install(&preview), Install::Download, "the repository carries no previews");
    }

    /// The test overrides parse what RELEASING.md documents and ignore
    /// anything else; without them there are none.
    #[test]
    fn test_overrides_parse() {
        assert_eq!(parse_overrides(None, None), TestOverrides::default());
        assert_eq!(parse_test_version("0.1.0-rc.1").as_deref(), Some("0.1.0-rc.1"));
        assert_eq!(parse_test_version(" v0.1.0 ").as_deref(), Some("0.1.0"));
        assert_eq!(parse_test_version("rc1"), None);
        assert_eq!(parse_test_install("package"), Some((Installation::ArchRelease, false)));
        assert_eq!(parse_test_install("repository"), Some((Installation::ArchRelease, true)));
        assert_eq!(parse_test_install("klipp-git"), Some((Installation::ArchSource, false)));
        assert_eq!(parse_test_install("source"), Some((Installation::Source, false)));
        assert_eq!(parse_test_install("Package"), None);
        let both = parse_overrides(Some("0.1.0-rc.1"), Some("repository"));
        assert_eq!(both, TestOverrides { version: Some("0.1.0-rc.1".into()), install: Some((Installation::ArchRelease, true)) });
        assert_eq!(parse_overrides(Some("nope"), Some("nope")), TestOverrides::default());
    }

    /// Each test installation gives About its action, for a preview as
    /// the real releases are, and the row names the version tested as.
    #[test]
    fn each_test_install_maps_to_its_action() {
        for (value, package, want) in [
            ("package", true, Install::Updater),
            ("package", false, Install::Download),
            ("repository", false, Install::Updater),
            ("klipp-git", true, Install::Steps(ARCH_SOURCE_STEPS)),
            ("source", true, Install::Steps(SOURCE_STEPS)),
        ] {
            let overrides = parse_overrides(Some("0.1.0-rc.1"), Some(value));
            let (installation, repository) = overrides.install.clone().unwrap();
            let updates = Updates {
                result: Some(Check {
                    release: Ok(Some(Version::parse("0.1.0-rc.2").unwrap())),
                    installation,
                    repository,
                    preview: true,
                    testing: overrides.version.clone(),
                    forced_repository: repository,
                    package,
                }),
                ..Default::default()
            };
            match updates.status() {
                Status::Available { install, testing, .. } => {
                    assert_eq!(install, want, "{value}");
                    assert_eq!(testing, Some("0.1.0-rc.1"));
                }
                other => panic!("{value}: {other:?}"),
            }
        }
    }

    /// A release offers its package when it carries the file pacman's
    /// version spelling gives and the sums; the updater is given the version
    /// unless the repository carries it.
    #[test]
    fn a_package_install_downloads_the_release_package() {
        let rc = Version::parse("0.1.0-rc.2").unwrap();
        assert_eq!(package_file(&rc), "klipp-0.1.0rc.2-1-x86_64.pkg.tar.zst");
        assert_eq!(package_file(&Version::parse("0.2.0").unwrap()), "klipp-0.2.0-1-x86_64.pkg.tar.zst");
        let releases: Vec<Release> = serde_json::from_str(
            r#"[{"tag_name":"v0.1.0-rc.2","draft":false,"prerelease":true,"assets":[
                    {"name":"klipp-0.1.0-rc.2-source.tar.gz"},{"name":"klipp-0.1.0rc.2-1-x86_64.pkg.tar.zst"},{"name":"SHA256SUMS"}]},
                {"tag_name":"v0.1.0-rc.3","draft":false,"prerelease":true,"assets":[{"name":"SHA256SUMS"}]},
                {"tag_name":"v0.1.0-rc.4","draft":false,"prerelease":true}]"#,
        )
        .unwrap();
        assert!(has_package(&releases, &rc));
        assert!(!has_package(&releases, &Version::parse("0.1.0-rc.3").unwrap()), "sums but no package");
        assert!(!has_package(&releases, &Version::parse("0.1.0-rc.4").unwrap()), "no assets");

        let mut updates = available(Installation::ArchRelease);
        let check = updates.result.as_mut().unwrap();
        check.release = Ok(Some(rc.clone()));
        check.package = true;
        assert_eq!(updates.updater_args(), Some(vec!["0.1.0-rc.2".to_owned()]), "a preview isn't in the repository");
        updates.result.as_mut().unwrap().repository = false;
        assert!(matches!(updates.status(), Status::Available { install: Install::Updater, .. }));
        assert_eq!(updates.updater_args(), Some(vec!["0.1.0-rc.2".to_owned()]));
        let mut stable = available(Installation::ArchRelease);
        assert_eq!(stable.updater_args(), Some(Vec::new()), "a stable release comes from the repository");
        stable.result.as_mut().unwrap().repository = false;
        assert_eq!(stable.status(), Status::Available { version: "0.20.0".into(), notes: format!("{RELEASES}/tag/v0.20.0"), install: Install::Download, error: None, testing: None }, "no package, no repository");
    }

    /// The status follows the check: idle, checking while it runs, then
    /// what it found or why it failed.
    #[test]
    fn the_status_follows_the_check() {
        let mut updates = Updates::default();
        assert_eq!(updates.status(), Status::Idle);
        let (tx, rx) = mpsc::channel::<Check>();
        updates.pending = Some(rx);
        assert_eq!(updates.status(), Status::Checking);
        drop(tx);
        let mut prefs = checking();
        frames(&mut updates, &mut prefs, true);
        assert!(matches!(updates.status(), Status::Failed(_)), "a check that stops is a failure");
        updates.stopped = None;
        updates.result = Some(Check { release: Ok(None), installation: Installation::Source, repository: false, preview: false, testing: None, forced_repository: false, package: false });
        assert_eq!(updates.status(), Status::Latest { previews: false, testing: None });
        updates.result.as_mut().unwrap().release = Err("Offline.".into());
        assert_eq!(updates.status(), Status::Failed("Offline."));
    }

    /// Preferences' "Check for updates…" opens About and checks there.
    #[test]
    fn preferences_check_opens_about() {
        let mut updates = Updates::default();
        let mut about = false;
        updates.check_in_about(&mut about);
        assert!(about && updates.check_requested);
    }

    #[test]
    fn no_background_work_without_opt_in_even_after_startup_delay() {
        let mut prefs = GeneralPrefs::default();
        let mut updates = Updates {
            started: Instant::now() - Duration::from_secs(60),
            ..Default::default()
        };
        ui_text(&mut updates, &mut prefs);
        assert!(updates.pending.is_none());
        assert!(updates.result.is_none());
        assert_eq!(prefs.last_update_check, 0);
    }

    #[test]
    fn preferences_require_fresh_consent_and_preserve_explicit_opt_in() {
        let mut json = serde_json::to_value(GeneralPrefs::default()).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("automatic_update_checks_opt_in");
        let missing: GeneralPrefs = serde_json::from_value(json.clone()).unwrap();
        assert!(!missing.check_for_updates);
        // The first implementation saved true by default; do not treat that
        // ambiguous legacy value as permission to start network requests.
        json["check_for_updates"] = true.into();
        let legacy: GeneralPrefs = serde_json::from_value(json.clone()).unwrap();
        assert!(!legacy.check_for_updates);
        json["automatic_update_checks_opt_in"] = true.into();
        let opted_in: GeneralPrefs = serde_json::from_value(json).unwrap();
        assert!(opted_in.check_for_updates);
        let roundtrip: GeneralPrefs =
            serde_json::from_value(serde_json::to_value(opted_in).unwrap()).unwrap();
        assert!(roundtrip.check_for_updates);
    }

    /// Asked from the first start; unanswered, at the next two starts
    /// too, then it counts as no and isn't asked again.
    #[test]
    fn the_question_is_asked_three_starts_then_counts_as_no() {
        let mut prefs = GeneralPrefs::default();
        for start in 1..=ASK_STARTS {
            assert!(ask_this_start(&mut prefs), "start {start}");
        }
        assert!(!ask_this_start(&mut prefs));
        assert!(prefs.update_checks_answered && !prefs.check_for_updates);
        assert!(!ask_this_start(&mut prefs), "and stays answered");
    }

    /// An answer, or checks already turned on, is never asked about.
    #[test]
    fn an_answer_or_checks_already_on_are_not_asked() {
        for yes in [true, false] {
            let mut prefs = GeneralPrefs::default();
            assert!(ask_this_start(&mut prefs));
            answer(&mut prefs, yes);
            assert_eq!(prefs.check_for_updates, yes);
            assert!(!ask_this_start(&mut prefs));
        }
        // Turned on in an earlier build, where only the user could.
        let mut json = serde_json::to_value(GeneralPrefs::default()).unwrap();
        json.as_object_mut().unwrap().remove("update_checks_answered");
        json.as_object_mut().unwrap().remove("update_checks_asked");
        json["automatic_update_checks_opt_in"] = true.into();
        let mut on: GeneralPrefs = serde_json::from_value(json.clone()).unwrap();
        assert!(!ask_this_start(&mut on));
        // Off there was written whether chosen or not: asked.
        json["automatic_update_checks_opt_in"] = false.into();
        let mut off: GeneralPrefs = serde_json::from_value(json).unwrap();
        assert!(ask_this_start(&mut off));
    }

    /// The question shows at the first start, with its wording, keeps
    /// the keys while it is up, and goes once answered.
    #[test]
    fn the_question_shows_at_the_first_start_until_answered() {
        let mut prefs = GeneralPrefs::default();
        let mut updates = Updates::default();
        let text = ui_text(&mut updates, &mut prefs);
        assert!(text.contains("Check for new Klipp versions automatically?"), "{text}");
        assert!(text.contains("Klipp asks GitHub once a day; no project information is sent."));
        assert!(text.contains("Yes, check daily") && text.contains("No thanks"));
        assert!(updates.holds_keys(), "the keys stay with the question");
        assert_eq!(prefs.update_checks_asked, 1, "one start, however many frames");
        answer(&mut prefs, false);
        assert!(!ui_text(&mut updates, &mut prefs).contains("automatically?"));
        assert!(!updates.holds_keys(), "and go back to the app once it is answered");
    }

    fn checking() -> GeneralPrefs {
        GeneralPrefs { check_for_updates: true, update_checks_answered: true, preview_updates: Some(false), ..Default::default() }
    }

    fn frames(updates: &mut Updates, prefs: &mut GeneralPrefs, idle: bool) -> String {
        let ctx = egui::Context::default();
        let mut text = String::new();
        for _ in 0..3 {
            let output = ctx.run(egui::RawInput::default(), |ctx| updates.show(ctx, prefs, &mut false, !idle, &mut false));
            text = output.shapes.iter().filter_map(|s| match &s.shape { egui::epaint::Shape::Text(t) => Some(t.galley.job.text.clone()), _ => None }).collect::<Vec<_>>().join("\n");
        }
        text
    }

    /// A check that ends with a project open doesn't put a notice over
    /// the edit and take its keys: it waits until none is open.
    #[test]
    fn the_new_version_waits_while_a_project_is_open() {
        let mut prefs = checking();
        let mut updates = available(Installation::Source);
        assert!(!frames(&mut updates, &mut prefs, false).contains("available"), "not over an edit");
        assert!(!updates.holds_keys(), "and the keys stay with the edit");
        assert!(frames(&mut updates, &mut prefs, true).contains("0.20.0 available"), "shown once no project is open");
        assert!(updates.holds_keys());
    }

    /// Closed, or its details opened in About, the notice stays away
    /// until the next start; it isn't dismissed for good. No separate
    /// updates window comes up.
    #[test]
    fn the_new_version_closed_or_opened_comes_back_only_at_the_next_start() {
        let mut prefs = checking();
        let mut closed = available(Installation::Source);
        closed.put_off();
        assert!(!frames(&mut closed, &mut prefs, true).contains("available"), "closed");
        assert!(!closed.holds_keys());

        let mut details = available(Installation::Source);
        let mut about = false;
        let ctx = egui::Context::default();
        crate::panels::about::probe::click(&ctx, "Show details…", |ctx| details.show(ctx, &mut prefs, &mut false, false, &mut about));
        assert!(about, "Show details… opens About");
        let after = ctx.run(egui::RawInput::default(), |ctx| details.show(ctx, &mut prefs, &mut false, false, &mut about));
        let after: Vec<_> = crate::panels::about::probe::texts(&after).into_iter().map(|(t, _)| t).collect();
        assert!(!after.iter().any(|t| t.contains("available") || t.contains("Installed version")), "and nothing else: {after:?}");
        assert!(matches!(details.status(), Status::Available { .. }), "About shows the result");
        assert!(!frames(&mut details, &mut prefs, true).contains("available"), "not straight back once About closes");
        assert_eq!(prefs.dismissed_update, None, "neither dismisses it");
        let mut next_start = available(Installation::Source);
        assert!(frames(&mut next_start, &mut prefs, true).contains("0.20.0 available"), "back at the next start");
    }

    /// With About open the notice waits: About shows the result itself.
    #[test]
    fn the_notice_is_not_drawn_over_about() {
        let mut prefs = checking();
        let mut updates = available(Installation::Source);
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let out = ctx.run(egui::RawInput::default(), |ctx| updates.show(ctx, &mut prefs, &mut false, false, &mut true));
            assert!(crate::panels::about::probe::texts(&out).is_empty());
        }
        assert!(!updates.holds_keys());
    }

    /// The separate updates window is gone, whatever the check found.
    #[test]
    fn there_is_no_updates_window() {
        let source = include_str!("updates.rs");
        let window = ["Window::new(\"Klipp ", "updates\")"].concat();
        assert!(!source.contains(&window));
        let mut prefs = checking();
        for installation in [Installation::ArchRelease, Installation::ArchSource, Installation::Source] {
            let mut updates = available(installation);
            updates.put_off();
            assert_eq!(frames(&mut updates, &mut prefs, true), "", "nothing drawn outside About");
        }
    }

    /// Both notices sit in the middle of the window, not in a corner.
    #[test]
    fn the_notices_are_centred() {
        let rect_of = |updates: &mut Updates, prefs: &mut GeneralPrefs, id: &str| {
            let ctx = egui::Context::default();
            let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))), ..Default::default() };
            for _ in 0..3 {
                let _ = ctx.run(input(), |ctx| updates.show(ctx, prefs, &mut false, false, &mut false));
            }
            ctx.memory(|m| m.area_rect(egui::Id::new(id))).expect("drawn")
        };
        let centre = egui::pos2(800.0, 450.0);
        let mut prefs = GeneralPrefs::default();
        let question = rect_of(&mut Updates::default(), &mut prefs, "update_checks_question");
        assert!((question.center() - centre).length() < 2.0, "the question at {question:?}");
        let mut prefs = checking();
        let mut updates = available(Installation::Source);
        let notice = rect_of(&mut updates, &mut prefs, "update_notice");
        assert!((notice.center() - centre).length() < 2.0, "the notice at {notice:?}");
        assert!(updates.holds_keys(), "the notice keeps the keys too");
    }

    #[test]
    fn missing_arch_tools_and_unmanaged_paths_do_not_probe() {
        let missing = Path::new("/klipp-test-missing/pacman");
        assert_eq!(
            probe_installation(Path::new("/usr/lib/klipp/klipp"), missing, missing),
            (Installation::Source, false)
        );
        assert_eq!(
            probe_installation(
                Path::new("/tmp/klipp-app"),
                Path::new("/usr/bin/pacman"),
                Path::new("/usr/bin/pacman-conf")
            ),
            (Installation::Source, false)
        );
    }
}
