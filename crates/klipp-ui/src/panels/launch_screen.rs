use crate::widgets::menu::MenuRows;
use crate::prefs::RecentProject;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use std::collections::HashMap;
use std::path::PathBuf;

/// The narrowest a card may get before the grid drops a column, and
/// the widest it may grow before one more column is better than more
/// size.
///
/// The card used to be a fixed 220 square, and the column count came
/// from dividing the available width by it — so a wider display got
/// *more small cards*, not a better look at any of them. At 3072 px
/// that meant ten cards across, their metadata truncated, and two
/// thirds of the window empty below.
/// Chosen so a wide window gives *fewer, larger* cards rather than
/// more small ones. At 200 the grid produced twelve columns of 211 px
/// on a 2715 px page — narrower than the fixed 220 it replaced, which
/// is the opposite of the point. At 280 the same page gives nine
/// columns of 287, and a half-width window four of 285: near enough
/// the same card either way, and big enough that the metadata line
/// stops truncating.
/// What a card should be, near enough, at any window width.
const CARD_TARGET_W: f32 = 300.0;
/// The range it may be pushed to when the target does not divide the
/// width evenly.
const CARD_MIN_W: f32 = 240.0;
const CARD_MAX_W: f32 = 340.0;
const CARD_GAP: f32 = 16.0;
/// The space under a card's thumbnail: above the name, the name's own
/// line, each detail line, and below the last.
const CARD_INFO_TOP: f32 = 10.0;
const CARD_NAME_LINE: f32 = 19.0;
const CARD_DETAIL_LINE: f32 = 15.0;
const CARD_INFO_BOTTOM: f32 = 8.0;

/// How tall a card of width `w` is with `lines` detail lines under its
/// name: a 16:9 thumbnail, then exactly the text, and nothing below it.
///
/// The card used to take a fixed aspect, with the thumbnail at 58% of
/// it, so whatever the text did not fill was an empty band at the
/// bottom of every card.
fn card_height(w: f32, lines: usize) -> f32 {
    1.0 + (w - 2.0) * 9.0 / 16.0 + CARD_INFO_TOP + CARD_NAME_LINE + lines as f32 * CARD_DETAIL_LINE + CARD_INFO_BOTTOM
}

/// How wide each card is, and how many fit, for a given grid width, and
/// how tall a card is with `lines` detail lines.
///
/// Columns are chosen to land the card nearest [`CARD_TARGET_W`],
/// rounding rather than flooring, so the card stays about the same
/// size at every window width and the grid simply holds more of them.
///
/// Flooring against a *minimum* is what the first version did, and it
/// only sets a floor: a 1169 px grid took three columns and then
/// stretched each card to 379x447 to fill the row — cards at nearly
/// the maximum size, seven rows deep, which is what "the cards look
/// stretched" was. The same code gave 287 px cards at 2715 px, so it
/// looked correct wherever it was measured and wrong at the size the
/// window actually spends its time.
///
/// Rounding to a target gives 280 at 1169 and 287 at 2715 — the same
/// card either way.
fn card_metrics(grid_width: f32, lines: usize) -> (usize, f32, f32) {
    let cols = (grid_width / CARD_TARGET_W).round().max(1.0) as usize;
    let gaps = CARD_GAP * (cols.saturating_sub(1)) as f32;
    let w = ((grid_width - gaps) / cols as f32).clamp(CARD_MIN_W, CARD_MAX_W);
    (cols, w, card_height(w, lines))
}
const SIDEBAR_W: f32 = 300.0;

/// The sidebar's rows: a recent project, a template, and the footer.
const RECENT_ROW_H: f32 = 46.0;
const TEMPLATE_ROW_H: f32 = 24.0;
const FOOTER_H: f32 = 76.0;

/// Where Help goes.
const HELP_URL: &str = "https://github.com/davidkodar/klipp";

/// How many frames a project's skim strip holds: the steps a pointer
/// moving across a card scrubs through.
pub const SKIM_FRAMES: usize = 12;

/// The project's skim strip, beside its poster frame (#1847).
pub fn skim_strip_path(poster: &std::path::Path) -> PathBuf {
    poster.with_extension("strip.png")
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum SortMode {
    #[default]
    Modified,
    Name,
    Size,
    /// How long the cut is. The obvious fourth key for video work, and
    /// `duration_secs` was already on every recent project — it is
    /// printed on the cards, it just could not be sorted by.
    Duration,
    Resolution,
    Fps,
    Location,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Name => "Name",
            Self::Size => "Size",
            Self::Duration => "Length",
            Self::Resolution => "Resolution",
            Self::Fps => "FPS",
            Self::Location => "Location",
        }
    }

    /// Which way a column sorts when first chosen: newest, longest,
    /// largest first; names and places A to Z.
    fn descending_first(self) -> bool {
        !matches!(self, Self::Name | Self::Location)
    }
}

/// The modes the grid's sort button steps through.
const GRID_SORTS: [SortMode; 4] = [SortMode::Modified, SortMode::Name, SortMode::Size, SortMode::Duration];

#[derive(Clone, Copy, PartialEq, Default)]
enum ViewMode {
    #[default]
    Grid,
    List,
}

/// What the background check found about one listed project.
#[derive(Clone, Debug, Default, PartialEq)]
struct ProjectStatus {
    /// An auto-save newer than its last save: work not saved.
    newer_autosave: bool,
    /// Who has it open now, when someone does.
    open_by: Option<String>,
}

/// A mark beside a recent project in the sidebar, each with its own
/// shape and a tooltip that says what it means.
#[derive(Clone, Debug, PartialEq)]
enum Mark {
    Pinned,
    OpenNow(String),
    Unsaved,
    MissingMedia,
}

impl Mark {
    fn tooltip(&self) -> String {
        match self {
            Self::Pinned => "Pinned to the top of the list".into(),
            Self::OpenNow(who) => format!("Open now ({who})"),
            Self::Unsaved => "Unsaved changes: an auto-save is newer than the last save".into(),
            Self::MissingMedia => "Missing media: some of its files cannot be found".into(),
        }
    }
}

fn marks_for(rp: &RecentProject, status: Option<&ProjectStatus>) -> Vec<Mark> {
    let mut marks = Vec::new();
    if rp.pinned {
        marks.push(Mark::Pinned);
    }
    if let Some(who) = status.and_then(|s| s.open_by.clone()) {
        marks.push(Mark::OpenNow(who));
    }
    if status.is_some_and(|s| s.newer_autosave) {
        marks.push(Mark::Unsaved);
    }
    if rp.missing_media == Some(true) {
        marks.push(Mark::MissingMedia);
    }
    marks
}

/// A project's poster frame, and a soft copy of it to fill the space a
/// picture of another shape leaves.
#[derive(Clone)]
struct Poster {
    sharp: egui::TextureHandle,
    soft: egui::TextureHandle,
}

impl Poster {
    fn aspect(&self) -> f32 {
        let [w, h] = self.sharp.size();
        w as f32 / h.max(1) as f32
    }
}

/// A template the sidebar offers: one of the built-in formats or one of
/// the user's own (#618).
#[derive(Clone, Debug, PartialEq)]
struct TemplateEntry {
    name: String,
    /// Its format, or what it holds, said short.
    detail: String,
    source: TemplateSource,
}

#[derive(Clone, Debug, PartialEq)]
enum TemplateSource {
    /// A place in the New Project dialog's built-in list.
    BuiltIn(usize),
    /// A template file of the user's.
    User(PathBuf),
}

impl TemplateEntry {
    /// The built-in format it is, when it is one.
    fn format(&self) -> Option<&'static crate::panels::new_project_dialog::Format> {
        match self.source {
            TemplateSource::BuiltIn(i) => crate::panels::new_project_dialog::FORMATS.get(i),
            TemplateSource::User(_) => None,
        }
    }

    /// The command that opens an unsaved Untitled project from it, at
    /// `rate` where the format offers that: nothing is written until the
    /// project is edited or saved (#1864).
    fn create(&self, rate: Option<crate::panels::new_project_dialog::Rate>) -> AppCommand {
        match (&self.source, self.format()) {
            (TemplateSource::User(path), _) => AppCommand::NewUntitledProject { fps_num: 0, fps_den: 0, width: 0, height: 0, template: Some(path.clone()), hdr: false },
            (TemplateSource::BuiltIn(_), Some(f)) => {
                let (fps_num, fps_den) = crate::panels::new_project_dialog::format_rate(f, rate).unwrap_or((0, 0));
                AppCommand::NewUntitledProject { fps_num, fps_den, width: f.width, height: f.height, template: None, hdr: f.hdr }
            }
            (TemplateSource::BuiltIn(_), None) => AppCommand::NewProject,
        }
    }
}

/// What the footer says of the disk: room on the projects drive and the
/// render cache's size.
#[derive(Clone, Debug, PartialEq)]
struct Storage {
    drive: PathBuf,
    free_bytes: Option<u64>,
    render_cache_bytes: u64,
}

