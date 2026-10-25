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

fn fetch(current: &str, preview: bool) -> Result<Option<Version>, String> {
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
            _ => "Could not check for updates. Check your connection and try again.".to_owned(),
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
    newest(&releases, current, preview)
}

struct Check {
    release: Result<Option<Version>, String>,
    installation: Installation,
    repository: bool,
    preview: bool,
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
    open: bool,
    check_on_open: bool,
    started: Instant,
    pending: Option<mpsc::Receiver<Check>>,
    result: Option<Check>,
    launch_error: Option<String>,
}

impl Default for Updates {
    fn default() -> Self {
        Self {
            asked: false,
            asking: false,
            showing: false,
            later: false,
            open: false,
            check_on_open: false,
            started: Instant::now(),
            pending: None,
            result: None,
            launch_error: None,
        }
    }
}

impl Updates {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.check_on_open = true;
    }

    fn start(&mut self, ctx: &egui::Context, prefs: &mut GeneralPrefs, dirty: &mut bool) {
        if self.pending.is_some() {
            return;
        }
        prefs.last_update_check = now();
        *dirty = true;
        self.result = None;
        let preview = include_previews(prefs);
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let exe = std::env::current_exe().unwrap_or_default();
            let (install, repository) = probe_installation(
                &exe,
                Path::new("/usr/bin/pacman"),
                Path::new("/usr/bin/pacman-conf"),
            );
            let release = fetch(klipp_core::build_info::get().version, preview);
            let _ = tx.send(Check {
                release,
                installation: install,
                repository,
                preview,
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

    /// The updates window opened from the new version's notice. Closing it
    /// puts the notice off too, rather than bringing it straight back.
    fn show_details(&mut self) {
        self.open = true;
        self.put_off();
    }

    /// Whether a notice is up that keeps the keys: the question, or a new
    /// version's notice, drawn this frame.
    pub(crate) fn holds_keys(&self) -> bool {
        self.showing
    }

    /// The question, the new version's notice and the updates window.
    /// `idle` says nothing is being edited (no project is open): only then
    /// does a new version's notice come up, so a check that ends mid-edit
    /// waits for it rather than taking the keys.
    pub(crate) fn show(&mut self, ctx: &egui::Context, prefs: &mut GeneralPrefs, dirty: &mut bool, idle: bool) {
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
        if std::mem::take(&mut self.check_on_open) {
            self.start(ctx, prefs, dirty);
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(check) => {
                    self.result = Some(check);
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.launch_error =
                        Some("The update check stopped unexpectedly. Please try again.".into());
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
        if prefs.check_for_updates {
            ctx.request_repaint_after(Duration::from_secs(30));
            if let Some(Check {
                release: Ok(Some(version)),
                ..
            }) = &self.result
            {
                let version = version.to_string();
                if idle && !self.later && !self.open && prefs.dismissed_update.as_deref() != Some(&version) {
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
                                self.show_details();
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
        }
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Klipp updates").open(&mut open).collapsible(false).resizable(false)
            .default_width(420.0).show(ctx, |ui| {
                ui.label(format!("Installed version: {}", klipp_core::build_info::get().version));
                if ui.checkbox(&mut prefs.check_for_updates, "Check automatically once a day").changed() {
                    prefs.update_checks_answered = true;
                    *dirty = true;
                }
                if preview_switch(prefs, ui, |ui, on| ui.checkbox(on, "Include preview releases")) {
                    *dirty = true; self.result = None;
                }
                if prefs.preview_updates.is_none() && previews_by_default(klipp_core::build_info::get().version) {
                    ui.small("On by default because this is a preview build.");
                }
                ui.small("Checks GitHub for releases. No project information is sent.");
                if ui.add_enabled(self.pending.is_none(), egui::Button::new("Check now")).clicked() {
                    self.launch_error = None;
                    self.start(ctx, prefs, dirty);
                }
                if self.pending.is_some() { ui.horizontal(|ui| { ui.spinner(); ui.label("Checking…"); }); }
                if let Some(check) = &self.result {
                    match &check.release {
                        Ok(Some(version)) => {
                            ui.label(format!("Version {version} is available."));
                            ui.hyperlink_to("Release notes and downloads", format!("{RELEASES}/tag/v{version}"));
                            match check.installation {
                                Installation::ArchRelease if check.repository && version.pre.is_empty() => {
                                    ui.label("Save your work and finish any exports first. Pacman will review and update the system as well as Klipp.");
                                    if ui.button("Open system updater…").clicked() {
                                        // Fixed, package-owned helper; no downloaded command or URL is executed.
                                        match Command::new("/usr/bin/klipp-update").spawn() {
                                            Ok(mut child) => { std::thread::spawn(move || { let _ = child.wait(); }); }
                                            Err(_) => self.launch_error = Some("Could not open the updater. Run sudo pacman -Syu klipp in a terminal.".into()),
                                        }
                                    }
                                    ui.small("After a successful update, close and reopen Klipp. Repository availability may lag the release announcement.");
                                }
                                Installation::ArchRelease => {
                                    ui.label("Use the release download, or configure the Klipp package repository. Preview releases require a manual installation.");
                                }
                                Installation::ArchSource => {
                                    ui.label("Installed from source (klipp-git). In your checkout, pull the changes, then run makepkg -si in packaging/. Git pull alone does not update the installed app.");
                                }
                                Installation::Source => {
                                    ui.label("Development or unmanaged build. Update your source checkout and rebuild, or install a release package.");
                                }
                            }
                        }
                        Ok(None) => { ui.label("No newer published release was found for this channel."); }
                        Err(message) => { ui.label(message); }
                    }
                }
                if let Some(error) = &self.launch_error { ui.label(error); }
                ui.hyperlink_to("All releases", RELEASES);
            });
        self.open = open;
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
                updates.show(ctx, prefs, &mut false, true)
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

    #[test]
    fn package_action_is_not_shown_to_source_users() {
        let mut prefs = GeneralPrefs { preview_updates: Some(false), ..Default::default() };
        let mut updates = available(Installation::ArchRelease);
        updates.open = true;
        assert!(ui_text(&mut updates, &mut prefs).contains("Open system updater"));
        updates.result.as_mut().unwrap().installation = Installation::ArchSource;
        let text = ui_text(&mut updates, &mut prefs);
        assert!(!text.contains("Open system updater"));
        assert!(text.contains("makepkg -si"));
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
            let output = ctx.run(egui::RawInput::default(), |ctx| updates.show(ctx, prefs, &mut false, idle));
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

    /// Closed, or its details opened and closed, the notice stays away
    /// until the next start; it isn't dismissed for good.
    #[test]
    fn the_new_version_closed_or_opened_comes_back_only_at_the_next_start() {
        let mut prefs = checking();
        let mut closed = available(Installation::Source);
        closed.put_off();
        assert!(!frames(&mut closed, &mut prefs, true).contains("available"), "closed");
        assert!(!closed.holds_keys());

        let mut details = available(Installation::Source);
        details.show_details();
        assert!(frames(&mut details, &mut prefs, true).contains("Installed version"), "the details window");
        details.open = false;
        assert!(!frames(&mut details, &mut prefs, true).contains("available"), "not straight back once the details close");
        assert_eq!(prefs.dismissed_update, None, "neither dismisses it");
        let mut next_start = available(Installation::Source);
        assert!(frames(&mut next_start, &mut prefs, true).contains("0.20.0 available"), "back at the next start");
    }

    /// Both notices sit in the middle of the window, not in a corner.
    #[test]
    fn the_notices_are_centred() {
        let rect_of = |updates: &mut Updates, prefs: &mut GeneralPrefs, id: &str| {
            let ctx = egui::Context::default();
            let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 900.0))), ..Default::default() };
            for _ in 0..3 {
                let _ = ctx.run(input(), |ctx| updates.show(ctx, prefs, &mut false, true));
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
