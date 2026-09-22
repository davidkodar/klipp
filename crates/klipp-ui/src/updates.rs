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
        let preview = prefs.preview_updates;
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let exe = std::env::current_exe().unwrap_or_default();
            let owner = Command::new("/usr/bin/pacman")
                .args(["-Qqo"])
                .arg(&exe)
                .output()
                .ok();
            let owner = owner
                .as_ref()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout));
            let install = installation(&exe, owner.as_deref());
            let repository = install == Installation::ArchRelease
                && Command::new("/usr/bin/pacman-conf")
                    .args(["--repo", "klipp", "Server"])
                    .output()
                    .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
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

    pub(crate) fn show(&mut self, ctx: &egui::Context, prefs: &mut GeneralPrefs, dirty: &mut bool) {
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
            .is_some_and(|r| r.preview != prefs.preview_updates)
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
                if prefs.dismissed_update.as_deref() != Some(&version) && !self.open {
                    egui::Area::new(egui::Id::new("update_notice"))
                        .anchor(egui::Align2::RIGHT_TOP, [-16.0, 48.0])
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if ui.button(format!("Klipp {version} available")).clicked() {
                                        self.open = true;
                                    }
                                    if ui.small_button("Dismiss").clicked() {
                                        prefs.dismissed_update = Some(version.clone());
                                        *dirty = true;
                                    }
                                });
                            });
                        });
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
                if ui.checkbox(&mut prefs.check_for_updates, "Check automatically once a day").changed() { *dirty = true; }
                if ui.checkbox(&mut prefs.preview_updates, "Include preview releases").changed() {
                    *dirty = true; self.result = None;
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
}