/// `path` with the home folder written `~`.
fn tilde(path: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(std::path::Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A quiet text link at `pos`: secondary text, lit on hover.
fn quiet_link(ui: &mut egui::Ui, pos: egui::Pos2, text: &str, theme: &KlippTheme) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(11.0), theme.text_secondary);
    let rect = egui::Rect::from_min_size(pos, g.size());
    let resp = ui.interact(rect, ui.id().with(("launch_link", text)), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let colour = if resp.hovered() { theme.text_primary } else { theme.text_secondary };
    ui.painter().galley_with_override_text_color(pos, g, colour);
    resp
}

/// The name a project goes by: its file's, as Files and Open show it,
/// not a name stored inside it.
fn named_by_file(rp: &RecentProject) -> RecentProject {
    let mut rp = rp.clone();
    if let Some(stem) = rp.path.file_stem().map(|s| s.to_string_lossy().into_owned()).filter(|s| !s.is_empty()) {
        rp.name = stem;
    }
    rp
}

/// What the last frame drew, for the tests to find it by.
#[cfg(test)]
#[derive(Default)]
struct Seen {
    /// Each sidebar row: its project, its rect, and the poster texture
    /// drawn in it.
    sidebar: Vec<(PathBuf, egui::Rect, Option<egui::TextureId>)>,
    marks: Vec<(PathBuf, Mark, egui::Rect)>,
    cards: Vec<(PathBuf, egui::Rect)>,
}

#[derive(Default)]
pub struct LaunchScreen {
    search_query: String,
    posters: HashMap<PathBuf, Poster>,
    /// Each poster's skim strip once read, or when it was last looked
    /// for: it is written a little after the save that made the poster.
    strips: HashMap<PathBuf, (Option<Poster>, std::time::Instant)>,
    /// Posters there but not readable, not read again every frame.
    unreadable_posters: std::collections::HashSet<PathBuf>,
    renaming_project: Option<(PathBuf, String)>,
    /// The project Move to Trash asks about, until it is answered.
    confirm_trash: Option<PathBuf>,
    logo_texture: Option<egui::TextureHandle>,
    sort_mode: SortMode,
    sort_descending: bool,
    sort_chosen: bool,
    view_mode: ViewMode,
    /// Each listed project's auto-save and lock state, re-read from disk
    /// every few seconds rather than every repaint.
    status: HashMap<PathBuf, ProjectStatus>,
    status_checked_at: Option<std::time::Instant>,
    /// The check in flight. It stats every listed project, and a stat on
    /// a dead network mount can block for minutes, so it runs on its own
    /// thread (as the FILES browser reads directories) and a new one is
    /// not started while one is still out.
    status_rx: Option<std::sync::mpsc::Receiver<HashMap<PathBuf, ProjectStatus>>>,
    /// The grid shows the projects in the projects folder, not the recent
    /// ones (#1143).
    folder_view: bool,
    /// The projects folder's projects, re-read every few seconds on a
    /// thread of their own, as the auto-save check is: the folder may be
    /// on a mount that blocks.
    folder_projects: Vec<RecentProject>,
    folder_scanned: Option<(PathBuf, std::time::Instant)>,
    folder_rx: Option<std::sync::mpsc::Receiver<(PathBuf, Vec<RecentProject>)>>,
    /// Where the user's project templates are kept (#618).
    pub templates_dir: Option<PathBuf>,
    /// The user's templates, read every few seconds.
    user_templates: Vec<TemplateEntry>,
    templates_read_at: Option<std::time::Instant>,
    /// The footer's figures, measured on a thread of their own.
    storage: Option<Storage>,
    storage_checked_at: Option<std::time::Instant>,
    storage_rx: Option<std::sync::mpsc::Receiver<Storage>>,
    /// What's New is open.
    show_whats_new: bool,
    /// The frame rate last chosen in a format's rate menu, which every
    /// format offering it starts at (#1864).
    pub last_rate: Option<crate::panels::new_project_dialog::Rate>,
    #[cfg(test)]
    seen: Seen,
    /// Links opened outside Klipp, kept across frames for the tests.
    #[cfg(test)]
    opened: Vec<&'static str>,
}

impl LaunchScreen {
    /// Remove a cached thumbnail texture so it gets reloaded on next draw.
    pub fn invalidate_thumbnail(&mut self, path: &std::path::Path) {
        self.posters.remove(path);
        self.strips.remove(path);
        self.unreadable_posters.remove(path);
    }

    /// Read the user's templates afresh every few seconds.
    fn refresh_templates(&mut self) {
        if self.templates_read_at.is_some_and(|t| t.elapsed().as_secs() < 5) {
            return;
        }
        self.templates_read_at = Some(std::time::Instant::now());
        self.user_templates = self.templates_dir.as_deref()
            .map(klipp_project::template::list_templates)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(path, t)| t.ok().map(|t| TemplateEntry { name: t.name.clone(), detail: "Yours".into(), source: TemplateSource::User(path) }))
            .collect();
    }

    /// The templates offered: the user's own first, then the built-in
    /// formats, the same list the New Project dialog has.
    fn template_entries(&self) -> Vec<TemplateEntry> {
        let built_in = crate::panels::new_project_dialog::FORMATS.iter().enumerate().map(|(i, f)| {
            let detail = if f.width == 0 { "From the clip".to_string() } else { format!("{}\u{00D7}{}", f.width, f.height) };
            TemplateEntry { name: f.name.to_string(), detail, source: TemplateSource::BuiltIn(i) }
        });
        self.user_templates.iter().cloned().chain(built_in).collect()
    }

    /// Take the last storage measure, and start the next every fifteen
    /// seconds: a stat of the drive and a listing of the render cache,
    /// both of which can block on a slow disk.
    fn refresh_storage(&mut self, ctx: &egui::Context, projects_folder: Option<&std::path::Path>) {
        if let Some(rx) = &self.storage_rx {
            match rx.try_recv() {
                Ok(found) => {
                    self.storage = Some(found);
                    self.storage_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.storage_rx = None,
            }
        }
        if self.storage_checked_at.is_some_and(|t| t.elapsed().as_secs() < 15) {
            return;
        }
        self.storage_checked_at = Some(std::time::Instant::now());
        let drive = projects_folder.map(std::path::Path::to_path_buf)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let render = dirs::cache_dir().unwrap_or_default().join("klipp").join("render");
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("launch-storage".into())
            .spawn(move || {
                let free_bytes = crate::inspector::info_tab::disk_free_bytes(&drive);
                let (render_cache_bytes, _) = crate::cache_manager::scan_dir_size(&render);
                let _ = tx.send(Storage { drive, free_bytes, render_cache_bytes });
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.storage_rx = Some(rx);
        }
    }

    /// The skim strip of `rp`, looked for again every few seconds until
    /// it is there.
    fn strip(&mut self, ctx: &egui::Context, rp: &RecentProject) -> Option<Poster> {
        let poster = rp.thumbnail_path.as_ref()?;
        if let Some((strip, at)) = self.strips.get(poster) {
            if strip.is_some() || at.elapsed().as_secs() < 3 {
                return strip.clone();
            }
        }
        let path = skim_strip_path(poster);
        let strip = std::fs::read(&path).ok().and_then(|data| image_from_png(&data).ok()).map(|img| {
            let name = path.to_string_lossy();
            let soft = crate::app::alloc_texture_logged(ctx, &format!("{name}#soft"), soften(&img, 12 * SKIM_FRAMES), egui::TextureOptions::LINEAR);
            let sharp = crate::app::alloc_texture_logged(ctx, &name, img, egui::TextureOptions::LINEAR);
            Poster { sharp, soft }
        });
        self.strips.insert(poster.clone(), (strip.clone(), std::time::Instant::now()));
        strip
    }

    /// The poster of `rp`, read from disk the first time it is asked for.
    fn poster(&mut self, ctx: &egui::Context, rp: &RecentProject) -> Option<Poster> {
        let path = rp.thumbnail_path.as_ref()?;
        if let Some(p) = self.posters.get(path) {
            return Some(p.clone());
        }
        if self.unreadable_posters.contains(path) || !path.exists() {
            return None;
        }
        let Some(img) = std::fs::read(path).ok().and_then(|data| image_from_png(&data).ok()) else {
            self.unreadable_posters.insert(path.clone());
            return None;
        };
        let name = path.to_string_lossy();
        let soft = crate::app::alloc_texture_logged(ctx, &format!("{name}#soft"), soften(&img, 12), egui::TextureOptions::LINEAR);
        let sharp = crate::app::alloc_texture_logged(ctx, &name, img, egui::TextureOptions::LINEAR);
        let poster = Poster { sharp, soft };
        self.posters.insert(path.clone(), poster.clone());
        Some(poster)
    }

    /// Take the last status check's result, and start the next one every
    /// few seconds when none is running.
    fn refresh_status(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if let Some(rx) = &self.status_rx {
            match rx.try_recv() {
                Ok(found) => {
                    self.status = found;
                    self.status_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.status_rx = None,
            }
        }
        if self.status_checked_at.is_some_and(|t| t.elapsed().as_secs() < 3) {
            return;
        }
        self.status_checked_at = Some(std::time::Instant::now());
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("launch-status-check".into())
            .spawn(move || {
                use klipp_project::lock::{self, Liveness, LockState};
                let me = lock::Holder::current();
                let found = paths.into_iter()
                    .map(|p| {
                        let newer_autosave = crate::app::has_newer_autosave(&p);
                        let open_by = match lock::inspect(&p, &me) {
                            LockState::Held(info, Liveness::Active) => Some(info.who()),
                            _ => None,
                        };
                        (p, ProjectStatus { newer_autosave, open_by })
                    })
                    .collect();
                let _ = tx.send(found);
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.status_rx = Some(rx);
        }
    }

    /// Take the last scan of the projects folder `dir`, and start the next
    /// one every few seconds, or at once when the folder changed.
    fn refresh_folder_projects(&mut self, ctx: &egui::Context, dir: &std::path::Path) {
        if let Some(rx) = &self.folder_rx {
            match rx.try_recv() {
                Ok((scanned, found)) => {
                    if scanned == dir {
                        self.folder_projects = found;
                    }
                    self.folder_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.folder_rx = None,
            }
        }
        let fresh = self.folder_scanned.as_ref().is_some_and(|(d, t)| d == dir && t.elapsed().as_secs() < 3);
        if fresh {
            return;
        }
        if self.folder_scanned.as_ref().is_some_and(|(d, _)| d != dir) {
            self.folder_projects.clear();
        }
        self.folder_scanned = Some((dir.to_path_buf(), std::time::Instant::now()));
        let dir = dir.to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("launch-projects-folder".into())
            .spawn(move || {
                let found = crate::project_paths::projects_in(&dir);
                let _ = tx.send((dir, found));
                ctx.request_repaint();
            });
        if spawned.is_ok() {
            self.folder_rx = Some(rx);
        }
    }

    fn ensure_logo_texture(&mut self, ctx: &egui::Context) -> &egui::TextureHandle {
        self.logo_texture.get_or_insert_with(|| {
            let png_bytes = include_bytes!("../../../../assets/branding/klipp-refined/klipp-icon-128.png");
            let image = crate::ui_util::decode_png_or_fallback(png_bytes);
            crate::app::alloc_texture_logged(ctx, "klipp_launch_logo", image, egui::TextureOptions::LINEAR)
        })
    }

    /// `projects_folder` is the folder the grid can list projects from
    /// instead of the recent ones (#1143).
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        projects_folder: Option<&std::path::Path>,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        #[cfg(test)]
        {
            self.seen = Seen::default();
        }
        self.ensure_logo_texture(ctx);
        if let Some(dir) = projects_folder.filter(|_| self.folder_view) {
            self.refresh_folder_projects(ctx, dir);
        }
        let recent: Vec<RecentProject> = recent.iter().map(named_by_file).collect();
        let in_folder = self.folder_view && projects_folder.is_some();
        let shown: Vec<RecentProject> = if in_folder { self.folder_projects.iter().map(named_by_file).collect() } else { recent.clone() };
        let mut checked: Vec<PathBuf> = recent.iter().chain(&shown).map(|rp| rp.path.clone()).collect();
        checked.sort();
        checked.dedup();
        self.refresh_status(ctx, checked);
        let full_rect = ui.max_rect();

        // Fill background
        ui.painter()
            .rect_filled(full_rect, 0.0, theme.bg_void);

        // Sidebar (left)
        let sidebar_rect = egui::Rect::from_min_size(
            full_rect.min,
            egui::vec2(SIDEBAR_W, full_rect.height()),
        );

        // Main area (right of sidebar)
        let main_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.min.x + SIDEBAR_W + 1.0, full_rect.min.y),
            full_rect.max,
        );

        // Draw sidebar
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
            ui.painter()
                .rect_filled(sidebar_rect, 0.0, theme.bg_deep);
            ui.painter().vline(
                sidebar_rect.max.x,
                sidebar_rect.y_range(),
                egui::Stroke::new(1.0_f32, theme.border),
            );

            self.draw_sidebar(ctx, ui, &recent, projects_folder, theme, &mut cmds);
        });

        // Draw main area with project grid
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(main_rect), |ui| {
            self.draw_main_area(ctx, ui, &shown, projects_folder, theme, &mut cmds);
        });

        // Rename project popup
        if let Some((ref rename_path, _)) = self.renaming_project.clone() {
            let mut open = true;
            let mut done = false;
            egui::Window::new("Rename Project")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        let buf = &mut self.renaming_project.as_mut().unwrap().1;
                        let resp = ui.text_edit_singleline(buf);
                        // Typing goes into the name the moment the
                        // window opens, as in a file manager's rename.
                        if ui.memory(|m| m.focused().is_none()) {
                            resp.request_focus();
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            done = true;
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() {
                            done = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.renaming_project = None;
                        }
                    });
                });
            if !open {
                self.renaming_project = None;
            }
            if done {
                let new_name = self.renaming_project.as_ref()
                    .map(|(_, buf)| buf.trim().to_string())
                    .unwrap_or_default();
                if !new_name.is_empty() {
                    cmds.push(AppCommand::RenameProjectOnDisk {
                        old_path: rename_path.clone(),
                        new_name,
                    });
                }
                self.renaming_project = None;
            }
        }

        // What's New: the changes in this build, newest first.
        if self.show_whats_new {
            let mut open = true;
            egui::Window::new("What's New")
                .id(egui::Id::new("launch_whats_new"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(version_line(klipp_core::build_info::get())).color(theme.text_secondary));
                    ui.add_space(6.0);
                    let changes = klipp_core::build_info::changes();
                    if changes.is_empty() {
                        ui.label(egui::RichText::new("This build recorded no changes.").color(theme.text_dim));
                    }
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for change in changes {
                            ui.label(format!("\u{2022} {change}"));
                        }
                    });
                });
            if !open {
                self.show_whats_new = false;
            }
        }

        // Move to Trash asks first. The folder goes to the trash, from
        // where the file manager can put it back; nothing deletes it.
        if let Some(path) = self.confirm_trash.clone() {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let mut open = true;
            let mut answer = None;
            egui::Window::new("Move to Trash")
                .id(egui::Id::new("launch_confirm_trash"))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("Move \u{201C}{name}\u{201D} to the trash?"));
                    ui.label(
                        egui::RichText::new("The project folder can be put back from the trash. Its media files are not touched.")
                            .color(theme.text_dim)
                            .size(crate::theme::type_scale::BODY),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            answer = Some(false);
                        }
                        if ui.button("Move to Trash").clicked() {
                            answer = Some(true);
                        }
                    });
                });
            if answer == Some(true) {
                cmds.push(AppCommand::MoveProjectToTrash { path });
            }
            if !open || answer.is_some() {
                self.confirm_trash = None;
            }
        }

        cmds
    }

    fn draw_sidebar(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        projects_folder: Option<&std::path::Path>,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let area = ui.max_rect();
        let pad_x = 24.0;
        let button_w = SIDEBAR_W - pad_x * 2.0;

        // ── Logo ──
        let logo_size = 80.0;
        let logo_center = egui::pos2(area.center().x, area.min.y + 55.0);
        let logo_rect = egui::Rect::from_center_size(
            logo_center,
            egui::vec2(logo_size, logo_size),
        );
        let logo_tex = self.logo_texture.as_ref().unwrap();
        ui.painter().image(
            logo_tex.id(),
            logo_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // The wordmark, as About and Preferences draw it.
        let word = crate::widgets::wordmark::paint(ui, egui::pos2(logo_center.x, logo_center.y + logo_size * 0.5 + 12.0), 34.0);

        // The running build: its version and build number, as About says.
        let build = klipp_core::build_info::get();
        ui.painter().text(
            egui::pos2(logo_center.x, word.bottom() + 8.0),
            egui::Align2::CENTER_CENTER,
            version_line(build),
            egui::FontId::proportional(11.0),
            theme.text_secondary,
        );

        // ── Action Buttons ──
        let buttons_y = word.bottom() + 34.0;

        // + New Project (orange filled)
        let new_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad_x, buttons_y),
            egui::vec2(button_w, 38.0),
        );
        let new_resp = ui.allocate_rect(new_rect, egui::Sense::click());
        let new_bg = if new_resp.hovered() { theme.accent } else { theme.accent_dim };
        ui.painter().rect_filled(new_rect, 4.0, new_bg);
        ui.painter().text(
            new_rect.center(),
            egui::Align2::CENTER_CENTER,
            "+ New Project",
            egui::FontId::proportional(13.0),
            theme.bg_void,
        );
        if new_resp.clicked() {
            cmds.push(AppCommand::NewProject);
        }

        // Open Project... (outlined)
        let open_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad_x, buttons_y + 48.0),
            egui::vec2(button_w, 36.0),
        );
        let open_resp = ui.allocate_rect(open_rect, egui::Sense::click());
        let open_bg = if open_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
        ui.painter().rect_filled(open_rect, 4.0, open_bg);
        ui.painter().rect_stroke(open_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border_light));
        ui.painter().text(
            open_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Open Project\u{2026}",
            egui::FontId::proportional(13.0),
            theme.text_primary,
        );
        if open_resp.clicked() {
            crate::file_dialog::spawn(crate::project_paths::pick_project_commands);
        }

        // No "Import Media" here.
        //
        // It dispatched `ImportMedia`, which loads the file into the
        // untitled project's media pool — and the launch screen is
        // shown precisely while there is no project open, which
        // importing does not change. So the file went into a pool the
        // user could not see, on a screen that did not move.
        //
        // Importing is a thing you do *to* a project. The two entry
        // points above are the ones that produce one.

        // The sections run down from the buttons, each as tall as what it
        // holds, so with fewer recents the templates move up; the footer
        // keeps the bottom.
        let footer_top = area.max.y - FOOTER_H;
        let mut y = buttons_y + 84.0 + 26.0;
        let heading = |ui: &egui::Ui, y: f32, text: &str| {
            ui.painter().text(
                egui::pos2(area.min.x + pad_x, y),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
        };

        // ── Recent Projects List ──
        if !recent.is_empty() {
            heading(ui, y, "RECENT");

            // A few, not all of them.
            //
            // This filled the sidebar with as many as would fit — all
            // twenty, on a tall window — beside a grid showing the same
            // twenty. Two complete lists of the same projects, side by
            // side.
            //
            // It is not redundant in principle: the grid follows the
            // chosen sort, this always follows recency (pinned first),
            // so with sort set to Name or Size it is still the way back
            // to what you had open. That is a shortcut, and a shortcut
            // is short.
            const MAX_RECENT: usize = 8;
            let item_h = RECENT_ROW_H;
            // Room is kept below for the templates' heading and two of them.
            let fits = ((footer_top - y - 20.0 - 14.0 - 20.0 - 2.0 * TEMPLATE_ROW_H) / item_h).floor().max(1.0) as usize;
            let items_to_show = recent.len().min(fits).min(MAX_RECENT);
            let now = chrono::Local::now().naive_local();

            for (i, rp) in sidebar_order(recent).into_iter().take(items_to_show).enumerate() {
                let row_y = y + 20.0 + i as f32 * item_h;
                let item_rect = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + pad_x - 8.0, row_y),
                    egui::vec2(button_w + 16.0, item_h - 4.0),
                );
                let resp = ui.allocate_rect(item_rect, egui::Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(item_rect, 4.0, theme.bg_hover);
                }

                // The project's poster frame, 16:9, where a coloured bar
                // keyed to its name used to say nothing about it.
                let thumb_rect = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + pad_x, item_rect.center().y - 16.0),
                    egui::vec2(56.0, 31.5),
                );
                let poster = self.poster(ctx, rp);
                draw_poster(ui.painter(), thumb_rect, poster.as_ref(), None, theme);
                #[cfg(test)]
                self.seen.sidebar.push((rp.path.clone(), item_rect, poster.as_ref().map(|p| p.sharp.id())));

                // The marks, right to left from the row's end.
                let name_x = thumb_rect.max.x + 10.0;
                let name_y = row_y + 6.0;
                let marks = marks_for(rp, self.status.get(&rp.path));
                let mut mark_x = item_rect.max.x - 8.0;
                for (k, mark) in marks.iter().enumerate().rev() {
                    let mark_rect = egui::Rect::from_center_size(egui::pos2(mark_x - 6.0, name_y + 7.0), egui::vec2(12.0, 12.0));
                    draw_mark(ui.painter(), mark_rect.center(), mark, theme);
                    ui.interact(mark_rect, ui.id().with(("launch_mark", &rp.path, k)), egui::Sense::hover())
                        .on_hover_text(mark.tooltip());
                    #[cfg(test)]
                    self.seen.marks.push((rp.path.clone(), mark.clone(), mark_rect));
                    mark_x -= 14.0;
                }

                // Project name, then when · how long · what rate.
                let text_w = (mark_x - name_x - 4.0).max(0.0);
                let name = layout_truncated(
                    ui.painter(),
                    &rp.name,
                    egui::FontId::proportional(12.0),
                    if resp.hovered() { theme.text_primary } else { theme.text_secondary },
                    text_w,
                );
                ui.painter().galley(egui::pos2(name_x, name_y), name, theme.text_secondary);
                let detail = sidebar_detail(rp, now);
                if !detail.is_empty() {
                    let g = layout_truncated(
                        ui.painter(),
                        &detail,
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
                        (item_rect.max.x - 8.0 - name_x).max(0.0),
                    );
                    ui.painter().galley(egui::pos2(name_x, name_y + 17.0), g, theme.text_dim);
                }

                if resp.clicked() {
                    cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                }
                resp.context_menu_rows(|ui| {
                    crate::widgets::menu::menu_body(ui, |ui| {
                        if crate::widgets::menu::menu_item(ui, "Open", "").clicked() {
                            cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                            ui.close_menu();
                        }
                        let pin = if rp.pinned { "Unpin" } else { "Pin to Top" };
                        if crate::widgets::menu::menu_item(ui, pin, "").clicked() {
                            cmds.push(AppCommand::PinRecentProject { path: rp.path.clone(), pinned: !rp.pinned });
                            ui.close_menu();
                        }
                        if crate::widgets::menu::menu_item(ui, "Show in Files", "").clicked() {
                            cmds.push(AppCommand::RevealInFileManager { path: rp.path.clone() });
                            ui.close_menu();
                        }
                        ui.separator();
                        if crate::widgets::menu::menu_item(ui, "Remove from Recent", "").clicked() {
                            cmds.push(AppCommand::RemoveRecentProject { path: rp.path.clone() });
                            ui.close_menu();
                        }
                    });
                });
            }
            y += 20.0 + items_to_show as f32 * item_h + 14.0;
        }

        // ── Start from a template ──
        // The user's own templates (#618) and the built-in formats, each
        // an Untitled project in one click, written only once it is
        // edited or saved (#1864). A format made at several rates has a
        // menu of them, set at the last rate chosen. More… opens the full
        // chooser.
        self.refresh_templates();
        let rows_fit = ((footer_top - y - 20.0 - TEMPLATE_ROW_H - 8.0) / TEMPLATE_ROW_H).floor().max(0.0) as usize;
        if footer_top - y > 20.0 + TEMPLATE_ROW_H {
            heading(ui, y, "START FROM A TEMPLATE");
            let entries = self.template_entries();
            let shown = entries.len().min(rows_fit);
            for (i, entry) in entries.iter().take(shown).enumerate() {
                let row = egui::Rect::from_min_size(
                    egui::pos2(area.min.x + pad_x - 8.0, y + 18.0 + i as f32 * TEMPLATE_ROW_H),
                    egui::vec2(button_w + 16.0, TEMPLATE_ROW_H - 2.0),
                );
                let resp = ui.allocate_rect(row, egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!("Create a project from {}", entry.name));
                if resp.hovered() {
                    ui.painter().rect_filled(row, 4.0, theme.bg_hover);
                }
                // The rate, at the row's end: a menu where there is a
                // choice.
                let rates = entry.format().map_or(&[][..], |f| f.rates);
                let rate = entry.format().and_then(|f| crate::panels::new_project_dialog::format_rate(f, self.last_rate));
                let mut right = row.max.x - 8.0;
                if let Some(r) = rate {
                    let label = if rates.len() > 1 {
                        format!("{} \u{25BE}", crate::panels::new_project_dialog::rate_label(r))
                    } else {
                        crate::panels::new_project_dialog::rate_label(r)
                    };
                    let g = ui.painter().layout_no_wrap(label, egui::FontId::proportional(10.0), theme.text_secondary);
                    let rate_rect = egui::Rect::from_min_size(egui::pos2(right - g.size().x, row.center().y - g.size().y / 2.0), g.size());
                    right = rate_rect.min.x - 8.0;
                    if rates.len() > 1 {
                        let rate_resp = ui.interact(rate_rect.expand(3.0), ui.id().with(("launch_rate", i)), egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Frame rate");
                        let colour = if rate_resp.hovered() { theme.text_primary } else { theme.text_secondary };
                        ui.painter().galley_with_override_text_color(rate_rect.min, g, colour);
                        let popup = ui.id().with(("launch_rate_menu", i));
                        if rate_resp.clicked() {
                            ui.memory_mut(|m| m.toggle_popup(popup));
                        }
                        egui::popup_below_widget(ui, popup, &rate_resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                            crate::widgets::menu::menu_body(ui, |ui| {
                                ui.set_min_width(90.0);
                                for r in rates {
                                    let label = format!("{} fps", crate::panels::new_project_dialog::rate_label(*r));
                                    if crate::widgets::menu::menu_choice(ui, &label, "", Some(*r) == rate).clicked() {
                                        self.last_rate = Some(*r);
                                        cmds.push(AppCommand::SetFormatRate { rate: *r });
                                    }
                                }
                            });
                        });
                    } else {
                        ui.painter().galley(rate_rect.min, g, theme.text_dim);
                    }
                }
                let detail_g = ui.painter().layout_no_wrap(entry.detail.clone(), egui::FontId::proportional(10.0), theme.text_dim);
                let detail_w = row.max.x - 8.0 - right + detail_g.size().x;
                ui.painter().galley(egui::pos2(right - detail_g.size().x, row.center().y - detail_g.size().y / 2.0), detail_g, theme.text_dim);
                let name_g = layout_truncated(
                    ui.painter(),
                    &entry.name,
                    egui::FontId::proportional(12.0),
                    if resp.hovered() { theme.text_primary } else { theme.text_secondary },
                    (row.width() - 24.0 - detail_w).max(0.0),
                );
                ui.painter().galley(egui::pos2(row.min.x + 8.0, row.center().y - name_g.size().y / 2.0), name_g, theme.text_secondary);
                if resp.clicked() {
                    cmds.push(entry.create(self.last_rate));
                }
            }
            let more_y = y + 18.0 + shown as f32 * TEMPLATE_ROW_H;
            if quiet_link(ui, egui::pos2(area.min.x + pad_x, more_y + 4.0), "More\u{2026}", theme).clicked() {
                cmds.push(AppCommand::NewProject);
            }
        }

        self.draw_footer(ui, egui::Rect::from_min_max(egui::pos2(area.min.x + pad_x, footer_top), egui::pos2(area.max.x - pad_x, area.max.y)), projects_folder, theme, cmds);
    }

    /// The footer: room on the projects drive and the render cache's size
    /// (Manage… opens the cache preferences), then Help, Keyboard
    /// Shortcuts and What's New.
    fn draw_footer(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        projects_folder: Option<&std::path::Path>,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        ui.painter().hline(rect.x_range(), rect.min.y, egui::Stroke::new(1.0_f32, theme.border));
        self.refresh_storage(ui.ctx(), projects_folder);
        let small = egui::FontId::proportional(10.0);
        let mut y = rect.min.y + 10.0;
        if let Some(storage) = &self.storage {
            if let Some(free) = storage.free_bytes {
                let line = format!("{} free on {}", format_size_compact(free), tilde(&storage.drive));
                let g = layout_truncated(ui.painter(), &line, small.clone(), theme.text_dim, rect.width());
                ui.painter().galley(egui::pos2(rect.min.x, y), g, theme.text_dim);
            }
            let cache = format!("Render cache {}", format_size_compact(storage.render_cache_bytes));
            let g = ui.painter().layout_no_wrap(cache, small.clone(), theme.text_dim);
            let w = g.size().x;
            ui.painter().galley(egui::pos2(rect.min.x, y + 15.0), g, theme.text_dim);
            if quiet_link(ui, egui::pos2(rect.min.x + w + 8.0, y + 15.0), "Manage\u{2026}", theme).clicked() {
                cmds.push(AppCommand::OpenPreferences { page: Some("cache".into()) });
            }
        }
        y += 38.0;
        let mut x = rect.min.x;
        for link in ["Help", "Keyboard Shortcuts", "What's New"] {
            let resp = quiet_link(ui, egui::pos2(x, y), link, theme);
            x = resp.rect.max.x + 14.0;
            if resp.clicked() {
                match link {
                    "Help" => {
                        #[cfg(not(test))]
                        let _ = crate::ui_util::open_external(HELP_URL);
                        #[cfg(test)]
                        self.opened.push(HELP_URL);
                    }
                    "Keyboard Shortcuts" => cmds.push(AppCommand::OpenPreferences { page: Some("keyboard".into()) }),
                    _ => self.show_whats_new = true,
                }
            }
        }
    }

    fn draw_main_area(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        recent: &[RecentProject],
        projects_folder: Option<&std::path::Path>,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let area = ui.max_rect();
        let pad = 28.0;

        // ── Header Row ──

        // "Projects" title (large)
        ui.painter().text(
            egui::pos2(area.min.x + pad, area.min.y + pad),
            egui::Align2::LEFT_TOP,
            "Projects",
            egui::FontId::proportional(26.0),
            theme.text_primary,
        );

        // Project count + last modified summary
        let most_recent_time = recent.first()
            .and_then(|rp| rp.last_saved.as_deref())
            .map(relative_time)
            .unwrap_or_default();
        // Which projects the grid shows: the recent ones, or those in the
        // projects folder, one click each to open (#1143). One segmented
        // control, as the workspace switch is, and a quiet button to
        // choose the folder — not three loose labels of three looks.
        let header_y = area.min.y + pad + 2.0;
        let mut header_x = area.min.x + pad + 150.0;
        if let Some(dir) = projects_folder {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string());
            let folder_label = format!("In {name}");
            let labels = ["Recent", folder_label.as_str()];
            let font = egui::FontId::proportional(crate::theme::type_scale::CONTROL);
            let size = crate::widgets::segmented::segmented_size(ui, &labels, &font, crate::theme::control::REGULAR);
            let seg_rect = egui::Rect::from_min_size(egui::pos2(header_x, header_y), size);
            if let Some(i) = crate::widgets::segmented::segmented_in(ui, seg_rect, &labels, usize::from(self.folder_view), theme, font) {
                self.folder_view = i == 1;
            }
            header_x = seg_rect.max.x + 8.0;
        }
        let choose = if projects_folder.is_some() { "Change Folder\u{2026}" } else { "Choose Projects Folder\u{2026}" };
        let choose_rect = egui::Rect::from_min_size(
            egui::pos2(header_x, header_y),
            egui::vec2((area.max.x - pad - header_x).max(0.0), crate::theme::control::REGULAR),
        );
        let choose_clicked = ui.allocate_new_ui(egui::UiBuilder::new().max_rect(choose_rect), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Button::new(
                    egui::RichText::new(choose).size(crate::theme::type_scale::CONTROL).color(theme.text_secondary),
                ).frame(false))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            }).inner
        }).inner;
        if choose_clicked {
            self.folder_view = true;
            crate::file_dialog::spawn(|| {
                rfd::FileDialog::new()
                    .set_title("Projects Folder")
                    .pick_folder()
                    .map(|path| vec![AppCommand::SetProjectsFolder { path }])
                    .unwrap_or_default()
            });
        }

        let in_folder = self.folder_view && projects_folder.is_some();
        let summary = if recent.is_empty() {
            "No projects".to_string()
        } else {
            let count_str = format!("{} project{}", recent.len(), if recent.len() == 1 { "" } else { "s" });
            if in_folder {
                format!("{count_str} in {}", projects_folder.map(|d| d.display().to_string()).unwrap_or_default())
            } else if most_recent_time.is_empty() {
                count_str
            } else {
                format!("{count_str} \u{00B7} Last modified {most_recent_time}")
            }
        };
        ui.painter().text(
            egui::pos2(area.min.x + pad, area.min.y + pad + 32.0),
            egui::Align2::LEFT_TOP,
            &summary,
            egui::FontId::proportional(12.0),
            theme.text_dim,
        );

        // ── Controls row: Search | Sort | View toggle ──
        let controls_y = area.min.y + pad + 56.0;
        let controls_right = area.max.x - pad;

        // Search bar, on the grid's left edge.
        //
        // It used to start at `pad + 200.0` — a fixed offset that put
        // it neither under the title nor against anything else, so it
        // floated in the middle of the header lining up with nothing.
        // Sharing the grid's left margin makes the row read as a row.
        let search_w = 260.0_f32.min((area.width() - pad * 2.0) * 0.4);
        let search_rect = egui::Rect::from_min_size(
            egui::pos2(area.min.x + pad, controls_y - 2.0),
            egui::vec2(search_w, 26.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(search_rect), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Search projects\u{2026}")
                    .desired_width(search_w),
            );
        });

        // Grid/List toggle (right side)
        let toggle_size = 24.0;
        let list_rect = egui::Rect::from_min_size(
            egui::pos2(controls_right - toggle_size, controls_y),
            egui::vec2(toggle_size, toggle_size),
        );
        let list_resp = ui.allocate_rect(list_rect, egui::Sense::click());
        let list_color = if self.view_mode == ViewMode::List { theme.text_primary } else { theme.text_dim };
        // List icon (three horizontal lines)
        for j in 0..3 {
            let ly = list_rect.min.y + 6.0 + j as f32 * 6.0;
            ui.painter().hline(
                list_rect.min.x + 4.0..=list_rect.max.x - 4.0,
                ly,
                egui::Stroke::new(2.0_f32, list_color),
            );
        }
        if list_resp.clicked() {
            self.view_mode = ViewMode::List;
        }

        let grid_rect = egui::Rect::from_min_size(
            egui::pos2(controls_right - toggle_size * 2.0 - 6.0, controls_y),
            egui::vec2(toggle_size, toggle_size),
        );
        let grid_resp = ui.allocate_rect(grid_rect, egui::Sense::click());
        let grid_color = if self.view_mode == ViewMode::Grid { theme.text_primary } else { theme.text_dim };
        // Grid icon (2x2 squares)
        let gs = 8.0;
        let gp = 2.0;
        for row in 0..2 {
            for col in 0..2 {
                let sr = egui::Rect::from_min_size(
                    egui::pos2(
                        grid_rect.min.x + 3.0 + col as f32 * (gs + gp),
                        grid_rect.min.y + 3.0 + row as f32 * (gs + gp),
                    ),
                    egui::vec2(gs, gs),
                );
                ui.painter().rect_filled(sr, 1.5, grid_color);
            }
        }
        if grid_resp.clicked() {
            self.view_mode = ViewMode::Grid;
        }

        // Sort dropdown
        let sort_x = controls_right - toggle_size * 2.0 - 6.0 - 10.0;
        let sort_w = 90.0;
        let sort_rect = egui::Rect::from_min_size(
            egui::pos2(sort_x - sort_w, controls_y),
            egui::vec2(sort_w, 24.0),
        );
        let sort_resp = ui.allocate_rect(sort_rect, egui::Sense::click());
        let sort_bg = if sort_resp.hovered() { theme.bg_hover } else { theme.bg_raised };
        ui.painter().rect_filled(sort_rect, 4.0, sort_bg);
        ui.painter().rect_stroke(sort_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border));
        ui.painter().text(
            egui::pos2(sort_rect.center().x - 4.0, sort_rect.center().y),
            egui::Align2::CENTER_CENTER,
            self.sort_mode.label(),
            egui::FontId::proportional(12.0),
            theme.text_secondary,
        );
        // Dropdown arrow (painter-drawn triangle)
        {
            let cx = sort_rect.max.x - 14.0;
            let cy = sort_rect.center().y;
            let s = 3.5;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(cx - s, cy - s * 0.5),
                    egui::pos2(cx + s, cy - s * 0.5),
                    egui::pos2(cx, cy + s * 0.7),
                ],
                theme.text_dim,
                egui::Stroke::NONE,
            ));
        }
        // Sort dropdown menu
        sort_resp.context_menu_rows(|ui| {
            crate::widgets::menu::menu_body(ui, |ui| {
                for mode in GRID_SORTS {
                    if crate::widgets::menu::menu_choice(ui, mode.label(), "", self.sort_mode == mode).clicked() {
                        self.set_sort(mode);
                        ui.close_menu();
                    }
                }
            });
        });
        if sort_resp.clicked() {
            // Cycle through sort modes on click
            let next = GRID_SORTS.iter().position(|m| *m == self.sort_mode).map_or(0, |i| (i + 1) % GRID_SORTS.len());
            self.set_sort(GRID_SORTS[next]);
        }

        // ── Section header ──
        let section_y = controls_y + 36.0;
        if self.view_mode == ViewMode::Grid {
            ui.painter().text(
                egui::pos2(area.min.x + pad, section_y),
                egui::Align2::LEFT_TOP,
                "ALL PROJECTS",
                egui::FontId::proportional(10.0),
                theme.text_dim,
            );
        }

        // Filter and sort
        let query = self.search_query.to_lowercase();
        let mut filtered: Vec<&RecentProject> = recent
            .iter()
            .filter(|rp| query.is_empty() || rp.name.to_lowercase().contains(&query))
            .collect();
        // Until a sort is chosen the list keeps the order it came in:
        // most recent first, as the preferences keep it.
        if self.sort_chosen {
            sort_projects(&mut filtered, self.sort_mode, self.sort_descending);
        }

        // ── Project Grid / List ──
        let grid_top = section_y + 22.0;
        let grid_width = area.width() - pad * 2.0;

        let scroll_area = egui::Rect::from_min_max(
            egui::pos2(area.min.x, grid_top),
            area.max,
        );

        if self.view_mode == ViewMode::Grid {
            self.draw_grid(ctx, ui, scroll_area, pad, grid_width, &filtered, theme, cmds);
        } else {
            self.draw_list(ctx, ui, scroll_area, pad, grid_width, &filtered, theme, cmds);
        }
    }

    fn draw_grid(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        scroll_area: egui::Rect,
        pad: f32,
        grid_width: f32,
        filtered: &[&RecentProject],
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        // Every card is as tall as the most any card has to say, so a
        // row lines up and none has an empty band under its text.
        let lines = filtered.iter().map(|rp| card_lines(rp).len()).max().unwrap_or(0);
        let (cols, card_w, card_h) = card_metrics(grid_width, lines);

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(scroll_area), |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // "New Project" card is first
                let new_card_rect = egui::Rect::from_min_size(
                    ui.min_rect().min + egui::vec2(pad, 0.0),
                    egui::vec2(card_w, card_h),
                );
                self.draw_new_project_card(ui, new_card_rect, theme, cmds);

                let mut card_idx = 1usize; // Start at 1 (slot 0 = New Project)

                // Workspace grouping
                let mut workspace_groups: HashMap<PathBuf, (String, Vec<&RecentProject>)> = HashMap::new();
                let mut standalone: Vec<&RecentProject> = Vec::new();

                for rp in filtered {
                    if let Some(parent) = rp.path.parent() {
                        if klipp_project::workspace::is_workspace(parent) {
                            let ws_name = klipp_project::workspace::load_workspace(parent)
                                .map(|w| w.name)
                                .unwrap_or_else(|| parent.file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "Workspace".into()));
                            workspace_groups
                                .entry(parent.to_path_buf())
                                .or_insert_with(|| (ws_name, Vec::new()))
                                .1
                                .push(rp);
                        } else {
                            standalone.push(rp);
                        }
                    } else {
                        standalone.push(rp);
                    }
                }

                // Workspace groups
                let mut ws_keys: Vec<_> = workspace_groups.keys().cloned().collect();
                ws_keys.sort();
                for ws_path in &ws_keys {
                    let (ws_name, projects) = &workspace_groups[ws_path];
                    // Start on fresh row
                    card_idx = card_idx.div_ceil(cols) * cols;
                    let row = card_idx / cols;
                    let y_offset = row as f32 * (card_h + CARD_GAP);

                    let header_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(pad, y_offset),
                        egui::vec2(grid_width, 22.0),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, y_offset + 22.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        header_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        format!("\u{1F4C1} {} \u{2014} {} project(s)", ws_name, projects.len()),
                        egui::FontId::proportional(12.0),
                        theme.text_secondary,
                    );

                    card_idx = (card_idx / cols + 1) * cols;

                    for rp in projects {
                        let col = card_idx % cols;
                        let row = card_idx / cols;
                        let x = pad + col as f32 * (card_w + CARD_GAP);
                        let y = row as f32 * (card_h + CARD_GAP);

                        let card_rect = egui::Rect::from_min_size(
                            ui.min_rect().min + egui::vec2(x, y),
                            egui::vec2(card_w, card_h),
                        );
                        ui.allocate_at_least(
                            egui::vec2(grid_width, (row + 1) as f32 * (card_h + CARD_GAP)),
                            egui::Sense::hover(),
                        );
                        self.draw_card(ctx, ui, card_rect, rp, theme, cmds);
                        card_idx += 1;
                    }

                    card_idx = card_idx.div_ceil(cols) * cols;
                }

                // Standalone projects
                if !standalone.is_empty() && !workspace_groups.is_empty() {
                    card_idx = card_idx.div_ceil(cols) * cols;
                    let row = card_idx / cols;
                    let y_offset = row as f32 * (card_h + CARD_GAP);
                    let header_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(pad, y_offset),
                        egui::vec2(grid_width, 22.0),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, y_offset + 22.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        header_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        "Other Projects",
                        egui::FontId::proportional(12.0),
                        theme.text_secondary,
                    );
                    card_idx = (card_idx / cols + 1) * cols;
                }

                for rp in &standalone {
                    let col = card_idx % cols;
                    let row = card_idx / cols;
                    let x = pad + col as f32 * (card_w + CARD_GAP);
                    let y = row as f32 * (card_h + CARD_GAP);

                    let card_rect = egui::Rect::from_min_size(
                        ui.min_rect().min + egui::vec2(x, y),
                        egui::vec2(card_w, card_h),
                    );
                    ui.allocate_at_least(
                        egui::vec2(grid_width, (row + 1) as f32 * (card_h + CARD_GAP)),
                        egui::Sense::hover(),
                    );
                    self.draw_card(ctx, ui, card_rect, rp, theme, cmds);
                    card_idx += 1;
                }

                if filtered.is_empty() {
                    self.draw_empty_state(ui, pad, theme);
                }
            });
        });
    }

    /// The empty grid or list: no projects at all, or none matching.
    fn draw_empty_state(&self, ui: &mut egui::Ui, pad: f32, theme: &KlippTheme) {
        ui.add_space(24.0);
        // Left-aligned under the grid, not centred in
        // whatever width happens to be left over — centred,
        // it landed adrift in the middle-right of an empty
        // page, related to nothing on screen. Here it reads
        // as a caption to the New Project card above it.
        ui.horizontal(|ui| {
            ui.add_space(pad);
            ui.vertical(|ui| {
                // Two different emptinesses, and they were
                // being told apart by nobody: a new user with
                // no projects at all was told their *search*
                // had matched nothing, on the first screen they
                // ever saw, without having typed anything.
                if self.search_query.trim().is_empty() {
                    ui.label(
                        egui::RichText::new("No projects yet")
                            .color(theme.text_primary)
                            .size(15.0),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(
                            "Start one with New Project, or open an existing .klipp file.",
                        )
                        .color(theme.text_dim)
                        .size(12.0),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(format!(
                            "No projects match \u{201C}{}\u{201D}",
                            self.search_query.trim()
                        ))
                        .color(theme.text_dim)
                        .size(13.0),
                    );
                }
            });
        });
    }

    /// Sort by `mode`: a new column starts its own way round, and the
    /// column already in force turns the other way.
    fn set_sort(&mut self, mode: SortMode) {
        if self.sort_chosen && self.sort_mode == mode {
            self.sort_descending = !self.sort_descending;
        } else {
            self.sort_mode = mode;
            self.sort_descending = mode.descending_first();
        }
        self.sort_chosen = true;
    }

    /// The list view: a table, one project a row, sorted by any column.
    #[allow(clippy::too_many_arguments)]
    fn draw_list(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        scroll_area: egui::Rect,
        pad: f32,
        grid_width: f32,
        filtered: &[&RecentProject],
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let columns = list_columns(grid_width);
        let header_h = 24.0;
        let left = scroll_area.min.x + pad;

        // The header: each column's name, the one in force lit with
        // its direction; a click sorts by it, again turns it round.
        let header_rect = egui::Rect::from_min_size(egui::pos2(left, scroll_area.min.y - 4.0), egui::vec2(grid_width, header_h));
        let mut x = left;
        for (mode, w) in &columns {
            let cell = egui::Rect::from_min_size(egui::pos2(x, header_rect.min.y), egui::vec2(*w, header_h));
            let resp = ui.interact(cell, ui.id().with(("launch_list_header", mode.label())), egui::Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let active = self.sort_chosen && self.sort_mode == *mode;
            let label = if active {
                format!("{} {}", mode.label(), if self.sort_descending { "\u{25BE}" } else { "\u{25B4}" })
            } else {
                mode.label().to_string()
            };
            let colour = if active || resp.hovered() { theme.text_primary } else { theme.text_dim };
            let (anchor, at) = if list_column_is_numeric(*mode) {
                (egui::Align2::RIGHT_CENTER, egui::pos2(cell.max.x - 10.0, cell.center().y))
            } else {
                (egui::Align2::LEFT_CENTER, egui::pos2(cell.min.x, cell.center().y))
            };
            ui.painter().text(at, anchor, label, egui::FontId::proportional(crate::theme::type_scale::CAPTION), colour);
            if resp.clicked() {
                self.set_sort(*mode);
            }
            x += w;
        }
        ui.painter().hline(
            header_rect.x_range(),
            header_rect.max.y,
            egui::Stroke::new(1.0_f32, theme.border),
        );

        let body = egui::Rect::from_min_max(egui::pos2(scroll_area.min.x, header_rect.max.y + 1.0), scroll_area.max);
        let mut sorted: Vec<&RecentProject> = filtered.to_vec();
        if self.sort_chosen {
            sort_projects(&mut sorted, self.sort_mode, self.sort_descending);
        }
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(body), |ui| {
            egui::ScrollArea::vertical().id_salt("launch_list").show(ui, |ui| {
                let row_h = 40.0;
                for rp in &sorted {
                    let (full, resp) = ui.allocate_exact_size(
                        egui::vec2(grid_width + pad * 2.0, row_h),
                        egui::Sense::click(),
                    );
                    let rect = egui::Rect::from_min_max(egui::pos2(full.min.x + pad, full.min.y), egui::pos2(full.max.x - pad, full.max.y));
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 4.0, theme.bg_hover);
                    }
                    let mut x = rect.min.x;
                    for (mode, w) in &columns {
                        let cell = egui::Rect::from_min_size(egui::pos2(x, rect.min.y), egui::vec2(*w, row_h));
                        x += w;
                        if *mode == SortMode::Name {
                            // The poster, small, then the name and its
                            // missing-media mark.
                            let thumb = egui::Rect::from_min_size(egui::pos2(cell.min.x, cell.center().y - 13.5), egui::vec2(48.0, 27.0));
                            let poster = self.poster(ctx, rp);
                            draw_poster(ui.painter(), thumb, poster.as_ref(), None, theme);
                            let mark_w = if rp.missing_media == Some(true) { 18.0 } else { 0.0 };
                            let g = layout_truncated(
                                ui.painter(),
                                &rp.name,
                                egui::FontId::proportional(13.0),
                                theme.text_primary,
                                (cell.max.x - thumb.max.x - 22.0 - mark_w).max(0.0),
                            );
                            let name_pos = egui::pos2(thumb.max.x + 10.0, cell.center().y - g.size().y / 2.0);
                            let name_w = g.size().x;
                            ui.painter().galley(name_pos, g, theme.text_primary);
                            if rp.missing_media == Some(true) {
                                draw_mark(ui.painter(), egui::pos2(name_pos.x + name_w + 10.0, cell.center().y), &Mark::MissingMedia, theme);
                            }
                            continue;
                        }
                        let text = list_cell(rp, *mode);
                        let max_w = (w - 12.0).max(0.0);
                        let g = layout_truncated(
                            ui.painter(),
                            &text,
                            egui::FontId::proportional(crate::theme::type_scale::BODY),
                            theme.text_secondary,
                            max_w,
                        );
                        let y = cell.center().y - g.size().y / 2.0;
                        let pos = if list_column_is_numeric(*mode) {
                            egui::pos2(cell.max.x - 10.0 - g.size().x, y)
                        } else {
                            egui::pos2(cell.min.x, y)
                        };
                        ui.painter().galley(pos, g, theme.text_secondary);
                    }

                    let resp = resp.on_hover_text(rp.path.display().to_string());
                    if resp.clicked() {
                        cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                    }
                    resp.context_menu_rows(|ui| {
                        self.project_context_menu(ui, rp, cmds);
                    });

                    ui.painter().hline(
                        rect.x_range(),
                        rect.max.y,
                        egui::Stroke::new(0.5_f32, theme.border),
                    );
                }

                if sorted.is_empty() {
                    self.draw_empty_state(ui, pad, theme);
                }
            });
        });
    }

    fn draw_new_project_card(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click());
        let hovered = resp.hovered();

        // Dashed border card
        let stroke_color = if hovered { theme.accent } else { theme.border_light };
        let dash_len = 8.0;
        let gap_len = 5.0;
        let rounding = 8.0;

        // Draw dashed rectangle
        draw_dashed_rect(ui.painter(), rect, rounding, stroke_color, 1.5, dash_len, gap_len);

        if hovered {
            ui.painter().rect_filled(
                rect.shrink(1.0),
                rounding,
                egui::Color32::from_rgba_unmultiplied(220, 220, 228, 8),
            );
        }

        // + icon (circle with plus)
        let center = egui::pos2(rect.center().x, rect.center().y - 16.0);
        let icon_r = 22.0;
        let icon_color = if hovered { theme.accent } else { theme.text_dim };
        ui.painter().circle_stroke(center, icon_r, egui::Stroke::new(1.5_f32, icon_color));
        // Horizontal bar
        ui.painter().hline(
            center.x - 8.0..=center.x + 8.0,
            center.y,
            egui::Stroke::new(2.0_f32, icon_color),
        );
        // Vertical bar
        ui.painter().vline(
            center.x,
            center.y - 8.0..=center.y + 8.0,
            egui::Stroke::new(2.0_f32, icon_color),
        );

        // "New Project" text
        ui.painter().text(
            egui::pos2(rect.center().x, rect.center().y + 24.0),
            egui::Align2::CENTER_CENTER,
            "New Project",
            egui::FontId::proportional(13.0),
            if hovered { theme.accent } else { theme.text_dim },
        );

        if resp.clicked() {
            cmds.push(AppCommand::NewProject);
        }
    }

    fn draw_card(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        rp: &RecentProject,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click());
        let hovered = resp.hovered();
        #[cfg(test)]
        self.seen.cards.push((rp.path.clone(), rect));

        // Card background
        let bg = if hovered { theme.bg_hover } else { theme.bg_raised };
        let stroke = if hovered {
            egui::Stroke::new(1.5_f32, theme.accent)
        } else {
            egui::Stroke::new(1.0_f32, theme.border)
        };
        ui.painter().rect(rect, 8.0, bg, stroke);

        // The thumbnail: 16:9, the card's full width.
        let thumb_rect = egui::Rect::from_min_size(
            rect.min + egui::vec2(1.0, 1.0),
            egui::vec2(rect.width() - 2.0, (rect.width() - 2.0) * 9.0 / 16.0),
        );
        let thumb_rounding = egui::Rounding { nw: 7.0, ne: 7.0, sw: 0.0, se: 0.0 };

        let poster = self.poster(ctx, rp);
        // Across the thumbnail the pointer scrubs through the project,
        // as it does over media in the browser: a frame of the skim
        // strip for where it is, and the browser's line to show where.
        let scrub_at = ui.input(|i| i.pointer.hover_pos()).filter(|p| hovered && thumb_rect.contains(*p));
        let strip = if poster.is_some() && scrub_at.is_some() { self.strip(ctx, rp) } else { None };
        if let (Some(strip), Some(at)) = (&strip, scrub_at) {
            ui.painter().rect_filled(thumb_rect, thumb_rounding, theme.bg_void);
            let frame = skim_frame((at.x - thumb_rect.min.x) / thumb_rect.width());
            let n = SKIM_FRAMES as f32;
            let crop = egui::Rect::from_min_max(egui::pos2(frame as f32 / n, 0.0), egui::pos2((frame + 1) as f32 / n, 1.0));
            draw_poster(ui.painter(), thumb_rect, Some(strip), Some(crop), theme);
            ui.painter().line_segment(
                [egui::pos2(at.x, thumb_rect.min.y), egui::pos2(at.x, thumb_rect.max.y)],
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 255, 255, 100)),
            );
        } else if poster.is_some() {
            ui.painter().rect_filled(thumb_rect, thumb_rounding, theme.bg_void);
            draw_poster(ui.painter(), thumb_rect, poster.as_ref(), None, theme);
        } else {
            // A poster frame is taken from the first media file in the
            // project's manifest, so no thumbnail means no media: the
            // project is empty. Say that, rather than leaving a hole.
            //
            // It used to fill with `project_color` at full strength —
            // a saturated block brighter than any real frame on the
            // page, so the eye went first to the projects with the
            // least in them.
            ui.painter().rect_filled(thumb_rect, thumb_rounding, theme.bg_raised);

            // "Empty project", not the Klipp logo and "Klipp project".
            //
            // Every card in this list is a Klipp project — saying so on
            // each one is a caption that repeats the context it sits
            // in, and with eight empty projects it was eight copies of
            // the logo competing with the real thumbnails beside them.
            //
            // What distinguishes these cards is that they are empty,
            // which is what the words now say. A bare panel would not
            // do: with nothing in it at all it reads as a thumbnail
            // that failed to load rather than a project with nothing
            // in it yet.
            ui.painter().text(
                thumb_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Empty project",
                egui::FontId::proportional(12.0),
                theme.text_dim,
            );
        }

        // No timeline strip.
        //
        // There was one, and it was a hash of the project's *name*:
        // the number of clips, their widths and their colours all came
        // out of `name.bytes()`, with a white playhead drawn at a fixed
        // 35% of the width. It described nothing. Two projects with the
        // same name drew the same edit; renaming a project changed the
        // edit it appeared to contain.
        //
        // On a card whose whole job is "is this the project I want",
        // that is not decoration — it is invented data in the place a
        // user looks for real data. The counts below are measured, and
        // they say what the strip pretended to.

        // Missing media warning badge (top-right of thumbnail)
        if rp.missing_media == Some(true) {
            draw_missing_media_badge(ui, egui::pos2(thumb_rect.max.x, thumb_rect.min.y), theme.marker_warning);
        }

        // Unsaved work: an auto-save newer than the project. Without this
        // the card shows the last save ("Empty project" when there never
        // was one) and nothing says the work is there to restore.
        if self.status.get(&rp.path).is_some_and(|s| s.newer_autosave) {
            let g = layout_truncated(
                ui.painter(),
                "Unsaved work recovered \u{2014} open to restore",
                egui::FontId::proportional(10.0),
                theme.bg_void,
                thumb_rect.width() - 20.0,
            );
            let pill = egui::Rect::from_min_size(
                egui::pos2(thumb_rect.min.x + 6.0, thumb_rect.max.y - g.size().y - 12.0),
                g.size() + egui::vec2(8.0, 6.0),
            );
            ui.painter().rect_filled(pill, 4.0, theme.accent);
            ui.painter().galley(pill.min + egui::vec2(4.0, 3.0), g, theme.bg_void);
        }

        // ── Info: the name, then each detail line, and nothing below ──
        let info_top = thumb_rect.max.y + CARD_INFO_TOP;
        let left_x = rect.min.x + 10.0;
        let max_w = rect.width() - 20.0;

        // Project name. Truncate with an ellipsis if the title
        // wouldn't fit the card width — hard-clipping via clip_rect
        // chopped letters mid-glyph on long names.
        let name_galley = layout_truncated(
            ui.painter(),
            &rp.name,
            egui::FontId::proportional(13.0),
            theme.text_primary,
            max_w,
        );
        ui.painter().galley(
            egui::pos2(left_x, info_top),
            name_galley,
            theme.text_primary,
        );

        // The details, in the proportional face like the rest of the
        // card: its figures are tabular, so numbers still line up from
        // card to card without a typewriter face among the words.
        let mut y = info_top + CARD_NAME_LINE;
        for line in card_lines(rp) {
            let (text, x, w) = match &line {
                CardLine::Modified(t) => {
                    let icon_size = 8.0;
                    draw_clock_icon(ui.painter(), egui::pos2(left_x + icon_size / 2.0, y + 6.0), icon_size, theme.text_secondary);
                    (t.as_str(), left_x + icon_size + 4.0, (max_w - icon_size - 4.0).max(0.0))
                }
                CardLine::Text(t) => (t.as_str(), left_x, max_w),
            };
            let g = layout_truncated(
                ui.painter(),
                text,
                egui::FontId::proportional(crate::theme::type_scale::CAPTION),
                theme.text_dim,
                w,
            );
            ui.painter().galley(egui::pos2(x, y), g, theme.text_dim);
            y += CARD_DETAIL_LINE;
        }

        // Where the file is, when the pointer is on the card's words
        // (the picture is for looking at).
        let info_rect = egui::Rect::from_min_max(egui::pos2(rect.min.x, thumb_rect.max.y), rect.max);
        let pointer_on_info = ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| info_rect.contains(p));
        let resp = if pointer_on_info { resp.on_hover_text(rp.path.display().to_string()) } else { resp };

        // Click to open
        if resp.clicked() {
            cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
        }

        // Right-click context menu
        resp.context_menu_rows(|ui| {
            self.project_context_menu(ui, rp, cmds);
        });
    }

    fn project_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        rp: &RecentProject,
        cmds: &mut Vec<AppCommand>,
    ) {
        use crate::widgets::menu::{menu_body, menu_item};
        menu_body(ui, |ui| {
            if menu_item(ui, "Open", "").clicked() {
                cmds.push(AppCommand::OpenProject { path: rp.path.clone() });
                ui.close_menu();
            }
            if menu_item(ui, "Duplicate", "").clicked() {
                cmds.push(AppCommand::DuplicateProject { path: rp.path.clone() });
                ui.close_menu();
            }
            if menu_item(ui, "Rename\u{2026}", "").clicked() {
                self.renaming_project = Some((rp.path.clone(), rp.name.clone()));
                ui.close_menu();
            }
            if menu_item(ui, "Show in Files", "").clicked() {
                cmds.push(AppCommand::RevealInFileManager { path: rp.path.clone() });
                ui.close_menu();
            }
            ui.separator();
            if menu_item(ui, "Remove from Recent", "").clicked() {
                cmds.push(AppCommand::RemoveRecentProject { path: rp.path.clone() });
                ui.close_menu();
            }
            if ui
                .add(egui::Button::new(egui::RichText::new("Move to Trash\u{2026}").color(ui.visuals().error_fg_color)))
                .clicked()
            {
                self.confirm_trash = Some(rp.path.clone());
                ui.close_menu();
            }
        });
    }
}

