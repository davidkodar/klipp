//! The multicam angle grid (#84): every angle of the group in play, as a
//! picture, in the Source viewer's place. Cutting a multicam is watching
//! the angles and choosing; a row of labelled buttons made it guessing.
//!
//! Tiles are decoded, not rendered: each angle's skim transcode (else its
//! proxy, else the original) at tile size, by a worker with decoders of
//! its own, so neither playback nor the timeline's skim is disturbed. A
//! tile carries no grade. While playing the tiles refresh at about ten
//! frames a second, and the angle being cut to plays in the Program
//! viewer at full rate; paused, each tile shows the frame at the playhead.
//! When the tiles can't keep that up the grid says at what rate they
//! refresh, rather than look stuck.
//!
//! The grid is composed on the GPU: each tile is a texture of its own,
//! updated in place as its frame arrives, and drawn as a quad scaled into
//! its cell; nothing tiles or copies pictures on the CPU.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use klipp_core::{ClipId, GroupId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::{Sequence, TrackKind};

/// The multicam group the viewer is about at `playhead`: the selected
/// clip's, else the one whose primary is under the playhead.
pub fn group_at(seq: &Sequence, selected: Option<ClipId>, playhead: Timecode) -> Option<GroupId> {
    let group = selected.and_then(|id| seq.all_clips().find(|c| c.id == id)).and_then(|c| c.multicam_group);
    seq.multicam_at(group, playhead).or_else(|| group.and(seq.multicam_at(None, playhead))).map(|m| m.group)
}

/// That group's angles (the ones it has, not 0..n) and the angle in
/// effect, from [`Sequence::multicam_at`]. `None` without a group of two
/// angles or more.
pub fn view_at(seq: &Sequence, selected: Option<ClipId>, playhead: Timecode) -> Option<(Vec<u8>, u8)> {
    let m = seq.multicam_at(Some(group_at(seq, selected, playhead)?), playhead)?;
    (m.angles.len() >= 2).then_some((m.angles, m.active))
}

/// What multicam mode shows (#1844): with the mode off, nothing, whatever
/// is under the playhead; on, the angles of the multicam clip under the
/// playhead, else of the one opened. `None` when the mode is on with no
/// multicam clip to show, for the Source viewer's note.
pub fn mode_view(seq: &Sequence, mode: bool, opened: Option<ClipId>, playhead: Timecode) -> Option<(Vec<u8>, u8)> {
    if !mode {
        return None;
    }
    view_at(seq, opened, playhead)
}

/// What the multicam mode key (`toggle_multicam_mode`, Shift+Alt+M by
/// default) sends: the mode the other way.
pub fn switch_mode(on: bool) -> klipp_core::AppCommand {
    klipp_core::AppCommand::SetMulticamMode { on: !on, open: None }
}

/// The Source viewer's Angles button, at the top right of `rect`, in the
/// viewer controls' toggle style (accent on the subtle accent fill while
/// on). The command that switches the mode, when clicked.
pub fn angles_button(ui: &mut egui::Ui, rect: egui::Rect, on: bool, theme: &crate::theme::KlippTheme) -> Option<klipp_core::AppCommand> {
    let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
    let text_w = ui.painter().layout_no_wrap("Angles".into(), font.clone(), theme.text_primary).size().x;
    let size = egui::vec2(text_w + 16.0, crate::theme::control::COMPACT);
    let r = egui::Rect::from_min_size(egui::pos2(rect.right() - 8.0 - size.x, rect.top() + 8.0), size);
    let resp = ui.interact(r, ui.id().with("multicam_angles_button"), egui::Sense::click());
    let colour = if on { theme.accent } else if resp.hovered() { theme.text_primary } else { theme.text_secondary };
    let fill = if on { theme.accent_subtle } else { theme.bg_panel.gamma_multiply(0.85) };
    ui.painter().rect_filled(r, crate::theme::radius::CONTROL, fill);
    ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, "Angles", font, colour);
    let resp = resp.on_hover_text(if on { "Multicam mode is on: the Source viewer shows the angles (Shift+Alt+M)" } else { "Show the multicam angles in the Source viewer (Shift+Alt+M)" });
    resp.clicked().then_some(klipp_core::AppCommand::SetMulticamMode { on: !on, open: None })
}

