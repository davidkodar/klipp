use crate::error::RenderError;
use klipp_core::{ExportSettings, Keyframe, RationalTime};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use std::collections::HashMap;
use std::path::PathBuf;

// ═══════════════════════════════════════════════════════════════════
// Snapshot types — frozen copies of sequence data at export time
// ═══════════════════════════════════════════════════════════════════

/// Complete frozen copy of the sequence at export time.
/// The export thread reads only this — it never touches the live Sequence.
#[derive(Clone)]
pub struct SequenceSnapshot {
    pub fps: f64,
    /// Exact frame rate numerator (e.g. 24000 for 23.976).
    pub fps_num: u32,
    /// Exact frame rate denominator (e.g. 1001 for 23.976).
    pub fps_den: u32,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    pub duration_frames: u64,
    /// If set, only export this frame range (start inclusive, end exclusive).
    pub export_range: Option<(u64, u64)>,
    /// Everything the video render reads: see [`VideoTimeline`].
    pub video: VideoTimeline,
    /// Audio tracks.
    pub audio_tracks: Vec<AudioTrackSnapshot>,
    /// How a track's pan position becomes left and right gains.
    ///
    /// Carried so the render mixes the way playback does. The export
    /// used to have the constant-power law written into it, which is
    /// 0.707 at centre — so every stereo track came out of a render
    /// 3 dB below what it sounded like, and the user's choice of law
    /// was ignored. Defaults to `PanLaw::default()` (Linear, unity at
    /// centre) for a caller that has no preferences to hand.
    pub pan_law: klipp_core::PanLaw,
    /// Global 3D LUT path if loaded.
    pub global_lut_path: Option<PathBuf>,
    /// Subtitle tracks (text content + timing, independent of burn-in).
    pub subtitle_tracks: Vec<SubtitleTrackSnapshot>,
    /// Frozen chapter entries (millisecond time base). Populated from
    /// `seq.markers` only when `ExportSettings.embed_chapters` is true;
    /// already shifted into export-range-relative coordinates when a
    /// zone export is active, and markers outside the range are dropped.
    pub chapters: Vec<klipp_core::ChapterSnapshot>,
    /// Project working colorspace at the moment of snapshot. Read
    /// alongside `ExportSettings.odt` by the encoder's display
    /// transform to dispatch the right linear → display path.
    /// Defaults to [`klipp_core::WorkingColorspace::Rec709Linear`] for back-compat with snapshot
    /// constructors that pre-date the field.
    #[doc(hidden)]
    pub working_colorspace: klipp_core::WorkingColorspace,
    /// The project's grade encoding (#467) when the export began. The
    /// render reads this, not the live setting, so changing it in the
    /// UI mid-export cannot change the grade partway through the file.
    pub grade_encoding: klipp_core::GradeEncoding,
}

/// The video half of an export: the timeline exactly as the viewer's
/// engine receives it — flattened by the same
/// `klipp_timeline::flatten::flatten_sequence` — rendered by the same
/// `klipp_compose::Renderer`. Keeping the timeline as-is, rather than a
/// converted copy, is what lets export and viewer agree by construction.
#[derive(Clone)]
pub struct VideoTimeline {
    /// The flattened sequence. Text and subtitle tracks are hidden when
    /// subtitles are not burned in.
    pub sequence: Sequence,
    /// The file behind each media clip, resolved at snapshot time.
    pub media: HashMap<klipp_core::MediaId, MediaFile>,
    /// Strength of the global LUT (`SequenceSnapshot::global_lut_path`).
    pub global_lut_opacity: f32,
}

impl Default for VideoTimeline {
    /// No video: an empty timeline.
    fn default() -> Self {
        let mut sequence = Sequence::new_default();
        sequence.tracks.clear();
        Self { sequence, media: HashMap::new(), global_lut_opacity: 1.0 }
    }
}

/// A media file as export decodes it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MediaFile {
    pub path: PathBuf,
    /// The media's camera log format, when it is one (S-Log3, V-Log, ...).
    /// File tags never say so; the viewer takes it from the media pool,
    /// and export must decode the file the same way.
    pub log_format: Option<klipp_core::LogFormat>,
    /// The media's own rotation, from its display matrix. Phone footage
    /// is landscape pixels plus a 90° tag.
    ///
    /// A clip carries a rotation too, and export used to read only that
    /// one. The two are not the same thing: the clip's is a copy each
    /// clip-creating path has to remember to make, and any that forgets
    /// leaves a clip that *plays* upright — the viewer's decoder takes
    /// rotation from the media pool — and *exports* sideways. This is
    /// the fallback for that, so a clip that never got its copy still
    /// renders the right way up.
    pub source_rotation: i32,
    /// How long the media is, in seconds, when known: the source a
    /// temporal node may read neighbours from, handles included.
    pub duration_secs: Option<f64>,
    /// The media's frame rate (numerator, denominator), when known: a
    /// temporal node's neighbours are its captures either side, and its
    /// analysis assets' key (M4 step 9).
    pub fps: Option<(u32, u32)>,
    /// The media's size, when known: its analysis assets' key (M4 step 9).
    pub resolution: Option<(u32, u32)>,
    /// The transfer its frames decode scene-referred through (HDR or
    /// camera log), `None` for SDR: a media source reading it gives its
    /// frame this display step (#1857).
    pub scene_transfer: Option<klipp_color::hdr::HdrTransfer>,
}

impl MediaFile {
    /// A media item's file, as export decodes it.
    pub fn of(item: &klipp_media::MediaItem) -> Self {
        MediaFile {
            path: item.path.clone(),
            log_format: item.camera_log_format(),
            scene_transfer: item.scene_transfer(),
            source_rotation: item.source_rotation,
            duration_secs: Some(item.duration.to_secs_f64()),
            fps: Some((item.fps.num, item.fps.den)),
            resolution: Some((item.resolution.width, item.resolution.height)),
        }
    }
}

#[derive(Clone)]
pub struct AudioTrackSnapshot {
    pub volume: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub clips: Vec<AudioClipSnapshot>,
    /// The track's own automation (#2306): its volume and pan over the
    /// sequence, evaluated as playback evaluates it.
    pub automation: klipp_timeline::TrackAutomation,
    /// The track's own effects (#2560), run on its summed clips before
    /// its fader, as the playback mixer runs them.
    pub effects: Vec<klipp_core::AudioEffectInstance>,
}

#[derive(Clone)]
pub struct AudioClipSnapshot {
    pub timeline_start_frame: u64,
    pub timeline_end_frame: u64,
    pub source_start_secs: f64,
    pub path: PathBuf,
    /// Per-clip audio DSP effects (EQ, compressor, reverb, etc.)
    pub audio_effects: Vec<klipp_core::AudioEffectInstance>,
    /// Per-audio-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    pub audio_effect_keyframes: HashMap<String, Vec<Keyframe>>,
    /// The part of the file the clip plays, as its clip has it: what a
    /// Normalize in its chain is measured over (#2778).
    pub source_range: (f64, f64),
    /// What the clip does to its sound: playback applies this per clip,
    /// and the render applies the same.
    pub shape: AudioClipShape,
    /// A retimed clip's sound, read at its speed (#642); `None` at 1x.
    pub varispeed: Option<VarispeedRead>,
    /// Where the clip's sound starts and ends on the timeline, seconds,
    /// crossfade handles included: a sound placed by the sample starts
    /// inside a frame (#1588), so the mixdown starts it on its own
    /// sample. The frames above are those it sounds in. `None` for one
    /// that starts and ends on them.
    pub exact_secs: Option<(f64, f64)>,
}