// ── Helpers ──

/// The strip frame for a pointer `fraction` of the way across.
fn skim_frame(fraction: f32) -> usize {
    ((fraction.clamp(0.0, 1.0) * SKIM_FRAMES as f32) as usize).min(SKIM_FRAMES - 1)
}

/// One line of a card's details.
#[derive(Clone, Debug, PartialEq)]
enum CardLine {
    Text(String),
    /// When it was saved, drawn after a clock.
    Modified(String),
}

/// A card's detail lines: its picture format and size, what it holds,
/// and when it was saved — each only when there is something to say.
fn card_lines(rp: &RecentProject) -> Vec<CardLine> {
    let mut lines = Vec::new();
    let meta = build_meta_string(rp);
    if !meta.is_empty() {
        lines.push(CardLine::Text(meta));
    }
    let counts = build_counts_string(rp);
    if !counts.is_empty() {
        lines.push(CardLine::Text(counts));
    }
    let when = rp.last_saved.as_deref().map(relative_time).unwrap_or_default();
    if !when.is_empty() {
        lines.push(CardLine::Modified(when));
    }
    lines
}

/// The sidebar's order: pinned projects first, then by recency, each
/// group in the order the list keeps.
fn sidebar_order(recent: &[RecentProject]) -> Vec<&RecentProject> {
    let mut order: Vec<&RecentProject> = recent.iter().collect();
    order.sort_by_key(|rp| !rp.pinned);
    order
}

