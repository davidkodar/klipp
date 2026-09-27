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
                let (path, fallback) = if let Some(skim) = item.skim_file.as_ref().filter(|p| p.exists()) {
                    (skim.clone(), Some(item.path.clone()))
                } else if let Some(proxy) = item.proxy_path.as_ref().filter(|p| item.proxy_ready && p.exists()) {
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

/// The grid's decoder, its pacing and its tiles.
pub struct GridRuntime {
    worker: klipp_engine::skim::SkimBatchWorker<(u64, u8)>,
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
}

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
        let repaint = ctx.clone();
        let worker = klipp_engine::skim::SkimBatchWorker::spawn(10, move |job, result| {
            let (generation, angle) = job.tag;
            let _ = tx.send((generation, angle, result));
            repaint.request_repaint();
        });
        Self {
            worker,
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
        }
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
            if generation == self.generation {
                self.answered += 1;
            }
            let Ok(frame) = result else { continue };
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
        self.sources.clear();
        self.last_key = None;
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
            (None, Some(_)) => {}
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

    #[test]
    fn the_grid_is_square_ish() {
        assert_eq!(grid_shape(2), (1, 2));
        assert_eq!(grid_shape(3), (2, 2));
        assert_eq!(grid_shape(4), (2, 2));
        assert_eq!(grid_shape(5), (2, 3));
        assert_eq!(grid_shape(9), (3, 3));
    }
}
