//! Bounce a track to a media file (#3264), on the app's side: the dialog,
//! the job, and the clip placed when the file is written. See
//! [`klipp_engine::bounce`].
//!
//! The file goes into the project, `<project>/media/bounces`: project
//! media, never cache, so clearing frozen frames or the render cache
//! leaves it. When it is written it is read as any media is, and then, as
//! one undo step: the media added to the pool, a new track above the
//! original holding the clip (the track's opacity copied, the clips' blend
//! mode on the clip), its sound on a new track linked to it, the original
//! hidden and its linked sound silenced. Undo takes all of it back and
//! leaves the file, which the orphan scan finds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use klipp_core::{AppCommand, BounceFormat, MediaId, RationalTime, TrackId};
use klipp_document::history::EditCommand;
use klipp_engine::bounce::{self, BounceRequest, BounceResult};
use klipp_jobs::BackgroundJob;
use klipp_timeline::{Clip, Track, TrackKind};

use super::KlippApp;

/// What the dialog shows of a track, worked out when it opens.
pub(crate) struct BounceDialog {
    pub track_id: TrackId,
    pub name: String,
    pub format: BounceFormat,
    pub sound: bool,
    /// Why it can't be bounced, when it can't.
    pub refusal: Option<String>,
    /// The span in seconds, and its frames.
    pub span_secs: Option<(f64, f64)>,
    pub frames: i64,
    pub size: (u32, u32),
    /// Whether sound is linked to its clips, and why it can't go in when
    /// it can't.
    pub has_sound: bool,
    pub sound_refusal: Option<&'static str>,
}

/// A bounce under way.
struct Running {
    track_id: TrackId,
    name: String,
    span: (RationalTime, RationalTime),
    sound: bool,
    job: Arc<klipp_jobs::ThreadJob>,
    done: mpsc::Receiver<BounceResult>,
}

/// A written file being read, then placed.
struct ToPlace {
    track_id: TrackId,
    name: String,
    span: (RationalTime, RationalTime),
    sound: bool,
}

#[derive(Default)]
pub(crate) struct Bounces {
    pub dialog: Option<BounceDialog>,
    running: Vec<Running>,
    to_place: HashMap<MediaId, ToPlace>,
}

/// `<project>/media/bounces`, made when missing, when `media` and it are
/// real folders inside the project (not links): where a bounce writes.
/// `None` otherwise, a link leading out of the project among them.
fn bounces_inside(project: &std::path::Path) -> Option<PathBuf> {
    let real_dir = |p: &std::path::Path| match std::fs::symlink_metadata(p) {
        Ok(m) => m.file_type().is_dir(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    };
    let media = project.join("media");
    let dir = media.join("bounces");
    if !real_dir(&media) || !real_dir(&dir) {
        return None;
    }
    // Neither is a link, so both are inside the project.
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A size on disk as said in the dialog: "2.1 GB", "340 MB".
fn size_said(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", (bytes as f64 / 1e6).ceil() as u64)
    }
}

/// A track name as a file name: letters, digits, spaces, dashes and
/// underscores kept, anything else a dash.
fn file_name_of(name: &str) -> String {
    let kept: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '-' }).collect();
    let kept = kept.trim().to_string();
    if kept.is_empty() { "Track".into() } else { kept }
}

impl KlippApp {
    /// The folder a bounce writes to, or why there is none.
    fn bounce_folder(&self) -> Result<PathBuf, String> {
        let project = self.state.project_path.clone().filter(|p| !crate::project_paths::is_untitled(Some(p)) && self.read_only_project.as_ref() != Some(p)).ok_or("save the project first: the bounced file is kept in it")?;
        bounces_inside(&project).ok_or_else(|| format!("{} has no media folder of its own to write into", project.display()))
    }