/// The sidebar's second line: when it was saved, how long the cut is,
/// and its frame rate — "Today 13:26 · 1:55 · 24 fps".
fn sidebar_detail(rp: &RecentProject, now: chrono::NaiveDateTime) -> String {
    let mut parts = Vec::new();
    if let Some(when) = rp.last_saved.as_deref().and_then(|s| when_saved(s, now)) {
        parts.push(when);
    }
    if let Some(dur) = rp.duration_secs {
        parts.push(format_length(dur));
    }
    if let Some(fps) = rp.fps.as_deref() {
        parts.push(fps_label(fps));
    }
    parts.join(" \u{00B7} ")
}

/// A save time as a day and a clock time: "Today 13:26", "Yesterday
/// 09:05", "Mon 13:26" within the week, then "12 Oct", and "12 Oct 2025"
/// in another year.
fn when_saved(iso: &str, now: chrono::NaiveDateTime) -> Option<String> {
    let dt = parse_saved(iso)?;
    let days = (now.date() - dt.date()).num_days();
    let time = dt.format("%H:%M");
    Some(match days {
        0 => format!("Today {time}"),
        1 => format!("Yesterday {time}"),
        2..=6 => format!("{} {time}", dt.format("%a")),
        _ if dt.date().format("%Y").to_string() == now.date().format("%Y").to_string() => dt.format("%-d %b").to_string(),
        _ => dt.format("%-d %b %Y").to_string(),
    })
}