/// The Source viewer in multicam mode with no multicam clip to show: a
/// short note in the middle of `rect`.
pub fn no_multicam_note(ui: &egui::Ui, rect: egui::Rect, theme: &crate::theme::KlippTheme) {
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "No multicam clip at the playhead",
        egui::FontId::proportional(crate::theme::type_scale::BODY),
        theme.text_secondary,
    );
}

/// One angle of the group: its number (0 the primary), a name to label
/// it with, and what to decode for it at the playhead, when it has
/// picture there.
#[derive(Debug, Clone, PartialEq)]
pub struct AngleSource {
    pub angle: u8,
    pub name: String,
    pub decode: Option<Decode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decode {
    pub path: PathBuf,
    /// The original, when `path` is its skim transcode or proxy.
    pub fallback: Option<PathBuf>,
    pub rotation: i32,
    pub secs: f64,
}

/// Every angle of `group`, in angle order (not track order: a group made
/// by sound numbers its angles by file), with the picture clip of each
/// at `playhead`.
pub fn angle_sources(seq: &Sequence, pool: &MediaPool, group: GroupId, playhead: Timecode) -> Vec<AngleSource> {
    let fps = seq.fps.as_f64();
    let pictures: Vec<&klipp_timeline::Clip> = seq
        .tracks
        .iter()
        .filter(|t| t.kind != TrackKind::Audio)
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.multicam_group == Some(group) && c.enabled)
        .collect();
    let angles: BTreeSet<u8> = pictures.iter().map(|c| c.multicam_angle).collect();
    angles
        .into_iter()
        .map(|angle| {
            let of_angle = || pictures.iter().filter(move |c| c.multicam_angle == angle);
            let under = of_angle().find(|c| playhead >= c.timeline_in && playhead < c.timeline_out);
            let any = under.or_else(|| of_angle().next());
            let item = any.and_then(|c| c.media_id()).and_then(|id| pool.get(&id));
            let name = item.map(|i| i.filename.clone()).unwrap_or_else(|| format!("Angle {}", angle as usize + 1));
            let decode = under.and_then(|c| {
                let item = pool.get(&c.media_id()?)?;
                let secs = c.source_time_for_sequence_time(playhead, fps)?.to_secs_f64();
                // No file checks here, on the UI thread, for every angle of
                // every batch (#84: they could stall on network footage):
                // a skim file or proxy that is gone fails to open in the
                // worker, which then decodes the original.
                let (path, fallback) = if let Some(skim) = item.skim_file.as_ref() {
                    (skim.clone(), Some(item.path.clone()))
                } else if let Some(proxy) = item.proxy_path.as_ref().filter(|_| item.proxy_ready) {
                    (proxy.clone(), Some(item.path.clone()))
                } else {
                    (item.path.clone(), None)
                };
                let rotation = item.effective_rotation_for(&path);
                Some(Decode { path, fallback, rotation, secs })
            });
            AngleSource { angle, name, decode }
        })
        .collect()
}

/// Rows and columns for `n` tiles: square-ish, wider than tall.
pub fn grid_shape(n: usize) -> (usize, usize) {
    let cols = (n as f64).sqrt().ceil().max(1.0) as usize;
    let rows = n.div_ceil(cols).max(1);
    (rows, cols)
}

/// A decoded tile frame, tagged with the batch that asked for it.
type Delivered = (u64, u8, Result<klipp_engine::skim::SkimFrame, String>);

/// Decoders the grid starts with; a group with more angles gets one each.
const MIN_DECODERS: usize = 10;
/// And no more than this: past it, decoders are evicted and reopened.
const MAX_DECODERS: usize = 25;

/// How many decoders a grid of `angles` keeps open: one per angle, so
/// eleven angles or more do not evict each other's decoder every batch
/// (#84), never fewer than ten nor more than 25.
pub fn decoders_for(angles: usize) -> usize {
    angles.clamp(MIN_DECODERS, MAX_DECODERS)
}