    /// Open the dialog for `track_id`: what it would bounce, or why not.
    pub(super) fn open_bounce_dialog(&mut self, track_id: TrackId) {
        let seq = self.sequence.snapshot();
        let working = klipp_engine::video_decoder::working_colorspace();
        let index = seq.tracks.iter().position(|t| t.id == track_id);
        let name = index.map(|i| seq.tracks[i].name.clone()).unwrap_or_default();
        let span = index.and_then(|i| bounce::span(&seq, i));
        let refusal = match (index, self.bounce_folder()) {
            (None, _) => Some("no such track".to_string()),
            (_, Err(why)) => Some(why),
            (Some(i), Ok(_)) => bounce::refusal(&seq, i, working),
        };
        let frame = |f: i64| f as f64 * f64::from(seq.fps.den) / f64::from(seq.fps.num.max(1));
        let has_sound = klipp_render::stem::linked_sound_snapshot(&seq, &[], &self.state.media_pool, track_id).ok().flatten().is_some();
        let sound_refusal = klipp_render::stem::linked_sound_refusal(&seq, track_id);
        self.bounces.dialog = Some(BounceDialog {
            track_id,
            name,
            format: BounceFormat::default(),
            sound: has_sound && sound_refusal.is_none(),
            refusal,
            span_secs: span.map(|(a, b)| (frame(a), frame(b))),
            frames: span.map_or(0, |(a, b)| b - a),
            size: (seq.resolution.width, seq.resolution.height),
            has_sound,
            sound_refusal,
        });
    }