impl AudioClipSnapshot {
    /// Where the sound starts and ends on the timeline, seconds: its
    /// exact times, or its frames'.
    pub fn span_secs(&self, fps: f64) -> (f64, f64) {
        self.exact_secs.unwrap_or((self.timeline_start_frame as f64 / fps, self.timeline_end_frame as f64 / fps))
    }

    /// The frames a sound from `start` to `end` seconds is heard in: the
    /// one it starts inside, to the one it ends inside.
    fn frames_of(start: f64, end: f64, fps: f64) -> (u64, u64) {
        ((start * fps + 1e-9).floor().max(0.0) as u64, (end * fps - 1e-9).ceil().max(0.0) as u64)
    }
}

/// How a retimed clip's sound is read (#642): at `speed`, backward when
/// negative, so the second `e` of the clip on the timeline plays source
/// second `source_in + e·speed` going forward, `source_out + e·speed`
/// reversed. `head_secs` is the timeline time the clip's sound starts
/// before its own start, a crossfade's handle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VarispeedRead {
    pub speed: f64,
    pub source_in: f64,
    pub source_out: f64,
    pub head_secs: f64,
    /// Read with its pitch kept, forward only (#925).
    pub keep_pitch: bool,
}

impl VarispeedRead {
    /// `clip`'s, when it is retimed.
    pub fn of(clip: &klipp_timeline::Clip, head_secs: f64) -> Option<Self> {
        clip.is_varispeed().then(|| Self { speed: clip.speed, source_in: clip.source_in.to_secs_f64(), source_out: clip.source_out.to_secs_f64(), head_secs, keep_pitch: clip.keep_pitch })
    }

    /// Where decoding starts: early enough for a forward clip's handle,
    /// read at its speed; a reversed clip's handle lies past its out.
    pub fn decode_from(&self) -> f64 {
        (self.source_in - self.head_secs * self.speed.abs()).max(0.0)
    }

    /// The source second the clip's sound plays `secs` after its sound
    /// starts on the timeline.
    pub fn source_at(&self, secs: f64) -> f64 {
        let e = secs - self.head_secs;
        if self.speed < 0.0 { self.source_out + e * self.speed } else { self.source_in + e * self.speed }
    }
}

/// A clip's own sound settings — gain, fades, volume, pan, channel
/// layout, with their keyframes and modifiers — as playback applies
/// them in the PCM feeder and the output. Carried on the snapshot so a
/// render is the mix the viewer played, not a mix of raw files.
///
/// For an audio clip linked to a video clip the settings are the video
/// clip's, as in playback: the user sets them on the picture.
#[derive(Clone, Debug)]
pub struct AudioClipShape {
    pub volume: f32,
    /// Pre-fader gain.
    pub gain: f32,
    /// Balance: `-1` full left, `0` centre at unity, `1` full right.
    pub pan: f32,
    pub fade_in_secs: f32,
    pub fade_out_secs: f32,
    /// `0` linear, `1` exponential, `2` logarithmic, `3` S-curve.
    pub fade_in_curve: u8,
    pub fade_out_curve: u8,
    /// `0` as decoded, `1` mono downmix, `2` left and right swapped.
    pub channel_config: u8,
    /// The clip's length in seconds, which is where its fade-out ends.
    pub clip_duration_secs: f64,
    pub volume_keyframes: Vec<Keyframe>,
    pub gain_keyframes: Vec<Keyframe>,
    pub pan_keyframes: Vec<Keyframe>,
    pub param_modifiers: HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    /// The modifiers' random seed, the clip id's.
    pub seed: u64,
    /// A crossfade at either end (see `klipp_timeline::crossfade`): the
    /// snapshot's frames and source start already include the head and
    /// tail, so `clip_pos` counts from the extended start; these say how
    /// much of that is handle, and over how long the constant-power
    /// crossfade runs at each end.
    pub head_secs: f64,
    pub tail_secs: f64,
    pub crossfade_in_secs: f64,
    pub crossfade_out_secs: f64,
    /// For an angle of a multicam group: the stretches, in seconds from
    /// the in point, over which its sound is heard, the rest gated out
    /// (see `Sequence::multicam_sound_spans`). `None` is all of it.
    pub heard: Option<Vec<(f64, f64)>>,
}

impl Default for AudioClipShape {
    fn default() -> Self {
        Self {
            volume: 1.0,
            gain: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: 0,
            fade_out_curve: 0,
            channel_config: 0,
            clip_duration_secs: f64::MAX,
            volume_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            param_modifiers: HashMap::new(),
            seed: 0,
            head_secs: 0.0,
            tail_secs: 0.0,
            crossfade_in_secs: 0.0,
            crossfade_out_secs: 0.0,
            heard: None,
        }
    }
}

impl AudioClipShape {
    /// `clip`'s settings, over `duration_secs` of sound.
    pub fn of(clip: &klipp_timeline::Clip, duration_secs: f64) -> Self {
        use klipp_timeline::{ChannelConfig, FadeCurve};
        let curve = |c: FadeCurve| match c {
            FadeCurve::Linear => 0,
            FadeCurve::Exponential => 1,
            FadeCurve::Logarithmic => 2,
            FadeCurve::SCurve => 3,
            FadeCurve::ConstantPower => 4,
        };
        Self {
            volume: clip.volume,
            gain: clip.gain,
            pan: clip.pan,
            fade_in_secs: clip.fade_in_secs,
            fade_out_secs: clip.fade_out_secs,
            fade_in_curve: curve(clip.fade_in_curve),
            fade_out_curve: curve(clip.fade_out_curve),
            channel_config: match clip.channel_config {
                ChannelConfig::Default | ChannelConfig::Surround51 | ChannelConfig::Surround71 => 0,
                ChannelConfig::MonoDownmix => 1,
                ChannelConfig::SwapLR => 2,
                ChannelConfig::LeftOnly => 5,
                ChannelConfig::RightOnly => 6,
            },
            clip_duration_secs: duration_secs,
            volume_keyframes: clip.volume_keyframes.clone(),
            gain_keyframes: clip.gain_keyframes.clone(),
            pan_keyframes: clip.pan_keyframes.clone(),
            param_modifiers: clip.param_modifiers().clone(),
            seed: clip.id.0.as_u64_pair().0,
            head_secs: 0.0,
            tail_secs: 0.0,
            crossfade_in_secs: 0.0,
            crossfade_out_secs: 0.0,
            heard: None,
        }
    }

    /// Heard only over `spans` of the clip, as a multicam's sound rule
    /// says: see [`Self::heard`].
    pub fn heard_over(mut self, spans: Option<Vec<(f64, f64)>>) -> Self {
        self.heard = spans;
        self
    }

    /// With the crossfades `e` at the clip's ends. A crossfade takes the
    /// place of the clip's own fade at that end: the two would multiply
    /// into a dip, and a fade to silence under the outgoing clip's tail
    /// would silence the handle the crossfade plays.
    pub fn with_crossfades(mut self, e: klipp_timeline::crossfade::Extent) -> Self {
        self.head_secs = e.head_secs;
        self.tail_secs = e.tail_secs;
        self.crossfade_in_secs = e.fade_in_secs;
        self.crossfade_out_secs = e.fade_out_secs;
        if e.fade_in_secs > 0.0 {
            self.fade_in_secs = 0.0;
        }
        if e.fade_out_secs > 0.0 {
            self.fade_out_secs = 0.0;
        }
        self
    }

    /// Whether the clip can never be heard: volume zero with nothing
    /// animating it. Such a clip need not be decoded.
    pub fn is_silent(&self) -> bool {
        if self.heard.as_ref().is_some_and(|s| s.is_empty()) {
            return true;
        }
        self.volume == 0.0
            && self.volume_keyframes.is_empty()
            && !self.param_modifiers.contains_key(&klipp_core::ParamId::Volume)
    }
}