/// The grid's decoder, its pacing and its tiles.
pub struct GridRuntime {
    worker: klipp_engine::skim::SkimBatchWorker<(u64, u8)>,
    /// How many decoders `worker` has room for.
    decoders: usize,
    /// For a new worker when a group needs more decoders.
    ctx: egui::Context,
    tx: crossbeam_channel::Sender<Delivered>,
    rx: crossbeam_channel::Receiver<Delivered>,
    generation: u64,
    asked: usize,
    answered: usize,
    asked_at: Instant,
    last_key: Option<(GroupId, i64, u32)>,
    playing: bool,
    pub sources: Vec<AngleSource>,
    pub tiles: HashMap<u8, egui::TextureHandle>,
    /// The batch each tile's frame came from: paused, a tile from an
    /// older one is still on its way to the playhead, and is drawn dimmed
    /// rather than passed off as the frame there.
    tile_generation: HashMap<u8, u64>,
    /// Tiles of a group no longer shown, for the caller to let go.
    retired: Vec<egui::TextureHandle>,
    /// Why an angle's frame could not be made, last time it was asked
    /// for: shown on its tile rather than leaving it blank (#84).
    pub failed: HashMap<u8, String>,
    /// When each recent batch was fully answered while playing: the rate
    /// the tiles refresh at.
    completed: std::collections::VecDeque<Instant>,
}

/// A batch worker with room for `decoders`, delivering into `tx`.
fn spawn_worker(ctx: &egui::Context, tx: &crossbeam_channel::Sender<Delivered>, decoders: usize) -> klipp_engine::skim::SkimBatchWorker<(u64, u8)> {
    let (tx, repaint) = (tx.clone(), ctx.clone());
    klipp_engine::skim::SkimBatchWorker::spawn(decoders, move |job, result| {
        let (generation, angle) = job.tag;
        let _ = tx.send((generation, angle, result));
        repaint.request_repaint();
    })
}

/// The window the tiles' refresh rate is measured over.
const REFRESH_WINDOW: Duration = Duration::from_secs(2);
/// While playing, a new batch at most this often.
const PLAYING_INTERVAL: Duration = Duration::from_millis(100);
/// A batch not fully answered by then is given up on.
const BATCH_TIMEOUT: Duration = Duration::from_millis(600);
/// Tiles are decoded no taller than this: a 3x3 grid in a half-width
/// viewer is a few hundred pixels a tile even on a 4K screen.
const TILE_MAX_HEIGHT: u32 = 540;