/// A length as "1:55", or "1:02:03" past the hour.
fn format_length(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "24fps" or "29.970fps", as the list keeps rates, read as "24 fps"
/// and "29.97 fps".
fn fps_label(fps: &str) -> String {
    let number = fps.trim().trim_end_matches("fps").trim();
    let number = if number.contains('.') { number.trim_end_matches('0').trim_end_matches('.') } else { number };
    format!("{number} fps")
}

/// The list view's columns and their widths for a table `width` wide:
/// the fixed ones as wide as their figures, the name and the place
/// sharing what is left.
fn list_columns(width: f32) -> Vec<(SortMode, f32)> {
    let fixed = [
        (SortMode::Modified, 130.0),
        (SortMode::Duration, 76.0),
        (SortMode::Resolution, 104.0),
        (SortMode::Fps, 76.0),
        (SortMode::Size, 84.0),
    ];
    let rest = (width - fixed.iter().map(|(_, w)| w).sum::<f32>()).max(240.0);
    let mut columns = vec![(SortMode::Name, rest * 0.6)];
    columns.extend(fixed);
    columns.push((SortMode::Location, rest * 0.4));
    columns
}

/// Figures read right-aligned, so their places line up down a column.
fn list_column_is_numeric(mode: SortMode) -> bool {
    matches!(mode, SortMode::Duration | SortMode::Fps | SortMode::Size)
}

/// What a list row shows under `column`.
fn list_cell(rp: &RecentProject, column: SortMode) -> String {
    match column {
        SortMode::Name => rp.name.clone(),
        SortMode::Modified => rp.last_saved.as_deref()
            .and_then(|s| when_saved(s, chrono::Local::now().naive_local()))
            .unwrap_or_default(),
        SortMode::Duration => rp.duration_secs.map(format_length).unwrap_or_default(),
        SortMode::Resolution => rp.resolution.clone().unwrap_or_default(),
        SortMode::Fps => rp.fps.as_deref().map(fps_label).unwrap_or_default(),
        SortMode::Size => rp.total_size_bytes.map(format_size_compact).unwrap_or_default(),
        SortMode::Location => rp.path.parent().map(|p| p.display().to_string()).unwrap_or_default(),
    }
}

/// Sort `list` by `mode`. A project missing the value sorts last either
/// way round, rather than ahead of every real one.
fn sort_projects(list: &mut [&RecentProject], mode: SortMode, descending: bool) {
    fn pixels(res: &str) -> Option<f64> {
        let (w, h) = res.split_once(['x', '\u{00D7}'])?;
        Some(w.trim().parse::<f64>().ok()? * h.trim().parse::<f64>().ok()?)
    }
    fn number(rp: &RecentProject, mode: SortMode) -> Option<f64> {
        match mode {
            SortMode::Size => rp.total_size_bytes.map(|b| b as f64),
            SortMode::Duration => rp.duration_secs,
            SortMode::Resolution => rp.resolution.as_deref().and_then(pixels),
            SortMode::Fps => rp.fps.as_deref().and_then(|f| f.trim().trim_end_matches("fps").trim().parse().ok()),
            SortMode::Modified => rp.last_saved.as_deref().and_then(parse_saved).map(|d| d.and_utc().timestamp() as f64),
            SortMode::Name | SortMode::Location => None,
        }
    }
    let text = |rp: &RecentProject| match mode {
        SortMode::Location => rp.path.parent().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default(),
        _ => rp.name.to_lowercase(),
    };
    list.sort_by(|a, b| {
        let order = match mode {
            SortMode::Name | SortMode::Location => text(a).cmp(&text(b)),
            _ => match (number(a, mode), number(b, mode)) {
                (Some(x), Some(y)) => x.total_cmp(&y),
                (Some(_), None) => return std::cmp::Ordering::Less,
                (None, Some(_)) => return std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            },
        };
        if descending { order.reverse() } else { order }
    });
}

/// Draw a poster into `rect`: fitted whole, with a soft, darkened copy
/// of the same picture filling what it leaves — so a vertical project
/// sits in its own colours rather than between black bars. `crop` picks
/// a part of the picture (a frame of a strip) instead of all of it.
fn draw_poster(painter: &egui::Painter, rect: egui::Rect, poster: Option<&Poster>, crop: Option<egui::Rect>, theme: &KlippTheme) {
    let Some(poster) = poster else {
        painter.rect_filled(rect, 3.0, theme.bg_raised);
        painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.0_f32, theme.border));
        return;
    };
    let uv = crop.unwrap_or(egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)));
    let img_aspect = poster.aspect() * uv.width() / uv.height().max(f32::EPSILON);
    let rect_aspect = rect.width() / rect.height().max(1.0);
    if (img_aspect - rect_aspect).abs() / rect_aspect < 0.02 {
        painter.image(poster.sharp.id(), rect, uv, egui::Color32::WHITE);
        return;
    }
    // The fill: the soft picture scaled to cover, its middle shown.
    let cover = if img_aspect > rect_aspect {
        let f = rect_aspect / img_aspect;
        egui::Rect::from_min_max(egui::pos2(0.5 - f / 2.0, 0.0), egui::pos2(0.5 + f / 2.0, 1.0))
    } else {
        let f = img_aspect / rect_aspect;
        egui::Rect::from_min_max(egui::pos2(0.0, 0.5 - f / 2.0), egui::pos2(1.0, 0.5 + f / 2.0))
    };
    let cover_uv = egui::Rect::from_min_max(
        egui::pos2(uv.min.x + cover.min.x * uv.width(), uv.min.y + cover.min.y * uv.height()),
        egui::pos2(uv.min.x + cover.max.x * uv.width(), uv.min.y + cover.max.y * uv.height()),
    );
    painter.image(poster.soft.id(), rect, cover_uv, egui::Color32::from_gray(150));
    let fitted = if img_aspect > rect_aspect {
        egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), rect.width() / img_aspect))
    } else {
        egui::Rect::from_center_size(rect.center(), egui::vec2(rect.height() * img_aspect, rect.height()))
    };
    painter.image(poster.sharp.id(), fitted, uv, egui::Color32::WHITE);
}