/// The clip whose sound settings `clip` plays with: its linked video
/// clip when it has one on the timeline, else itself.
fn sound_settings_source<'a>(seq: &'a Sequence, clip: &'a klipp_timeline::Clip) -> &'a klipp_timeline::Clip {
    clip.linked_clip_id
        .and_then(|lid| seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == lid))
        .unwrap_or(clip)
}

#[derive(Debug, Clone)]
pub struct SubtitleClipSnapshot {
    pub timeline_start_frame: u64,
    pub timeline_end_frame: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct SubtitleTrackSnapshot {
    pub clips: Vec<SubtitleClipSnapshot>,
}

// ═══════════════════════════════════════════════════════════════════
// Construction
// ═══════════════════════════════════════════════════════════════════

impl SequenceSnapshot {
    /// Freeze the entire sequence at this moment. All MediaIds are resolved
    /// to file paths via `media_pool`. RationalTime values are converted to
    /// frame numbers and f64 seconds. The `global_lut_path` is passed from
    /// the engine's ColorPipeline.
    /// `use_marked_range`: if true AND the sequence has in/out points set,
    /// only export that range. If false, export the entire sequence.
    pub fn from_sequence(
        seq: &Sequence,
        media_pool: &MediaPool,
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
        working_colorspace: klipp_core::WorkingColorspace,
    ) -> Result<Self, RenderError> {
        Self::from_sequence_with_all(
            seq,
            &[],
            media_pool,
            settings,
            global_lut_path,
            use_marked_range,
            working_colorspace,
        )
    }

    /// Freeze the sequence, resolving compound clips (nested sequences) by
    /// flattening their inner clips into the parent timeline. `all_sequences`
    /// must contain every sequence in the project so that
    /// `ClipKind::Sequence { sequence_id, .. }` can be resolved.
    /// `working_colorspace` is the project's, and it is a parameter rather
    /// than a field the caller patches afterwards because every render
    /// decision downstream reads it: the display transform, the ACES ODT's
    /// input basis, an HDR delivery's matrix. It used to default here and
    /// be overwritten by each caller, which worked until one forgot — the
    /// headless export tool printed "AcesCG" and rendered Rec.709, and
    /// nothing anywhere said so. Now forgetting is a compile error.
    #[allow(clippy::too_many_arguments)]
    pub fn from_sequence_with_all(
        seq: &Sequence,
        all_sequences: &[Sequence],
        media_pool: &MediaPool,
        settings: &ExportSettings,
        global_lut_path: Option<PathBuf>,
        use_marked_range: bool,
        working_colorspace: klipp_core::WorkingColorspace,
    ) -> Result<Self, RenderError> {
        // The render copy, made here as the viewer makes it: grade groups
        // and each track's effects baked onto the clips (#846). The sound
        // below is read from this sequence before it is flattened, so it
        // must carry its tracks' audio chains already; flattening it again
        // finds every clip baked and leaves it.
        let baked_all: Option<Vec<Sequence>> = all_sequences.iter().any(Sequence::bakes_for_render).then(|| all_sequences.iter().map(|s| s.baked_for_render().into_owned()).collect());
        let all_sequences = baked_all.as_deref().unwrap_or(all_sequences);
        let baked = seq.baked_for_render();
        let seq: &Sequence = &baked;
        // The delivery frame rate. When the export overrides the
        // sequence's, every frame count below is at the override:
        // the render loop asks the timeline for frame `n` at
        // `n / fps`, so a 24 fps sequence delivered at 12 fps renders
        // every other frame and stays two seconds long. It used to set
        // only the container's rate, so the same file carried all 48
        // frames tagged 12 fps — four seconds of slow motion with the
        // audio, still at two seconds, running away from it.
        let out_fps = settings.fps.unwrap_or(seq.fps);
        let fps = out_fps.as_f64();
        let burn_in = settings.burn_in_subtitles;

        // With both flags on, subtitles are composited into the video AND
        // emitted as a separate soft-subtitle stream — the viewer sees them
        // twice. The default ExportSettings in older code paths has both
        // true, so flag it loudly so the user (or the caller building the
        // settings) can turn one off.
        if burn_in && settings.embed_subtitles {
            tracing::warn!(
                "Export settings have burn_in_subtitles and embed_subtitles \
                 both enabled — subtitles will be rendered into the video \
                 AND emitted as a separate soft-subtitle stream (double \
                 render). Disable one of the two in the export settings."
            );
        }

        let export_range = if use_marked_range {
            match (&seq.in_point, &seq.out_point) {
                (Some(in_pt), Some(out_pt)) => {
                    let start = time_to_frame(in_pt, fps);
                    let end = time_to_frame(out_pt, fps);
                    Some((start, end))
                }
                _ => None,
            }
        } else {
            None
        };

        let total_duration = seq.total_duration();
        let duration_frames = time_to_frame(&total_duration, fps);

        let mut audio_tracks = Vec::new();
        for track in seq.tracks.iter().rev() {
            if track.kind == TrackKind::Audio {
                audio_tracks.push(snapshot_audio_track(track, seq, all_sequences, media_pool, fps, 0)?);
            }
        }
        // A video clip with no audio clip linked to it plays the sound
        // in its file, as it does in the viewer.
        for track in seq.tracks.iter().rev() {
            if track.kind == TrackKind::Video {
                if let Some(t) = snapshot_video_track_sound(track, seq, media_pool, fps) {
                    audio_tracks.push(t);
                }
            }
        }

        // Video: the viewer's flattened timeline, as it is. Subtitle
        // tracks render into the picture only when burning captions in
        // (the user can still export sidecar SRT/VTT); titles on text
        // tracks are part of the picture either way (#2426).
        let mut timeline = klipp_timeline::flatten::flatten_sequence(seq, all_sequences);
        // A precomposition's sound, wherever the precomposition sits:
        // the runtime tracks flattening made for it, the scene's and the
        // containing track's controls already composed into them — the
        // one path that sound takes, so the viewer and the export agree.
        for track in timeline.tracks.iter().filter(|t| t.precomposition_sound) {
            audio_tracks.push(snapshot_audio_track(track, &timeline, all_sequences, media_pool, fps, 0)?);
        }
        if !burn_in {
            for t in &mut timeline.tracks {
                if t.kind == TrackKind::Subtitle {
                    t.visible = false;
                }
            }
        }
        let mut media = HashMap::new();
        // A precomposition's scene is rendered from its own clips, so
        // their media is the export's media too, however deep the scene.
        fn each_clip<'a>(tracks: &'a [Track], out: &mut Vec<&'a Clip>) {
            for clip in tracks.iter().flat_map(|t| &t.clips) {
                out.push(clip);
                if let Some(scene) = &clip.precomp {
                    each_clip(&scene.tracks, out);
                }
            }
        }
        let mut clips = Vec::new();
        each_clip(&timeline.tracks, &mut clips);
        for clip in clips {
            if let ClipKind::Media { media_id } = &clip.kind {
                let media_id: &klipp_core::MediaId = media_id;
                let item = media_pool
                    .get(media_id)
                    .ok_or_else(|| RenderError::Snapshot(format!("media {media_id:?} not in pool")))?;
                media.insert(*media_id, MediaFile::of(item));
            }
            // A media source's item (#1746): one not in the pool is drawn
            // clear and said, not a failed export.
            for (item, _) in klipp_compose::resolve::media_sources(clip) {
                if let Some((id, item)) = klipp_compose::resolve::media_id(&item).and_then(|id| Some((id, media_pool.get(&id)?))) {
                    media.insert(id, MediaFile::of(item));
                }
            }
        }