impl GridRuntime {
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Delivered>();
        Self {
            worker: spawn_worker(ctx, &tx, MIN_DECODERS),
            decoders: MIN_DECODERS,
            ctx: ctx.clone(),
            tx,
            rx,
            generation: 0,
            asked: 0,
            answered: 0,
            asked_at: Instant::now(),
            last_key: None,
            playing: false,
            sources: Vec::new(),
            tiles: HashMap::new(),
            tile_generation: HashMap::new(),
            retired: Vec::new(),
            failed: HashMap::new(),
            completed: std::collections::VecDeque::new(),
        }
    }

    /// The batches fully answered a second while playing, over the last two
    /// seconds; `None` paused, or before a second of play.
    pub fn refresh_rate(&self) -> Option<f32> {
        if !self.playing {
            return None;
        }
        let now = Instant::now();
        let recent = self.completed.iter().filter(|t| now.duration_since(**t) <= REFRESH_WINDOW).count();
        let first = self.completed.front()?;
        (now.duration_since(*first) >= Duration::from_secs(1) || recent > 0).then(|| recent as f32 / REFRESH_WINDOW.as_secs_f32())
    }

    /// Whether the tiles refresh slower than they are asked to while
    /// playing, by a clear margin: what the grid says on screen.
    pub fn falling_behind(&self) -> Option<f32> {
        let rate = self.refresh_rate()?;
        let asked = 1.0 / PLAYING_INTERVAL.as_secs_f32();
        (self.completed.len() >= 2 && rate < asked * 0.7).then_some(rate)
    }

    /// Ask for the tiles of `group` at `playhead`: once per position when
    /// paused, and while playing every tenth of a second once the last
    /// batch is in (or given up on).
    pub fn request(&mut self, seq: &Sequence, pool: &MediaPool, group: GroupId, playhead: Timecode, playing: bool) {
        let key = (group, playhead.value, playhead.rate);
        // Another group: its cameras are not these. Keyed by angle alone,
        // the old group's pictures showed under the new group's labels.
        if self.last_key.is_some_and(|(g, _, _)| g != group) {
            self.retired.extend(self.tiles.drain().map(|(_, t)| t));
            self.tile_generation.clear();
            self.failed.clear();
        }
        self.playing = playing;
        let due = if playing {
            let done = self.answered >= self.asked || self.asked_at.elapsed() >= BATCH_TIMEOUT;
            done && self.asked_at.elapsed() >= PLAYING_INTERVAL
        } else {
            self.last_key != Some(key)
        };
        if !due {
            return;
        }
        self.last_key = Some(key);
        self.sources = angle_sources(seq, pool, group, playhead);
        // More angles than decoders: a worker with one each. The old one
        // ends when it is dropped.
        let wanted = decoders_for(self.sources.len());
        if wanted > self.decoders {
            self.worker = spawn_worker(&self.ctx, &self.tx, wanted);
            self.decoders = wanted;
        }
        self.generation += 1;
        let batch: Vec<_> = self
            .sources
            .iter()
            .filter_map(|s| {
                let d = s.decode.as_ref()?;
                Some(klipp_engine::skim::SkimJob {
                    path: d.path.clone(),
                    fallback_path: d.fallback.clone(),
                    rotation: d.rotation,
                    secs: d.secs,
                    max_height: TILE_MAX_HEIGHT,
                    tag: (self.generation, s.angle),
                })
            })
            .collect();
        self.asked = batch.len();
        self.answered = 0;
        self.asked_at = Instant::now();
        self.worker.request(batch);
    }

    /// Upload what has arrived. A tile keeps its texture and has it
    /// replaced in place; a new size gets a new one, the old handed to
    /// `drop_old` (a texture freed mid-frame trips wgpu).
    pub fn drain(&mut self, ctx: &egui::Context, mut drop_old: impl FnMut(egui::TextureHandle)) {
        while let Ok((generation, angle, result)) = self.rx.try_recv() {
            let current = generation == self.generation;
            if current {
                self.answered += 1;
                if self.answered == self.asked && self.playing {
                    let now = Instant::now();
                    self.completed.push_back(now);
                    while self.completed.front().is_some_and(|t| now.duration_since(*t) > REFRESH_WINDOW) {
                        self.completed.pop_front();
                    }
                }
            }
            let frame = match result {
                Ok(frame) => frame,
                Err(e) => {
                    if current {
                        self.failed.insert(angle, e);
                    }
                    continue;
                }
            };
            // Only this batch's frame answers this batch's failure: a late
            // success from an older one must not clear it (#84 review).
            if current {
                self.failed.remove(&angle);
            }
            // A frame for a group no longer shown (its batch was asked
            // for before the switch) is not this group's.
            if !self.sources.iter().any(|s| s.angle == angle) {
                continue;
            }
            self.tile_generation.insert(angle, generation);
            let image = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
            match self.tiles.get_mut(&angle) {
                Some(tex) if tex.size() == image.size => tex.set(image, egui::TextureOptions::LINEAR),
                _ => {
                    let tex = ctx.load_texture(format!("multicam_tile_{angle}"), image, egui::TextureOptions::LINEAR);
                    if let Some(old) = self.tiles.insert(angle, tex) {
                        drop_old(old);
                    }
                }
            }
        }
    }

    /// Tiles let go of since the last call, for the caller to drop after
    /// the frame (a texture freed mid-frame trips wgpu).
    pub fn take_retired(&mut self) -> Vec<egui::TextureHandle> {
        std::mem::take(&mut self.retired)
    }

    /// Whether `angle`'s tile is from an older batch than the one asked
    /// for last, while paused.
    pub fn is_stale(&self, angle: u8) -> bool {
        !self.playing && self.tile_generation.get(&angle).is_some_and(|g| *g < self.generation)
    }

    /// Let the tiles go when the grid is not shown.
    pub fn clear(&mut self, mut drop_old: impl FnMut(egui::TextureHandle)) {
        for (_, tex) in self.tiles.drain() {
            drop_old(tex);
        }
        for tex in self.retired.drain(..) {
            drop_old(tex);
        }
        self.tile_generation.clear();
        self.failed.clear();
        self.sources.clear();
        self.last_key = None;
        self.completed.clear();
    }
}