/// A soft copy of `img`: averaged down to `across` pixels wide, which
/// the linear filter spreads back out as a blur.
fn soften(img: &egui::ColorImage, across: usize) -> egui::ColorImage {
    let [w, h] = img.size;
    let sw = across.min(w.max(1)).max(1);
    let sh = ((h * sw) / w.max(1)).clamp(1, 24);
    let mut pixels = Vec::with_capacity(sw * sh);
    for y in 0..sh {
        for x in 0..sw {
            let (x0, x1) = (x * w / sw, ((x + 1) * w / sw).max(x * w / sw + 1).min(w));
            let (y0, y1) = (y * h / sh, ((y + 1) * h / sh).max(y * h / sh + 1).min(h));
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let c = img.pixels[yy * w + xx];
                    sum[0] += c.r() as u32;
                    sum[1] += c.g() as u32;
                    sum[2] += c.b() as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            pixels.push(egui::Color32::from_rgb((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8));
        }
    }
    egui::ColorImage { size: [sw, sh], pixels }
}

/// Draw one sidebar mark centred on `c`.
fn draw_mark(painter: &egui::Painter, c: egui::Pos2, mark: &Mark, theme: &KlippTheme) {
    match mark {
        Mark::Pinned => {
            // A pin: its head and its point.
            painter.circle_filled(c + egui::vec2(0.0, -2.0), 3.0, theme.text_secondary);
            painter.line_segment([c + egui::vec2(0.0, 0.0), c + egui::vec2(0.0, 5.0)], egui::Stroke::new(1.5_f32, theme.text_secondary));
        }
        Mark::OpenNow(_) => {
            painter.circle_filled(c, 3.5, theme.marker_success);
        }
        Mark::Unsaved => {
            painter.circle_stroke(c, 3.5, egui::Stroke::new(1.5_f32, theme.accent));
        }
        Mark::MissingMedia => {
            let s = 5.0;
            painter.add(egui::Shape::convex_polygon(
                vec![c + egui::vec2(0.0, -s), c + egui::vec2(-s, s * 0.8), c + egui::vec2(s, s * 0.8)],
                theme.marker_warning,
                egui::Stroke::NONE,
            ));
            painter.text(c + egui::vec2(0.0, 1.0), egui::Align2::CENTER_CENTER, "!", egui::FontId::proportional(8.0), egui::Color32::from_rgb(30, 25, 10));
        }
    }
}


/// Lay out a single-line galley that truncates with an ellipsis when
/// the text wouldn't fit `max_w`. Project-card fields (name,
/// fps·res·dur·size, clip/track counts, timestamp) all use this so
/// nothing ever paints past the card's right edge.
fn layout_truncated(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('\u{2026}'),
    };
    painter.layout_job(job)
}

fn format_size_compact(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// "Version 0.1.0 · Build 2711": the build number says which build of a
/// version this is, and is left out where the binary recorded none.
fn version_line(build: &klipp_core::build_info::BuildInfo) -> String {
    if build.number > 0 {
        format!("Version {} · Build {}", build.version, build.number)
    } else {
        format!("Version {}", build.version)
    }
}

/// "1920×1080 · 24 fps · 1:55 · 1.2 GB": the picture, the length, the
/// size on disk.
fn build_meta_string(rp: &RecentProject) -> String {
    let mut parts = Vec::new();
    if let Some(ref res) = rp.resolution {
        parts.push(res.replace('x', "\u{00D7}"));
    }
    if let Some(ref fps) = rp.fps {
        parts.push(fps_label(fps));
    }
    if let Some(dur) = rp.duration_secs {
        parts.push(format_length(dur));
    }
    if let Some(size) = rp.total_size_bytes {
        parts.push(format_size_compact(size));
    }
    parts.join(" \u{00B7} ")
}

/// "2 sequences · 14 clips · 4 tracks": what the project holds.
fn build_counts_string(rp: &RecentProject) -> String {
    let plural = |n: u32, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if let Some(n) = rp.sequence_count {
        parts.push(plural(n, "sequence"));
    }
    if let Some(n) = rp.clip_count {
        parts.push(plural(n, "clip"));
    }
    if let Some(n) = rp.track_count {
        parts.push(plural(n, "track"));
    }
    parts.join(" \u{00B7} ")
}

/// Draw a small warning triangle icon for missing media.
fn draw_missing_media_badge(ui: &egui::Ui, top_right: egui::Pos2, fill: egui::Color32) {
    let size = 18.0;
    let margin = 6.0;
    let center = egui::pos2(top_right.x - margin - size / 2.0, top_right.y + margin + size / 2.0);

    // Triangle background
    let tri_top = egui::pos2(center.x, center.y - size / 2.0);
    let tri_bl = egui::pos2(center.x - size / 2.0, center.y + size / 2.0);
    let tri_br = egui::pos2(center.x + size / 2.0, center.y + size / 2.0);

    let painter = ui.painter();
    painter.add(egui::Shape::convex_polygon(
        vec![tri_top, tri_bl, tri_br],
        fill,
        egui::Stroke::NONE,
    ));

    // Exclamation mark
    painter.text(
        egui::pos2(center.x, center.y + 1.5),
        egui::Align2::CENTER_CENTER,
        "!",
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(30, 25, 10),
    );
}

/// Draw a small clock icon using painter calls.
fn draw_clock_icon(painter: &egui::Painter, center: egui::Pos2, size: f32, color: egui::Color32) {
    let r = size / 2.0;
    // Circle outline
    painter.circle_stroke(center, r, egui::Stroke::new(1.0_f32, color));
    // Hour hand (pointing up-right, ~2 o'clock)
    painter.line_segment(
        [center, egui::pos2(center.x + r * 0.4, center.y - r * 0.35)],
        egui::Stroke::new(1.0_f32, color),
    );
    // Minute hand (pointing up)
    painter.line_segment(
        [center, egui::pos2(center.x, center.y - r * 0.55)],
        egui::Stroke::new(1.0_f32, color),
    );
}

/// Draw a dashed rounded rectangle.
fn draw_dashed_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: f32,
    color: egui::Color32,
    width: f32,
    dash: f32,
    gap: f32,
) {
    // Approximate with line segments along each side (simplified — straight edges)
    let sides = [
        // Top
        (egui::pos2(rect.min.x + rounding, rect.min.y), egui::pos2(rect.max.x - rounding, rect.min.y)),
        // Right
        (egui::pos2(rect.max.x, rect.min.y + rounding), egui::pos2(rect.max.x, rect.max.y - rounding)),
        // Bottom
        (egui::pos2(rect.max.x - rounding, rect.max.y), egui::pos2(rect.min.x + rounding, rect.max.y)),
        // Left
        (egui::pos2(rect.min.x, rect.max.y - rounding), egui::pos2(rect.min.x, rect.min.y + rounding)),
    ];

    let stroke = egui::Stroke::new(width, color);

    for (start, end) in sides {
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1.0 { continue; }
        let ux = dx / len;
        let uy = dy / len;

        let mut d = 0.0;
        let mut drawing = true;
        while d < len {
            let seg = if drawing { dash } else { gap };
            let seg_end = (d + seg).min(len);
            if drawing {
                let p0 = egui::pos2(start.x + ux * d, start.y + uy * d);
                let p1 = egui::pos2(start.x + ux * seg_end, start.y + uy * seg_end);
                painter.line_segment([p0, p1], stroke);
            }
            d = seg_end;
            drawing = !drawing;
        }
    }

    // Corner arcs (approximate with a few line segments each)
    let corners = [
        (egui::pos2(rect.max.x - rounding, rect.min.y + rounding), 270.0), // top-right
        (egui::pos2(rect.max.x - rounding, rect.max.y - rounding), 0.0),   // bottom-right
        (egui::pos2(rect.min.x + rounding, rect.max.y - rounding), 90.0),  // bottom-left
        (egui::pos2(rect.min.x + rounding, rect.min.y + rounding), 180.0), // top-left
    ];
    let segments = 6;
    for (center, start_angle) in corners {
        for i in 0..segments {
            let a0 = (start_angle + 90.0 * i as f32 / segments as f32).to_radians();
            let a1 = (start_angle + 90.0 * (i + 1) as f32 / segments as f32).to_radians();
            let p0 = egui::pos2(center.x + rounding * a0.cos(), center.y + rounding * a0.sin());
            let p1 = egui::pos2(center.x + rounding * a1.cos(), center.y + rounding * a1.sin());
            // Dash pattern for corners
            if i % 2 == 0 {
                painter.line_segment([p0, p1], stroke);
            }
        }
    }
}

/// A saved time as the list keeps it, in local time.
fn parse_saved(iso: &str) -> Option<chrono::NaiveDateTime> {
    use chrono::NaiveDateTime;

    // Strip trailing 'Z' (legacy UTC marker)
    let cleaned = iso.trim_end_matches('Z');

    NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| NaiveDateTime::parse_from_str(cleaned, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(&format!("{cleaned} 00:00:00"), "%Y-%m-%d %H:%M:%S"))
        .ok()
}

/// Parse ISO 8601 datetime string and return a human-friendly relative time.
fn relative_time(iso: &str) -> String {
    let Some(dt) = parse_saved(iso) else { return String::new() };

    // Compare against local time (timestamps are stored in local time)
    let now = chrono::Local::now().naive_local();
    let diff = now.signed_duration_since(dt);
    let time_str = dt.format("%H:%M").to_string();

    let secs = diff.num_seconds();
    if secs < 0 {
        return "just now".into();
    }
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    let weeks = days / 7;

    if mins < 1 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins} min{} ago", if mins == 1 { "" } else { "s" })
    } else if hours < 24 {
        format!("Today at {time_str}")
    } else if days == 1 {
        format!("Yesterday at {time_str}")
    } else if days < 7 {
        format!("{days} days ago")
    } else if weeks == 1 {
        "Last week".into()
    } else if weeks < 5 {
        format!("{weeks} weeks ago")
    } else {
        dt.format("%b %d, %Y").to_string()
    }
}