        // Collect subtitle tracks regardless of burn_in setting.
        // Subtitle text + timing is always available for soft embedding.
        let mut subtitle_tracks = Vec::new();
        // Subtitle tracks are the captions; text tracks stand in only when
        // there are none, or an embedded subtitle stream would carry the
        // titles and lower thirds too.
        let has_subtitles = seq.tracks.iter().any(|t| t.kind == TrackKind::Subtitle && !t.clips.is_empty());
        for track in seq.tracks.iter() {
            let wanted = if has_subtitles { track.kind == TrackKind::Subtitle } else { track.kind == TrackKind::Text };
            if !wanted {
                continue;
            }
            let mut clips = Vec::new();
            for clip in &track.clips {
                if let ClipKind::Title(data) = &clip.kind {
                    clips.push(SubtitleClipSnapshot {
                        timeline_start_frame: time_to_frame(&clip.timeline_in, fps),
                        timeline_end_frame: time_to_frame(&clip.timeline_out, fps),
                        text: data.text(),
                    });
                }
            }
            if !clips.is_empty() {
                subtitle_tracks.push(SubtitleTrackSnapshot { clips });
            }
        }

        // Chapter snapshot — only markers explicitly tagged
        // `MarkerKind::Chapter` are baked into the container. Mixing
        // workflow markers (ToDo, Comment, BestTake) into chapter
        // atoms would clutter player-side chapter menus, so we
        // require the explicit kind.
        //
        // Chapter N ends at chapter N+1's start; the last chapter
        // ends at the export duration. When a zone export is
        // active, timestamps are shifted relative to the zone start
        // and markers outside the zone are skipped.
        let chapters = if settings.embed_chapters {
            let total_duration_secs = total_duration.to_secs_f64();
            let total_duration_ms = (total_duration_secs * 1000.0) as u64;
            let (export_start_ms, export_end_ms) = if let Some((start_f, end_f)) = export_range {
                let start_ms = (start_f as f64 * 1000.0 / fps) as u64;
                let end_ms = (end_f as f64 * 1000.0 / fps) as u64;
                (start_ms, end_ms)
            } else {
                (0u64, total_duration_ms)
            };
            let export_duration_ms = export_end_ms.saturating_sub(export_start_ms);

            // Step 1: collect in-range Chapter-kind markers as
            // (shifted_ms, title).
            let in_range: Vec<(u64, String)> = seq
                .markers
                .iter()
                .filter(|m| m.kind == klipp_core::MarkerKind::Chapter)
                .filter_map(|m| {
                    let ms = (m.position.to_secs_f64() * 1000.0) as u64;
                    if ms < export_start_ms || ms >= export_end_ms {
                        return None;
                    }
                    Some((ms - export_start_ms, m.label.clone()))
                })
                .collect();

            // Step 2: pair each marker with the next marker's start (or export
            // end for the last). Iter is already sorted by position because
            // sequence markers are inserted in order.
            let mut out: Vec<klipp_core::ChapterSnapshot> = Vec::with_capacity(in_range.len());
            for i in 0..in_range.len() {
                let (start, ref title) = in_range[i];
                let end = if i + 1 < in_range.len() {
                    in_range[i + 1].0
                } else {
                    export_duration_ms
                };
                if end <= start {
                    continue;
                }
                out.push(klipp_core::ChapterSnapshot {
                    start_ms: start,
                    end_ms: end,
                    title: title.clone(),
                });
            }
            out
        } else {
            Vec::new()
        };

        Ok(Self {
            fps,
            fps_num: out_fps.num,
            fps_den: out_fps.den,
            width: seq.resolution.width,
            height: seq.resolution.height,
            sample_rate: 48000, // Standard NLE audio rate
            duration_frames,
            export_range,
            video: VideoTimeline { sequence: timeline, media, global_lut_opacity: 1.0 },
            audio_tracks,
            pan_law: settings.pan_law,
            global_lut_path,
            subtitle_tracks,
            chapters,
            working_colorspace,
            grade_encoding: klipp_core::grade_encoding(),
        })
    }

    /// Total frames to export, respecting export_range.
    pub fn frames_to_export(&self) -> u64 {
        match self.export_range {
            Some((start, end)) => end.saturating_sub(start),
            None => self.duration_frames,
        }
    }

    /// Start frame of the export range, or 0.
    pub fn start_frame(&self) -> u64 {
        self.export_range.map(|(s, _)| s).unwrap_or(0)
    }

    /// End frame of the export range (exclusive), or duration_frames.
    pub fn end_frame(&self) -> u64 {
        self.export_range.map(|(_, e)| e).unwrap_or(self.duration_frames)
    }

    /// The media files whose sound the export plays, each once, in track
    /// order: clips inside the exported range, on tracks that are heard
    /// (not muted; only the soloed ones when any is), nested sequences'
    /// clips with them, as the mixdown takes them. What the export owes
    /// credit for (#1604).
    pub fn heard_media(&self) -> Vec<PathBuf> {
        let any_solo = self.audio_tracks.iter().any(|t| t.solo);
        let (start, end) = (self.start_frame(), self.end_frame());
        let mut seen = std::collections::HashSet::new();
        self.audio_tracks
            .iter()
            .filter(|t| !t.muted && (!any_solo || t.solo))
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.timeline_start_frame < end && c.timeline_end_frame > start)
            .filter(|c| seen.insert(c.path.clone()))
            .map(|c| c.path.clone())
            .collect()
    }
}

/// Maximum nesting depth for compound clips to prevent infinite recursion.
const MAX_COMPOUND_DEPTH: u8 = 8;

fn snapshot_audio_track(
    track: &Track,
    seq: &Sequence,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
) -> Result<AudioTrackSnapshot, RenderError> {
    let mut clips = Vec::new();
    for clip in &track.clips {
        // A retimed or reversed clip is silent, as it plays (#617, #638):
        // read at 1x its sound ran out of step with its picture.
        if !clip.enabled || !clip.plays_its_sound() { continue; }
        // A precomposition's sound is not taken here: flattening puts
        // it on runtime tracks of its own, with the scene's controls,
        // and the snapshot takes those below — once.
        if let ClipKind::Sequence { sequence_id, .. } = &clip.kind {
            if depth < MAX_COMPOUND_DEPTH {
                flatten_compound_audio_clips(clip, *sequence_id, all_sequences, media_pool, fps, depth, &mut clips)?;
                continue;
            }
        }

        if let Some(media_id) = clip.media_id() {
            if let Some(media) = media_pool.get(&media_id) {
                if media.has_audio {
                    // The crossfades' handles on whole sequence frames:
                    // the clip's start frame moves back by the head and
                    // its source start by those same frames as seconds,
                    // so the sample at the cut is its in point exactly.
                    // Moving the frame by the rounded head and the source
                    // by the exact one — they differ when the clip's rate
                    // is not the sequence's, or its in point is off the
                    // frame grid — played the incoming clip up to a frame
                    // ahead of its picture for its whole length.
                    let frames = |secs: f64| (secs * fps).round() as u64;
                    let xfade = klipp_timeline::crossfade::extent(track, clip).on_frames(fps);
                    let (head_frames, tail_frames) = (frames(xfade.head_secs), frames(xfade.tail_secs));
                    // A multicam angle's sound, linked to its picture, is
                    // heard where the group's sound rule says.
                    let shape = AudioClipShape::of(sound_settings_source(seq, clip), clip.duration().to_secs_f64())
                        .with_crossfades(xfade)
                        .heard_over(seq.multicam_sound_spans(clip));
                    if shape.is_silent() {
                        continue;
                    }
                    // Stretched by the crossfades' handles: the incoming
                    // clip starts sounding early, the outgoing one ends late.
                    let varispeed = VarispeedRead::of(clip, xfade.head_secs);
                    let start = (clip.timeline_in.to_secs_f64() - head_frames as f64 / fps).max(0.0);
                    let end = clip.timeline_out.to_secs_f64() + tail_frames as f64 / fps;
                    let (start_frame, end_frame) = AudioClipSnapshot::frames_of(start, end, fps);
                    clips.push(AudioClipSnapshot {
                        timeline_start_frame: start_frame,
                        timeline_end_frame: end_frame,
                        exact_secs: Some((start, end)),
                        source_start_secs: varispeed.map_or(clip.source_in.to_secs_f64() - xfade.head_secs, |v| v.decode_from()),
                        path: media.path.clone(),
                        audio_effects: clip.audio_effects.clone(),
                        audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
                        source_range: (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64()),
                        shape,
                        varispeed,
                    });
                }
            }
        }
    }

    Ok(AudioTrackSnapshot {
        volume: track.volume,
        pan: track.pan,
        muted: track.muted,
        solo: track.solo,
        clips,
        automation: track.automation.clone(),
        effects: track.effects.audio.clone(),
    })
}