/// Paint the grid into `rect` and answer which angle was clicked, if any
/// (0-based). `active` is the angle the cut list has in effect.
pub fn show(ui: &mut egui::Ui, rect: egui::Rect, runtime: &GridRuntime, active: u8, theme: &crate::theme::KlippTheme) -> Option<u8> {
    ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
    let sources = &runtime.sources;
    if sources.is_empty() {
        return None;
    }
    let (rows, cols) = grid_shape(sources.len());
    let gap = 4.0;
    let cell = egui::vec2(
        ((rect.width() - gap * (cols as f32 + 1.0)) / cols as f32).max(1.0),
        ((rect.height() - gap * (rows as f32 + 1.0)) / rows as f32).max(1.0),
    );
    let mut clicked = None;
    for (i, source) in sources.iter().enumerate() {
        let (r, c) = (i / cols, i % cols);
        let cell_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + gap + c as f32 * (cell.x + gap), rect.top() + gap + r as f32 * (cell.y + gap)),
            cell,
        );
        let resp = ui.interact(cell_rect, ui.id().with(("multicam_tile", source.angle)), egui::Sense::click());
        let painter = ui.painter_at(cell_rect);
        painter.rect_filled(cell_rect, 2.0, theme.bg_panel);
        match (runtime.tiles.get(&source.angle), &source.decode) {
            (Some(tex), Some(_)) => {
                let size = tex.size_vec2();
                let scale = (cell_rect.width() / size.x).min(cell_rect.height() / size.y);
                let img = egui::Rect::from_center_size(cell_rect.center(), size * scale);
                // Dimmed while still on its way to the playhead's frame.
                let tint = if runtime.is_stale(source.angle) { egui::Color32::from_gray(110) } else { egui::Color32::WHITE };
                painter.image(tex.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
            }
            (_, None) => {
                painter.text(cell_rect.center(), egui::Align2::CENTER_CENTER, "No picture here", egui::FontId::proportional(12.0), theme.text_dim);
            }
            (None, Some(_)) => {
                // Nothing could make this angle's frame: say so, and why
                // on hover, rather than leave the tile blank (#84).
                if let Some(why) = runtime.failed.get(&source.angle) {
                    painter.text(cell_rect.center(), egui::Align2::CENTER_CENTER, "Can't decode this angle", egui::FontId::proportional(12.0), theme.text_dim);
                    resp.clone().on_hover_text(why.as_str());
                }
            }
        }
        // The angle's number, as keys 1-9 cut to it, and its name.
        // The angle being cut to reads at a glance: its label bar in the
        // accent colour and a frame round it, over the picture.
        let is_active = source.angle == active;
        let label = format!("{}  {}{}", source.angle as usize + 1, source.name, if is_active { "  ● LIVE" } else { "" });
        let label_rect = egui::Rect::from_min_size(cell_rect.min, egui::vec2(cell_rect.width(), 22.0));
        let (bar, text) = if is_active { (theme.accent, theme.text_on_accent) } else { (egui::Color32::from_black_alpha(160), egui::Color32::WHITE) };
        painter.rect_filled(label_rect, 0.0, bar);
        painter.text(label_rect.left_center() + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(12.0), text);
        let stroke = if is_active {
            egui::Stroke::new(4.0_f32, theme.accent)
        } else if resp.hovered() {
            egui::Stroke::new(2.0_f32, egui::Color32::from_white_alpha(180))
        } else {
            egui::Stroke::NONE
        };
        painter.rect_stroke(cell_rect.shrink(2.0), 2.0, stroke);
        if resp.clicked() {
            clicked = Some(source.angle);
        }
    }
    // Degrading honestly: the tiles refresh slower than asked, and the
    // grid says so, while the angle cut to plays at full rate.
    if let Some(rate) = runtime.falling_behind() {
        let text = format!("Angles at {rate:.0} fps · the live angle plays at full rate in the Program viewer");
        let galley = ui.painter().layout_no_wrap(text, egui::FontId::proportional(11.0), egui::Color32::WHITE);
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + 8.0, rect.bottom() - galley.size().y - 10.0), galley.size() + egui::vec2(10.0, 4.0));
        ui.painter().rect_filled(r, 3.0, egui::Color32::from_black_alpha(190));
        ui.painter().galley(r.min + egui::vec2(5.0, 2.0), galley, egui::Color32::WHITE);
    }
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track};

    fn tc(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn angle(group: GroupId, n: u8, from: f64, to: f64) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(from), tc(to), tc(0.0), tc(to - from));
        c.multicam_group = Some(group);
        c.multicam_angle = n;
        c
    }

    fn synced(clips: Vec<(Clip, TrackKind)>) -> Sequence {
        let mut seq = Sequence::new_default();
        seq.tracks = clips
            .into_iter()
            .enumerate()
            .map(|(i, (c, k))| {
                let mut t = Track::new(&format!("T{i}"), k);
                t.clips = vec![c];
                t
            })
            .collect();
        seq
    }

    /// As the creation paths make a group: picture clips grouped, sound
    /// not. With nothing selected the group under the playhead is shown;
    /// its angles are the ones it has, not 0..n.
    #[test]
    fn the_group_under_the_playhead_without_a_selection() {
        let g = GroupId::new();
        let mut primary = angle(g, 0, 0.0, 10.0);
        primary.multicam_cuts = vec![(4.0, 2)];
        let mut sound = angle(g, 0, 0.0, 10.0);
        sound.multicam_group = None;
        let seq = synced(vec![(primary, TrackKind::Video), (sound, TrackKind::Audio), (angle(g, 2, 0.0, 10.0), TrackKind::Video)]);
        assert_eq!(view_at(&seq, None, tc(1.0)), Some((vec![0, 2], 0)));
        assert_eq!(view_at(&seq, None, tc(5.0)), Some((vec![0, 2], 2)));
        assert_eq!(view_at(&seq, None, tc(12.0)), None, "past the group");
        // A clip outside the group selected: the group under the playhead.
        let mut lone = angle(GroupId::new(), 0, 0.0, 10.0);
        lone.multicam_group = None;
        let lone_id = lone.id;
        let mut with_lone = seq.clone();
        let mut t = Track::new("Other", TrackKind::Video);
        t.clips = vec![lone];
        with_lone.tracks.push(t);
        assert_eq!(view_at(&with_lone, Some(lone_id), tc(5.0)), Some((vec![0, 2], 2)));
    }

    /// Angles in angle order whatever the tracks, the picture clip of
    /// each (never its sound), what to decode for it at the playhead (its
    /// own source time, from its own start), and no picture for an angle
    /// that has not started.
    #[test]
    fn angle_sources_come_in_angle_order_with_their_picture() {
        let g = GroupId::new();
        let mut pool = MediaPool::default();
        let mut media = |name: &str| pool.add(klipp_media::MediaItem::synthetic_for_test(PathBuf::from(format!("/nonexistent/{name}")), None));
        let (m0, m1, m2) = (media("a.mov"), media("b.mov"), media("c.mov"));
        let with = |mut c: Clip, m| { c.kind = klipp_timeline::ClipKind::Media { media_id: m }; c };
        let seq = synced(vec![
            (with(angle(g, 2, 3.0, 10.0), m2), TrackKind::Video),
            (with(angle(g, 0, 0.0, 10.0), m0), TrackKind::Video),
            (with(angle(g, 0, 0.0, 10.0), m0), TrackKind::Audio),
            (with(angle(g, 1, 1.0, 10.0), m1), TrackKind::Video),
        ]);
        let sources = angle_sources(&seq, &pool, g, tc(2.0));
        assert_eq!(sources.iter().map(|s| s.angle).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert_eq!(sources.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), vec!["a.mov", "b.mov", "c.mov"]);
        let d0 = sources[0].decode.as_ref().expect("angle 0 is under the playhead");
        assert_eq!(d0.path, PathBuf::from("/nonexistent/a.mov"));
        assert!(d0.fallback.is_none(), "no skim file or proxy: the original, nothing to fall back to");
        assert!((d0.secs - 2.0).abs() < 1e-6);
        let d1 = sources[1].decode.as_ref().expect("angle 1 started at 1 s");
        assert!((d1.secs - 1.0).abs() < 1e-6, "its own source time: {}", d1.secs);
        assert!(sources[2].decode.is_none(), "angle 2 starts at 3 s");
    }

    /// #84: a skim file or proxy is chosen without asking the disk (the
    /// worker falls back to the original when it is gone), and a group of
    /// more than ten angles keeps a decoder for each.
    #[test]
    fn sources_are_chosen_without_file_checks_and_decoders_fit_the_group() {
        let g = GroupId::new();
        let mut pool = MediaPool::default();
        let mut item = klipp_media::MediaItem::synthetic_for_test(PathBuf::from("/nonexistent/a.mov"), None);
        item.skim_file = Some(PathBuf::from("/nonexistent/a.skim.mkv"));
        let m = pool.add(item);
        let mut c = angle(g, 0, 0.0, 10.0);
        c.kind = klipp_timeline::ClipKind::Media { media_id: m };
        let mut other = angle(g, 1, 0.0, 10.0);
        other.kind = klipp_timeline::ClipKind::Media { media_id: m };
        let seq = synced(vec![(c, TrackKind::Video), (other, TrackKind::Video)]);
        let d = angle_sources(&seq, &pool, g, tc(1.0))[0].decode.clone().expect("a picture");
        assert_eq!(d.path, PathBuf::from("/nonexistent/a.skim.mkv"), "the skim file, though it is not on disk");
        assert_eq!(d.fallback, Some(PathBuf::from("/nonexistent/a.mov")), "the original behind it");
        assert_eq!((decoders_for(2), decoders_for(10), decoders_for(16), decoders_for(40)), (10, 10, 16, 25));
    }

    #[test]
    fn the_grid_is_square_ish() {
        assert_eq!(grid_shape(2), (1, 2));
        assert_eq!(grid_shape(3), (2, 2));
        assert_eq!(grid_shape(4), (2, 2));
        assert_eq!(grid_shape(5), (2, 3));
        assert_eq!(grid_shape(9), (3, 3));
    }

    /// A tile's frame, as the worker delivers one.
    fn frame() -> klipp_engine::skim::SkimFrame {
        klipp_engine::skim::SkimFrame { width: 2, height: 2, rgba: vec![255; 16], reused_decoder: false, repeated: false }
    }

    /// #84 review: a failure is cleared only by the batch it belongs to;
    /// a late success from an older batch leaves the current one's.
    #[test]
    fn a_late_success_does_not_clear_this_batchs_failure() {
        let ctx = egui::Context::default();
        let mut grid = GridRuntime::new(&ctx);
        grid.sources = vec![AngleSource { angle: 1, name: "b".into(), decode: None }];
        grid.generation = 2;
        grid.asked = 1;
        grid.tx.send((2, 1, Err("no such file".into()))).unwrap();
        grid.drain(&ctx, drop);
        assert_eq!(grid.failed.get(&1).map(String::as_str), Some("no such file"));
        grid.tx.send((1, 1, Ok(frame()))).unwrap();
        grid.drain(&ctx, drop);
        assert!(grid.failed.contains_key(&1), "the older batch's frame left it");
        assert!(grid.is_stale(1), "and its frame is drawn as on its way");
        // This batch's own success clears it.
        grid.generation = 3;
        grid.tx.send((3, 1, Ok(frame()))).unwrap();
        grid.drain(&ctx, drop);
        assert!(!grid.failed.contains_key(&1));
    }

    /// #84: while playing, the grid measures how often whole batches come
    /// back; slower than asked by a clear margin, it says so. Paused, it
    /// says nothing.
    #[test]
    fn the_grid_says_when_its_tiles_fall_behind() {
        let ctx = egui::Context::default();
        let mut grid = GridRuntime::new(&ctx);
        grid.sources = vec![AngleSource { angle: 0, name: "a".into(), decode: None }];
        grid.playing = true;
        // Two batches answered, over a second and a half: about 1 fps.
        let now = Instant::now();
        grid.completed = [now - Duration::from_millis(1500), now - Duration::from_millis(500)].into_iter().collect();
        let rate = grid.falling_behind().expect("behind");
        assert!((rate - 1.0).abs() < 0.01, "{rate}");
        // Answering every tenth of a second: not behind.
        grid.completed = (0..20).map(|k| now - Duration::from_millis(100 * k)).rev().collect();
        assert_eq!(grid.falling_behind(), None);
        // A batch answered while playing is counted.
        grid.completed.clear();
        grid.generation = 5;
        grid.asked = 1;
        grid.answered = 0;
        grid.tx.send((5, 0, Ok(frame()))).unwrap();
        grid.drain(&ctx, drop);
        assert_eq!(grid.completed.len(), 1);
        grid.playing = false;
        assert_eq!(grid.refresh_rate(), None, "paused: nothing to say");
        // Drawn: the line on the grid when behind.
        grid.playing = true;
        grid.completed = [now - Duration::from_millis(1500), now - Duration::from_millis(500)].into_iter().collect();
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                show(ui, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 360.0)), &grid, 0, &crate::theme::KlippTheme::default());
            });
        });
        let said = out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text().starts_with("Angles at 1 fps")));
        assert!(said);
    }

    /// #1844: multicam mode shows nothing while it is off, however the
    /// playhead sits on a multicam clip; on, the group under the playhead,
    /// else the opened clip's when the playhead is on it.
    #[test]
    fn the_playhead_crossing_a_multicam_clip_leaves_the_mode_off() {
        let g = GroupId::new();
        let a = angle(g, 0, 0.0, 10.0);
        let opened = a.id;
        let seq = synced(vec![(a, TrackKind::Video), (angle(g, 1, 0.0, 10.0), TrackKind::Video)]);
        assert_eq!(mode_view(&seq, false, None, tc(5.0)), None, "off: the playhead on a multicam clip shows nothing");
        assert_eq!(mode_view(&seq, true, None, tc(5.0)).map(|(a, _)| a), Some(vec![0, 1]), "on: its angles");
        assert_eq!(mode_view(&seq, true, Some(opened), tc(5.0)).map(|(a, _)| a), Some(vec![0, 1]));
        assert_eq!(mode_view(&seq, true, None, tc(12.0)), None, "on, past the clip: nothing, for the note");
    }

    /// One frame of the Angles button at the top right of an 800×450
    /// viewer, with `events`: its text and what it sent.
    fn button_frame(ctx: &egui::Context, on: bool, events: Vec<egui::Event>) -> (Option<(egui::Pos2, egui::Color32)>, Option<klipp_core::AppCommand>) {
        let theme = crate::theme::KlippTheme::default();
        let mut sent = None;
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 450.0))), events, ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| sent = angles_button(ui, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 450.0)), on, &theme));
        });
        let text = out.shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == "Angles" => Some((t.pos + t.galley.size() / 2.0, t.galley.job.sections[0].format.color)),
            _ => None,
        });
        (text, sent)
    }

    /// #1844: the Angles button, clicked for real, turns the mode on, and
    /// clicked again turns it off; on, it shows in the toggle style's
    /// accent, off it does not.
    #[test]
    fn the_angles_button_switches_the_mode() {
        let theme = crate::theme::KlippTheme::default();
        for on in [false, true] {
            let ctx = egui::Context::default();
            let (Some((at, colour)), None) = button_frame(&ctx, on, Vec::new()) else { panic!("the button is drawn") };
            assert_eq!(colour == theme.accent, on, "the accent only while on");
            assert!(at.x > 700.0 && at.y < 40.0, "top right: {at:?}");
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut sent = button_frame(&ctx, on, vec![egui::Event::PointerMoved(at), press(true)]).1;
            sent = sent.or(button_frame(&ctx, on, vec![press(false)]).1);
            assert!(matches!(sent, Some(klipp_core::AppCommand::SetMulticamMode { on: to, open: None }) if to == !on), "{sent:?}");
        }
    }

    /// #1844: Shift+Alt+M switches the mode either way; another key does
    /// not.
    #[test]
    fn the_shortcut_switches_the_mode() {
        let kb = crate::keybindings::KeyBindings::default();
        let pressed = |key: egui::Key, modifiers: egui::Modifiers, on: bool| {
            let ctx = egui::Context::default();
            let mut sent = None;
            let _ = ctx.run(egui::RawInput { events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }], modifiers, ..Default::default() }, |ctx| {
                // As the app's shortcuts read it.
                sent = ctx.input(|i| kb.is_action_pressed("toggle_multicam_mode", i).then(|| switch_mode(on)));
            });
            sent
        };
        let shift_alt = egui::Modifiers { shift: true, alt: true, ..Default::default() };
        assert!(matches!(pressed(egui::Key::M, shift_alt, false), Some(klipp_core::AppCommand::SetMulticamMode { on: true, .. })));
        assert!(matches!(pressed(egui::Key::M, shift_alt, true), Some(klipp_core::AppCommand::SetMulticamMode { on: false, .. })));
        assert!(pressed(egui::Key::M, egui::Modifiers::NONE, false).is_none(), "M alone is a marker");
    }

    /// #1844: switched on, a single viewer shows the Source viewer, where
    /// the grid is; the opened clip is kept until the mode is off.
    #[test]
    fn turning_it_on_shows_the_source_viewer() {
        let mut state = crate::state::AppState::default();
        let clip = ClipId::new();
        state.viewer_mode = klipp_core::ViewerMode::Program;
        state.set_multicam_mode(true, Some(clip));
        assert!(state.multicam_mode);
        assert_eq!(state.multicam_opened, Some(clip));
        if !state.viewers_side_by_side() && !state.single_viewer_is_program() {
            assert_eq!(state.viewer_mode, klipp_core::ViewerMode::Source);
        }
        state.set_multicam_mode(false, None);
        assert!(!state.multicam_mode && state.multicam_opened.is_none());
    }
}