/// Decode a PNG file into an egui ColorImage.
fn image_from_png(data: &[u8]) -> Result<egui::ColorImage, String> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    // png 0.18: output_buffer_size returns None when the dimensions
    // would overflow usize. Treat that as a decode failure.
    let buf_size = reader.output_buffer_size()
        .ok_or_else(|| "PNG dimensions overflow buffer size".to_string())?;
    let mut buf = vec![0u8; buf_size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let bytes = &buf[..info.buffer_size()];

    let w = info.width as usize;
    let h = info.height as usize;

    let pixels: Vec<egui::Color32> = match info.color_type {
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .map(|c| egui::Color32::from_rgb(c[0], c[1], c[2]))
            .collect(),
        png::ColorType::Rgba => bytes
            .chunks_exact(4)
            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
            .collect(),
        _ => return Err("Unsupported PNG colour type".into()),
    };

    Ok(egui::ColorImage {
        size: [w, h],
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sidebar names the version and its build number, and no
    /// platform: Klipp runs on Linux only.
    #[test]
    fn the_version_line_carries_the_build_number() {
        let mut build = klipp_core::build_info::BuildInfo { version: "0.1.0", number: 2711, commit: "672c2162", dirty: false, profile: "release", exe: None, built_at: None };
        assert_eq!(version_line(&build), "Version 0.1.0 · Build 2711");
        build.number = 0;
        assert_eq!(version_line(&build), "Version 0.1.0");
    }

    // ── #1847: the welcome screen drawn ──

    type Shapes = Vec<egui::epaint::ClippedShape>;

    /// One frame of the welcome screen with `events`: the commands it
    /// gave and what it drew.
    fn frame(ctx: &egui::Context, screen: &mut LaunchScreen, recent: &[RecentProject], folder: Option<&std::path::Path>, events: Vec<egui::Event>) -> (Vec<AppCommand>, Shapes) {
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), events, ..Default::default() };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                cmds = screen.show(ctx, ui, recent, folder, &theme);
            });
        });
        (cmds, out.shapes)
    }

    fn texts(shapes: &Shapes) -> Vec<(String, egui::Rect)> {
        shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), egui::Rect::from_min_size(t.pos, t.galley.size()))),
            _ => None,
        }).collect()
    }

    fn at(shapes: &Shapes, label: &str) -> egui::Pos2 {
        let all = texts(shapes);
        all.iter().find(|(t, _)| t == label).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("{label} is drawn: {:?}", all.iter().map(|t| &t.0).collect::<Vec<_>>()))
    }

    /// Each textured mesh drawn: its texture, where, and the part of the
    /// texture shown.
    fn images(shapes: &Shapes) -> Vec<(egui::TextureId, egui::Rect, egui::Rect)> {
        shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Mesh(m) if m.texture_id != egui::TextureId::default() && !m.vertices.is_empty() => {
                let rect = egui::Rect::from_points(&m.vertices.iter().map(|v| v.pos).collect::<Vec<_>>());
                let uv = egui::Rect::from_points(&m.vertices.iter().map(|v| v.uv).collect::<Vec<_>>());
                Some((m.texture_id, rect, uv))
            }
            _ => None,
        }).collect()
    }

    fn click_with(ctx: &egui::Context, screen: &mut LaunchScreen, recent: &[RecentProject], folder: Option<&std::path::Path>, pos: egui::Pos2, button: egui::PointerButton) -> Vec<AppCommand> {
        let press = |pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, screen, recent, folder, vec![egui::Event::PointerMoved(pos), press(true)]).0;
        cmds.extend(frame(ctx, screen, recent, folder, vec![press(false)]).0);
        cmds.extend(frame(ctx, screen, recent, folder, Vec::new()).0);
        cmds
    }

    fn click(ctx: &egui::Context, screen: &mut LaunchScreen, recent: &[RecentProject], pos: egui::Pos2) -> Vec<AppCommand> {
        click_with(ctx, screen, recent, None, pos, egui::PointerButton::Primary)
    }

    /// A PNG of `w`x`h`, each pixel coloured by `colour(x)`.
    fn png(path: &std::path::Path, w: u32, h: u32, colour: impl Fn(u32) -> [u8; 3]) {
        let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
        let mut enc = png::Encoder::new(file, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().unwrap();
        let data: Vec<u8> = (0..h).flat_map(|_| (0..w).flat_map(&colour)).collect();
        writer.write_image_data(&data).unwrap();
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-launch-1847-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn project(dir: &std::path::Path, name: &str) -> RecentProject {
        let mut rp = RecentProject::from_path(dir.join(format!("{name}.klipp")));
        rp.last_saved = Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
        rp.duration_secs = Some(115.0);
        rp.fps = Some("24fps".into());
        rp.resolution = Some("1920\u{00D7}1080".into());
        rp.total_size_bytes = Some(3 << 20);
        rp.sequence_count = Some(2);
        rp.clip_count = Some(14);
        rp.track_count = Some(4);
        rp.missing_media = Some(false);
        rp
    }

    /// A screen whose status check is taken as given, not run.
    fn screen_with(status: &[(PathBuf, ProjectStatus)]) -> LaunchScreen {
        LaunchScreen { status: status.iter().cloned().collect(), status_checked_at: Some(std::time::Instant::now()), ..Default::default() }
    }

    /// The sidebar shows each project's poster, pinned ones first, the
    /// second line as when · length · rate, and a mark for each of open
    /// now, unsaved and missing media, each saying what it means.
    #[test]
    fn the_sidebar_shows_posters_and_clear_marks() {
        let dir = temp_dir();
        let poster = dir.join("Trip.png");
        png(&poster, 32, 18, |_| [200, 80, 40]);
        let mut trip = project(&dir, "Trip");
        trip.thumbnail_path = Some(poster);
        let mut empty = project(&dir, "Empty");
        empty.missing_media = Some(true);
        let mut pinned = project(&dir, "Pinned");
        pinned.pinned = true;
        let recent = vec![trip.clone(), empty.clone(), pinned.clone()];
        let mut screen = screen_with(&[
            (trip.path.clone(), ProjectStatus { newer_autosave: true, open_by: Some("ann@studio".into()) }),
        ]);
        let ctx = egui::Context::default();
        ctx.style_mut(|s| {
            s.interaction.tooltip_delay = 0.0;
            s.interaction.show_tooltips_only_when_still = false;
        });
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());

        let rows: Vec<_> = screen.seen.sidebar.iter().map(|(p, _, t)| (p.clone(), t.is_some())).collect();
        assert_eq!(rows, vec![(pinned.path.clone(), false), (trip.path.clone(), true), (empty.path.clone(), false)], "pinned first; a poster where there is one");
        let (_, row, tex) = &screen.seen.sidebar[1];
        let drawn = images(&shapes);
        let thumb = drawn.iter().find(|(t, _, _)| Some(*t) == *tex).expect("the poster is drawn");
        assert!(row.contains_rect(thumb.1), "inside its row");
        assert!((thumb.1.width() / thumb.1.height() - 16.0 / 9.0).abs() < 0.05, "16:9: {:?}", thumb.1);

        let detail = format!("Today {} \u{00B7} 1:55 \u{00B7} 24 fps", chrono::Local::now().format("%H:%M"));
        assert!(texts(&shapes).iter().any(|(t, _)| *t == detail), "{detail}");

        let marks: Vec<_> = screen.seen.marks.iter().map(|(p, m, _)| (p.clone(), m.clone())).collect();
        for want in [
            (pinned.path.clone(), Mark::Pinned),
            (trip.path.clone(), Mark::OpenNow("ann@studio".into())),
            (trip.path.clone(), Mark::Unsaved),
            (empty.path.clone(), Mark::MissingMedia),
        ] {
            assert!(marks.contains(&want), "{want:?} in {marks:?}");
        }
        assert_eq!(marks.len(), 4, "no unexplained dot: {marks:?}");
        // Each says what it is.
        let unsaved = screen.seen.marks.iter().find(|(_, m, _)| *m == Mark::Unsaved).unwrap().2;
        frame(&ctx, &mut screen, &recent, None, vec![egui::Event::PointerMoved(unsaved.center())]);
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        assert!(texts(&shapes).iter().any(|(t, _)| t.starts_with("Unsaved changes")), "the tooltip");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A sidebar row's menu: Open, Pin, Show in Files, Remove from Recent.
    #[test]
    fn the_sidebar_menu_pins_shows_and_removes() {
        let dir = temp_dir();
        let recent = vec![project(&dir, "Trip")];
        let path = recent[0].path.clone();
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let row = screen.seen.sidebar[0].1.center();
        let open_menu = |screen: &mut LaunchScreen| {
            click_with(&ctx, screen, &recent, None, row, egui::PointerButton::Secondary);
            frame(&ctx, screen, &recent, None, Vec::new()).1
        };
        let shapes = open_menu(&mut screen);
        let all: Vec<String> = texts(&shapes).into_iter().map(|t| t.0).collect();
        for item in ["Open", "Pin to Top", "Show in Files", "Remove from Recent"] {
            assert!(all.iter().any(|t| t == item), "{item} in {all:?}");
        }
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Pin to Top"));
        assert!(matches!(&cmds[..], [AppCommand::PinRecentProject { path: p, pinned: true }] if *p == path), "{cmds:?}");
        let shapes = open_menu(&mut screen);
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Show in Files"));
        assert!(matches!(&cmds[..], [AppCommand::RevealInFileManager { path: p }] if *p == path), "{cmds:?}");
        let shapes = open_menu(&mut screen);
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Remove from Recent"));
        assert!(matches!(&cmds[..], [AppCommand::RemoveRecentProject { path: p }] if *p == path), "{cmds:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A card is as tall as what it says: the last line of text sits
    /// just above its bottom edge, and a project with less to say gets a
    /// shorter card, not an empty band.
    #[test]
    fn cards_fit_their_content() {
        let dir = temp_dir();
        let full = vec![project(&dir, "Trip"), project(&dir, "Hike")];
        let bare = vec![RecentProject::from_path(dir.join("Bare.klipp"))];
        let ctx = egui::Context::default();
        let mut heights = Vec::new();
        for recent in [&full, &bare] {
            let mut screen = screen_with(&[]);
            frame(&ctx, &mut screen, recent, None, Vec::new());
            let (_, shapes) = frame(&ctx, &mut screen, recent, None, Vec::new());
            let all = texts(&shapes);
            for (_, card) in &screen.seen.cards {
                let bottom = all.iter().filter(|(_, r)| card.contains(r.center())).map(|(_, r)| r.max.y).fold(f32::MIN, f32::max);
                let gap = card.max.y - bottom;
                assert!((0.0..=12.0).contains(&gap), "text ends {gap} px above the card's edge");
            }
            heights.push(screen.seen.cards[0].1.height());
        }
        assert!(heights[1] < heights[0] - 30.0, "a card with nothing to say is shorter: {heights:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A card's details are in the proportional face, not monospace, and
    /// say how many sequences there are.
    #[test]
    fn card_details_are_proportional_with_the_sequence_count() {
        let dir = temp_dir();
        let recent = vec![project(&dir, "Trip")];
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        let card = screen.seen.cards[0].1;
        let details: Vec<_> = shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) if card.contains(t.pos) => Some((t.galley.text().to_string(), t.galley.job.sections[0].format.font_id.family.clone())),
            _ => None,
        }).collect();
        assert!(details.iter().any(|(t, _)| t == "2 sequences \u{00B7} 14 clips \u{00B7} 4 tracks"), "{details:?}");
        assert!(details.iter().any(|(t, _)| t.starts_with("1920\u{00D7}1080 \u{00B7} 24 fps \u{00B7} 1:55")), "{details:?}");
        assert!(details.iter().all(|(_, f)| *f == egui::FontFamily::Proportional), "{details:?}");
        // Its figures are tabular, so they still line up.
        let width = |s: &str| ctx.fonts(|f| f.layout_no_wrap(s.into(), egui::FontId::proportional(10.0), egui::Color32::WHITE).size().x);
        let ones = width("1111");
        for d in ["0000", "4444", "8888"] {
            assert!((width(d) - ones).abs() < 0.01, "{d}: {} vs {ones}", width(d));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A card's menu: Open, Duplicate, Rename, Show in Files, Remove from
    /// Recent, and Move to Trash — which asks first, and does nothing on
    /// Cancel.
    #[test]
    fn the_card_menu_duplicates_renames_removes_and_trashes_with_confirmation() {
        let dir = temp_dir();
        let recent = vec![project(&dir, "Trip")];
        let path = recent[0].path.clone();
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let card = screen.seen.cards[0].1;
        let pick = |screen: &mut LaunchScreen, item: &str| {
            click_with(&ctx, screen, &recent, None, card.center(), egui::PointerButton::Secondary);
            let shapes = frame(&ctx, screen, &recent, None, Vec::new()).1;
            let all: Vec<String> = texts(&shapes).into_iter().map(|t| t.0).collect();
            for want in ["Open", "Duplicate", "Rename\u{2026}", "Show in Files", "Remove from Recent", "Move to Trash\u{2026}"] {
                assert!(all.iter().any(|t| t == want), "{want} in {all:?}");
            }
            let cmds = click(&ctx, screen, &recent, at(&shapes, item));
            (cmds, frame(&ctx, screen, &recent, None, Vec::new()).1)
        };
        let (cmds, _) = pick(&mut screen, "Duplicate");
        assert!(matches!(&cmds[..], [AppCommand::DuplicateProject { path: p }] if *p == path), "{cmds:?}");
        let (cmds, _) = pick(&mut screen, "Remove from Recent");
        assert!(matches!(&cmds[..], [AppCommand::RemoveRecentProject { path: p }] if *p == path), "{cmds:?}");
        let (cmds, _) = pick(&mut screen, "Show in Files");
        assert!(matches!(&cmds[..], [AppCommand::RevealInFileManager { path: p }] if *p == path), "{cmds:?}");

        // Rename: the name is typed into, and Rename renames.
        let (cmds, _) = pick(&mut screen, "Rename\u{2026}");
        assert!(cmds.is_empty());
        frame(&ctx, &mut screen, &recent, None, vec![egui::Event::Text(" 2".into())]);
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Rename"));
        assert!(matches!(&cmds[..], [AppCommand::RenameProjectOnDisk { old_path, new_name }] if *old_path == path && new_name == "Trip 2"), "{cmds:?}");

        // Trash: asked, Cancel does nothing.
        let (cmds, shapes) = pick(&mut screen, "Move to Trash\u{2026}");
        assert!(cmds.is_empty(), "nothing is trashed before the answer: {cmds:?}");
        assert!(texts(&shapes).iter().any(|(t, _)| t == "Move \u{201C}Trip\u{201D} to the trash?"));
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Cancel"));
        assert!(cmds.is_empty() && screen.confirm_trash.is_none(), "{cmds:?}");
        // Asked again, and answered.
        let (_, shapes) = pick(&mut screen, "Move to Trash\u{2026}");
        // The window's title says it too; the button is the lower one.
        let button = texts(&shapes).into_iter().filter(|(t, _)| t == "Move to Trash").max_by(|a, b| a.1.min.y.total_cmp(&b.1.min.y)).expect("the button").1.center();
        let cmds = click(&ctx, &mut screen, &recent, button);
        assert!(matches!(&cmds[..], [AppCommand::MoveProjectToTrash { path: p }] if *p == path), "{cmds:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The list view is a table: a click on a column's name sorts by it,
    /// a second turns it round, and a project without the value is last.
    #[test]
    fn the_list_sorts_by_its_columns() {
        let dir = temp_dir();
        let mut a = project(&dir, "Alpha");
        a.total_size_bytes = Some(5 << 20);
        let mut b = project(&dir, "Bravo");
        b.total_size_bytes = Some(9 << 20);
        let mut c = project(&dir, "Charlie");
        c.total_size_bytes = None;
        let recent = vec![a, b, c];
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        screen.view_mode = ViewMode::List;
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        for column in ["Name", "Modified", "Length", "Resolution", "FPS", "Size", "Location"] {
            at(&shapes, column);
        }
        let order = |shapes: &Shapes| {
            // The table's rows, not the sidebar's.
            let mut rows: Vec<_> = texts(shapes).into_iter().filter(|(t, r)| r.min.x > SIDEBAR_W && ["Alpha", "Bravo", "Charlie"].contains(&t.as_str())).collect();
            rows.sort_by(|x, y| x.1.min.y.total_cmp(&y.1.min.y));
            rows.into_iter().map(|r| r.0).collect::<Vec<_>>()
        };
        click(&ctx, &mut screen, &recent, at(&shapes, "Size"));
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        assert_eq!(order(&shapes), ["Bravo", "Alpha", "Charlie"], "largest first");
        click(&ctx, &mut screen, &recent, at(&shapes, "Size \u{25BE}"));
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        assert_eq!(order(&shapes), ["Alpha", "Bravo", "Charlie"], "turned round, the unknown still last");
        click(&ctx, &mut screen, &recent, at(&shapes, "Name"));
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        assert_eq!(order(&shapes), ["Alpha", "Bravo", "Charlie"]);
        click(&ctx, &mut screen, &recent, at(&shapes, "Name \u{25B4}"));
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        assert_eq!(order(&shapes), ["Charlie", "Bravo", "Alpha"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The header's Recent / In <folder> is the shared segmented control
    /// (the active segment lit as the workspace switch lights it), and
    /// Change Folder is a quiet, frameless button.
    #[test]
    fn the_header_uses_the_shared_controls() {
        let dir = temp_dir();
        let folder = dir.join("Projects");
        std::fs::create_dir_all(&folder).unwrap();
        let recent = vec![project(&dir, "Trip")];
        let theme = KlippTheme::default();
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, Some(&folder), Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, Some(&folder), Vec::new());
        let lit = |shapes: &Shapes, label: &str| {
            let p = at(shapes, label);
            shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == theme.accent_subtle && r.rect.contains(p)))
        };
        assert!(lit(&shapes, "Recent") && !lit(&shapes, "In Projects"));
        let change = at(&shapes, "Change Folder\u{2026}");
        assert!(!shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect.contains(change) && r.fill != egui::Color32::TRANSPARENT && r.rect.width() < 300.0)), "no frame behind Change Folder");
        click_with(&ctx, &mut screen, &recent, Some(&folder), at(&shapes, "In Projects"), egui::PointerButton::Primary);
        let (_, shapes) = frame(&ctx, &mut screen, &recent, Some(&folder), Vec::new());
        assert!(screen.folder_view && lit(&shapes, "In Projects") && !lit(&shapes, "Recent"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A vertical project's card fills the space beside its picture with
    /// a soft copy of the same picture, not black bars.
    #[test]
    fn a_vertical_poster_sits_on_a_soft_fill_of_itself() {
        let dir = temp_dir();
        let poster = dir.join("Tall.png");
        png(&poster, 18, 32, |x| [x as u8 * 10, 90, 160]);
        let mut tall = project(&dir, "Tall");
        tall.thumbnail_path = Some(poster.clone());
        let recent = vec![tall];
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        let card = screen.seen.cards[0].1;
        let p = screen.posters[&poster].clone();
        let in_card: Vec<_> = images(&shapes).into_iter().filter(|(_, r, _)| card.contains(r.center())).collect();
        let soft = in_card.iter().find(|(t, _, _)| *t == p.soft.id()).expect("the soft fill is drawn");
        let sharp = in_card.iter().find(|(t, _, _)| *t == p.sharp.id()).expect("the picture is drawn");
        assert!((soft.1.width() - (card.width() - 2.0)).abs() < 1.0, "the fill spans the thumbnail: {:?}", soft.1);
        assert!(sharp.1.width() < soft.1.width() * 0.5 && soft.1.contains_rect(sharp.1), "the picture whole, in the middle");
        assert!(p.soft.size()[0] <= 12, "soft: a few pixels across");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Moving across a card's thumbnail scrubs through the project: the
    /// frame of its skim strip under the pointer, further along further
    /// right, and the poster again once the pointer leaves.
    #[test]
    fn hovering_a_card_scrubs_through_the_project() {
        let dir = temp_dir();
        let poster = dir.join("Trip.png");
        png(&poster, 32, 18, |_| [10, 10, 10]);
        png(&skim_strip_path(&poster), 32 * SKIM_FRAMES as u32, 18, |x| [(x / 32) as u8 * 20, 0, 0]);
        let mut trip = project(&dir, "Trip");
        trip.thumbnail_path = Some(poster.clone());
        let recent = vec![trip];
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let card = screen.seen.cards[0].1;
        let thumb_y = card.min.y + 40.0;
        let shown_at = |screen: &mut LaunchScreen, fraction: f32| {
            let x = card.min.x + 1.0 + (card.width() - 2.0) * fraction;
            frame(&ctx, screen, &recent, None, vec![egui::Event::PointerMoved(egui::pos2(x, thumb_y))]);
            let shapes = frame(&ctx, screen, &recent, None, Vec::new()).1;
            images(&shapes).into_iter().filter(|(_, r, _)| card.contains(r.center())).map(|(t, _, uv)| (t, uv)).collect::<Vec<_>>()
        };
        let early = shown_at(&mut screen, 0.05);
        let strip = screen.strips[&poster].0.clone().expect("the strip is read");
        let frame_of = |shown: &[(egui::TextureId, egui::Rect)]| {
            let uv = shown.iter().find(|(t, _)| *t == strip.sharp.id()).expect("a strip frame is shown").1;
            (uv.min.x * SKIM_FRAMES as f32).round() as usize
        };
        assert_eq!(frame_of(&early), 0);
        assert_eq!(frame_of(&shown_at(&mut screen, 0.5)), 6);
        assert_eq!(frame_of(&shown_at(&mut screen, 0.97)), SKIM_FRAMES - 1);
        let resting = shown_at(&mut screen, -2.0);
        assert!(resting.iter().all(|(t, _)| *t != strip.sharp.id()), "off the card, the poster");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_sidebar_line_reads_day_length_and_rate() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap().and_hms_opt(18, 0, 0).unwrap();
        assert_eq!(when_saved("2026-10-13 13:26:00", now).unwrap(), "Today 13:26");
        assert_eq!(when_saved("2026-10-12 09:05:00", now).unwrap(), "Yesterday 09:05");
        assert_eq!(when_saved("2026-10-08 09:05:00", now).unwrap(), "Thu 09:05");
        assert_eq!(when_saved("2026-03-02 09:05:00", now).unwrap(), "2 Mar");
        assert_eq!(when_saved("2025-03-02 09:05:00", now).unwrap(), "2 Mar 2025");
        assert_eq!(format_length(115.4), "1:55");
        assert_eq!(format_length(3723.0), "1:02:03");
        assert_eq!(fps_label("24fps"), "24 fps");
        assert_eq!(fps_label("29.970fps"), "29.97 fps");
        let mut rp = RecentProject::from_path(PathBuf::from("/p/A.klipp"));
        rp.last_saved = Some("2026-10-13 13:26:00".into());
        rp.duration_secs = Some(115.0);
        rp.fps = Some("24fps".into());
        assert_eq!(sidebar_detail(&rp, now), "Today 13:26 \u{00B7} 1:55 \u{00B7} 24 fps");
    }

    // ── #1854: names, eight recents, templates, the footer ──

    /// A project goes by its file's name on its card and in the sidebar,
    /// whatever name is stored inside it, and Rename starts from that.
    #[test]
    fn a_project_shows_and_renames_by_its_file_name() {
        let dir = temp_dir();
        let mut rp = project(&dir, "Two Shores");
        rp.name = "Main Sequence".into();
        let recent = vec![rp];
        let path = recent[0].path.clone();
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        let all = texts(&shapes);
        assert!(!all.iter().any(|(t, _)| t == "Main Sequence"), "the stored name is not shown");
        let card = screen.seen.cards[0].1;
        let row = screen.seen.sidebar[0].1;
        assert!(all.iter().any(|(t, r)| t == "Two Shores" && card.contains(r.center())), "on the card");
        assert!(all.iter().any(|(t, r)| t == "Two Shores" && row.contains(r.center())), "in the sidebar");
        click_with(&ctx, &mut screen, &recent, None, card.center(), egui::PointerButton::Secondary);
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        click(&ctx, &mut screen, &recent, at(&shapes, "Rename\u{2026}"));
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        frame(&ctx, &mut screen, &recent, None, vec![egui::Event::Text(" 2".into())]);
        let shapes = frame(&ctx, &mut screen, &recent, None, Vec::new()).1;
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Rename"));
        assert!(matches!(&cmds[..], [AppCommand::RenameProjectOnDisk { old_path, new_name }] if *old_path == path && new_name == "Two Shores 2"), "{cmds:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Recent holds up to eight, pinned first; with fewer, the templates
    /// below move up.
    #[test]
    fn eight_recents_show_pinned_first() {
        let dir = temp_dir();
        let mut recent: Vec<_> = (0..10).map(|i| project(&dir, &format!("P{i}"))).collect();
        recent[9].pinned = true;
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, many) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        let rows: Vec<_> = screen.seen.sidebar.iter().map(|(p, _, _)| p.file_stem().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(rows, ["P9", "P0", "P1", "P2", "P3", "P4", "P5", "P6"]);
        let few = &recent[..2];
        let mut screen = screen_with(&[]);
        frame(&ctx, &mut screen, few, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, few, None, Vec::new());
        assert_eq!(screen.seen.sidebar.len(), 2);
        let heading = |s: &Shapes| at(s, "START FROM A TEMPLATE").y;
        assert!(heading(&shapes) < heading(&many) - 5.0 * RECENT_ROW_H, "{} vs {}", heading(&shapes), heading(&many));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// #1864: a template entry opens an Untitled project in one click and
    /// writes nothing: the command names no place, and the projects folder
    /// is as it was. A built-in format is made at its rate, a format with
    /// several has a menu of them, and the rate chosen is where the next
    /// one starts. The user's own come first; More… opens the chooser.
    #[test]
    fn a_template_entry_opens_an_untitled_project_and_writes_nothing() {
        use klipp_project::template::{ProjectSetup, ProjectTemplate, TemplateColour};
        let dir = temp_dir();
        let folder = dir.join("Videos");
        std::fs::create_dir_all(&folder).unwrap();
        let templates = dir.join("templates");
        let colour = TemplateColour { working_colorspace: Default::default(), grade_encoding: Default::default(), global_lut_path: None, global_lut_opacity: 1.0, global_lut_enabled: false };
        ProjectTemplate::capture("Weekly Series", "", &ProjectSetup { sequences: &[klipp_timeline::Sequence::new_default()], pool: &klipp_media::MediaPool::new(), colour }).save(&templates).unwrap();
        let recent = vec![project(&dir, "Trip")];
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        screen.templates_dir = Some(templates.clone());
        let run = |screen: &mut LaunchScreen, events| frame(&ctx, screen, &recent, Some(&folder), events).1;
        run(&mut screen, Vec::new());
        let shapes = run(&mut screen, Vec::new());
        assert!(at(&shapes, "Weekly Series").y < at(&shapes, "Match first clip").y, "the user's own first");
        let press = |screen: &mut LaunchScreen, at: egui::Pos2| click_with(&ctx, screen, &recent, Some(&folder), at, egui::PointerButton::Primary);

        let cmds = press(&mut screen, at(&shapes, "Vertical (social)"));
        assert!(matches!(&cmds[..], [AppCommand::NewUntitledProject { width: 1080, height: 1920, fps_num: 30, fps_den: 1, template: None, hdr: false }]), "{cmds:?}");
        let cmds = press(&mut screen, at(&shapes, "HDR UHD (HLG / PQ)"));
        assert!(matches!(&cmds[..], [AppCommand::NewUntitledProject { width: 3840, height: 2160, fps_num: 25, fps_den: 1, hdr: true, .. }]), "{cmds:?}");
        let cmds = press(&mut screen, at(&shapes, "Weekly Series"));
        assert!(matches!(&cmds[..], [AppCommand::NewUntitledProject { template: Some(p), .. }] if p.parent() == Some(templates.as_path())), "{cmds:?}");
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0, "nothing is written");

        // HD's rate menu: 23.976 chosen, HD is made at it, and UHD, which
        // offers it, starts there too.
        let hd_row = at(&shapes, "HD 1080p").y;
        let rate = texts(&shapes).into_iter().find(|(t, r)| t == "25 \u{25BE}" && (r.center().y - hd_row).abs() < 6.0).expect("HD's rate menu").1.center();
        assert!(press(&mut screen, rate).is_empty(), "the menu opens, nothing is made");
        let shapes = run(&mut screen, Vec::new());
        for r in ["25 fps", "29.97 fps", "23.976 fps"] {
            at(&shapes, r);
        }
        let cmds = press(&mut screen, at(&shapes, "23.976 fps"));
        assert!(matches!(&cmds[..], [AppCommand::SetFormatRate { rate: (24000, 1001) }]), "kept in the preferences: {cmds:?}");
        let shapes = run(&mut screen, Vec::new());
        let cmds = press(&mut screen, at(&shapes, "HD 1080p"));
        assert!(matches!(&cmds[..], [AppCommand::NewUntitledProject { width: 1920, height: 1080, fps_num: 24000, fps_den: 1001, .. }]), "{cmds:?}");
        let cmds = press(&mut screen, at(&shapes, "UHD 4K"));
        assert!(matches!(&cmds[..], [AppCommand::NewUntitledProject { width: 3840, fps_num: 24000, fps_den: 1001, .. }]), "{cmds:?}");
        let cmds = press(&mut screen, at(&shapes, "Vertical (social)"));
        assert!(matches!(&cmds[..], [AppCommand::NewUntitledProject { fps_num: 30, .. }]), "a format without it keeps its own: {cmds:?}");

        let cmds = press(&mut screen, at(&shapes, "More\u{2026}"));
        assert!(matches!(&cmds[..], [AppCommand::NewProject]), "{cmds:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The footer: room on the projects drive and the render cache's
    /// size, Manage… for the cache preferences, and Help, Keyboard
    /// Shortcuts and What's New, each opening what it names.
    #[test]
    fn the_footer_shows_storage_and_opens_its_links() {
        klipp_core::build_info::set_changes("Cards fit their content\nThe sidebar shows posters\n");
        let recent = vec![RecentProject::from_path(PathBuf::from("/nowhere/Trip.klipp"))];
        let ctx = egui::Context::default();
        let mut screen = screen_with(&[]);
        screen.storage = Some(Storage { drive: PathBuf::from("/media/work/Projects"), free_bytes: Some(412 << 30), render_cache_bytes: 3 << 30 });
        screen.storage_checked_at = Some(std::time::Instant::now());
        frame(&ctx, &mut screen, &recent, None, Vec::new());
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        at(&shapes, "412.0 GB free on /media/work/Projects");
        at(&shapes, "Render cache 3.0 GB");
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Manage\u{2026}"));
        assert!(matches!(&cmds[..], [AppCommand::OpenPreferences { page: Some(p) }] if p == "cache"), "{cmds:?}");
        let cmds = click(&ctx, &mut screen, &recent, at(&shapes, "Keyboard Shortcuts"));
        assert!(matches!(&cmds[..], [AppCommand::OpenPreferences { page: Some(p) }] if p == "keyboard"), "{cmds:?}");
        click(&ctx, &mut screen, &recent, at(&shapes, "Help"));
        assert_eq!(screen.opened, [HELP_URL]);
        click(&ctx, &mut screen, &recent, at(&shapes, "What's New"));
        let (_, shapes) = frame(&ctx, &mut screen, &recent, None, Vec::new());
        at(&shapes, "\u{2022} Cards fit their content");
        at(&shapes, "\u{2022} The sidebar shows posters");
    }
}