/// The sound of a video track's clips that have no audio clip linked to
/// them: those play the sound in their files, as in the viewer, with
/// their own settings. `None` when the track has no such clip.
fn snapshot_video_track_sound(track: &Track, seq: &Sequence, media_pool: &MediaPool, fps: f64) -> Option<AudioTrackSnapshot> {
    let mut clips = Vec::new();
    for clip in &track.clips {
        if !clip.enabled || !clip.plays_its_sound() || matches!(clip.kind, ClipKind::Sequence { .. } | ClipKind::Precomposition { .. }) {
            continue;
        }
        let Some(media) = clip.media_id().and_then(|id| media_pool.get(&id)) else { continue };
        if !media.has_audio {
            continue;
        }
        let partner_on_audio = clip.linked_clip_id.is_some_and(|lid| {
            seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == lid))
        });
        if partner_on_audio {
            continue;
        }
        let shape = AudioClipShape::of(clip, clip.duration().to_secs_f64()).heard_over(seq.multicam_sound_spans(clip));
        if shape.is_silent() {
            continue;
        }
        let varispeed = VarispeedRead::of(clip, 0.0);
        let (start, end) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
        let (start_frame, end_frame) = AudioClipSnapshot::frames_of(start, end, fps);
        clips.push(AudioClipSnapshot {
            timeline_start_frame: start_frame,
            timeline_end_frame: end_frame,
            exact_secs: Some((start, end)),
            source_start_secs: varispeed.map_or(clip.source_in.to_secs_f64(), |v| v.decode_from()),
            path: media.path.clone(),
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
            source_range: (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64()),
            shape,
            varispeed,
        });
    }
    if clips.is_empty() {
        return None;
    }
    Some(AudioTrackSnapshot { volume: track.volume, pan: track.pan, muted: track.muted, solo: track.solo, clips, automation: track.automation.clone(), effects: track.effects.audio.clone() })
}

/// Flatten a compound clip's inner sequence audio tracks into the parent audio clip list.
fn flatten_compound_audio_clips(
    compound: &klipp_timeline::Clip,
    sequence_id: klipp_core::SequenceId,
    all_sequences: &[Sequence],
    media_pool: &MediaPool,
    fps: f64,
    depth: u8,
    out: &mut Vec<AudioClipSnapshot>,
) -> Result<(), RenderError> {
    let flat_clips = klipp_timeline::flatten::flatten_clip_for_track(
        compound,
        sequence_id,
        all_sequences,
        TrackKind::Audio,
        depth,
    );
    for clip in flat_clips {
        if !clip.plays_its_sound() {
            continue;
        }
        let Some(media_id) = clip.media_id() else { continue };
        let Some(media) = media_pool.get(&media_id) else { continue };
        if !media.has_audio {
            continue;
        }
        let varispeed = VarispeedRead::of(&clip, 0.0);
        let (start, end) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
        let (start_frame, end_frame) = AudioClipSnapshot::frames_of(start, end, fps);
        out.push(AudioClipSnapshot {
            timeline_start_frame: start_frame,
            timeline_end_frame: end_frame,
            exact_secs: Some((start, end)),
            source_start_secs: varispeed.map_or(clip.source_in.to_secs_f64(), |v| v.decode_from()),
            path: media.path.clone(),
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
            source_range: (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64()),
            shape: AudioClipShape::of(&clip, clip.duration().to_secs_f64()),
            varispeed,
        });
    }
    Ok(())
}