    /// The dialog, when open: the command its Bounce sends, once.
    pub(super) fn show_bounce_dialog(&mut self, ctx: &egui::Context) -> Option<AppCommand> {
        let dlg = self.bounces.dialog.as_mut()?;
        let mut close = false;
        let mut send = None;
        egui::Area::new(egui::Id::new("bounce_backdrop")).fixed_pos(egui::pos2(0.0, 0.0)).order(egui::Order::PanelResizeLine).show(ctx, |ui| {
            let screen = ctx.screen_rect();
            if ui.allocate_rect(screen, egui::Sense::click()).clicked() {
                close = true;
            }
            ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
        });
        egui::Window::new("Bounce to Media File").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.set_min_width(440.0);
            ui.add_space(4.0);
            match &dlg.refusal {
                Some(why) => {
                    ui.label(format!("{} can't be bounced: {why}.", dlg.name));
                }
                None => {
                    if let Some((a, b)) = dlg.span_secs {
                        ui.label(format!("{}: {} frames at {}×{}, from {a:.2} s to {b:.2} s, drawn alone with its transparency.", dlg.name, dlg.frames, dlg.size.0, dlg.size.1));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("Format");
                        ui.radio_value(&mut dlg.format, BounceFormat::ProRes4444, BounceFormat::ProRes4444.label());
                        ui.radio_value(&mut dlg.format, BounceFormat::Ffv1, BounceFormat::Ffv1.label());
                    });
                    let disk = match bounce::estimate_bytes(dlg.format, dlg.size, dlg.frames) {
                        Some(bytes) => format!("Up to about {} on disk: less where the picture is simple.", size_said(bytes)),
                        None => "Lossless: its size follows the picture, often several times ProRes 4444's.".into(),
                    };
                    ui.label(egui::RichText::new(disk).weak());
                    if dlg.has_sound {
                        ui.add_space(4.0);
                        match dlg.sound_refusal {
                            Some(why) => {
                                ui.label(egui::RichText::new(format!("Its linked sound can't go in: {why}.")).weak());
                            }
                            None => {
                                ui.checkbox(&mut dlg.sound, "Include its linked sound");
                            }
                        }
                    }
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(bounce::CLIPPING_NOTE).weak());
                    ui.label(egui::RichText::new("The file is kept in the project's media folder. A clip of it goes on a new track above, and the original is hidden. One undo takes it back.").weak());
                }
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.button(if dlg.refusal.is_some() { "Close" } else { "Cancel" }).clicked() {
                    close = true;
                }
                if dlg.refusal.is_none() {
                    ui.add_space(8.0);
                    if ui.button(egui::RichText::new("Bounce").strong()).clicked() {
                        send = Some(AppCommand::BounceTrack { track_id: dlg.track_id, format: dlg.format, sound: dlg.sound && dlg.has_sound });
                        close = true;
                    }
                }
            });
        });
        if close {
            self.bounces.dialog = None;
        }
        send
    }

    /// Start bouncing `track_id`: the job, its file in the project. The
    /// reason when it can't.
    pub(super) fn start_bounce(&mut self, track_id: TrackId, format: BounceFormat, sound: bool) -> Result<(), String> {
        let folder = self.bounce_folder()?;
        let seq = self.sequence.snapshot();
        let working = klipp_engine::video_decoder::working_colorspace();
        let index = seq.tracks.iter().position(|t| t.id == track_id).ok_or("no such track")?;
        if let Some(why) = bounce::refusal(&seq, index, working) {
            return Err(why);
        }
        if sound {
            if let Some(why) = klipp_render::stem::linked_sound_refusal(&seq, track_id) {
                return Err(format!("its sound can't go in: {why}"));
            }
        }
        let (first, end) = bounce::span(&seq, index).ok_or("nothing on the track to bounce")?;
        let pool = &self.state.media_pool;
        let sound_snap = if sound { klipp_render::stem::linked_sound_snapshot(&seq, &[], pool, track_id).map_err(|e| format!("its sound couldn't be read: {e}"))? } else { None };
        let name = seq.tracks[index].name.clone();
        let with_sound = sound_snap.is_some();
        let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M-%S");
        let path = folder.join(format!("{} {stamp}.{}", file_name_of(&name), format.container().extension()));
        let files: Arc<HashMap<MediaId, klipp_render::MediaFile>> = Arc::new(pool.all().map(|i| (i.id, klipp_render::MediaFile::of(i))).collect());
        let fps = seq.fps.as_f64();
        let at = |f: i64| RationalTime { value: f * i64::from(seq.fps.den), rate: seq.fps.num };
        let span = (at(first), at(end));
        let req = BounceRequest {
            seq: Arc::clone(&seq),
            track: index,
            first,
            end,
            size: (seq.resolution.width, seq.resolution.height),
            working,
            format,
            path,
            sound: sound_snap,
        };
        let (tx, done) = mpsc::channel();
        let job = bounce::spawn_bounce(req, move || klipp_render::ExportFrameSource::shared(files, fps, working), move |r| {
            let _ = tx.send(r);
        });
        self.job_scheduler.submit(Arc::clone(&job) as Arc<dyn BackgroundJob>);
        self.state.toast.show(format!("Bouncing {name}: its progress is in the job list"));
        self.bounces.running.push(Running { track_id, name, span, sound: with_sound, job, done });
        Ok(())
    }

    /// Each frame: a bounce finished is read as media, to be placed when
    /// it is in the pool ([`Self::place_bounce`]); one that failed or was
    /// cancelled says so.
    pub(super) fn poll_bounces(&mut self) {
        let mut finished = Vec::new();
        self.bounces.running.retain(|r| match r.done.try_recv() {
            Ok(result) => {
                finished.push((r.track_id, r.name.clone(), r.span, r.sound, result));
                false
            }
            Err(mpsc::TryRecvError::Empty) => !r.job.is_done(),
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for (track_id, name, span, sound, result) in finished {
            match (result.path, result.error, result.cancelled) {
                (Some(path), _, _) => {
                    let media_id = MediaId::new();
                    // A file Klipp made needs no analysis of its own.
                    self.state.pending_skip_analysis.insert(media_id);
                    self.bounces.to_place.insert(media_id, ToPlace { track_id, name, span, sound });
                    let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path, media_id });
                }
                (None, _, true) => self.state.toast.show(format!("The bounce of {name} was cancelled")),
                (None, why, false) => self.state.toast.show(format!("{name} couldn't be bounced: {}", why.unwrap_or_else(|| "it stopped".into()))),
            }
        }
    }

    /// A bounced file in the pool, placed as one undo step: see the
    /// module's doc. Nothing for any other media.
    pub(super) fn place_bounce(&mut self, media_id: MediaId) {
        let Some(place) = self.bounces.to_place.remove(&media_id) else { return };
        let Some(item) = self.state.media_pool.get(&media_id).cloned() else { return };
        let edits = bounce_edits(self.state.sequence(), &item, &place);
        if edits.is_empty() {
            self.state.toast.show(format!("{} was bounced, but its track is gone: the file is in the media pool", place.name));
            return;
        }
        let label = format!("Bounce {}", place.name);
        self.apply_edits_as_one(&edits, &label);
        self.state.toast.show(format!("{} bounced to {}", place.name, item.filename));
    }
}

/// The edits placing a bounced file, as one step: see the module's doc.
/// Empty when the track bounced is gone.
fn bounce_edits(seq: &klipp_timeline::Sequence, item: &klipp_media::MediaItem, place: &ToPlace) -> Vec<EditCommand> {
    let mut s = seq.clone();
    let Some(index) = s.tracks.iter().position(|t| t.id == place.track_id) else { return Vec::new() };
    let (from, to) = place.span;
    let len = to - from;
    let zero = RationalTime { value: 0, rate: from.rate };
    let original = s.tracks[index].clone();
    let mut edits = vec![EditCommand::AddMediaToPool { item: item.clone() }];

    let mut picture = Clip::media(item.id, from, to, zero, len);
    if let Some(mode) = original.clips.first().map(|c| c.blend_mode).filter(|m| original.clips.iter().all(|c| c.blend_mode == *m)) {
        picture.blend_mode = mode;
    }
    let mut sound = place.sound.then(|| Clip::media(item.id, from, to, zero, len));
    if let Some(sound) = &mut sound {
        (picture.linked_clip_id, sound.linked_clip_id) = (Some(sound.id), Some(picture.id));
    }
    let mut track = Track::new(&format!("{} (bounced)", original.name), TrackKind::Video);
    track.opacity = original.opacity;
    track.clips = vec![picture];
    edits.push(EditCommand::AddTrack { index, track: track.clone() });
    s.tracks.insert(index, track);

    // The original, hidden, now one below.
    let mut hidden = original.clone();
    hidden.visible = false;
    edits.push(EditCommand::SetTrack { description: "Bounce", track_index: index + 1, old: Box::new(original.clone()), new: Box::new(hidden.clone()) });
    s.tracks[index + 1] = hidden;

    if let Some(sound) = sound {
        // Its sound silenced where it was, and on a track of its own above
        // the first track that held it.
        let ids: std::collections::HashSet<_> = original.clips.iter().map(|c| c.id).collect();
        let linked = |c: &Clip| c.linked_clip_id.is_some_and(|l| ids.contains(&l)) || original.clips.iter().any(|vc| vc.linked_clip_id == Some(c.id));
        let mut first_sound = None;
        for i in 0..s.tracks.len() {
            let t = &s.tracks[i];
            if t.kind != TrackKind::Audio || !t.clips.iter().any(linked) {
                continue;
            }
            first_sound.get_or_insert(i);
            let mut quiet = t.clone();
            quiet.clips.iter_mut().filter(|c| linked(c)).for_each(|c| c.enabled = false);
            edits.push(EditCommand::SetTrack { description: "Bounce", track_index: i, old: Box::new(t.clone()), new: Box::new(quiet.clone()) });
            s.tracks[i] = quiet;
        }
        let at = first_sound.unwrap_or(s.tracks.len());
        let mut audio = Track::new(&format!("{} (bounced)", original.name), TrackKind::Audio);
        audio.clips = vec![sound];
        edits.push(EditCommand::AddTrack { index: at, track: audio.clone() });
        s.tracks.insert(at, audio);
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(f: i64) -> RationalTime {
        RationalTime::from_frames(f, 24)
    }

    /// The edits: the media, a new track above with the clip (the track's
    /// opacity, the clips' blend mode), the original hidden below it, its
    /// linked sound silenced and the bounced sound on a track above it,
    /// linked to the picture.
    #[test]
    fn a_bounced_file_is_placed_above_the_track_it_replaces() {
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.opacity = 0.7;
        let mut a = Clip::media(MediaId::new(), tc(0), tc(12), tc(0), tc(12));
        a.blend_mode = klipp_core::BlendMode::Screen;
        let mut b = Clip::media(MediaId::new(), tc(18), tc(30), tc(0), tc(12));
        b.blend_mode = klipp_core::BlendMode::Screen;
        let mut s = Clip::media(MediaId::new(), tc(0), tc(12), tc(0), tc(12));
        (a.linked_clip_id, s.linked_clip_id) = (Some(s.id), Some(a.id));
        v1.clips = vec![a, b];
        let mut a1 = Track::new("A1", TrackKind::Audio);
        let other = Clip::media(MediaId::new(), tc(40), tc(50), tc(0), tc(10));
        a1.clips = vec![s, other];
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks = vec![Track::new("V0", TrackKind::Video), v1.clone(), a1];
        let item = klipp_media::MediaItem::synthetic_for_test("/p/media/bounces/V1.mov".into(), None);
        let place = ToPlace { track_id: v1.id, name: "V1".into(), span: (tc(0), tc(30)), sound: true };
        let edits = bounce_edits(&seq, &item, &place);
        // Applied in order to a copy, as the document applies them.
        let mut after = seq.clone();
        for e in &edits {
            match e {
                EditCommand::AddTrack { index, track } => after.tracks.insert(*index, track.clone()),
                EditCommand::SetTrack { track_index, new, .. } => after.tracks[*track_index] = (**new).clone(),
                EditCommand::AddMediaToPool { item: i } => assert_eq!(i.id, item.id),
                other => panic!("unexpected edit {other:?}"),
            }
        }
        let names: Vec<&str> = after.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["V0", "V1 (bounced)", "V1", "V1 (bounced)", "A1"]);
        let bounced = &after.tracks[1];
        assert_eq!((bounced.opacity, bounced.clips.len()), (0.7, 1));
        let clip = &bounced.clips[0];
        assert_eq!((clip.timeline_in, clip.timeline_out, clip.media_id(), clip.blend_mode), (tc(0), tc(30), Some(item.id), klipp_core::BlendMode::Screen));
        assert!(!after.tracks[2].visible && after.tracks[2].clips.len() == 2, "the original kept, hidden");
        let sound = &after.tracks[3].clips[0];
        assert_eq!((sound.linked_clip_id, clip.linked_clip_id), (Some(clip.id), Some(sound.id)), "the bounced sound linked to its picture");
        assert!(!after.tracks[4].clips[0].enabled && after.tracks[4].clips[1].enabled, "the linked sound silenced, the rest of its track not");
        assert_eq!(edits.len(), 5, "pool, track, hide, silence, sound track");
    }

    #[test]
    fn without_sound_or_its_track_less_is_placed() {
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![Clip::media(MediaId::new(), tc(0), tc(12), tc(0), tc(12))];
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks = vec![v1.clone()];
        let item = klipp_media::MediaItem::synthetic_for_test("/p/media/bounces/V1.mov".into(), None);
        let place = |sound| ToPlace { track_id: v1.id, name: "V1".into(), span: (tc(0), tc(12)), sound };
        assert_eq!(bounce_edits(&seq, &item, &place(false)).len(), 3, "pool, track, hide");
        seq.tracks.clear();
        assert!(bounce_edits(&seq, &item, &place(false)).is_empty(), "the track gone: nothing placed");
    }

    #[test]
    fn a_track_name_is_a_safe_file_name_and_the_folder_is_inside_the_project() {
        assert_eq!(file_name_of("V1 / ../etc"), "V1 - ---etc");
        assert_eq!(file_name_of("  "), "Track");
        let project = std::env::temp_dir().join(format!("klipp-bounce-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(&project).unwrap();
        assert_eq!(bounces_inside(&project), Some(project.join("media").join("bounces")), "made inside the project");
        let elsewhere = std::env::temp_dir().join(format!("klipp-bounce-elsewhere-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::remove_dir_all(project.join("media")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join("media")).unwrap();
        assert_eq!(bounces_inside(&project), None, "a link leading out of the project: no folder");
        std::fs::remove_file(project.join("media")).unwrap();
        std::fs::create_dir_all(project.join("media")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, project.join("media").join("bounces")).unwrap();
        assert_eq!(bounces_inside(&project), None, "its bounces folder a link: no folder");
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }
}