/// Convert a RationalTime to a frame number at the given fps.
fn time_to_frame(rt: &RationalTime, fps: f64) -> u64 {
    let secs = rt.to_secs_f64();
    (secs * fps).round() as u64
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{ClipId, LogFormat, ExportAudioCodec, ExportFormat, ExportVideoCodec, Fps, MediaId, Resolution, EncoderPreset};
    use klipp_media::{MediaItem, MediaItemKind, MediaPool};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    fn make_fps24() -> Fps {
        Fps::new(24, 1)
    }

    fn make_sequence() -> Sequence {
        Sequence::new_default()
    }

    fn make_rational(frames: i64, rate: u32) -> RationalTime {
        RationalTime::from_frames(frames, rate)
    }

    fn make_media_item(id: MediaId, path: &str) -> MediaItem {
        MediaItem {
            id,
            path: PathBuf::from(path),
            kind: MediaItemKind::File,
            filename: path.into(),
            duration: make_rational(240, 24), // 10 seconds
            fps: make_fps24(),
            resolution: Resolution {
                width: 1920,
                height: 1080,
            },
            video_codec: "h264".into(),
            audio_codec: "aac".into(),
            has_audio: true,
            file_size_bytes: 1_000_000,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: vec![],
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            added_at: 0,
            fingerprint: None,
            cache_key: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: klipp_media::ColorLabel::default(),
            colour_space: klipp_media::ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            parent_media_id: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            dolby_vision_profile: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            audio_role: None,
            span: None,
        }
    }

    fn make_marker_at_secs(secs: f64, label: &str) -> klipp_timeline::Marker {
        // These tests cover chapter-snapshot behaviour, so the default
        // helper produces Chapter-kind markers (the only kind that
        // gets baked into chapter atoms). The non-Chapter exclusion
        // test below uses an inline construction with a different kind.
        klipp_timeline::Marker { review: None,
            id: klipp_core::MarkerId::new(),
            position: klipp_core::Timecode {
                value: (secs * 24.0).round() as i64,
                rate: 24,
            },
            label: label.to_string(),
            color: klipp_core::MarkerColor::Blue,
            duration: None,
            comment: String::new(),
            kind: klipp_core::MarkerKind::Chapter,
        }
    }


    fn make_sequence_with_length_secs(secs: f64) -> Sequence {
        let mut seq = make_sequence();
        let frames = (secs * 24.0).round() as i64;
        let gap = Clip::gap(make_rational(0, 24), make_rational(frames, 24));
        seq.tracks[0].clips.push(gap);
        seq
    }

    fn make_settings() -> ExportSettings {
        ExportSettings {
            pan_law: Default::default(),
            output_path: PathBuf::from("/tmp/test.mp4"),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 18,
            preset: EncoderPreset::Medium,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192), video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
            bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer,
            mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS }
    }

    /// #1604: what an export credits is the sound it plays: each file
    /// once, the clips inside its range, on heard tracks only (not a
    /// muted one, and only the soloed one when there is one).
    #[test]
    fn heard_media_is_what_the_export_plays() {
        let ids: Vec<MediaId> = (0..4).map(|_| MediaId::new()).collect();
        let mut pool = MediaPool::new();
        for (id, name) in ids.iter().zip(["a", "b", "c", "d"]) {
            pool.add(make_media_item(*id, &format!("/media/{name}.wav")));
        }
        let at = |id: MediaId, from: i64, to: i64| Clip::media(id, make_rational(from * 24, 24), make_rational(to * 24, 24), make_rational(0, 24), make_rational((to - from) * 24, 24));
        let mut seq = make_sequence();
        seq.tracks.retain(|t| t.kind != TrackKind::Audio);
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips = vec![at(ids[0], 0, 2), at(ids[1], 4, 6), at(ids[0], 6, 8)];
        let mut a2 = Track::new("A2", TrackKind::Audio);
        a2.muted = true;
        a2.clips = vec![at(ids[2], 0, 8)];
        seq.tracks.extend([a1, a2]);
        let heard = |seq: &Sequence, ranged: bool| -> Vec<String> {
            SequenceSnapshot::from_sequence(seq, &pool, &make_settings(), None, ranged, klipp_core::WorkingColorspace::Rec709Linear)
                .unwrap()
                .heard_media()
                .iter()
                .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(heard(&seq, false), ["a", "b"], "each once, the muted track's not at all");
        seq.in_point = Some(make_rational(3 * 24, 24));
        seq.out_point = Some(make_rational(5 * 24, 24));
        assert_eq!(heard(&seq, true), ["b"], "only what plays in the range");
        let mut a3 = Track::new("A3", TrackKind::Audio);
        a3.solo = true;
        a3.clips = vec![at(ids[3], 0, 8)];
        seq.tracks.push(a3);
        assert_eq!(heard(&seq, false), ["d"], "a soloed track is all that is heard");
    }

    /// The export dialog's frame-rate override has to change how many
    /// frames are rendered, not just the rate written on the container.
    /// A 10 s sequence at 24 fps delivered at 12 fps is 120 frames of
    /// 10 s — and its audio clips, marked range and subtitles are
    /// placed at 12 fps too, or they would land at the wrong time.
    #[test]
    fn a_frame_rate_override_renders_at_that_rate() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));
        let mut seq = make_sequence();
        let mut clip = Clip::media(media_id, make_rational(24, 24), make_rational(240, 24), make_rational(0, 24), make_rational(216, 24));
        clip.id = ClipId::new();
        seq.tracks[0].clips.push(clip.clone());
        seq.tracks[1].clips.push(clip);
        seq.in_point = Some(make_rational(48, 24));
        seq.out_point = Some(make_rational(144, 24));
        let mut settings = make_settings();
        settings.fps = Some(Fps::new(12, 1));

        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, true, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        assert_eq!((snap.fps_num, snap.fps_den), (12, 1));
        assert_eq!(snap.duration_frames, 120, "10 s at 12 fps");
        assert_eq!(snap.export_range, Some((24, 72)), "the marked range, at 12 fps");
        let a = &snap.audio_tracks[0].clips[0];
        assert_eq!((a.timeline_start_frame, a.timeline_end_frame), (12, 120), "the audio clip, at 12 fps");
    }

    #[test]
    fn snapshot_captures_export_range() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(240, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(240, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(240, 24))
        };

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(clip);
        seq.in_point = Some(make_rational(24, 24));  // 1 second in
        seq.out_point = Some(make_rational(120, 24)); // 5 seconds in

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, true, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(snap.export_range, Some((24, 120)));
        assert_eq!(snap.frames_to_export(), 96);
        assert_eq!(snap.start_frame(), 24);
        assert_eq!(snap.end_frame(), 120);
    }

    #[test]
    fn chapter_end_times_inferred_correctly() {
        // Three markers at 0s, 10s, 30s inside a 60s sequence.
        // Expected: chapter ends at 10000, 30000, 60000 ms.
        let mut seq = make_sequence_with_length_secs(60.0);
        seq.markers.push(make_marker_at_secs(0.0, "Intro"));
        seq.markers.push(make_marker_at_secs(10.0, "Act 1"));
        seq.markers.push(make_marker_at_secs(30.0, "Act 2"));

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(snap.chapters.len(), 3, "three markers → three chapters");
        assert_eq!(snap.chapters[0].start_ms, 0);
        assert_eq!(snap.chapters[0].end_ms, 10_000);
        assert_eq!(snap.chapters[0].title, "Intro");
        assert_eq!(snap.chapters[1].start_ms, 10_000);
        assert_eq!(snap.chapters[1].end_ms, 30_000);
        assert_eq!(snap.chapters[2].start_ms, 30_000);
        assert_eq!(snap.chapters[2].end_ms, 60_000);
    }

    #[test]
    fn chapters_shifted_for_zone_export() {
        // Markers at 5s and 15s; zone export covers [10s, 30s].
        // The 5s marker is outside the zone and must be dropped; the
        // 15s marker shifts to start_ms = 5000 (relative to zone start)
        // and its end clamps to the zone duration 20000.
        let mut seq = make_sequence_with_length_secs(60.0);
        seq.markers.push(make_marker_at_secs(5.0, "Outside"));
        seq.markers.push(make_marker_at_secs(15.0, "Inside"));
        seq.in_point = Some(make_rational(240, 24));  // 10s
        seq.out_point = Some(make_rational(720, 24)); // 30s

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, true, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(
            snap.chapters.len(),
            1,
            "5s marker is outside the zone — only 15s marker survives"
        );
        assert_eq!(snap.chapters[0].start_ms, 5_000, "15s - 10s zone start = 5000ms");
        assert_eq!(snap.chapters[0].end_ms, 20_000, "clamped to zone duration");
        assert_eq!(snap.chapters[0].title, "Inside");
    }

    #[test]
    fn chapters_empty_when_no_markers() {
        // embed_chapters is true but sequence has no markers — should
        // produce an empty chapters Vec, not an error or panic.
        let seq = make_sequence_with_length_secs(60.0);

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert!(snap.chapters.is_empty());
    }

    #[test]
    fn chapters_empty_when_embed_disabled() {
        // Even with markers, embed_chapters=false → no snapshot entries.
        let mut seq = make_sequence_with_length_secs(60.0);
        seq.markers.push(make_marker_at_secs(10.0, "Act 1"));
        seq.markers.push(make_marker_at_secs(30.0, "Act 2"));

        let settings = make_settings(); // embed_chapters: false by default

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert!(snap.chapters.is_empty());
    }

    #[test]
    fn chapter_filter_excludes_non_chapter_kinds() {
        // Mix of marker kinds. Only Chapter-kind markers should land
        // in the chapter snapshot — the others are workflow markers
        // (ToDo / Comment / BestTake / Section / None) that don't
        // belong in player-side chapter menus.
        let mut seq = make_sequence_with_length_secs(60.0);
        // Default helper produces Chapter-kind markers.
        seq.markers.push(make_marker_at_secs(0.0, "Real chapter A"));
        seq.markers.push(make_marker_at_secs(30.0, "Real chapter B"));
        // Inline non-Chapter markers — must be filtered out.
        for (secs, label, kind) in [
            (5.0_f64, "todo note", klipp_core::MarkerKind::ToDo),
            (10.0, "comment note", klipp_core::MarkerKind::Comment),
            (15.0, "best take", klipp_core::MarkerKind::BestTake),
            (20.0, "section break", klipp_core::MarkerKind::Section),
            (25.0, "untyped", klipp_core::MarkerKind::None),
        ] {
            seq.markers.push(klipp_timeline::Marker { review: None,
                id: klipp_core::MarkerId::new(),
                position: klipp_core::Timecode {
                    value: (secs * 24.0).round() as i64,
                    rate: 24,
                },
                label: label.to_string(),
                color: klipp_core::MarkerColor::Yellow,
                duration: None,
                comment: String::new(),
                kind,
            });
        }

        let mut settings = make_settings();
        settings.embed_chapters = true;

        let pool = MediaPool::new();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(
            snap.chapters.len(),
            2,
            "only the two Chapter-kind markers survive the filter"
        );
        assert_eq!(snap.chapters[0].title, "Real chapter A");
        assert_eq!(snap.chapters[1].title, "Real chapter B");
        // Chapter B should end at the export end (60s) since no
        // Chapter-kind marker follows it — the workflow markers
        // between A and B don't pin its end time.
        assert_eq!(snap.chapters[0].end_ms, 30_000);
        assert_eq!(snap.chapters[1].end_ms, 60_000);
    }

    #[test]
    fn snapshot_audio_tracks() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip.mp4"));

        let clip = Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(24, 24),
            source_out: make_rational(72, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };

        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip); // A1
        seq.tracks[1].volume = 0.8;
        seq.tracks[1].pan = -0.5;

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(snap.audio_tracks.len(), 1);
        let at = &snap.audio_tracks[0];
        assert_eq!(at.volume, 0.8);
        assert_eq!(at.pan, -0.5);
        assert_eq!(at.clips.len(), 1);
        assert_eq!(at.clips[0].source_start_secs, 1.0);
    }

    /// #846: the export carries each track's effects on its clips as the
    /// viewer's render copy does: a sound with its track's audio chain
    /// after its own, a picture with its track's effects after its own, in
    /// the same order as the flattened sequence the viewer is given.
    #[test]
    fn the_export_carries_each_tracks_effects_as_the_viewer_does() {
        use klipp_effects::BuiltinEffect as E;
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip.mp4"));
        let clip = || Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(24, 24),
            source_out: make_rational(72, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };
        let fx = |kind: &str| klipp_core::AudioEffectInstance { effect_type: kind.into(), enabled: true, wet_dry: 1.0, params: vec![] };
        let mut seq = make_sequence();
        let mut picture = clip();
        picture.set_effects(vec![E::Opacity { value: 0.9 }]);
        seq.tracks[0].clips.push(picture); // V1
        seq.tracks[0].effects.set_video_effect_list(vec![E::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.7 }], vec![]);
        let mut sound = clip();
        sound.audio_effects = vec![fx("eq")];
        seq.tracks[1].clips.push(sound); // A1
        seq.tracks[1].effects.audio = vec![fx("limiter")];

        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let chain: Vec<&str> = snap.audio_tracks[0].clips[0].audio_effects.iter().map(|e| e.effect_type.as_str()).collect();
        assert_eq!(chain, vec!["eq"], "the clip's own chain");
        let track: Vec<&str> = snap.audio_tracks[0].effects.iter().map(|e| e.effect_type.as_str()).collect();
        assert_eq!(track, vec!["limiter"], "the track's, on the track, for its summed clips (#2560)");
        let exported = klipp_timeline::test_clip::effects(&snap.video.sequence.tracks[0].clips[0]);
        assert!(matches!(exported.as_slice(), [E::Opacity { .. }, E::Vignette { .. }]), "{exported:?}");
        let viewer = klipp_timeline::flatten::flatten_sequence(&seq, std::slice::from_ref(&seq));
        assert_eq!(format!("{:?}", klipp_timeline::test_clip::effects(&viewer.tracks[0].clips[0])), format!("{exported:?}"), "the viewer's render copy has the same");
    }

    /// #642: a retimed clip's sound is in the export, read at its speed
    /// from its own range, as playback reads it; a ramped one, which has
    /// no one speed, is left out. The clip at 1x is read as ever.
    #[test]
    fn a_retimed_clips_sound_is_read_at_its_speed_and_a_ramp_left_out() {
        let (talk, music, ramp) = (MediaId::new(), MediaId::new(), MediaId::new());
        let mut pool = MediaPool::new();
        pool.add(make_media_item(talk, "/media/talk.mp4"));
        pool.add(make_media_item(music, "/media/music.wav"));
        pool.add(make_media_item(ramp, "/media/ramp.mp4"));
        let clip = |media_id, speed: f64| Clip {
            kind: ClipKind::Media { media_id },
            source_in: make_rational(24, 24),
            source_out: make_rational(120, 24),
            speed,
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        };
        let mut seq = make_sequence();
        seq.tracks[1].clips.push(clip(talk, -2.0));
        let mut bed = Track::new("A2", TrackKind::Audio);
        bed.clips.push(clip(music, 1.0));
        seq.tracks.push(bed);
        let mut ramped = Track::new("A3", TrackKind::Audio);
        let mut r = clip(ramp, 1.0);
        r.speed_keyframes = vec![klipp_core::Keyframe { time_secs: 0.0, value: 2.0, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }];
        ramped.clips.push(r);
        seq.tracks.push(ramped);
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let heard: Vec<_> = snap.audio_tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.path.to_string_lossy().to_string(), c.varispeed)).collect();
        assert_eq!(heard.len(), 2, "the ramp left out: {heard:?}");
        let read_of = |path: &str| heard.iter().find(|(p, _)| p == path).map(|(_, v)| *v).unwrap_or_else(|| panic!("{path} not heard: {heard:?}"));
        let v = read_of("/media/talk.mp4").expect("the reversed 2x clip is read at its speed");
        assert_eq!((v.speed, v.source_in, v.source_out), (-2.0, 1.0, 5.0));
        // Reversed, its first moment is its range's end, a second in two
        // seconds back.
        assert!((v.source_at(0.0) - 5.0).abs() < 1e-9 && (v.source_at(1.0) - 3.0).abs() < 1e-9);
        assert_eq!(read_of("/media/music.wav"), None, "the bed at 1x");
    }

    #[test]
    fn snapshot_collects_subtitle_tracks() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let mut seq = make_sequence();
        // Add a video clip so duration is non-zero
        seq.tracks[0].clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(120, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(120, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(120, 24))
        });

        // Add a subtitle track with two clips
        let mut sub_track = klipp_timeline::Track::new("Subtitles", TrackKind::Subtitle);
        sub_track.clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
                let tb = klipp_timeline::TextBox {
                    text: "Hello World".to_string(),
                    ..Default::default()
                };
                klipp_timeline::TitleData::single_text(tb)
            }),
            timeline_in: make_rational(24, 24),
            timeline_out: make_rational(72, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(48, 24),
            ..Clip::gap(make_rational(24, 24), make_rational(72, 24))
        });
        sub_track.clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
                let tb = klipp_timeline::TextBox {
                    text: "Second subtitle".to_string(),
                    ..Default::default()
                };
                klipp_timeline::TitleData::single_text(tb)
            }),
            timeline_in: make_rational(96, 24),
            timeline_out: make_rational(120, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            ..Clip::gap(make_rational(96, 24), make_rational(120, 24))
        });
        seq.tracks.push(sub_track);

        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(snap.subtitle_tracks.len(), 1);
        assert_eq!(snap.subtitle_tracks[0].clips.len(), 2);
        assert_eq!(snap.subtitle_tracks[0].clips[0].text, "Hello World");
        assert_eq!(snap.subtitle_tracks[0].clips[0].timeline_start_frame, 24);
        assert_eq!(snap.subtitle_tracks[0].clips[0].timeline_end_frame, 72);
        assert_eq!(snap.subtitle_tracks[0].clips[1].text, "Second subtitle");
        assert_eq!(snap.subtitle_tracks[0].clips[1].timeline_start_frame, 96);
        assert_eq!(snap.subtitle_tracks[0].clips[1].timeline_end_frame, 120);
    }

    #[test]
    fn snapshot_subtitle_tracks_present_even_without_burn_in() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));

        let mut seq = make_sequence();
        seq.tracks[0].clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(48, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(48, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(48, 24))
        });

        let mut sub_track = klipp_timeline::Track::new("Subs", TrackKind::Subtitle);
        sub_track.clips.push(Clip {
            id: ClipId::new(),
            kind: ClipKind::Title({
                let tb = klipp_timeline::TextBox {
                    text: "Test".to_string(),
                    ..Default::default()
                };
                klipp_timeline::TitleData::single_text(tb)
            }),
            timeline_in: make_rational(0, 24),
            timeline_out: make_rational(24, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(24, 24),
            ..Clip::gap(make_rational(0, 24), make_rational(24, 24))
        });
        seq.tracks.push(sub_track);

        // burn_in OFF — subtitle data should still be in subtitle_tracks
        let mut settings = make_settings();
        settings.burn_in_subtitles = false;
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();

        assert_eq!(snap.subtitle_tracks.len(), 1, "subtitle_tracks should be populated even without burn-in");
        assert_eq!(snap.subtitle_tracks[0].clips[0].text, "Test");
    }

    #[test]
    fn snapshot_no_subtitle_tracks_when_none_exist() {
        let pool = MediaPool::new();
        let seq = make_sequence();
        let settings = make_settings();
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        assert!(snap.subtitle_tracks.is_empty());
    }

    fn media_clip(media_id: MediaId, from: i64, to: i64) -> Clip {
        Clip {
            id: ClipId::new(),
            kind: ClipKind::Media { media_id },
            timeline_in: make_rational(from, 24),
            timeline_out: make_rational(to, 24),
            source_in: make_rational(0, 24),
            source_out: make_rational(to - from, 24),
            ..Clip::gap(make_rational(from, 24), make_rational(to, 24))
        }
    }

    /// The video half is the viewer's flattened timeline, and every media
    /// clip's file is in the media table.
    #[test]
    fn video_timeline_is_the_flattened_sequence_with_its_media() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));
        let mut seq = make_sequence();
        seq.tracks[0].clips.push(media_clip(media_id, 0, 48));
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let flat = klipp_timeline::flatten::flatten_sequence(&seq, &[]);
        assert_eq!(snap.video.sequence.tracks.len(), flat.tracks.len());
        assert_eq!(snap.video.sequence.tracks[0].clips[0].id, seq.tracks[0].clips[0].id);
        assert_eq!(
            snap.video.media.get(&media_id),
            Some(&MediaFile { path: PathBuf::from("/media/clip1.mp4"), log_format: None, source_rotation: 0, duration_secs: Some(pool.get(&media_id).unwrap().duration.to_secs_f64()), fps: Some((pool.get(&media_id).unwrap().fps.num, pool.get(&media_id).unwrap().fps.den)), resolution: Some((pool.get(&media_id).unwrap().resolution.width, pool.get(&media_id).unwrap().resolution.height)), scene_transfer: None }),
        );
    }

    /// #237: the export's picture runs a grade group's members with the
    /// group's grade around their own, as the viewer does.
    #[test]
    fn the_export_runs_a_grade_groups_grade() {
        let media_id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(make_media_item(media_id, "/media/clip1.mp4"));
        let mut seq = make_sequence();
        let mut group = klipp_timeline::GradeGroup::new("Scene");
        let mut post = klipp_nodes::NodeGraph::default();
        let node = post.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).map(|n| {
            n.params.exposure = Some(-1.0);
            n.id
        }).unwrap();
        group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(post));
        let mut clip = media_clip(media_id, 0, 48);
        clip.grade_group = Some(group.id);
        seq.grade_groups = vec![group];
        seq.tracks[0].clips.push(clip);
        let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
        let graph = klipp_timeline::GradeAccess::grade(&snap.video.sequence.tracks[0].clips[0]);
        assert!(graph.nodes.iter().any(|n| n.id == node), "the group's post-clip grade is in the exported clip's grade");
        assert!(klipp_timeline::GradeAccess::grade(&seq.tracks[0].clips[0]).nodes.iter().all(|n| n.id != node), "and not in the document's");
    }

    /// The export decoder gets the same log format the viewer's decoder
    /// registry holds: the user's choice, else detection; and nothing for
    /// media that is not camera log, including an explicit sRGB override
    /// of a detected log.
    #[test]
    fn media_table_carries_the_effective_camera_log_format() {
        let cases: [(LogFormat, bool, LogFormat, Option<LogFormat>); 4] = [
            (LogFormat::Srgb, false, LogFormat::SLog3, Some(LogFormat::SLog3)), // detected
            (LogFormat::VLog, true, LogFormat::SLog3, Some(LogFormat::VLog)),   // user's choice wins
            (LogFormat::Srgb, true, LogFormat::SLog3, None),                    // explicit sRGB override
            (LogFormat::Srgb, false, LogFormat::PQ, None),                      // PQ: the file says so
        ];
        for (chosen, user_set, detected, want) in cases {
            let media_id = MediaId::new();
            let mut item = make_media_item(media_id, "/media/log.mp4");
            item.log_format = chosen;
            item.log_format_user_set = user_set;
            item.detected_log_format = detected;
            let mut pool = MediaPool::new();
            pool.add(item);
            let mut seq = make_sequence();
            seq.tracks[0].clips.push(media_clip(media_id, 0, 24));
            let snap = SequenceSnapshot::from_sequence(&seq, &pool, &make_settings(), None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
            assert_eq!(
                snap.video.media[&media_id].log_format, want,
                "chosen {chosen:?} (user set {user_set}), detected {detected:?}",
            );
        }
    }

    /// Subtitle tracks render into the picture only when burning captions
    /// in; titles on a text track are burnt in either way (#2426).
    #[test]
    fn subtitle_tracks_are_hidden_unless_burned_in() {
        let mut seq = make_sequence();
        seq.tracks.insert(0, Track::new("Subtitles", TrackKind::Subtitle));
        seq.tracks.insert(0, Track::new("Titles", TrackKind::Text));
        let pool = MediaPool::new();
        for burn in [true, false] {
            let mut settings = make_settings();
            settings.burn_in_subtitles = burn;
            let snap = SequenceSnapshot::from_sequence(&seq, &pool, &settings, None, false, klipp_core::WorkingColorspace::Rec709Linear).unwrap();
            let sub = snap.video.sequence.tracks.iter().find(|t| t.kind == TrackKind::Subtitle).unwrap();
            assert_eq!(sub.visible, burn);
            let titles = snap.video.sequence.tracks.iter().find(|t| t.kind == TrackKind::Text).unwrap();
            assert!(titles.visible, "titles stay with burn_in_subtitles {burn}");
        }
    }

    /// A media clip whose file is missing from the pool cannot be exported.
    #[test]
    fn missing_media_is_an_error() {
        let mut seq = make_sequence();
        seq.tracks[0].clips.push(media_clip(MediaId::new(), 0, 24));
        assert!(SequenceSnapshot::from_sequence(&seq, &MediaPool::new(), &make_settings(), None, false, klipp_core::WorkingColorspace::Rec709Linear).is_err());
    }
}
