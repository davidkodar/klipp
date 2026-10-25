//! Export: the dialog and its settings, the checks before a render (media
//! offline, a source about to be written over), the render queue's jobs,
//! one per marker segment when asked, and the presets (#826).

use super::*;

/// Height reserved at the foot of the Export workspace's column for its
/// action bar. Enough for one row of regular controls plus the rule
/// above it and breathing room either side.
/// The summary line above the Export buttons: one row of small text
/// plus the gap under it.
pub(super) const EXPORT_SUMMARY_H: f32 = 18.0;

/// The Export panel's pinned foot at a comfortable size. Tall enough
/// for the summary line *and* the buttons — it was 48, sized for the
/// buttons alone, and adding the line above them pushed them out of
/// the reserved strip and under the status bar.
pub(super) const EXPORT_ACTION_BAR_H: f32 = 48.0 + EXPORT_SUMMARY_H;

/// How tall the Export foot is for a panel of `available_h`, and
/// whether the summary line fits inside it.
///
/// The buttons are the point of that strip: Export is the panel's
/// reason to exist, and a primary action sliced in half by the status
/// bar is worse than no summary at all. So when there is not room for
/// both, the summary goes and the buttons keep their full height —
/// and the strip is never taller than what it was given, whatever the
/// window is doing.
///
/// Half the panel is the most the foot may take, so a short panel
/// still shows some of the form above it.
pub(super) fn export_foot_layout(available_h: f32) -> ExportFoot {
    let buttons = crate::theme::control::REGULAR;
    let both = EXPORT_SUMMARY_H + buttons;
    let cap = (available_h * 0.5).max(0.0);
    let (bar_h, show_summary) = if cap >= both {
        // Room for both: the comfortable height, clamped to the cap
        // and never below what the content needs.
        (EXPORT_ACTION_BAR_H.min(cap).max(both), true)
    } else {
        // Buttons only, whole — unless the panel is smaller than one
        // row, at which point there is nothing to be done but not
        // overflow.
        (buttons.min(available_h.max(0.0)), false)
    };
    // Clamped to the strip as well. A panel with no height at all —
    // mid-resize, or collapsed — has nowhere to put a button, and
    // claiming otherwise is how content ends up drawn past the edge.
    let content_h = if show_summary { both } else { buttons }.min(bar_h);
    // The top pad the drawing uses. Halved, because the content is
    // centred in the strip — and computed *here* rather than at the
    // call site, because that is where it went wrong: the padding was
    // worked out for the button row alone while the summary was drawn
    // above it, so the two together ran past the bottom of the strip.
    let pad = ((bar_h - content_h) * 0.5).max(0.0);
    ExportFoot { bar_h, pad, content_h, show_summary }
}

/// The Export foot's measurements: the strip, the pad above the
/// content, what the content comes to, and whether the summary is part
/// of it.
#[derive(Clone, Copy, Debug)]
pub(super) struct ExportFoot {
    pub(super) bar_h: f32,
    pub(super) pad: f32,
    pub(super) content_h: f32,
    pub(super) show_summary: bool,
}

pub(super) struct ExportDialogState {
    pub(super) output_path: String,
    pub(super) format: ExportFormat,
    pub(super) video_codec: ExportVideoCodec,
    pub(super) crf: u32,
    pub(super) preset: EncoderPreset,
    pub(super) audio_codec: ExportAudioCodec,
    pub(super) audio_bitrate: u32,
    pub(super) confirm_overwrite: bool,
    /// The export preferences say to overwrite a file already there
    /// without asking (`OverwriteMode::Always`); set as the dialog opens.
    pub(super) overwrite_without_asking: bool,
    /// Range selection for export.
    /// - 0 = Full timeline
    /// - 1 = Marked in/out range
    /// - 2 = Selected clips
    /// - 3 = Between markers (one file per range)
    pub(super) range_mode: u8,
    /// In multi-marker mode, include the segment from sequence start
    /// to the first marker as its own export.
    pub(super) include_pre_marker_segment: bool,
    /// In multi-marker mode, include the segment from the last marker
    /// to sequence end as its own export.
    pub(super) include_post_marker_segment: bool,
    pub(super) selected_preset_id: Option<String>,
    pub(super) save_preset_name: String,
    pub(super) show_save_preset: bool,
    /// Override resolution (None = use sequence resolution).
    pub(super) resolution_override: u8, // 0=Sequence, 1=720p, 2=1080p, 3=4K, 4=Custom
    pub(super) custom_width: u32,
    pub(super) custom_height: u32,
    /// Override frame rate (None = use sequence fps).
    pub(super) fps_override: u8, // 0=Sequence, 1=24, 2=25, 3=29.97, 4=30, 5=60
    pub(super) rate_control: klipp_core::RateControl,
    pub(super) video_bitrate_kbps: u32,
    pub(super) burn_in_subtitles: bool,
    pub(super) embed_subtitles: bool,
    /// Carry the subtitle track as CEA-608 captions in the video stream
    /// (#1663); H.264 and H.265 only.
    pub(super) captions_608: bool,
    /// HDR10+ dynamic metadata on an H.265 PQ export (#468).
    pub(super) hdr10plus: bool,
    /// Write sequence markers as chapter metadata in the output container
    /// (MP4/MKV/MOV only — ignored for WebM). Opt-in; default false.
    pub(super) embed_chapters: bool,
    pub(super) normalize_loudness: bool,
    /// 0=YouTube/Spotify (-14), 1=Podcast (-16), 2=EBU R128 (-23), 3=ATSC A/85 (-24), 4=Custom
    pub(super) loudness_preset: u8,
    pub(super) custom_lufs: f32,
    /// The true-peak ceiling with a custom loudness target, dBTP: a
    /// preset's or a delivery spec's, kept rather than dropped to the
    /// default (#2650).
    pub(super) custom_true_peak: f32,
    /// Encoder bit depth. 0 = codec default (8-bit), 10 = 10-bit encode.
    /// Only meaningful for H.264, H.265 and AV1. (#65, #560)
    pub(super) bit_depth: u8,
    /// Whether an 8-bit file of the sequence will band (#2074).
    pub(super) banding: super::export_banding::BandingProbe,
    /// HDR transfer. Only surfaced in UI when bit_depth == 10 on H.264, H.265 or AV1. (#65)
    pub(super) hdr_mode: klipp_core::HdrMode,
    /// The delivery spec picked (#2650), by id, and the last check
    /// against it: what was checked, and each requirement.
    pub(super) delivery_spec: Option<String>,
    pub(super) delivery_result: Option<(String, Vec<klipp_core::delivery_spec::Finding>)>,
    /// A finished file's check, run on a worker: its answer when done.
    pub(super) delivery_file_check: std::sync::Arc<std::sync::Mutex<Option<Result<(String, Vec<klipp_core::delivery_spec::Finding>), String>>>>,
    pub(super) delivery_busy: bool,
    /// The name being typed for a spec of the person's own, while it is.
    pub(super) delivery_save_name: Option<String>,
    /// Display-boundary tone-map operator picked by the user.
    /// Default Hable matches prior behaviour.
    pub(super) tone_map_op: klipp_core::ToneMapOp,
    /// Output Display Transform — picks the delivery-target colour
    /// space of the rendered file. Default `MatchViewer` mirrors what
    /// the user has been seeing while editing.
    pub(super) odt: klipp_core::OdtChoice,
    /// The project's default output transform the dialog last took
    /// (#466): a new default, or another project's, is taken again; the
    /// user's choice in the dialog stands until then.
    pub(super) odt_followed: Option<klipp_core::OdtChoice>,
    /// Mastering peak for a PQ delivery, cd/m². Only surfaced when the
    /// delivery is PQ through the viewer's own picture — HLG has a
    /// nominal peak of its own and the ACES transforms are named for
    /// theirs, so neither offers the choice.
    pub(super) mastering_peak_nits: f32,
}

impl Default for ExportDialogState {
    fn default() -> Self {
        Self {
            output_path: String::new(),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            crf: 23,
            preset: EncoderPreset::default(),
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate: 128,
            confirm_overwrite: false,
            overwrite_without_asking: false,
            range_mode: 0,
            include_pre_marker_segment: true,
            include_post_marker_segment: true,
            selected_preset_id: None,
            save_preset_name: String::new(),
            show_save_preset: false,
            resolution_override: 0,
            custom_width: 1920,
            custom_height: 1080,
            fps_override: 0,
            rate_control: klipp_core::RateControl::default(),
            video_bitrate_kbps: 10000,
            burn_in_subtitles: true,
            embed_subtitles: false,
            captions_608: false, hdr10plus: false,
            embed_chapters: false,
            normalize_loudness: false,
            loudness_preset: 0,
            custom_lufs: -14.0,
            custom_true_peak: klipp_core::DEFAULT_TRUE_PEAK_DBTP,
            bit_depth: 0,
            banding: Default::default(),
            hdr_mode: klipp_core::HdrMode::Sdr,
            delivery_spec: None,
            delivery_result: None,
            delivery_file_check: Default::default(),
            delivery_busy: false,
            delivery_save_name: None,
            tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer,
            odt_followed: None,
            mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
        }
    }
}

/// Seconds, as a length someone reads rather than a number.
pub(super) fn humanise_secs(secs: f64) -> String {
    // Clamped once, at the top. Clamping only the minute count let a
    // negative range — which an in-point dragged past its out-point
    // produces — fall through to the seconds branch and print itself.
    let secs = secs.max(0.0);
    let total = secs.round() as u64;
    let (m, sec) = (total / 60, total % 60);
    if m >= 60 {
        format!("{} h {:02} min {:02} s", m / 60, m % 60, sec)
    } else if m > 0 {
        format!("{m} min {sec:02} s")
    } else {
        format!("{secs:.1} s")
    }
}

pub(super) fn humanise_bytes(bytes: f64) -> String {
    if bytes >= 1_000_000_000.0 {
        format!("{:.1} GB", bytes / 1_000_000_000.0)
    } else {
        format!("{:.0} MB", bytes / 1_000_000.0)
    }
}

pub(super) fn dialog_resolution(dialog: &ExportDialogState) -> Option<klipp_core::Resolution> {
    match dialog.resolution_override {
        1 => Some(klipp_core::Resolution { width: 1280, height: 720 }),
        2 => Some(klipp_core::Resolution { width: 1920, height: 1080 }),
        3 => Some(klipp_core::Resolution { width: 3840, height: 2160 }),
        4 => Some(klipp_core::Resolution { width: dialog.custom_width, height: dialog.custom_height }),
        _ => None, // 0 = use sequence
    }
}

pub(super) fn dialog_fps(dialog: &ExportDialogState) -> Option<klipp_core::Fps> {
    match dialog.fps_override {
        1 => Some(klipp_core::Fps::new(24, 1)),
        2 => Some(klipp_core::Fps::new(25, 1)),
        3 => Some(klipp_core::Fps::new(30000, 1001)),
        4 => Some(klipp_core::Fps::new(30, 1)),
        5 => Some(klipp_core::Fps::new(60, 1)),
        _ => None, // 0 = use sequence
    }
}

/// Compute the frame range covering all selected clips. Returns None if no clips selected.
/// Note: this is a time range, not a clip filter — unselected clips that overlap the range
/// will still render, matching how in-to-out export works.
/// Replace characters illegal on Linux/Windows/macOS filenames with
/// underscores; trim leading/trailing whitespace and dots; fall back
/// to "segment" when the result would be empty.
pub(super) fn sanitise_filename(label: &str) -> String {
    let sanitised: String = label.chars().map(|c| match c {
        '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
        c => c,
    }).collect();
    let sanitised = sanitised.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if sanitised.is_empty() {
        "segment".to_string()
    } else {
        sanitised
    }
}

/// Where an export goes when nothing says otherwise: the user's Videos
/// folder where there is one, as every NLE does, else the home directory.
/// Where a new export goes: the export preferences' folder while it is
/// there, else the default (`default_export_dir`).
pub(super) fn export_folder(prefs: &crate::prefs::ExportPrefs) -> PathBuf {
    prefs.default_output_dir.clone().filter(|d| d.is_dir()).unwrap_or_else(default_export_dir)
}

/// The output path the Export dialog opens with, by the export
/// preferences' overwrite mode, and whether a file already there is
/// written over without asking. Ask keeps the path and asks at Export;
/// Always keeps it and does not ask; Never moves it to the next free
/// name (`name_1`, `name_2`, …) so nothing is written over.
pub(super) fn apply_overwrite_mode(mode: crate::prefs::OverwriteMode, path: &str) -> (String, bool) {
    use crate::prefs::OverwriteMode;
    match mode {
        OverwriteMode::Ask => (path.to_string(), false),
        OverwriteMode::Always => (path.to_string(), true),
        OverwriteMode::Never => {
            let p = std::path::Path::new(path);
            if !path.trim().is_empty() && p.exists() {
                (next_free_name(p, |c| c.exists()).to_string_lossy().to_string(), false)
            } else {
                (path.to_string(), false)
            }
        }
    }
}

pub(super) fn default_export_dir() -> PathBuf {
    dirs::video_dir()
        .filter(|d| d.is_dir())
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Build a unique export path under `base_dir` by suffixing `_2`, `_3`, …
/// when a candidate name has already been used in this batch. Tracks
/// taken names in `used` (caller owns the set).
pub(super) fn unique_export_path(
    base_dir: &std::path::Path,
    stem: &str,
    ext: &str,
    used: &mut std::collections::HashSet<String>,
) -> std::path::PathBuf {
    let mut candidate = format!("{}.{}", stem, ext);
    let mut counter = 2u32;
    while used.contains(&candidate) {
        candidate = format!("{}_{}.{}", stem, counter, ext);
        counter += 1;
    }
    used.insert(candidate.clone());
    base_dir.join(candidate)
}

/// Convert a Timecode to a frame number at the given fps. Mirrors the
/// helper in `klipp-render/src/snapshot.rs:693` (private there).
pub(super) fn timecode_to_frame(rt: &klipp_core::RationalTime, fps: f64) -> u64 {
    (rt.to_secs_f64() * fps).round() as u64
}

/// Multi-export helper: build one segment per range marker. Point markers
/// (`duration == None`) are skipped when explicit ranges are present. Used
/// by `enqueue_marker_segments` when at least one marker has a duration.
pub(super) fn build_range_marker_segments(
    markers: &[klipp_timeline::Marker],
    fps: f64,
) -> Vec<(u64, u64, String)> {
    let mut out = Vec::new();
    let mut sorted: Vec<&klipp_timeline::Marker> = markers.iter().collect();
    sorted.sort_by_key(|m| timecode_to_frame(&m.position, fps));
    for m in sorted {
        let Some(dur) = m.duration else { continue };
        let start = timecode_to_frame(&m.position, fps);
        let end = timecode_to_frame(&(m.position + dur), fps);
        if end <= start {
            continue; // zero-length or negative — skip
        }
        out.push((start, end, m.label.clone()));
    }
    out
}

/// Multi-export helper: treat consecutive point markers as segment
/// boundaries (Kdenlive convention — each segment is named after the
/// marker that ends it). The synthetic "start"/"end" pseudo-labels are
/// recognised by the caller and trigger the numeric segment_N fallback.
pub(super) fn build_inferred_marker_segments(
    markers: &[klipp_timeline::Marker],
    fps: f64,
    seq_end_frame: u64,
    include_pre: bool,
    include_post: bool,
) -> Vec<(u64, u64, String)> {
    let mut sorted: Vec<&klipp_timeline::Marker> = markers.iter().collect();
    sorted.sort_by_key(|m| timecode_to_frame(&m.position, fps));

    let mut boundaries: Vec<(u64, String)> = Vec::new();
    if include_pre {
        boundaries.push((0, "start".to_string()));
    } else if let Some(first) = sorted.first() {
        boundaries.push((timecode_to_frame(&first.position, fps), first.label.clone()));
    }
    for m in sorted.iter().skip(if include_pre { 0 } else { 1 }) {
        boundaries.push((timecode_to_frame(&m.position, fps), m.label.clone()));
    }
    if include_post {
        boundaries.push((seq_end_frame, "end".to_string()));
    }

    let mut segments = Vec::new();
    for i in 0..boundaries.len().saturating_sub(1) {
        let (start, _) = &boundaries[i];
        let (end, label) = &boundaries[i + 1];
        if *end <= *start {
            continue;
        }
        segments.push((*start, *end, label.clone()));
    }
    segments
}

pub(super) fn selected_clips_frame_range(
    selected: &[klipp_core::ClipId],
    seq: &klipp_timeline::Sequence,
    fps: f64,
) -> Option<(u64, u64)> {
    if selected.is_empty() {
        return None;
    }
    let mut min_in = f64::MAX;
    let mut max_out = f64::MIN;
    for track in &seq.tracks {
        for clip in &track.clips {
            if selected.contains(&clip.id) {
                let t_in = clip.timeline_in.to_secs_f64();
                let t_out = clip.timeline_out.to_secs_f64();
                if t_in < min_in { min_in = t_in; }
                if t_out > max_out { max_out = t_out; }
            }
        }
    }
    if min_in >= max_out {
        return None;
    }
    let start = (min_in * fps).round() as u64;
    let end = (max_out * fps).ceil() as u64;
    Some((start, end))
}

/// The true-peak ceiling the chosen loudness spec asks for: -2 dBTP for
/// ATSC A/85, and the renderer's -1 for the rest.
pub(super) fn dialog_true_peak(dialog: &ExportDialogState) -> Option<f32> {
    match (dialog.normalize_loudness, dialog.loudness_preset) {
        (true, 3) => Some(-2.0),
        // A custom target keeps its own ceiling, the default unless a
        // preset or a spec set another.
        (true, 4) => (dialog.custom_true_peak != klipp_core::DEFAULT_TRUE_PEAK_DBTP).then_some(dialog.custom_true_peak),
        _ => None,
    }
}

/// The loudness and true-peak ceiling playback is heard at (#1565): the
/// export dialog's when it normalises, else the sequence brief's, else
/// none; none too when the preference is off.
pub(super) fn delivery_target(dialog: &ExportDialogState, brief_lufs: Option<f32>, enabled: bool) -> Option<(f32, f32)> {
    if !enabled {
        return None;
    }
    match dialog_target_lufs(dialog) {
        Some(lufs) => Some((lufs, dialog_true_peak(dialog).unwrap_or(klipp_core::DEFAULT_TRUE_PEAK_DBTP))),
        None => brief_lufs.map(|l| (l, klipp_core::DEFAULT_TRUE_PEAK_DBTP)),
    }
}

pub(super) fn dialog_target_lufs(dialog: &ExportDialogState) -> Option<f32> {
    if !dialog.normalize_loudness { return None; }
    let lufs = match dialog.loudness_preset {
        0 => -14.0, // YouTube / Spotify
        1 => -16.0, // Podcast
        2 => -23.0, // EBU R128 broadcast
        3 => -24.0, // ATSC A/85 broadcast
        _ => dialog.custom_lufs,
    };
    Some(lufs)
}

/// Build ExportSettings from the current dialog state. Single source of truth
/// for all export buttons (Add to Queue, Export, Overwrite) — eliminates the
/// bug where Export/Overwrite previously hardcoded resolution/fps to None.
///
/// `pan_law` comes from Preferences rather than the dialog: it is a
/// project-wide mix setting, and it is a parameter rather than a lookup
/// so that every export button has to supply it. The export used to have
/// one law compiled into it and rendered 3 dB below playback.
pub(super) fn build_export_settings(
    dialog: &ExportDialogState,
    pan_law: klipp_core::PanLaw,
) -> ExportSettings {
    ExportSettings {
        pan_law,
        output_path: PathBuf::from(&dialog.output_path),
        format: dialog.format,
        video_codec: dialog.video_codec,
        resolution: dialog_resolution(dialog),
        fps: dialog_fps(dialog),
        crf: dialog.crf,
        preset: dialog.preset,
        audio_codec: dialog.audio_codec,
        audio_bitrate_kbps: Some(dialog.audio_bitrate),
        video_bitrate_kbps: if dialog.rate_control != klipp_core::RateControl::Crf {
            Some(dialog.video_bitrate_kbps)
        } else {
            None
        },
        rate_control: dialog.rate_control,
        burn_in_subtitles: dialog.burn_in_subtitles,
        embed_subtitles: dialog.embed_subtitles,
        target_lufs: dialog_target_lufs(dialog),
        true_peak_dbtp: dialog_true_peak(dialog),
        embed_chapters: dialog.embed_chapters,
        captions_608: dialog.captions_608 && matches!(dialog.video_codec, klipp_core::ExportVideoCodec::H264 | klipp_core::ExportVideoCodec::H265),
        hdr10plus: dialog.hdr10plus && dialog.video_codec == klipp_core::ExportVideoCodec::H265,
        bit_depth: dialog.bit_depth,
        hdr_mode: dialog.hdr_mode,
        tone_map_op: dialog.tone_map_op,
        odt: dialog.odt,
        mastering_peak_nits: dialog.mastering_peak_nits,
    }
}

/// A preset as the list names it: a user's marked ⚙, a 10-bit one
/// marked so (#2074).
pub(super) fn preset_label(preset: &ExportPreset) -> String {
    let mark = if preset.built_in { "" } else { "⚙ " };
    let depth = if preset.settings.is_10bit() { " · 10-bit" } else { "" };
    format!("{mark}{}{depth}", preset.name)
}

/// The dialog's settings as a preset holds them, with the frame size and
/// rate it sets: what a delivery check reads of an export (#2650).
pub(super) fn dialog_settings(d: &ExportDialogState) -> ExportPresetSettings {
    let r = |width, height| Some(klipp_core::Resolution { width, height });
    ExportPresetSettings {
        format: d.format,
        video_codec: d.video_codec,
        resolution: match d.resolution_override {
            1 => r(1280, 720),
            2 => r(1920, 1080),
            3 => r(3840, 2160),
            4 => r(d.custom_width, d.custom_height),
            _ => None,
        },
        fps: match d.fps_override {
            1 => Some(klipp_core::Fps::new(24, 1)),
            2 => Some(klipp_core::Fps::new(25, 1)),
            3 => Some(klipp_core::Fps::new(30000, 1001)),
            4 => Some(klipp_core::Fps::new(30, 1)),
            5 => Some(klipp_core::Fps::new(60, 1)),
            _ => None,
        },
        crf: d.crf,
        preset: d.preset,
        audio_codec: d.audio_codec,
        audio_bitrate_kbps: Some(d.audio_bitrate),
        video_bitrate_kbps: if d.rate_control != klipp_core::RateControl::Crf { Some(d.video_bitrate_kbps) } else { None },
        rate_control: d.rate_control,
        target_lufs: dialog_target_lufs(d),
        true_peak_dbtp: dialog_true_peak(d),
        bit_depth: d.bit_depth,
    }
}

/// A delivery spec's export on the dialog (#2650): its settings, then its
/// colour, HDR at the ten bits it needs (the preset's own depth is taken
/// only in SDR).
pub(super) fn apply_spec_to_dialog(dialog: &mut ExportDialogState, spec: &klipp_core::delivery_spec::DeliverySpec) {
    dialog.hdr_mode = klipp_core::HdrMode::Sdr;
    apply_preset_to_dialog(dialog, &spec.export);
    dialog.hdr_mode = spec.hdr;
    if spec.hdr != klipp_core::HdrMode::Sdr {
        dialog.bit_depth = spec.export.bit_depth.max(10);
    }
}

pub(super) fn apply_preset_to_dialog(dialog: &mut ExportDialogState, settings: &ExportPresetSettings) {
    dialog.format = settings.format;
    // A preset saved before the container rules may hold a pair FFmpeg
    // refuses (H.264 in WebM); it opens fitted to its container.
    (dialog.video_codec, dialog.audio_codec) = settings.format.fit(settings.video_codec, settings.audio_codec);
    dialog.crf = settings.crf;
    dialog.preset = settings.preset;
    dialog.audio_bitrate = settings.audio_bitrate_kbps.unwrap_or(192);
    dialog.rate_control = settings.rate_control;
    dialog.video_bitrate_kbps = settings.video_bitrate_kbps.unwrap_or(10000);
    // The preset's depth (#2074), except that HDR stays at the ten bits it
    // needs.
    if dialog.hdr_mode == klipp_core::HdrMode::Sdr {
        dialog.bit_depth = settings.bit_depth;
    }
    // Map resolution preset to override enum
    dialog.resolution_override = match settings.resolution {
        Some(klipp_core::Resolution { width: 1280, height: 720 }) => 1,
        Some(klipp_core::Resolution { width: 1920, height: 1080 }) => 2,
        Some(klipp_core::Resolution { width: 3840, height: 2160 }) => 3,
        Some(r) => { dialog.custom_width = r.width; dialog.custom_height = r.height; 4 }
        None => 0,
    };
    // Map fps preset to override enum
    dialog.fps_override = match settings.fps {
        Some(f) if (f.as_f64() - 24.0).abs() < 0.01 => 1,
        Some(f) if (f.as_f64() - 25.0).abs() < 0.01 => 2,
        Some(f) if (f.as_f64() - 29.97).abs() < 0.02 => 3,
        Some(f) if (f.as_f64() - 30.0).abs() < 0.01 => 4,
        Some(f) if (f.as_f64() - 60.0).abs() < 0.01 => 5,
        Some(_) => 0, // Unknown fps — fall back to Sequence
        None => 0,
    };
    match settings.target_lufs {
        Some(lufs) => {
            dialog.normalize_loudness = true;
            dialog.loudness_preset = match lufs as i32 {
                -14 => 0,
                -16 => 1,
                -23 => 2,
                -24 => 3,
                _ => {
                    dialog.custom_lufs = lufs;
                    dialog.custom_true_peak = settings.true_peak_dbtp.unwrap_or(klipp_core::DEFAULT_TRUE_PEAK_DBTP);
                    4
                }
            };
        }
        None => { dialog.normalize_loudness = false; }
    }
}

/// How far an export has been confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Confirmed {
    /// Asked for, nothing confirmed yet.
    Nothing,
    /// Media offline was shown and the export asked for anyway.
    Offline,
    /// A source about to be written over was shown and accepted too.
    Overwrite,
}

/// What comes next for an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExportStep {
    /// Ask about these clips, offline: file name and timeline position.
    AskOffline(Vec<(String, String)>),
    /// Ask about writing over this source file.
    AskOverwrite(String),
    /// Nothing stands in the way: render.
    Queue,
}

/// The rule before a render. Media offline is asked about first, then a
/// source about to be written over, each until it is confirmed. A
/// confirmed offline warning does not pass the overwrite check: writing a
/// render over a file the timeline is decoding destroys both, so only its
/// own confirmation lets it through.
pub(super) fn export_step(confirmed: Confirmed, offline: Vec<(String, String)>, written_over: Option<String>) -> ExportStep {
    if confirmed == Confirmed::Nothing && !offline.is_empty() {
        return ExportStep::AskOffline(offline);
    }
    match written_over {
        Some(name) if confirmed != Confirmed::Overwrite => ExportStep::AskOverwrite(name),
        _ => ExportStep::Queue,
    }
}

/// One line per clip whose tracked subject matte falls short of what a
/// render reads (#239), and per piece transition cutting round a subject
/// whose matte does (#985 phase 3): its media's name and place, and how
/// far the matte is, or the pass's percent while it runs (`running`);
/// with the command that tracks the rest the same way.
pub(super) fn matte_gap_lines(
    gaps: &[klipp_compose::subject_matte::Uncovered],
    seq: &klipp_timeline::Sequence,
    pool: &klipp_media::MediaPool,
    running: impl Fn(klipp_core::ClipId) -> Option<f32>,
) -> Vec<(klipp_core::ClipId, AppCommand, String)> {
    let mut out: Vec<(klipp_core::ClipId, AppCommand, String)> = Vec::new();
    let mut seen = Vec::new();
    for gap in gaps {
        let key = (gap.clip, gap.transition.map(|t| t.0));
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let Some(clip) = seq.all_clips().find(|c| c.id == gap.clip) else { continue };
        let name = clip.media_id().and_then(|m| pool.get(&m)).map_or_else(|| "Clip".to_string(), |i| i.filename.clone());
        let how = match running(gap.clip) {
            Some(p) => format!("tracking, {:.0}%", (p * 100.0).clamp(0.0, 100.0)),
            None => format!("{} of {} frames tracked", gap.covered, gap.needed),
        };
        let (track, place) = match gap.transition {
            Some((transition_id, _)) => (AppCommand::TrackTransitionSubject { transition_id }, " (transition)"),
            None => {
                // Bound by its model (#1881), or by the folder's.
                let model = match gap.subject {
                    Some(m) => if m == 1 { "foreground" } else { "subject" },
                    None => klipp_nodes::matte_sequence::read_manifest(&gap.folder).map_or("subject", |m| crate::subject_track::model_name(&m.model)),
                };
                (AppCommand::TrackSubject { clip_id: gap.clip, model: model.to_string(), into: Some(klipp_core::SubjectMatteUse::Grade) }, "")
            }
        };
        out.push((gap.clip, track, format!("{name} at {}{place}: {how}", clip.timeline_in.to_timecode_string())));
    }
    out
}

/// The tracked subject mattes `seq` reads that don't cover what it
/// reads: on clips' grades (#239) and cut by piece transitions (#985
/// phase 3), the latter found by the media as linked now.
pub(super) fn matte_gaps(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<klipp_compose::subject_matte::Uncovered> {
    klipp_compose::subject_matte::uncovered_with(seq, |clip, model| {
        let item = pool.get(&clip.media_id()?)?;
        klipp_compose::subject_matte::media_dir(&item.path, klipp_compose::subject_matte::decode_rotation(clip, item.source_rotation), model)
    }, |clip: &klipp_timeline::Clip| {
        let item = pool.get(&clip.media_id()?)?;
        Some(klipp_compose::subject_matte::last_frame_secs(item.duration.to_secs_f64(), item.fps.as_f64()))
    })
}

/// The clips of `seq` whose media is offline, by file name and timeline
/// position, for the warning before a render.
pub(super) fn offline_clips(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool) -> Vec<(String, String)> {
    seq.tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter_map(|clip| {
            let klipp_timeline::ClipKind::Media { media_id } = &clip.kind else { return None };
            let item = pool.get(media_id)?;
            item.offline.then(|| (item.filename.clone(), clip.timeline_in.to_timecode_string()))
        })
        .collect()
}

/// The file name of the source `output` would write over, if it is one
/// of `sources`. Both are canonicalised (`..`, symlinks); a path that does
/// not resolve (an export into a folder not made yet) is no conflict
/// rather than a guessed one.
pub(super) fn source_written_over<'a>(output: &std::path::Path, sources: impl IntoIterator<Item = &'a std::path::Path>) -> Option<String> {
    let canon_out = output.canonicalize().ok()?;
    sources.into_iter().find(|source| source.canonicalize().is_ok_and(|c| c == canon_out)).map(|source| {
        source.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| source.display().to_string())
    })
}

/// The first of `stem_1.ext`, `stem_2.ext` … beside `original` that is
/// free (`taken` says which are not), for Auto-Rename on the source
/// overwrite warning; `original` itself past 9999, so the warning comes
/// again rather than anything being written over.
pub(super) fn next_free_name(original: &std::path::Path, taken: impl Fn(&std::path::Path) -> bool) -> std::path::PathBuf {
    let parent = original.parent().unwrap_or(std::path::Path::new("."));
    let stem = original.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "output".into());
    let ext = original.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    (1..10000u32)
        .map(|n| parent.join(if ext.is_empty() { format!("{stem}_{n}") } else { format!("{stem}_{n}.{ext}") }))
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| original.to_path_buf())
}

/// Each marker segment's file and the label its job shows, from the path
/// the dialog chose: `<stem>_<marker label>.<ext>` beside it, or
/// `<stem>_segment_<n>` for the bookends and unnamed markers; each name
/// once, and `[n/total]` after it in the queue.
pub(super) fn segment_outputs(segments: &[(u64, u64, String)], base_path: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let ext = base_path.extension().and_then(|e| e.to_str()).unwrap_or("mp4").to_string();
    let base_dir = base_path.parent().unwrap_or(std::path::Path::new("."));
    let base_stem = base_path.file_stem().and_then(|s| s.to_str()).unwrap_or("export").to_string();
    let mut used_names = std::collections::HashSet::new();
    let total = segments.len();
    segments
        .iter()
        .enumerate()
        .map(|(i, (_, _, label))| {
            let name = if label.is_empty() || label == "start" || label == "end" {
                format!("{base_stem}_segment_{}", i + 1)
            } else {
                format!("{base_stem}_{}", sanitise_filename(label))
            };
            let output = unique_export_path(base_dir, &name, &ext, &mut used_names);
            let shown = format!("{} [{}/{total}]", output.file_name().and_then(|n| n.to_str()).unwrap_or("segment"), i + 1);
            (output, shown)
        })
        .collect()
}

impl KlippApp {
    /// The commands that export the sequence and look after its render
    /// queue and the export presets: `false` for any other.
    pub(super) fn handle_render_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::ShowRenderQueue => {
                self.state.show_render_queue = true;
                true
            }
            AppCommand::ClearCompletedJobs => {
                self.state.render_queue.clear_completed();
                true
            }
            AppCommand::ShowExportDialog => {
                self.state.show_export_dialog = true;
                self.prepare_export_defaults();
                // Restore last-used preset
                if self.export_dialog.selected_preset_id.is_none() {
                    if let Some(ref last_id) = self.prefs.export.last_preset_id {
                        self.export_dialog.selected_preset_id = Some(last_id.clone());
                        let all_presets = builtin_presets();
                        let custom = &self.prefs.export.custom_presets;
                        if let Some(preset) = all_presets.iter().chain(custom.iter()).find(|p| &p.id == last_id) {
                            apply_preset_to_dialog(&mut self.export_dialog, &preset.settings);
                        }
                    }
                }
                true
            }
            // The sequence exported, once nothing stands in the way: media
            // offline is asked about first, then a source about to be
            // written over (see `export_step`). The Force commands are
            // the answers to those two questions.
            AppCommand::ExportSequence { settings } => {
                self.export_or_ask(settings, Confirmed::Nothing);
                true
            }
            AppCommand::ForceExportSequence { settings } => {
                self.state.export_offline_warning = None;
                self.export_or_ask(settings, Confirmed::Offline);
                true
            }
            AppCommand::ForceExportSequenceOverwrite { settings } => {
                self.state.export_offline_warning = None;
                self.state.export_source_overwrite_warning = None;
                self.export_or_ask(settings, Confirmed::Overwrite);
                true
            }
            AppCommand::CancelExport => {
                // Cancel active render queue job
                if let Some(job) = self.state.render_queue.jobs.iter().find(|j| j.status == klipp_export::RenderJobStatus::Rendering) {
                    let id = job.id;
                    self.state.render_queue.cancel_job(id);
                }
                true
            }
            AppCommand::CancelExportJob { job_id } => {
                if let Ok(id) = uuid::Uuid::parse_str(job_id) {
                    self.state.render_queue.cancel_job(id);
                } else {
                    tracing::warn!("invalid render job ID {job_id:?} in cancellation command");
                }
                true
            }
            AppCommand::DeleteExportPreset { preset_id } => {
                let id = preset_id.clone();
                let before = self.prefs.export.custom_presets.len();
                self.prefs.export.custom_presets.retain(|p| p.id != id);
                if self.prefs.export.custom_presets.len() != before {
                    // If the deleted preset was the "last used" pointer,
                    // clear it so the export dialog reopens clean.
                    if self.prefs.export.last_preset_id.as_deref() == Some(id.as_str()) {
                        self.prefs.export.last_preset_id = None;
                    }
                    // Same for the active dialog selection.
                    if self.export_dialog.selected_preset_id.as_deref() == Some(id.as_str()) {
                        self.export_dialog.selected_preset_id = None;
                    }
                    self.prefs_dirty = true;
                    self.state.toast.show("Preset deleted".to_string());
                }
                true
            }
            AppCommand::RenameExportPreset { preset_id, new_name } => {
                let trimmed = new_name.trim().to_string();
                if !trimmed.is_empty() {
                    if let Some(p) = self.prefs.export.custom_presets
                        .iter_mut()
                        .find(|p| p.id == *preset_id && !p.built_in)
                    {
                        p.name = trimmed;
                        self.prefs_dirty = true;
                    }
                }
                true
            }
            AppCommand::DuplicateExportPreset { preset_id } => {
                let all_presets: Vec<klipp_core::ExportPreset> = klipp_core::builtin_presets()
                    .into_iter()
                    .chain(self.prefs.export.custom_presets.iter().cloned())
                    .collect();
                if let Some(src) = all_presets.iter().find(|p| &p.id == preset_id).cloned() {
                    let new_p = klipp_core::ExportPreset {
                        id: format!("custom-{}", uuid::Uuid::new_v4()),
                        name: format!("{} (copy)", src.name),
                        built_in: false,
                        settings: src.settings.clone(),
                    };
                    self.prefs.export.custom_presets.push(new_p);
                    self.prefs_dirty = true;
                    self.state.toast.show("Preset duplicated".to_string());
                }
                true
            }
            AppCommand::OpenExportDialogForSelectedClips => {
                // Dispatched by the clip-context "Export This Clip…"
                // gesture and the matching palette entry. We mirror
                // the Ctrl+E open path (path defaulting + dialog
                // state) and force the range radio to `2` (Selected
                // clips) — `selected_clips_frame_range` reads
                // `state.selected_clips` at export time and writes
                // the right `export_range` into the snapshot, so we
                // don't need to compute the range here.
                if self.state.selected_clips.is_empty() {
                    self.state.toast.show("No clips selected");
                    return true;
                }
                self.state.show_export_dialog = true;
                self.export_dialog.range_mode = 2;
                self.prepare_export_defaults();
                true
            }
            AppCommand::GenerateRenderScript { path } => {
                let path = path.clone();
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                // Use current export-dialog settings so users can tweak format/codec
                // via the normal Export dialog and then dump the equivalent script.
                let settings = build_export_settings(&self.export_dialog, self.prefs.audio.pan_law);

                // Compute output: same basename as script, but with the format's
                // native extension (mp4 / mkv / mov / webm).
                let mut output = path.clone();
                output.set_extension(settings.format.extension());
                let output_str = output.to_string_lossy().to_string();

                let script = generate_render_script(
                    seq,
                    &self.state.media_pool,
                    &settings,
                    &output_str,
                );

                // Count media clips on the first video track for the toast message.
                let clip_count = seq
                    .tracks
                    .iter()
                    .find(|t| t.kind == klipp_timeline::TrackKind::Video)
                    .map(|t| {
                        t.clips
                            .iter()
                            .filter(|c| {
                                c.enabled
                                    && matches!(c.kind, klipp_timeline::ClipKind::Media { .. })
                            })
                            .count()
                    })
                    .unwrap_or(0);

                match std::fs::write(&path, &script) {
                    Ok(_) => {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            if let Ok(meta) = std::fs::metadata(&path) {
                                let mut perms = meta.permissions();
                                perms.set_mode(0o755);
                                let _ = std::fs::set_permissions(&path, perms);
                            }
                        }
                        let plural = if clip_count == 1 { "" } else { "s" };
                        self.state.toast.show(format!(
                            "Render script written ({clip_count} clip{plural}). Run: bash {}",
                            path.display()
                        ));
                    }
                    Err(e) => {
                        self.state
                            .toast
                            .show(format!("Failed to write render script: {e}"));
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// If `output_path` resolves to the same file as any media-pool item,
    /// returns the matching item's filename. Used to warn the user before
    /// they overwrite a source file mid-edit (the encoder rewrites the
    /// file under the live decoder, invalidates filmstrip cache, and on
    /// reload the timeline references the rendered output rather than the
    /// camera-original).
    ///
    /// Path comparison: try canonicalisation (resolves `..`, symlinks,
    /// case-insensitive matches on relevant FSes); fall back to lexical
    /// equality if canonicalisation fails (e.g. the export path is in a
    /// directory that doesn't exist yet — won't conflict, so the
    /// fall-back trivially returns None for it). Both paths must
    /// canonicalise for a match — if only one resolves we treat it as
    /// "no conflict" rather than risk a false positive.
    pub(super) fn find_export_overwrite_conflict(
        &self,
        output_path: &std::path::Path,
    ) -> Option<String> {
        source_written_over(output_path, self.state.media_pool.all().filter(|item| !item.is_synthetic()).map(|item| item.path.as_path()))
    }

    /// Find next available numeric-suffixed filename (`output_1.mp4`,
    /// `output_2.mp4` …) when the user picks "Auto-Rename" on the
    /// source-overwrite warning. Walks suffix integers up to 9999 then
    /// gives up — far beyond any plausible real-world clash.
    pub(super) fn auto_rename_export_path(
        original: &std::path::Path,
    ) -> std::path::PathBuf {
        next_free_name(original, |candidate| candidate.exists())
    }

    /// Export `settings` as far as `confirmed` allows: ask about media
    /// offline, or a source about to be written over, when that is the
    /// next question (see [`export_step`]); otherwise queue the render.
    pub(super) fn export_or_ask(&mut self, settings: &klipp_core::ExportSettings, confirmed: Confirmed) {
        if self.ask_matte_gaps() {
            return;
        }
        let offline = if confirmed == Confirmed::Nothing {
            offline_clips(&self.state.sequences[self.state.active_sequence_idx], &self.state.media_pool)
        } else {
            Vec::new()
        };
        let written_over = if confirmed == Confirmed::Overwrite { None } else { self.find_export_overwrite_conflict(&settings.output_path) };
        match export_step(confirmed, offline, written_over) {
            ExportStep::AskOffline(clips) => self.state.export_offline_warning = Some((clips, Box::new(settings.clone()))),
            ExportStep::AskOverwrite(name) => self.state.export_source_overwrite_warning = Some((name, Box::new(settings.clone()))),
            ExportStep::Queue => self.queue_export(settings),
        }
    }

    /// Whether a tracked subject matte in the active sequence falls short
    /// of what a render reads, putting up the question when it does. No
    /// export goes ahead past it: a missing frame would otherwise render
    /// with its neighbour's matte (#239).
    pub(super) fn ask_matte_gaps(&mut self) -> bool {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let gaps = matte_gaps(seq, &self.state.media_pool);
        if gaps.is_empty() {
            return false;
        }
        let tracking = &self.state.tracking_subjects;
        let lines = matte_gap_lines(&gaps, seq, &self.state.media_pool, |clip| {
            let t = tracking.get(&clip)?;
            let s = klipp_analysis::service::subject_matte_status(t.media)?;
            matches!(s.state, klipp_analysis::service::State::Running | klipp_analysis::service::State::Queued).then(|| s.progress.unwrap_or(0.0))
        });
        self.state.export_matte_warning = Some(lines);
        true
    }

    /// Put the render of `settings` on the queue for the range the dialog
    /// chose: the whole sequence, its in and out points, the selected
    /// clips, or one job per marker segment. The three ways an export is
    /// confirmed each had a copy of this.
    pub(super) fn queue_export(&mut self, settings: &klipp_core::ExportSettings) {
        if self.export_dialog.range_mode == 3 {
            self.enqueue_marker_segments(settings);
            self.state.show_render_queue = true;
            return;
        }
        let use_marked = self.export_dialog.range_mode == 1;
        let mut snapshot = klipp_export::RenderSnapshot::from_sequence_with_all(
            &self.state.sequences[self.state.active_sequence_idx],
            &self.state.sequences,
            &self.state.media_pool,
            settings,
            // The confirmation dialogs used to drop the LUT here.
            self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
            use_marked,
            self.state.project_working_colorspace,
        );
        if let Ok(ref mut snap) = snapshot {
            if self.export_dialog.range_mode == 2 {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                snap.sequence_snapshot.export_range = selected_clips_frame_range(&self.state.selected_clips, seq, snap.sequence_snapshot.fps);
            }
            snap.sequence_snapshot.video.global_lut_opacity = self.state.global_lut_opacity;
        }
        let label = settings.output_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Render".into());
        match snapshot {
            Ok(snap) => {
                self.queue_job(settings.output_path.clone(), settings.clone(), label, snap);
            }
            Err(e) => tracing::error!("Failed to create render snapshot: {e}"),
        }
        self.state.show_render_queue = true;
    }

    /// Puts a render on the queue, and keeps what it renders for the 8-bit
    /// banding check unless that ran for this edit (#2074): checked once
    /// no render runs, never beside one.
    fn queue_job(&mut self, path: PathBuf, settings: klipp_core::ExportSettings, label: String, snap: klipp_export::RenderSnapshot) {
        let key = self.banding_key();
        if settings.video_codec.writes_8bit(settings.bit_depth) {
            self.export_dialog.banding.defer(key, snap.sequence_snapshot.clone());
        }
        let id = self.state.render_queue.add_job(path, settings, label, snap);
        self.export_dialog.banding.queued(id, key);
    }

    /// Marker multi-export: enqueue one render job per segment between
    /// adjacent markers in the active sequence. Bookend segments
    /// (sequence-start → first marker, last marker → sequence-end) are
    /// included only when the corresponding dialog checkbox is on.
    /// Zero-length segments (e.g. two markers at the same frame) are
    /// silently skipped.
    pub(super) fn enqueue_marker_segments(&mut self, settings: &klipp_core::ExportSettings) {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let fps = seq.fps.as_f64();

        // Explicit-range mode: if any marker carries a `duration`, the user
        // has drawn explicit regions and we export one file per region,
        // ignoring boundary-inference entirely. This is the more predictable
        // shape — point markers scattered next to explicit ranges would
        // otherwise produce surprising extra segments.
        let has_range_markers = seq.markers.iter().any(|m| m.duration.is_some());

        let segments: Vec<(u64, u64, String)> = if has_range_markers {
            build_range_marker_segments(&seq.markers, fps)
        } else {
            build_inferred_marker_segments(
                &seq.markers,
                fps,
                timecode_to_frame(&seq.total_duration(), fps),
                self.export_dialog.include_pre_marker_segment,
                self.export_dialog.include_post_marker_segment,
            )
        };

        if segments.is_empty() {
            self.state.toast.show("No valid segments to export.");
            return;
        }

        let mut job_count = 0u32;
        for (i, ((start_frame, end_frame, _), (output_path, display_label))) in segments.iter().zip(segment_outputs(&segments, &settings.output_path)).enumerate() {

            // Per-job ExportSettings clone with overridden output path.
            let mut job_settings: klipp_core::ExportSettings = settings.clone();
            job_settings.output_path = output_path.clone();

            // Build a snapshot for this segment. Pass `false` for
            // use_marked_range and override export_range manually so the
            // sequence in/out points (if any) don't interfere.
            let snap = klipp_export::RenderSnapshot::from_sequence_with_all(
                &self.state.sequences[self.state.active_sequence_idx],
                &self.state.sequences,
                &self.state.media_pool,
                &job_settings,
                self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
                false,
                self.state.project_working_colorspace,
            );
            let mut snap = match snap {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to create snapshot for segment {}: {e}", i + 1);
                    continue;
                }
            };
            snap.sequence_snapshot.export_range = Some((*start_frame, *end_frame));
            snap.sequence_snapshot.video.global_lut_opacity = self.state.global_lut_opacity;

            self.queue_job(output_path, job_settings, display_label, snap);
            job_count += 1;
        }

        self.state.toast.show(format!("Queued {job_count} export job(s)."));
    }

    /// An output path before the form is shown, so an export can never
    /// start with an empty name (that wrote `.partial` into the working
    /// directory). Named after the project, in the Videos folder
    /// (`XDG_VIDEOS_DIR`), else the home directory.
    pub(super) fn prepare_export_defaults(&mut self) {
        if self.export_dialog.output_path.trim().is_empty() {
            let home = export_folder(&self.prefs.export).to_string_lossy().to_string();
            let stem = self
                .state
                .project_path
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "output".into());
            self.export_dialog.output_path = format!("{home}/{stem}.{}", self.export_dialog.format.extension());
        }
        let (path, without_asking) = apply_overwrite_mode(self.prefs.export.overwrite_mode, &self.export_dialog.output_path);
        self.export_dialog.output_path = path;
        self.export_dialog.overwrite_without_asking = without_asking;
    }

    /// The export settings form: presets, output, codec, audio, range
    /// and the Export button. Hosted by the Export workspace's right
    /// column and by the Export dialog elsewhere.
    pub(super) fn export_settings_contents(&mut self, ui: &mut egui::Ui, all_presets: &[ExportPreset], cmds: &mut Vec<AppCommand>, in_dialog: bool) {
        follow_project_odt(&mut self.export_dialog.odt, &mut self.export_dialog.hdr_mode, &mut self.export_dialog.odt_followed, self.state.project_default_odt);
        use crate::widgets::inspector_group::section_card;
        let theme = self.theme.clone();
        let grid = |name: &str| egui::Grid::new(name).num_columns(2).spacing([12.0, 6.0]);
        let indent = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            ui.horizontal(|ui| { ui.add_space(12.0); ui.vertical(|ui| add(ui)); });
        };
        section_card(ui, "OUTPUT", None, &theme, |ui| { indent(ui, &mut |ui| {
                // Preset selector
                ui.horizontal_wrapped(|ui| {
                    ui.label("Preset:");
                    let selected_label = self.export_dialog.selected_preset_id.as_ref()
                        .and_then(|id| all_presets.iter().find(|p| &p.id == id))
                        .map(preset_label)
                        .unwrap_or_else(|| "Custom".to_string());
                    egui::ComboBox::from_id_salt("export_preset")
                        .selected_text(selected_label)
                        .width(0.0)
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(self.export_dialog.selected_preset_id.is_none(), "Custom").clicked() {
                                self.export_dialog.selected_preset_id = None;
                            }
                            ui.separator();
                            for preset in all_presets {
                                let label = preset_label(preset);
                                let is_selected = self.export_dialog.selected_preset_id.as_ref() == Some(&preset.id);
                                if ui.selectable_label(is_selected, &label).clicked() {
                                    self.export_dialog.selected_preset_id = Some(preset.id.clone());
                                    apply_preset_to_dialog(&mut self.export_dialog, &preset.settings);
                                }
                            }
                        });
                    if ui.button("Save as Preset").clicked() {
                        self.export_dialog.show_save_preset = true;
                        self.export_dialog.save_preset_name.clear();
                    }
                    // Opens the preset manager (#45) in a separate modal
                    // so the user can delete, rename, duplicate, or
                    // import/export user presets.
                    if ui.button("\u{2699} Manage\u{2026}").on_hover_text("Manage export presets").clicked() {
                        self.state.show_preset_manager = true;
                    }
                });
                ui.add_space(4.0);

                grid("export_grid_output").show(ui, |ui| {
                        ui.label("Output file:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.export_dialog.output_path)
                                .desired_width((ui.available_width() - 40.0).max(120.0)));
                            let picker = egui::Id::new("export-output-path");
                            if ui.button("...").clicked() {
                                crate::file_dialog::pick_path(picker, || {
                                    rfd::FileDialog::new().set_title("Export to...").save_file()
                                });
                            }
                            if let Some(crate::file_dialog::Picked::Path(path)) = crate::file_dialog::take_path(picker) {
                                self.export_dialog.output_path = path.to_string_lossy().to_string();
                            }
                        });
                        ui.end_row();

                        // Auto-update file extension when format changes
                        let old_ext = self.export_dialog.format.extension();

                        ui.label("Format:");
                        let prev_format = self.export_dialog.format;
                        egui::ComboBox::from_id_salt("export_format")
                            .selected_text(self.export_dialog.format.label())
                            .show_ui(ui, |ui| {
                                // Format options follow the codec's container
                                // requirements. ProRes/DNxHD must be in MOV;
                                // VP9 must be in WebM or MKV; the GIF codec
                                // requires the GIF container exclusively;
                                // H.264/H.265 accept any of the four
                                // mainstream containers.
                                for format in [ExportFormat::Mp4, ExportFormat::Mkv, ExportFormat::Mov, ExportFormat::WebM, ExportFormat::Gif] {
                                    ui.selectable_value(&mut self.export_dialog.format, format, format.label());
                                }
                                // The sound alone, no picture (#1813).
                                ui.separator();
                                for format in [ExportFormat::Mp3, ExportFormat::Wav, ExportFormat::M4a] {
                                    ui.selectable_value(&mut self.export_dialog.format, format, format.label());
                                }
                            });
                        // A codec the new container cannot hold gives way to
                        // the container's default, so every pair the dialog
                        // shows is one FFmpeg will write.
                        if self.export_dialog.format != prev_format {
                            (self.export_dialog.video_codec, self.export_dialog.audio_codec) =
                                self.export_dialog.format.fit(self.export_dialog.video_codec, self.export_dialog.audio_codec);
                        }
                        // Sound alone: what is and isn't used, and a WAV in
                        // place of an MP3 this FFmpeg can't write (#1813).
                        if self.export_dialog.format.is_audio_only() {
                            ui.end_row();
                            ui.label("");
                            let note = if self.export_dialog.format == ExportFormat::Mp3 && !klipp_render::audio_only::has_encoder(klipp_core::ExportAudioCodec::Mp3) {
                                "This FFmpeg has no MP3 encoder: the sound is written as WAV, with .wav."
                            } else {
                                "Sound only: the picture settings below are not used."
                            };
                            ui.label(egui::RichText::new(note).small().italics());
                        }
                        // Update file extension when format changes
                        if self.export_dialog.format != prev_format {
                            let new_ext = self.export_dialog.format.extension();
                            if self.export_dialog.output_path.ends_with(&format!(".{old_ext}")) {
                                let base = &self.export_dialog.output_path[..self.export_dialog.output_path.len() - old_ext.len()];
                                self.export_dialog.output_path = format!("{base}{new_ext}");
                            }
                        }
                        ui.end_row();

                        ui.label("Video codec:");
                        let prev_codec = self.export_dialog.video_codec;
                        egui::ComboBox::from_id_salt("export_vcodec")
                            .selected_text(self.export_dialog.video_codec.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::H264, "\u{26A1} H.264 — GPU accelerated");
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::H265, "\u{26A1} H.265 — GPU accelerated, best quality/size");
                                ui.separator();
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::Av1, "AV1 — next-gen, best compression");
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::Vp9, "VP9 — web video (YouTube, WebM)");
                                if self.export_dialog.format.holds_video(ExportVideoCodec::ProRes422) {
                                    ui.separator();
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes422, "ProRes 422 — NLE interchange");
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes422HQ, "ProRes 422 HQ — high quality");
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes4444, "ProRes 4444 — 4:4:4 mastering");
                                    ui.separator();
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::DNxHD115, "DNxHD 115 — Avid interchange");
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::DNxHD145, "DNxHD 145 — Avid high bitrate");
                                }
                            });
                        ui.end_row();

                        // Codec hint
                        ui.label("");
                        ui.colored_label(
                            self.theme.text_dim,
                            self.export_dialog.video_codec.hint_at_depth(self.export_dialog.bit_depth),
                        );
                        ui.end_row();

                        // Where an 8-bit file of this sequence will show
                        // steps in its smooth gradients (#2074): advice,
                        // never a setting changed.
                        if let Some(hint) = self.export_dialog.banding.hint_for(self.banding_key(), self.export_dialog.video_codec, self.export_dialog.bit_depth) {
                            ui.label("");
                            ui.label(egui::RichText::new(format!("\u{26A0} {hint}")).small().color(ui.visuals().warn_fg_color));
                            ui.end_row();
                        }

                        // A codec the container cannot hold moves the export
                        // to the codec's own container (ProRes and DNxHD to
                        // MOV, VP9 to WebM), and the audio follows.
                        if self.export_dialog.video_codec != prev_codec
                            && !self.export_dialog.format.holds_video(self.export_dialog.video_codec)
                        {
                            let old_fmt_ext = self.export_dialog.format.extension();
                            let format = self.export_dialog.video_codec.home_format();
                            self.export_dialog.format = format;
                            if !format.holds_audio(self.export_dialog.audio_codec) {
                                self.export_dialog.audio_codec = format.default_audio();
                            }
                            let new_fmt_ext = format.extension();
                            if self.export_dialog.output_path.ends_with(&format!(".{old_fmt_ext}")) {
                                let base = &self.export_dialog.output_path[..self.export_dialog.output_path.len() - old_fmt_ext.len()];
                                self.export_dialog.output_path = format!("{base}{new_fmt_ext}");
                            }
                        }
                        ui.end_row();

                });
        }); });
        section_card(ui, "DELIVERY SPEC", None, &theme, |ui| { indent(ui, &mut |ui| {
            self.delivery_spec_contents(ui, &theme);
        }); });
        section_card(ui, "VIDEO", None, &theme, |ui| { indent(ui, &mut |ui| {
                grid("export_grid_video").show(ui, |ui| {
                        // #65: bit-depth + HDR controls. H.264, H.265 and
                        // AV1 pick their depth and take HDR at 10 bits;
                        // ProRes is always 10-bit, so always can. DNxHD is
                        // 8-bit, and VP9 stays 8-bit and SDR here.
                        let picks_depth = matches!(
                            self.export_dialog.video_codec,
                            ExportVideoCodec::H264 | ExportVideoCodec::H265 | ExportVideoCodec::Av1
                        );
                        let prores = matches!(
                            self.export_dialog.video_codec,
                            ExportVideoCodec::ProRes422 | ExportVideoCodec::ProRes422HQ | ExportVideoCodec::ProRes4444
                        );
                        if picks_depth || prores {
                            if picks_depth {
                                ui.label("Bit depth:");
                                ui.horizontal(|ui| {
                                    ui.radio_value(&mut self.export_dialog.bit_depth, 0u8, "8-bit");
                                    ui.radio_value(&mut self.export_dialog.bit_depth, 10u8, "10-bit");
                                });
                                ui.end_row();
                            } else {
                                self.export_dialog.bit_depth = 0;
                            }

                            if prores || self.export_dialog.bit_depth == 10 {
                                ui.label("HDR:");
                                ui.horizontal(|ui| {
                                    ui.radio_value(
                                        &mut self.export_dialog.hdr_mode,
                                        klipp_core::HdrMode::Sdr,
                                        "SDR",
                                    );
                                    ui.radio_value(
                                        &mut self.export_dialog.hdr_mode,
                                        klipp_core::HdrMode::Hlg,
                                        "HLG",
                                    );
                                    ui.radio_value(
                                        &mut self.export_dialog.hdr_mode,
                                        klipp_core::HdrMode::Pq,
                                        "HDR10 (PQ)",
                                    );
                                });
                                ui.end_row();

                                // A codec that will encode this delivery
                                // but should not be the one chosen. Said
                                // where the choice is made rather than
                                // discovered after the render, and the fix
                                // is offered instead of only named: the
                                // user who hit this picked H.264 from a
                                // dropdown that called it "universal".
                                if let Some(advice) = self
                                    .export_dialog
                                    .video_codec
                                    .hdr_advice(self.export_dialog.hdr_mode)
                                {
                                    ui.label("");
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("\u{26A0} {}", advice.headline))
                                                .small()
                                                .color(ui.visuals().warn_fg_color),
                                        )
                                        .on_hover_text(advice.detail);
                                        if let Some(to) = advice.suggest {
                                            if ui
                                                .small_button(format!("Use {}", to.label()))
                                                .on_hover_text(advice.detail)
                                                .clicked()
                                            {
                                                self.export_dialog.video_codec = to;
                                            }
                                        }
                                    });
                                    ui.end_row();
                                }
                            } else {
                                // Keep state consistent: no 10-bit → SDR.
                                self.export_dialog.hdr_mode = klipp_core::HdrMode::Sdr;
                            }
                        } else {
                            // Other codecs: reset to 8-bit/SDR so the
                            // user-invisible state doesn't leak into the
                            // export settings.
                            self.export_dialog.bit_depth = 0;
                            self.export_dialog.hdr_mode = klipp_core::HdrMode::Sdr;
                        }

                        // An output transform only serves the deliveries
                        // it fits: the ACES PQ ones write PQ, the others
                        // SDR. Switching the delivery drops one that no
                        // longer fits.
                        //
                        // The working space rules some out too: the legacy
                        // sRGB path converts no primaries, so it is only
                        // sRGB in a Rec.709 project. A setting saved before
                        // the project moved to a wide gamut is dropped here
                        // the same way.
                        let delivery = self.export_dialog.hdr_mode;
                        let working = self.state.project_working_colorspace;
                        if !self.export_dialog.odt.fits(delivery)
                            || !self.export_dialog.odt.suits_working_space(working)
                        {
                            self.export_dialog.odt = klipp_core::OdtChoice::MatchViewer;
                        }

                        // Mastering peak — the display the HDR master is
                        // graded for. It is what camera-log highlights
                        // roll into, where the picture is held, and the
                        // mastering-display max luminance written into
                        // the file. Only a PQ delivery gets to pick one:
                        // HLG is defined on a 1000-nit nominal display
                        // (BT.2100), and the ACES PQ transforms are named
                        // for the peak they master to.
                        if delivery == klipp_core::HdrMode::Hlg {
                            ui.label("Mastering peak:");
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(
                                        "1000 nits — HLG is defined on a 1000-nit nominal display (BT.2100).",
                                    )
                                    .small()
                                    .italics(),
                                )
                                .wrap(),
                            );
                            ui.end_row();
                        } else if delivery == klipp_core::HdrMode::Pq && !self.export_dialog.odt.is_pq() {
                            ui.label("Mastering peak:");
                            ui.horizontal(|ui| {
                                for peak in klipp_core::MASTERING_PEAK_CHOICES {
                                    ui.radio_value(
                                        &mut self.export_dialog.mastering_peak_nits,
                                        peak,
                                        format!("{peak:.0} nits"),
                                    );
                                }
                            });
                            ui.end_row();
                            // Per-shot dynamic metadata (#468), H.265 only.
                            ui.label("");
                            let hevc = self.export_dialog.video_codec == klipp_core::ExportVideoCodec::H265;
                            ui.add_enabled_ui(hevc, |ui| {
                                ui.checkbox(&mut self.export_dialog.hdr10plus, "Dynamic metadata (ST 2094-40)")
                                    .on_hover_text("Per-shot light levels in the video stream, so a display dimmer than the master tone-maps each shot by its own highlights. H.265 only.");
                            });
                            ui.end_row();
                        }

                        // Tone-map operator. Applied at the display
                        // boundary inside `Encoder::encode_linear_frame`
                        // during export, after the grade and before sRGB
                        // quantise. ACES is the industry-standard filmic
                        // look; Hable is the previous default. An HDR
                        // delivery keeps its highlights instead, so it
                        // has none.
                        //
                        // Labelled "Tone map", not "Colour science".
                        // In Resolve "colour science" names the pipeline
                        // a project is graded through — YRGB against
                        // ACES — and a colourist reading that word here
                        // would expect to be choosing that, not a
                        // highlight roll-off curve. The control picks
                        // None / Reinhard / Hable / ACES, which is a
                        // tone map and nothing else.
                        // Only the legacy sRGB output reads it: "Match
                        // viewer" and the ACES transforms carry their own
                        // curves (`klipp_render::encoder::export_display`),
                        // so shown for those it was a setting that did
                        // nothing.
                        if delivery == klipp_core::HdrMode::Sdr && self.export_dialog.odt == klipp_core::OdtChoice::Srgb {
                            ui.label("Tone map:");
                            egui::ComboBox::from_id_salt("export_tone_map")
                                .selected_text(klipp_color::display::tone_map_op_label(
                                    self.export_dialog.tone_map_op,
                                ))
                                .show_ui(ui, |ui| {
                                    for op in klipp_color::display::TONE_MAP_OPS_ALL {
                                        ui.selectable_value(
                                            &mut self.export_dialog.tone_map_op,
                                            op,
                                            klipp_color::display::tone_map_op_label(op),
                                        );
                                    }
                                });
                            ui.end_row();
                        }

                        // Output Display Transform — picks the
                        // delivery-target colour space the rendered
                        // file lands in. Default `MatchViewer` gives
                        // back-compat: same pixels the user has been
                        // seeing while editing. The HDR + DCI cinema
                        // entries assume an AcesCG-basis grade chain
                        // (or a Rec.709 project, which the encoder
                        // walks into AcesCG before applying the ODT).
                        ui.label("Output transform:");
                        output_transform_menu(ui, &mut self.export_dialog.odt, delivery, working);
                        ui.end_row();

                        // Plain-language hint so the default isn't
                        // surprising. Only show under MatchViewer —
                        // the explicit ODTs are self-explanatory by
                        // their menu labels.
                        if self.export_dialog.odt == klipp_core::OdtChoice::MatchViewer {
                            let hint = if delivery == klipp_core::HdrMode::Sdr {
                                "Match Viewer = same colour space your monitor's been showing you while editing."
                                    .to_string()
                            } else {
                                // Name the peak the export actually masters
                                // to, so the hint cannot contradict the
                                // picker sitting above it.
                                let peak = if delivery == klipp_core::HdrMode::Hlg {
                                    klipp_core::DEFAULT_MASTERING_PEAK_NITS
                                } else {
                                    self.export_dialog.mastering_peak_nits
                                };
                                let titles = self
                                    .state
                                    .sequences
                                    .get(self.state.active_sequence_idx)
                                    .map_or(klipp_timeline::DEFAULT_TITLE_NITS, |s| s.title_nits)
                                    .min(peak);
                                format!(
                                    "Match Viewer = the viewer's picture with its highlights kept, mastered to {peak:.0} nits. \
                                     SDR material sits at 203 nits (BT.2408); titles at the sequence's title brightness \
                                     (Sequence Settings), {titles:.0} nits. The viewer itself shows the SDR version."
                                )
                            };
                            ui.label("");
                            ui.add(egui::Label::new(egui::RichText::new(hint).small().italics()).wrap());
                            ui.end_row();
                        }

                        // Rate control mode
                        if self.export_dialog.video_codec.uses_crf() {
                            ui.label("Rate control:");
                            egui::ComboBox::from_id_salt("export_rate_ctrl")
                                .selected_text(self.export_dialog.rate_control.label())
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.export_dialog.rate_control, klipp_core::RateControl::Crf, "CRF (Quality)");
                                    ui.selectable_value(&mut self.export_dialog.rate_control, klipp_core::RateControl::Cbr, "CBR (Constant Bitrate)");
                                    ui.selectable_value(&mut self.export_dialog.rate_control, klipp_core::RateControl::Vbr, "VBR (Variable Bitrate)");
                                });
                            ui.end_row();

                            match self.export_dialog.rate_control {
                                klipp_core::RateControl::Crf => {
                                    ui.label("Quality (CRF):");
                                    ui.add(egui::Slider::new(&mut self.export_dialog.crf, 0..=51));
                                    ui.end_row();
                                }
                                klipp_core::RateControl::Cbr | klipp_core::RateControl::Vbr => {
                                    let mode_label = if self.export_dialog.rate_control == klipp_core::RateControl::Cbr {
                                        "Bitrate (CBR):"
                                    } else {
                                        "Target bitrate (VBR):"
                                    };
                                    ui.label(mode_label);
                                    ui.add(egui::Slider::new(&mut self.export_dialog.video_bitrate_kbps, 500..=100_000).suffix(" kbps").logarithmic(true));
                                    ui.end_row();
                                }
                            }
                        }

                        // Encoder speed (only for codecs that support presets)
                        if self.export_dialog.video_codec.supports_preset() {
                            ui.label("Encoder speed:");
                            egui::ComboBox::from_id_salt("export_enc_speed")
                                .selected_text(self.export_dialog.preset.label())
                                .show_ui(ui, |ui| {
                                    for p in klipp_core::EncoderPreset::all() {
                                        ui.selectable_value(&mut self.export_dialog.preset, *p, p.label());
                                    }
                                });
                            ui.end_row();
                        }

                });
        }); });
        section_card(ui, "AUDIO", None, &theme, |ui| { indent(ui, &mut |ui| {
                grid("export_grid_audio").show(ui, |ui| {
                        // Audio settings disabled for GIF — the format
                        // can't carry audio, so the encoder silently
                        // drops it. Showing the controls in a disabled
                        // state is more informative than hiding them.
                        let audio_supported = self.export_dialog.format.supports_audio();
                        ui.add_enabled_ui(audio_supported, |ui| {
                            ui.label("Audio codec:");
                        });
                        ui.add_enabled_ui(audio_supported, |ui| {
                            egui::ComboBox::from_id_salt("export_acodec")
                                .selected_text(self.export_dialog.audio_codec.label())
                                .show_ui(ui, |ui| {
                                    // Only what the container holds: MOV takes no
                                    // Opus or FLAC, WebM only Opus.
                                    let format = self.export_dialog.format;
                                    for group in [
                                        &[(ExportAudioCodec::Aac, "AAC"), (ExportAudioCodec::Mp3, "MP3"), (ExportAudioCodec::Opus, "Opus")][..],
                                        &[(ExportAudioCodec::Flac, "FLAC (lossless)"), (ExportAudioCodec::Pcm16, "PCM 16-bit (WAV)"), (ExportAudioCodec::Pcm24, "PCM 24-bit (WAV)")][..],
                                        &[(ExportAudioCodec::Copy, ExportAudioCodec::Copy.label())][..],
                                    ] {
                                        let held: Vec<_> = group.iter().filter(|(c, _)| format.holds_audio(*c)).collect();
                                        if held.is_empty() {
                                            continue;
                                        }
                                        if group[0].0 != ExportAudioCodec::Aac {
                                            ui.separator();
                                        }
                                        for (codec, label) in held {
                                            ui.selectable_value(&mut self.export_dialog.audio_codec, *codec, *label);
                                        }
                                    }
                                });
                        });
                        ui.end_row();

                        ui.add_enabled_ui(audio_supported, |ui| {
                            ui.label("Audio bitrate:");
                        });
                        ui.add_enabled_ui(audio_supported, |ui| {
                            egui::ComboBox::from_id_salt("export_abitrate")
                                .selected_text(format!("{} kbps", self.export_dialog.audio_bitrate))
                                .show_ui(ui, |ui| {
                                    for rate in [128, 192, 256, 320] {
                                        ui.selectable_value(&mut self.export_dialog.audio_bitrate, rate, format!("{rate} kbps"));
                                    }
                                });
                        });
                        ui.end_row();

                        // Loudness normalization (also gated on audio support)
                        ui.add_enabled_ui(audio_supported, |ui| {
                            ui.label("Loudness:");
                        });
                        ui.add_enabled_ui(audio_supported, |ui| {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut self.export_dialog.normalize_loudness, "Normalize");
                                if self.export_dialog.normalize_loudness {
                                    let preset_labels = ["YouTube / Spotify (-14)", "Podcast (-16)", "EBU R128 (-23)", "ATSC A/85 (-24)", "Custom"];
                                    egui::ComboBox::from_id_salt("export_lufs_preset")
                                        .selected_text(preset_labels[self.export_dialog.loudness_preset as usize])
                                        .show_ui(ui, |ui| {
                                            for (i, label) in preset_labels.iter().enumerate() {
                                                ui.selectable_value(&mut self.export_dialog.loudness_preset, i as u8, *label);
                                            }
                                        });
                                    if self.export_dialog.loudness_preset == 4 {
                                        let mut v = self.export_dialog.custom_lufs;
                                        if ui.add(egui::DragValue::new(&mut v).range(-30.0..=-5.0).speed(0.5).suffix(" LUFS")).changed() {
                                            self.export_dialog.custom_lufs = v;
                                        }
                                        let mut peak = self.export_dialog.custom_true_peak;
                                        if ui.add(egui::DragValue::new(&mut peak).range(-6.0..=0.0).speed(0.1).prefix("peak ").suffix(" dBTP")).on_hover_text("The highest true peak the export lets through").changed() {
                                            self.export_dialog.custom_true_peak = peak;
                                        }
                                    }
                                }
                            });
                        });
                        ui.end_row();

                        // A loudness target with no audio stream measures, computes
                        // a gain and then writes nothing (#156).
                        if audio_supported
                            && self.export_dialog.normalize_loudness
                            && self.export_dialog.audio_codec == ExportAudioCodec::Copy
                        {
                            ui.label("");
                            ui.colored_label(theme.marker_warning, "No audio is written, so there is nothing to normalize.");
                            ui.end_row();
                        }

                });
        }); });
        section_card(ui, "SIZE AND RATE", None, &theme, |ui| { indent(ui, &mut |ui| {
                grid("export_grid_size").show(ui, |ui| {
                        // Resolution override
                        let seq_res = &self.state.sequences[self.state.active_sequence_idx].resolution;
                        let res_labels = [
                            format!("Sequence ({}x{})", seq_res.width, seq_res.height),
                            "720p (1280x720)".into(),
                            "1080p (1920x1080)".into(),
                            "4K (3840x2160)".into(),
                            "Custom".into(),
                        ];
                        ui.label("Resolution:");
                        egui::ComboBox::from_id_salt("export_resolution")
                            .selected_text(&res_labels[self.export_dialog.resolution_override as usize])
                            .show_ui(ui, |ui| {
                                for (i, label) in res_labels.iter().enumerate() {
                                    ui.selectable_value(&mut self.export_dialog.resolution_override, i as u8, label.as_str());
                                }
                            });
                        ui.end_row();

                        if self.export_dialog.resolution_override == 4 {
                            ui.label("Custom size:");
                            ui.horizontal(|ui| {
                                ui.add(egui::DragValue::new(&mut self.export_dialog.custom_width).range(128..=7680).suffix("w"));
                                ui.label("\u{00d7}");
                                ui.add(egui::DragValue::new(&mut self.export_dialog.custom_height).range(128..=4320).suffix("h"));
                            });
                            ui.end_row();
                        }

                        // Frame rate override
                        let seq_fps = &self.state.sequences[self.state.active_sequence_idx].fps;
                        let fps_labels = [
                            format!("Sequence ({:.3} fps)", seq_fps.as_f64()),
                            "24 fps".into(), "25 fps".into(), "29.97 fps".into(), "30 fps".into(), "60 fps".into(),
                        ];
                        ui.label("Frame rate:");
                        egui::ComboBox::from_id_salt("export_fps")
                            .selected_text(&fps_labels[self.export_dialog.fps_override as usize])
                            .show_ui(ui, |ui| {
                                for (i, label) in fps_labels.iter().enumerate() {
                                    ui.selectable_value(&mut self.export_dialog.fps_override, i as u8, label.as_str());
                                }
                            });
                        ui.end_row();
                });
        }); });

        // Export range — radio selection: Full / Marked range / Selected clips
        section_card(ui, "RANGE", None, &theme, |ui| { indent(ui, &mut |ui| {
                ui.radio_value(&mut self.export_dialog.range_mode, 0, "Full timeline");
                let has_range = self.state.sequences[self.state.active_sequence_idx].in_point.is_some()
                    && self.state.sequences[self.state.active_sequence_idx].out_point.is_some();
                if has_range {
                    let in_pt = self.state.sequences[self.state.active_sequence_idx].in_point.unwrap();
                    let out_pt = self.state.sequences[self.state.active_sequence_idx].out_point.unwrap();
                    ui.radio_value(&mut self.export_dialog.range_mode, 1,
                        format!("In/Out range ({} \u{2192} {})", in_pt.to_timecode_string(), out_pt.to_timecode_string()));
                }
                let sel_count = self.state.selected_clips.len();
                let sel_enabled = sel_count > 0;
                ui.add_enabled_ui(sel_enabled, |ui| {
                    let label = if sel_count > 0 {
                        format!("Selected clips ({sel_count})")
                    } else {
                        "Selected clips (none)".to_string()
                    };
                    ui.radio_value(&mut self.export_dialog.range_mode, 2, label);
                });
                // Marker-range multi-export: one file per segment between markers.
                let marker_count = self.state.sequences[self.state.active_sequence_idx].markers.len();
                let markers_enabled = marker_count > 0;
                ui.add_enabled_ui(markers_enabled, |ui| {
                    let label = if marker_count > 0 {
                        format!("Between markers \u{2014} one file per range ({marker_count} marker{})",
                            if marker_count == 1 { "" } else { "s" })
                    } else {
                        "Between markers \u{2014} one file per range".to_string()
                    };
                    ui.radio_value(&mut self.export_dialog.range_mode, 3, label);
                });
                if !markers_enabled {
                    ui.indent("markers_hint", |ui| {
                        ui.label(
                            egui::RichText::new("Add markers to the timeline to use this mode.")
                                .small()
                                .color(self.theme.text_dim),
                        );
                    });
                }
                if self.export_dialog.range_mode == 3 {
                    ui.indent("marker_options", |ui| {
                        ui.checkbox(
                            &mut self.export_dialog.include_pre_marker_segment,
                            "Include segment before first marker",
                        );
                        ui.checkbox(
                            &mut self.export_dialog.include_post_marker_segment,
                            "Include segment after last marker",
                        );
                    });
                }
                // Reset to Full if the selected mode is no longer valid
                if self.export_dialog.range_mode == 1 && !has_range {
                    self.export_dialog.range_mode = 0;
                }
                if self.export_dialog.range_mode == 2 && !sel_enabled {
                    self.export_dialog.range_mode = 0;
                }
                if self.export_dialog.range_mode == 3 && !markers_enabled {
                    self.export_dialog.range_mode = 0;
                }

                // Subtitles — only show options if the active sequence has subtitle/text tracks
                let has_subtitle_tracks = {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    seq.tracks.iter().any(|t| {
                        matches!(t.kind, klipp_timeline::TrackKind::Subtitle | klipp_timeline::TrackKind::Text)
                            && !t.clips.is_empty()
                    })
                };
                if has_subtitle_tracks {
                    ui.add_space(8.0);
                    // Burn-in works for any format (it's video pixels);
                    // soft-track embedding requires a format that
                    // carries subtitle streams (everything but GIF).
                    ui.checkbox(
                        &mut self.export_dialog.burn_in_subtitles,
                        "Burn subtitles into video",
                    );
                    ui.add_enabled_ui(self.export_dialog.format.supports_subtitles(), |ui| {
                        ui.checkbox(
                            &mut self.export_dialog.embed_subtitles,
                            "Embed subtitles as soft track",
                        );
                    });
                    // Captions in the picture's own stream (#1663): the
                    // A/53 data broadcast chains read, H.264 and H.265.
                    let carries_608 = matches!(self.export_dialog.video_codec, klipp_core::ExportVideoCodec::H264 | klipp_core::ExportVideoCodec::H265);
                    ui.add_enabled_ui(carries_608, |ui| {
                        ui.checkbox(&mut self.export_dialog.captions_608, "Closed captions in the video (CEA-608)")
                            .on_hover_text("Carry the subtitle track as CEA-608 closed captions inside the video stream, as broadcast and some streaming deliveries want. H.264 and H.265 only.");
                    });
                    if self.export_dialog.embed_subtitles
                        && matches!(self.export_dialog.format, klipp_core::ExportFormat::WebM)
                    {
                        ui.label(
                            egui::RichText::new("\u{26A0} WebM subtitle support varies by player")
                                .color(self.theme.marker_warning)
                                .size(11.0),
                        );
                    }
                }

                // Chapters — only show for formats that carry chapter metadata
                // (MP4/MKV/MOV). WebM and GIF have no chapter concept, so the
                // option is hidden entirely rather than rendered as a
                // checkbox that silently does nothing on export.
                let format_supports_chapters = self.export_dialog.format.supports_chapters();
                if format_supports_chapters {
                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.export_dialog.embed_chapters,
                        "Embed chapters from Chapter-kind markers",
                    ).on_hover_text(
                        "Write sequence markers tagged with kind = Chapter as chapter metadata in the output file. Other marker kinds (ToDo / Comment / Best Take / Section) are workflow notes and stay out of the container.",
                    );
                    if self.export_dialog.embed_chapters {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let chapter_count = seq.markers.iter()
                            .filter(|m| m.kind == klipp_core::MarkerKind::Chapter)
                            .count();
                        if chapter_count == 0 {
                            ui.indent("chapters_hint", |ui| {
                                let total_markers = seq.markers.len();
                                let msg = if total_markers == 0 {
                                    "\u{26A0} No markers in sequence".to_string()
                                } else {
                                    format!(
                                        "\u{26A0} No Chapter-kind markers ({total_markers} other marker{} found)",
                                        if total_markers == 1 { "" } else { "s" },
                                    )
                                };
                                ui.label(
                                    egui::RichText::new(msg)
                                        .color(self.theme.marker_warning)
                                        .size(11.0),
                                );
                            });
                        }
                    }
                }

        }); });

                // In the Export workspace these live in a bar pinned
                // to the foot of the column, so they cannot scroll away
                // — see `export_action_bar`. The dialog has no such
                // problem: it sizes itself to its contents.
                if in_dialog {
                    ui.add_space(12.0);
                    // The dialog lays out vertically and is not
                    // squeezed against a status bar, so the summary
                    // always fits there.
                    self.export_action_bar(ui, cmds, true, true);
                }
    }

    /// The two things this panel exists to do.
    ///
    /// Drawn as its own row so the Export workspace can pin it to the
    /// foot of the column rather than let it scroll off the bottom of
    /// the form. It did scroll off: at a half-height window the button
    /// that starts a render sat below the fold behind a scrollbar that
    /// egui only paints on hover, so the primary action of a page named
    /// Export had to be hunted for.
    ///
    /// `Export` is filled with the accent and `Add to Queue` is left as
    /// a plain button. They were previously identical, which left the
    /// panel with no primary action at all.
    /// What pressing Export is about to write: the range's length, the
    /// size and rate, the codecs, whether subtitles are going into the
    /// picture, and the file size where that can be said honestly.
    ///
    /// The form asked for seven things and described none of them. The
    /// one that actually cost something was subtitles: `burn_in_subtitles`
    /// defaults to on, its checkbox sits near the bottom of a long
    /// scroll, and burning them in is irreversible in the output — the
    /// only undo is rendering again. Saying so beside the button fixes
    /// the surprise without settling whether the default is right.
    pub(super) fn export_summary(&self) -> String {
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let seq_fps = seq.fps.as_f64();
        let fps = dialog_fps(&self.export_dialog).map(|f| f.as_f64()).unwrap_or(seq_fps);
        let res = dialog_resolution(&self.export_dialog)
            .unwrap_or(klipp_core::Resolution { width: seq.resolution.width, height: seq.resolution.height });

        // The range, in the sequence's own time.
        let secs = match self.export_dialog.range_mode {
            1 => match (seq.in_point, seq.out_point) {
                (Some(a), Some(b)) => (b - a).to_secs_f64().abs(),
                _ => seq.total_duration().to_secs_f64(),
            },
            2 => selected_clips_frame_range(&self.state.selected_clips, seq, seq_fps)
                .map(|(a, b)| (b - a) as f64 / seq_fps.max(1.0))
                .unwrap_or(0.0),
            // Between markers is one file per segment; a single length
            // would be the wrong number, so it is left out.
            3 => 0.0,
            _ => seq.total_duration().to_secs_f64(),
        };

        let mut parts: Vec<String> = Vec::new();
        if self.export_dialog.range_mode == 3 {
            parts.push("one file per marker range".to_string());
        } else if secs > 0.0 {
            parts.push(humanise_secs(secs));
        }
        parts.push(format!("{}x{}", res.width, res.height));
        parts.push(format!("{fps:.4} fps").replace(".0000 fps", " fps"));
        parts.push(format!(
            "{} / {}",
            self.export_dialog.video_codec.label(),
            self.export_dialog.audio_codec.label(),
        ));

        // Size, only where it is arithmetic rather than a guess.
        let px = res.width as f64 * res.height as f64;
        let audio_mbps = self.export_dialog.audio_bitrate as f64 / 1000.0;
        let video_mbps = match self.export_dialog.rate_control {
            klipp_core::RateControl::Cbr | klipp_core::RateControl::Vbr => {
                Some(self.export_dialog.video_bitrate_kbps as f64 / 1000.0)
            }
            // CRF has no target bitrate by construction: the encoder
            // spends what the picture needs. A number here would be
            // confidently wrong, which is worse than none.
            klipp_core::RateControl::Crf => intra_frame_mbps(self.export_dialog.video_codec, px, fps),
        };
        if let Some(v) = video_mbps {
            if secs > 0.0 {
                let bytes = (v + audio_mbps) * 1_000_000.0 / 8.0 * secs;
                parts.push(format!("~{}", humanise_bytes(bytes)));
            }
        } else if let (klipp_core::RateControl::Crf, Some((lo, hi))) =
            (self.export_dialog.rate_control, crf_mbps_range(self.export_dialog.video_codec, self.export_dialog.crf, px, fps))
        {
            // Quality has no one size: a range, from a still picture to
            // busy handheld footage, true where a single number would not
            // be. The queue measures the real one as it renders (#19).
            if secs > 0.0 {
                let at = |v: f64| (v + audio_mbps) * 1_000_000.0 / 8.0 * secs;
                parts.push(format!("{}\u{2013}{} by the picture", humanise_bytes(at(lo)), humanise_bytes(at(hi))));
            }
        }

        let has_subtitles = seq.tracks.iter().any(|t| {
            matches!(t.kind, klipp_timeline::TrackKind::Subtitle | klipp_timeline::TrackKind::Text)
                && !t.clips.is_empty()
        });
        if has_subtitles && self.export_dialog.burn_in_subtitles {
            parts.push("subtitles burned in".to_string());
        }
        parts.join("  \u{00b7}  ")
    }

    pub(super) fn export_action_bar(
        &mut self,
        ui: &mut egui::Ui,
        cmds: &mut Vec<AppCommand>,
        in_dialog: bool,
        show_summary: bool,
    ) {
        let theme = self.theme.clone();
        // Said above the buttons rather than in the form, so it is read
        // on the way to pressing Export rather than scrolled past.
        let summary = if show_summary { self.export_summary() } else { String::new() };
        if !summary.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(summary)
                        .size(11.0)
                        .color(theme.text_dim),
                );
            });
            ui.add_space(4.0);
        }
        // A pair the container cannot hold (from an older preset) is
        // said here and blocks the render, which would otherwise fail
        // only after the mixdown had run.
        let (format, video, audio) = (self.export_dialog.format, self.export_dialog.video_codec, self.export_dialog.audio_codec);
        let container_problem = klipp_core::ExportSettings { format, video_codec: video, audio_codec: audio, ..Default::default() }.container_problem();
        if let Some(problem) = &container_problem {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(egui::RichText::new(format!("\u{26A0} {problem}")).size(11.0).color(ui.visuals().warn_fg_color));
            });
            ui.add_space(4.0);
        }
        let path_given = !self.export_dialog.output_path.trim().is_empty();
        // An empty timeline has nothing to export (#655).
        let empty = self.state.sequence().has_nothing_to_export();
        let path_ok = path_given && container_problem.is_none() && !empty;
        let disabled_reason = if empty {
            "The timeline is empty: place some clips first"
        } else if path_given {
            "This container cannot hold the chosen codec"
        } else {
            "Choose an output file first"
        };
        let h = crate::theme::control::REGULAR;
        ui.horizontal(|ui| {
            // Secondary actions run from the left, the primary one is
            // pushed to the right edge. Two same-sized buttons huddled
            // at the left of a wide bar read as leftovers from the form
            // above rather than as the point of the panel.
            ui.add_space(12.0);
            if in_dialog
                && ui.add_sized([84.0, h], egui::Button::new("Cancel")).clicked()
            {
                self.state.show_export_dialog = false;
            }
            if ui
                .add_enabled(
                    path_ok,
                    egui::Button::new("Add to Queue").min_size(egui::vec2(120.0, h)),
                )
                .on_disabled_hover_text(disabled_reason)
                .clicked()
            {
                if self.ask_matte_gaps() {
                    return;
                }
                let path = PathBuf::from(&self.export_dialog.output_path);
                let settings = build_export_settings(&self.export_dialog, self.prefs.audio.pan_law);
                let use_marked = self.export_dialog.range_mode == 1;
                let mut snapshot = klipp_export::RenderSnapshot::from_sequence_with_all(
                    &self.state.sequences[self.state.active_sequence_idx],
                    &self.state.sequences,
                    &self.state.media_pool,
                    &settings,
                    self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
                    use_marked,
                    self.state.project_working_colorspace,
                );
                if self.export_dialog.range_mode == 2 {
                    if let Ok(ref mut snap) = snapshot {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        snap.sequence_snapshot.export_range = selected_clips_frame_range(&self.state.selected_clips, seq, snap.sequence_snapshot.fps);
                    }
                }
                if let Ok(ref mut snap) = snapshot {
                    snap.sequence_snapshot.video.global_lut_opacity = self.state.global_lut_opacity;
                }
                let label = path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Render".into());
                match snapshot {
                    Ok(snap) => {
                        self.queue_job(path, settings, label, snap);
                    }
                    Err(e) => {
                        tracing::error!("Failed to create render snapshot: {e}");
                    }
                }
                self.state.show_export_dialog = false;
                self.state.show_render_queue = true;
                self.prefs.export.last_preset_id = self.export_dialog.selected_preset_id.clone();
                self.prefs_dirty = true;
            }
            // The primary action, and the only thing in the row painted
            // with the accent.
            //
            // Disabled, it drops to a flat raised fill: an explicit
            // `.fill()` survives `add_enabled(false, ..)`, and a button
            // wearing the accent that does nothing when clicked is
            // worse than no emphasis at all.
            let (fill, fg) = if path_ok {
                (theme.accent, theme.text_on_accent)
            } else {
                (theme.bg_raised, theme.text_dim)
            };
            let draw_export = |ui: &mut egui::Ui| {
                let btn = egui::Button::new(egui::RichText::new("Export").color(fg))
                    .fill(fill)
                    .rounding(4.0)
                    .min_size(egui::vec2(140.0, h));
                ui.add_enabled(path_ok, btn)
                    .on_disabled_hover_text(disabled_reason)
                    .clicked()
            };

            // In the workspace bar the button is anchored to the right
            // edge, so which one does the thing the page is named after
            // is legible without reading either label. The dialog is
            // too narrow for that: right-aligning there pushed Export
            // on top of Add to Queue, so its buttons keep flowing from
            // the left with the accent alone carrying the emphasis.
            let export_clicked = if in_dialog {
                draw_export(ui)
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(12.0);
                    draw_export(ui)
                })
                .inner
            };

            if export_clicked {
                let path = PathBuf::from(&self.export_dialog.output_path);
                if path.exists() && !self.export_dialog.overwrite_without_asking {
                    self.export_dialog.confirm_overwrite = true;
                } else {
                    self.state.show_export_dialog = false;
                    self.prefs.export.last_preset_id = self.export_dialog.selected_preset_id.clone();
                    self.prefs_dirty = true;
                    cmds.push(AppCommand::ExportSequence {
                        settings: build_export_settings(&self.export_dialog, self.prefs.audio.pan_law),
                    });
                }
            }
        });
    }

    pub(super) fn show_export_dialog(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        // Build combined preset list for the combo box
        let all_presets: Vec<ExportPreset> = builtin_presets()
            .into_iter()
            .chain(self.prefs.export.custom_presets.iter().cloned())
            .collect();
        let mut cmds = Vec::new();

        let in_export_ws = self.state.active_workspace == crate::state::Workspace::Export;
        if !self.state.show_export_dialog && !in_export_ws {
            return cmds;
        }
        // What an 8-bit file of the sequence would show, checked once per
        // edit while the settings are open (#2074).
        let settings = build_export_settings(&self.export_dialog, self.prefs.audio.pan_law);
        if settings.video_codec.writes_8bit(settings.bit_depth) {
            self.want_banding_check(&settings);
        }

        // Outside the Export workspace the settings are a dialog; the
        // workspace hosts the same form in its right column.
        if !in_export_ws {
            let mut open = true;
            egui::Window::new("Export Sequence")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    self.export_settings_contents(ui, &all_presets, &mut cmds, true);
                });
            if !open {
                self.state.show_export_dialog = false;
                self.export_dialog.confirm_overwrite = false;
            }
        }

        // Overwrite confirmation dialog
        if self.export_dialog.confirm_overwrite {
            egui::Window::new("Overwrite File?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "\"{}\" already exists. Overwrite?",
                        self.export_dialog.output_path
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.export_dialog.confirm_overwrite = false;
                        }
                        if ui.button("Overwrite").clicked() {
                            self.export_dialog.confirm_overwrite = false;
                            self.state.show_export_dialog = false;
                            self.prefs.export.last_preset_id = self.export_dialog.selected_preset_id.clone();
                            self.prefs_dirty = true;
                            cmds.push(AppCommand::ExportSequence {
                                settings: build_export_settings(&self.export_dialog, self.prefs.audio.pan_law),
                            });
                        }
                    });
                });
        }

        // Save as Preset dialog
        if self.export_dialog.show_save_preset {
            egui::Window::new("Save as Preset")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Preset name:");
                        ui.text_edit_singleline(&mut self.export_dialog.save_preset_name);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.export_dialog.show_save_preset = false;
                        }
                        let name_valid = !self.export_dialog.save_preset_name.trim().is_empty();
                        if ui.add_enabled(name_valid, egui::Button::new("Save")).clicked() {
                            let id = format!("custom-{}", uuid::Uuid::new_v4());
                            let preset = ExportPreset {
                                id: id.clone(),
                                name: self.export_dialog.save_preset_name.trim().to_string(),
                                built_in: false,
                                settings: ExportPresetSettings {
                                    format: self.export_dialog.format,
                                    video_codec: self.export_dialog.video_codec,
                                    resolution: None,
                                    fps: None,
                                    crf: self.export_dialog.crf,
                                    preset: self.export_dialog.preset,
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: Some(self.export_dialog.audio_bitrate),
                                    video_bitrate_kbps: if self.export_dialog.rate_control != klipp_core::RateControl::Crf { Some(self.export_dialog.video_bitrate_kbps) } else { None },
                                    rate_control: self.export_dialog.rate_control,
                                    target_lufs: dialog_target_lufs(&self.export_dialog),
                                    true_peak_dbtp: dialog_true_peak(&self.export_dialog),
                                    bit_depth: self.export_dialog.bit_depth,
                                },
                            };
                            self.prefs.export.custom_presets.push(preset);
                            self.export_dialog.selected_preset_id = Some(id);
                            self.export_dialog.show_save_preset = false;
                            self.prefs_dirty = true;
                        }
                    });
                });
        }

        cmds
    }
}

impl KlippApp {
    /// The Export page's delivery spec (#2650): pick one and its settings
    /// are set; check the export, as these settings would make it, or a
    /// finished file against it, each requirement as pass, warn or fail.
    fn delivery_spec_contents(&mut self, ui: &mut egui::Ui, theme: &crate::theme::KlippTheme) {
        use klipp_core::delivery_spec::{self as ds, Status};
        // A file's check, back from its worker.
        if let Some(done) = self.export_dialog.delivery_file_check.lock().ok().and_then(|mut d| d.take()) {
            self.export_dialog.delivery_busy = false;
            match done {
                Ok(result) => self.export_dialog.delivery_result = Some(result),
                Err(why) => self.state.toast.show(format!("Check against spec: {why}")),
            }
        }
        let specs = ds::all_specs();
        let picked = self.export_dialog.delivery_spec.as_ref().and_then(|id| specs.iter().find(|s| &s.id == id)).cloned();
        ui.horizontal_wrapped(|ui| {
            ui.label("Spec:");
            egui::ComboBox::from_id_salt("export_delivery_spec")
                .selected_text(picked.as_ref().map_or("None", |s| s.name.as_str()))
                .width(0.0)
                .wrap_mode(egui::TextWrapMode::Extend)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(picked.is_none(), "None").clicked() {
                        self.export_dialog.delivery_spec = None;
                        self.export_dialog.delivery_result = None;
                    }
                    for spec in &specs {
                        let on = picked.as_ref().is_some_and(|p| p.id == spec.id);
                        if ui.selectable_label(on, &spec.name).on_hover_text(spec.sources.join("\n")).clicked() {
                            self.export_dialog.delivery_spec = Some(spec.id.clone());
                            self.export_dialog.selected_preset_id = None;
                            apply_spec_to_dialog(&mut self.export_dialog, spec);
                            self.export_dialog.delivery_result = None;
                        }
                    }
                });
        });
        let Some(spec) = picked else { return };
        ui.horizontal_wrapped(|ui| {
            if ui.button("Check the Export").on_hover_text("These settings, with the sequence's picture and sound, against the spec").clicked() {
                let seq = self.state.sequence();
                let captions = seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Subtitle && !t.clips.is_empty());
                let facts = ds::export_facts(&dialog_settings(&self.export_dialog), self.export_dialog.hdr_mode, seq.resolution, seq.fps.as_f64(), captions, None);
                self.export_dialog.delivery_result = Some(("This export".into(), ds::check(&spec, &facts)));
            }
            if ui.add_enabled(!self.export_dialog.delivery_busy, egui::Button::new("Check a File\u{2026}")).on_hover_text("A finished file against the spec: read with ffprobe, its loudness measured").clicked() {
                let picker = egui::Id::new("delivery-check-file");
                crate::file_dialog::pick_path(picker, || rfd::FileDialog::new().set_title("Check a file against the spec").pick_file());
            }
            if let Some(crate::file_dialog::Picked::Path(path)) = crate::file_dialog::take_path(egui::Id::new("delivery-check-file")) {
                self.export_dialog.delivery_busy = true;
                let (out, ctx, spec) = (std::sync::Arc::clone(&self.export_dialog.delivery_file_check), ui.ctx().clone(), spec.clone());
                std::thread::spawn(move || {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let result = klipp_render::delivery_facts::file_facts(&path, true).map(|facts| (name, ds::check(&spec, &facts)));
                    if let Ok(mut slot) = out.lock() {
                        *slot = Some(result);
                    }
                    ctx.request_repaint();
                });
            }
            if self.export_dialog.delivery_busy {
                ui.spinner();
            }
            if ui.button("Save as My Spec\u{2026}").on_hover_text("This spec's requirements with these export settings, under a name of your own. Its requirements can be edited in delivery-specs.json in Klipp's settings folder.").clicked() {
                self.export_dialog.delivery_save_name = Some(format!("{} (mine)", spec.name));
            }
        });
        if let Some(name) = self.export_dialog.delivery_save_name.as_mut() {
            let mut save = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label("Name:");
                ui.text_edit_singleline(name);
                save = ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Save")).clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if save {
                let mine = ds::DeliverySpec {
                    id: format!("delivery-mine-{}", uuid::Uuid::new_v4()),
                    name: name.trim().to_string(),
                    built_in: false,
                    sources: spec.sources.clone(),
                    export: dialog_settings(&self.export_dialog),
                    hdr: self.export_dialog.hdr_mode,
                    requirements: spec.requirements.clone(),
                };
                let mut all = ds::load_user_specs();
                let id = mine.id.clone();
                all.push(mine);
                match ds::save_user_specs(&all) {
                    Ok(()) => {
                        self.export_dialog.delivery_spec = Some(id);
                        self.state.toast.show("Saved as your own delivery spec");
                    }
                    Err(why) => self.state.toast.show(format!("Could not save the spec: {why}")),
                }
            }
            if save || cancel {
                self.export_dialog.delivery_save_name = None;
            }
        }
        if let Some((what, findings)) = &self.export_dialog.delivery_result {
            let (word, colour) = match ds::verdict(findings) {
                Status::Pass => ("meets", theme.marker_success),
                Status::Warn => ("meets, with warnings,", theme.marker_warning),
                Status::Fail => ("does not meet", theme.marker_error),
            };
            ui.label(egui::RichText::new(format!("{what} {word} {}", spec.name)).strong().color(colour));
            for f in findings {
                let (mark, colour) = match f.status {
                    Status::Pass => ("\u{2713}", theme.marker_success),
                    Status::Warn => ("!", theme.marker_warning),
                    Status::Fail => ("\u{2717}", theme.marker_error),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(mark).strong().color(colour));
                    ui.label(format!("{}: {} (wanted {})", f.what, f.found, f.wanted));
                    if !f.fix.is_empty() {
                        ui.label(egui::RichText::new(&f.fix).color(theme.text_secondary));
                    }
                });
            }
        }
    }
}

/// The export's output transform menu: every transform that can serve
/// `delivery` and suits a project in `working`, Rec.2020 SDR among the
/// SDR ones (#901).
/// Start the export at the project's default output transform (#466),
/// once for each default: a PQ one sets a PQ delivery with it. A choice
/// made in the dialog afterwards stands until the default changes.
pub(super) fn follow_project_odt(odt: &mut klipp_core::OdtChoice, delivery: &mut klipp_core::HdrMode, followed: &mut Option<klipp_core::OdtChoice>, project: klipp_core::OdtChoice) {
    if *followed == Some(project) {
        return;
    }
    *followed = Some(project);
    *odt = project;
    if project.is_pq() {
        *delivery = klipp_core::HdrMode::Pq;
    }
}

fn output_transform_menu(ui: &mut egui::Ui, odt: &mut klipp_core::OdtChoice, delivery: klipp_core::HdrMode, working: klipp_core::WorkingColorspace) {
    egui::ComboBox::from_id_salt("export_odt").selected_text(odt.label()).show_ui(ui, |ui| {
        for choice in klipp_core::OdtChoice::ALL.into_iter().filter(|c| c.fits(delivery) && c.suits_working_space(working)) {
            ui.selectable_value(odt, choice, choice.label());
        }
    });
}

#[cfg(test)]
mod output_transform_menu_tests {
    use klipp_core::{HdrMode, OdtChoice, WorkingColorspace};

    /// One frame of the menu with `events`: what it drew, and where.
    fn frame(ctx: &egui::Context, odt: &mut OdtChoice, delivery: HdrMode, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 500.0))), events, ..Default::default() };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| super::output_transform_menu(ui, odt, delivery, WorkingColorspace::AcesCG));
        });
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    fn click(ctx: &egui::Context, odt: &mut OdtChoice, delivery: HdrMode, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(ctx, odt, delivery, vec![egui::Event::PointerMoved(at), button(true)]);
        frame(ctx, odt, delivery, vec![egui::Event::PointerMoved(at), button(false)]);
        frame(ctx, odt, delivery, vec![egui::Event::PointerGone]);
    }

    fn at(drawn: &[(String, egui::Pos2)], text: &str) -> Option<egui::Pos2> {
        drawn.iter().rev().find(|(t, _)| t == text).map(|(_, p)| *p)
    }

    /// #901: an SDR export's menu offers Rec.2020 SDR, and picking it sets
    /// the export's transform; an HDR delivery's menu does not offer it.
    /// #466: the export starts at the project's default output
    /// transform, a PQ one with a PQ delivery; a choice made in the
    /// dialog stands until the default changes.
    #[test]
    fn the_export_starts_at_the_projects_default_output_transform() {
        let (mut odt, mut delivery, mut followed) = (OdtChoice::MatchViewer, HdrMode::Sdr, None);
        super::follow_project_odt(&mut odt, &mut delivery, &mut followed, OdtChoice::Rec2100Pq1000);
        assert_eq!((odt, delivery), (OdtChoice::Rec2100Pq1000, HdrMode::Pq));
        odt = OdtChoice::Rec2100Pq4000;
        super::follow_project_odt(&mut odt, &mut delivery, &mut followed, OdtChoice::Rec2100Pq1000);
        assert_eq!(odt, OdtChoice::Rec2100Pq4000, "the dialog's own choice stands");
        delivery = HdrMode::Sdr;
        super::follow_project_odt(&mut odt, &mut delivery, &mut followed, OdtChoice::DciP3D65_48);
        assert_eq!((odt, delivery), (OdtChoice::DciP3D65_48, HdrMode::Sdr), "a new default is taken");
    }

    #[test]
    fn rec2020_sdr_is_picked_from_an_sdr_exports_menu() {
        let ctx = egui::Context::default();
        let mut odt = OdtChoice::MatchViewer;
        frame(&ctx, &mut odt, HdrMode::Sdr, Vec::new());
        let shown = frame(&ctx, &mut odt, HdrMode::Sdr, Vec::new());
        click(&ctx, &mut odt, HdrMode::Sdr, at(&shown, "Match Viewer").expect("the menu shows its choice"));
        let open = frame(&ctx, &mut odt, HdrMode::Sdr, Vec::new());
        let item = at(&open, "Rec.2020 SDR").unwrap_or_else(|| panic!("the SDR menu offers Rec.2020 SDR: {open:?}"));
        click(&ctx, &mut odt, HdrMode::Sdr, item);
        assert_eq!(odt, OdtChoice::Rec2020Sdr);
        let shows = frame(&ctx, &mut odt, HdrMode::Sdr, Vec::new());
        assert!(at(&shows, "Rec.2020 SDR").is_some(), "the menu shows the pick: {shows:?}");

        // A PQ delivery's menu: no SDR transform to pick.
        let ctx = egui::Context::default();
        let mut odt = OdtChoice::MatchViewer;
        frame(&ctx, &mut odt, HdrMode::Pq, Vec::new());
        let shown = frame(&ctx, &mut odt, HdrMode::Pq, Vec::new());
        click(&ctx, &mut odt, HdrMode::Pq, at(&shown, "Match Viewer").unwrap());
        let open = frame(&ctx, &mut odt, HdrMode::Pq, Vec::new());
        assert!(at(&open, "Rec.2100 PQ (1000 nit)").is_some(), "the PQ menu is open: {open:?}");
        assert!(at(&open, "Rec.2020 SDR").is_none(), "an SDR transform offered for PQ: {open:?}");
    }
}

#[cfg(test)]
mod range_marker_segment_tests {
    use super::*;
    use klipp_core::{MarkerColor, MarkerId, RationalTime};

    fn mk(position_secs: f64, dur_secs: Option<f64>, label: &str) -> klipp_timeline::Marker {
        klipp_timeline::Marker { review: None,
            id: MarkerId::new(),
            position: RationalTime::from_secs_f64(position_secs, 24),
            label: label.into(),
            color: MarkerColor::Blue,
            duration: dur_secs.map(|s| RationalTime::from_secs_f64(s, 24)),
            comment: String::new(),
            kind: klipp_core::MarkerKind::None,
        }
    }

    #[test]
    fn range_segments_use_explicit_duration() {
        let markers = vec![
            mk(1.0, Some(2.0), "Act 1"),
            mk(5.0, Some(3.0), "Act 2"),
        ];
        let segs = build_range_marker_segments(&markers, 24.0);
        assert_eq!(segs.len(), 2, "got {segs:?}");
        // Act 1: 1s..3s at 24fps = frames 24..72.
        assert_eq!(segs[0].0, 24);
        assert_eq!(segs[0].1, 72);
        assert_eq!(segs[0].2, "Act 1");
        // Act 2: 5s..8s at 24fps = frames 120..192.
        assert_eq!(segs[1].0, 120);
        assert_eq!(segs[1].1, 192);
        assert_eq!(segs[1].2, "Act 2");
    }

    #[test]
    fn range_segments_skip_point_markers() {
        // Point marker sandwiched between two ranges — it's dropped because
        // the "any-range" mode ignores points entirely.
        let markers = vec![
            mk(1.0, Some(2.0), "Act 1"),
            mk(4.0, None, "Chapter Point"),
            mk(6.0, Some(2.0), "Act 2"),
        ];
        let segs = build_range_marker_segments(&markers, 24.0);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].2, "Act 1");
        assert_eq!(segs[1].2, "Act 2");
    }

    #[test]
    fn inferred_segments_infer_from_consecutive_points() {
        // Three point markers at 1s, 3s, 5s with seq length 10s.
        // No pre/post — boundaries are the three markers, segments:
        //   [1s..3s] ending at "Two"
        //   [3s..5s] ending at "Three"
        let markers = vec![
            mk(1.0, None, "One"),
            mk(3.0, None, "Two"),
            mk(5.0, None, "Three"),
        ];
        let segs = build_inferred_marker_segments(&markers, 24.0, 240, false, false);
        assert_eq!(segs.len(), 2);
        // 1s..3s = frames 24..72
        assert_eq!(segs[0].0, 24);
        assert_eq!(segs[0].1, 72);
        assert_eq!(segs[0].2, "Two");
        // 3s..5s = frames 72..120
        assert_eq!(segs[1].0, 72);
        assert_eq!(segs[1].1, 120);
        assert_eq!(segs[1].2, "Three");
    }

    #[test]
    fn inferred_segments_include_pre_post() {
        // Same markers, but with pre and post segments — we get 4 segments.
        let markers = vec![
            mk(1.0, None, "One"),
            mk(3.0, None, "Two"),
            mk(5.0, None, "Three"),
        ];
        let segs = build_inferred_marker_segments(&markers, 24.0, 240, true, true);
        assert_eq!(segs.len(), 4);
        // Pre: 0..24, ends at "One"
        assert_eq!(segs[0].0, 0);
        assert_eq!(segs[0].2, "One");
        // Last: 120..240, ends at "end" (synthetic)
        assert_eq!(segs[3].1, 240);
        assert_eq!(segs[3].2, "end");
    }
}

#[cfg(test)]
mod marker_export_tests {
    use super::*;

    #[test]
    fn sanitise_filename_replaces_illegal_chars() {
        assert_eq!(sanitise_filename("act/one"), "act_one");
        assert_eq!(sanitise_filename("scene:1"), "scene_1");
        assert_eq!(sanitise_filename("  .hidden"), "hidden");
        assert_eq!(sanitise_filename(""), "segment");
    }

    #[test]
    fn sanitise_filename_handles_all_illegal_chars() {
        assert_eq!(sanitise_filename(r#"a\b/c:d*e?f"g<h>i|j"#), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn unique_export_path_avoids_collisions() {
        let dir = std::path::Path::new("/tmp");
        let mut used = std::collections::HashSet::new();
        let p1 = unique_export_path(dir, "clip", "mp4", &mut used);
        let p2 = unique_export_path(dir, "clip", "mp4", &mut used);
        let p3 = unique_export_path(dir, "clip", "mp4", &mut used);
        assert!(p1.to_str().unwrap().ends_with("clip.mp4"));
        assert!(p2.to_str().unwrap().ends_with("clip_2.mp4"));
        assert!(p3.to_str().unwrap().ends_with("clip_3.mp4"));
    }

    #[test]
    fn unique_export_path_distinct_stems_dont_collide() {
        let dir = std::path::Path::new("/tmp");
        let mut used = std::collections::HashSet::new();
        let p1 = unique_export_path(dir, "intro", "mp4", &mut used);
        let p2 = unique_export_path(dir, "outro", "mp4", &mut used);
        assert!(p1.to_str().unwrap().ends_with("intro.mp4"));
        assert!(p2.to_str().unwrap().ends_with("outro.mp4"));
    }

    #[test]
    fn timecode_to_frame_rounds_to_nearest() {
        use klipp_core::RationalTime;
        // 2 seconds at 24 fps -> frame 48
        let tc = RationalTime { value: 48, rate: 24 };
        assert_eq!(timecode_to_frame(&tc, 24.0), 48);
        // 1.5 seconds at 30 fps -> frame 45
        let tc = RationalTime { value: 45, rate: 30 };
        assert_eq!(timecode_to_frame(&tc, 30.0), 45);
    }
}

#[cfg(test)]
mod export_foot_tests {
    use super::{export_foot_layout, EXPORT_ACTION_BAR_H, EXPORT_SUMMARY_H};
    use crate::theme::control::REGULAR;

    /// The rule: whatever the window is doing, the strip reserved for
    /// the Export foot is never taller than what it was given, and the
    /// buttons inside it are never shorter than a button.
    ///
    /// This is the one that matters. The summary line was added inside
    /// a strip sized for the buttons alone, which pushed Export — the
    /// panel's whole point — down under the status bar and sliced it in
    /// half. A fix for that height is not a guarantee; this is.
    #[test]
    fn the_foot_never_overflows_and_the_buttons_are_never_clipped() {
        let mut heights: Vec<f32> = (0..400).map(|i| i as f32 * 4.0).collect();
        heights.extend([0.0, 1.0, REGULAR, REGULAR * 2.0, 47.0, 48.0, 1440.0, 4320.0]);
        for h in heights {
            let f = export_foot_layout(h);
            assert!(f.bar_h >= 0.0, "negative strip at {h}");
            assert!(f.bar_h <= h + 1e-3, "strip {} exceeds the panel's {h}", f.bar_h);

            // What the drawing actually puts in: the pad, then the
            // content. Both together must sit inside the strip, or the
            // bottom of the content is under whatever follows it.
            let laid_out = f.pad + f.content_h;
            assert!(
                laid_out <= f.bar_h + 1e-3,
                "at {h}: pad {} + content {} = {laid_out} overflows the strip {}",
                f.pad,
                f.content_h,
                f.bar_h,
            );

            // What is drawn, clamped to the strip: a panel too small
            // for a button cannot be given one.
            let want = if f.show_summary { EXPORT_SUMMARY_H + REGULAR } else { REGULAR };
            let want = want.min(f.bar_h);
            assert!(
                (f.content_h - want).abs() < 1e-3,
                "at {h}: content {} does not match what is drawn ({want})",
                f.content_h,
            );
            if h >= REGULAR {
                assert!(
                    f.content_h >= REGULAR - 1e-3,
                    "at {h}: the buttons were squeezed to {}",
                    f.content_h,
                );
            }
        }
    }

    /// When the panel is too short for both, the summary gives way and
    /// the buttons keep their full height — not the other way round.
    #[test]
    fn a_short_panel_drops_the_summary_rather_than_the_buttons() {
        // Half of this is under the two rows but over one.
        let tight = (EXPORT_SUMMARY_H + REGULAR) * 2.0 - 4.0;
        let f = export_foot_layout(tight);
        assert!(!f.show_summary, "the summary was kept in a panel too short for it");
        assert!(f.content_h >= REGULAR - 1e-3, "the buttons were squeezed: {}", f.content_h);
        assert!(f.pad + f.content_h <= f.bar_h + 1e-3, "it still overflowed");
    }

    /// And a panel with room gets both.
    #[test]
    fn a_normal_panel_shows_the_summary() {
        let f = export_foot_layout(1440.0);
        assert!(f.show_summary, "a full-height panel hid the summary");
        assert!((f.bar_h - EXPORT_ACTION_BAR_H).abs() < 1e-3, "got {}", f.bar_h);
    }

    /// The comfortable height has to actually fit what goes in it,
    /// which is the invariant the original 48 quietly broke.
    #[test]
    fn the_declared_height_fits_its_contents() {
        assert!(
            EXPORT_ACTION_BAR_H >= EXPORT_SUMMARY_H + REGULAR,
            "{EXPORT_ACTION_BAR_H} cannot hold {EXPORT_SUMMARY_H} + {REGULAR}",
        );
    }
}

#[cfg(test)]
mod export_summary_tests {
    use super::{crf_mbps_range, humanise_bytes, humanise_secs, intra_frame_mbps};

    /// #19: a quality encode gets a range; better quality and more
    /// pixels cost more; HEVC less than H.264; a GIF has none.
    #[test]
    fn a_quality_export_is_sized_as_a_range() {
        use klipp_core::ExportVideoCodec as C;
        let hd = 1920.0 * 1080.0;
        let (lo, hi) = crf_mbps_range(C::H264, 23, hd, 30.0).unwrap();
        assert!((lo - 3.0).abs() < 1e-9 && (hi - 12.0).abs() < 1e-9);
        assert!(crf_mbps_range(C::H264, 17, hd, 30.0).unwrap().0 > lo * 1.9, "six steps better doubles it");
        assert!(crf_mbps_range(C::H264, 23, 3840.0 * 2160.0, 30.0).unwrap().1 > hi * 2.0, "4K costs more");
        assert!(crf_mbps_range(C::H265, 28, hd, 30.0).unwrap().1 < hi, "HEVC needs less");
        assert!(crf_mbps_range(C::Gif, 23, hd, 30.0).is_none());
        assert!(crf_mbps_range(C::ProRes422, 23, hd, 30.0).is_none(), "intra codecs are sized from their tables");
    }
    use klipp_core::ExportVideoCodec as C;

    /// The rule the whole feature rests on: a number is only shown
    /// where it is arithmetic. CRF has no target bitrate by
    /// construction — the encoder spends what the picture needs — so
    /// there is nothing to compute and nothing is claimed.
    #[test]
    fn the_codecs_that_cannot_be_sized_are_not_sized() {
        let px = 1920.0 * 1080.0;
        for codec in [C::H264, C::H265, C::Av1, C::Vp9, C::Gif] {
            assert_eq!(
                intra_frame_mbps(codec, px, 25.0),
                None,
                "{codec:?} was given a size it cannot have",
            );
        }
    }

    /// And the ones that can are, from their published rates.
    #[test]
    fn the_intra_frame_masters_are_sized_from_their_profiles() {
        let hd = 1920.0 * 1080.0;
        // Apple quotes ProRes 422 at 147 Mbps for 1080p29.97.
        let at_reference = intra_frame_mbps(C::ProRes422, hd, 29.97).unwrap();
        assert!((at_reference - 147.0).abs() < 0.5, "got {at_reference}");

        // HQ is about half again, 4444 about double.
        let hq = intra_frame_mbps(C::ProRes422HQ, hd, 29.97).unwrap();
        assert!(hq > at_reference * 1.4 && hq < at_reference * 1.6, "got {hq}");

        // Four times the pixels is four times the rate.
        let uhd = intra_frame_mbps(C::ProRes422, 3840.0 * 2160.0, 29.97).unwrap();
        assert!((uhd / at_reference - 4.0).abs() < 0.01, "got {}", uhd / at_reference);

        // And half the frame rate is half the rate.
        let slow = intra_frame_mbps(C::ProRes422, hd, 14.985).unwrap();
        assert!((slow / at_reference - 0.5).abs() < 0.01, "got {}", slow / at_reference);
    }

    /// A ProRes 422 master of a minute at 1080p29.97 is about 1.1 GB,
    /// which is the sort of number the form exists to warn about.
    #[test]
    fn a_minute_of_prores_is_about_a_gigabyte() {
        let mbps = intra_frame_mbps(C::ProRes422, 1920.0 * 1080.0, 29.97).unwrap();
        let bytes = mbps * 1_000_000.0 / 8.0 * 60.0;
        assert!(
            (1.0e9..1.3e9).contains(&bytes),
            "expected about 1.1 GB, got {}",
            humanise_bytes(bytes),
        );
    }

    #[test]
    fn lengths_read_as_lengths() {
        assert_eq!(humanise_secs(4.25), "4.2 s");
        assert_eq!(humanise_secs(84.0), "1 min 24 s");
        assert_eq!(humanise_secs(3725.0), "1 h 02 min 05 s");
        // A negative range — an in-point dragged past its out-point —
        // must not print itself.
        assert_eq!(humanise_secs(-3.0), "0.0 s");
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(humanise_bytes(148_000_000.0), "148 MB");
        assert_eq!(humanise_bytes(1_100_000_000.0), "1.1 GB");
    }
}

#[cfg(test)]
mod true_peak_tests {
    use super::*;

    #[test]
    fn the_atsc_entry_asks_for_minus_two_and_the_others_leave_the_default() {
        let mut d = ExportDialogState { normalize_loudness: true, loudness_preset: 3, ..Default::default() };
        assert_eq!((dialog_target_lufs(&d), dialog_true_peak(&d)), (Some(-24.0), Some(-2.0)));
        for other in [0, 1, 2, 4] {
            d.loudness_preset = other;
            assert_eq!(dialog_true_peak(&d), None, "entry {other}");
        }
        // No normalisation, no ceiling.
        d.loudness_preset = 3;
        d.normalize_loudness = false;
        assert_eq!(dialog_true_peak(&d), None);
    }
}

#[cfg(test)]
mod export_rule_tests {
    use super::{export_step, matte_gap_lines, next_free_name, offline_clips, segment_outputs, source_written_over, Confirmed, ExportStep};
    use std::path::{Path, PathBuf};

    fn offline() -> Vec<(String, String)> {
        vec![("a.mov".into(), "00:00:01:00".into())]
    }

    /// #826: offline media is asked about first, then a source about to be
    /// written over; a confirmed offline warning does not pass the
    /// overwrite check, only the overwrite's own confirmation does.
    #[test]
    fn an_export_asks_about_offline_media_then_about_a_source_it_would_destroy() {
        assert_eq!(export_step(Confirmed::Nothing, Vec::new(), None), ExportStep::Queue);
        assert_eq!(export_step(Confirmed::Nothing, offline(), Some("b.mov".into())), ExportStep::AskOffline(offline()), "offline first");
        assert_eq!(export_step(Confirmed::Offline, Vec::new(), Some("b.mov".into())), ExportStep::AskOverwrite("b.mov".into()), "past offline, still asked");
        assert_eq!(export_step(Confirmed::Nothing, Vec::new(), Some("b.mov".into())), ExportStep::AskOverwrite("b.mov".into()));
        assert_eq!(export_step(Confirmed::Overwrite, Vec::new(), Some("b.mov".into())), ExportStep::Queue, "accepted");
        assert_eq!(export_step(Confirmed::Offline, offline(), None), ExportStep::Queue, "offline was confirmed");
    }

    #[test]
    fn offline_clips_are_named_with_their_place() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/gone.mov".into(), None);
        item.offline = true;
        let gone = pool.add(item);
        let here = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/here.mov".into(), None));
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks[0].clips = vec![klipp_timeline::Clip::media(gone, tc(2.0), tc(4.0), tc(0.0), tc(2.0)), klipp_timeline::Clip::media(here, tc(4.0), tc(6.0), tc(0.0), tc(2.0))];
        let named = offline_clips(&seq, &pool);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].0, "gone.mov");
    }

    /// #239: a clip whose tracked matte falls short is named once, with
    /// how far it got, or its pass's percent while one runs.
    #[test]
    fn short_mattes_are_named_with_how_far_they_got() {
        let mut pool = klipp_media::MediaPool::new();
        let a = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/walk.mov".into(), None));
        let b = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/run.mov".into(), None));
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks[0].clips = vec![klipp_timeline::Clip::media(a, tc(0.0), tc(2.0), tc(0.0), tc(2.0)), klipp_timeline::Clip::media(b, tc(2.0), tc(4.0), tc(0.0), tc(2.0))];
        let (ca, cb) = (seq.tracks[0].clips[0].id, seq.tracks[0].clips[1].id);
        let gap = |clip, folder: &str| klipp_compose::subject_matte::Uncovered { clip, transition: None, subject: None, folder: PathBuf::from(folder), covered: 20, needed: 50, from_secs: 0.0, to_secs: 2.0 };
        // Two folders on the first clip: one line for it; a transition
        // cutting round its subject (#985 phase 3), another.
        let tid = klipp_core::TransitionId::new();
        let gaps = [gap(ca, "/nowhere/a"), gap(ca, "/nowhere/b"), gap(cb, "/nowhere/c"), klipp_compose::subject_matte::Uncovered { transition: Some((tid, 0)), ..gap(ca, "") }];
        let lines = matte_gap_lines(&gaps, &seq, &pool, |c| (c == cb).then_some(0.42));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].0, ca);
        assert!(matches!(&lines[0].1, klipp_core::AppCommand::TrackSubject { clip_id, model, .. } if *clip_id == ca && model == "subject"));
        assert!(lines[0].2.starts_with("walk.mov at "), "{}", lines[0].2);
        assert!(lines[0].2.ends_with("20 of 50 frames tracked"), "{}", lines[0].2);
        assert!(lines[1].2.ends_with("tracking, 42%"), "{}", lines[1].2);
        assert!(matches!(&lines[2].1, klipp_core::AppCommand::TrackTransitionSubject { transition_id } if *transition_id == tid));
        assert!(lines[2].2.contains("(transition): 20 of 50"), "{}", lines[2].2);
    }

    #[test]
    fn a_render_over_a_source_is_found_by_where_it_points() {
        let dir = std::env::temp_dir().join(format!("klipp-export-over-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let source = dir.join("take.mov");
        std::fs::write(&source, b"x").unwrap();
        let other = dir.join("other.mov");
        std::fs::write(&other, b"y").unwrap();
        let sources = [source.as_path(), other.as_path()];
        // The same file, reached another way.
        assert_eq!(source_written_over(&dir.join("sub/../take.mov"), sources), Some("take.mov".into()));
        assert_eq!(source_written_over(&dir.join("render.mp4"), sources), None, "a new file is no conflict");
        assert_eq!(source_written_over(&dir.join("nowhere/render.mp4"), sources), None, "nor one in a folder not made yet");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #2074: the list marks each 10-bit preset, and picking one sets the
    /// dialog's depth to the preset's: ten for the Main10 master, back to
    /// eight for an 8-bit preset after it.
    #[test]
    fn ten_bit_presets_are_labelled_and_carry_their_depth() {
        let presets = klipp_core::builtin_presets();
        let named = |n: &str| presets.iter().find(|p| p.name == n).unwrap();
        assert_eq!(super::preset_label(named("HEVC Main10 master")), "HEVC Main10 master · 10-bit");
        assert_eq!(super::preset_label(named("ProRes 422")), "ProRes 422 · 10-bit");
        assert_eq!(super::preset_label(named("YouTube 1080p")), "YouTube 1080p");
        let mut dialog = super::ExportDialogState::default();
        super::apply_preset_to_dialog(&mut dialog, &named("HEVC Main10 master").settings);
        assert_eq!((dialog.video_codec, dialog.bit_depth), (klipp_core::ExportVideoCodec::H265, 10));
        super::apply_preset_to_dialog(&mut dialog, &named("YouTube 1080p").settings);
        assert_eq!(dialog.bit_depth, 0);
    }

    #[test]
    fn auto_rename_takes_the_first_free_number() {
        let taken = [PathBuf::from("/out/film_1.mp4"), PathBuf::from("/out/film_2.mp4")];
        assert_eq!(next_free_name(Path::new("/out/film.mp4"), |p| taken.iter().any(|t| t == p)), PathBuf::from("/out/film_3.mp4"));
        assert_eq!(next_free_name(Path::new("/out/film"), |_| false), PathBuf::from("/out/film_1"));
        assert_eq!(next_free_name(Path::new("/out/film.mp4"), |_| true), PathBuf::from("/out/film.mp4"), "nothing free: the original, so the warning comes again");
    }

    #[test]
    fn marker_segments_are_named_by_their_markers_once_each() {
        let segments = vec![(0, 10, "start".to_string()), (10, 20, "Intro: take 2".to_string()), (20, 30, "Intro: take 2".to_string()), (30, 40, "end".to_string())];
        let out = segment_outputs(&segments, Path::new("/out/film.mov"));
        let names: Vec<String> = out.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names[0], "film_segment_1.mov", "a bookend is numbered");
        assert!(names[1].starts_with("film_Intro") && names[1].ends_with(".mov"), "{names:?}");
        assert_ne!(names[1], names[2], "the same label twice is two files");
        assert_eq!(names[3], "film_segment_4.mov");
        assert!(out.iter().all(|(p, _)| p.parent() == Some(Path::new("/out"))));
        assert_eq!(out[3].1, "film_segment_4.mov [4/4]");
        // No extension on the chosen path: mp4.
        assert!(segment_outputs(&segments[..1], Path::new("/out/film"))[0].0.to_string_lossy().ends_with(".mp4"));
    }
}

impl KlippApp {
    pub(super) fn poll_render_queue(&mut self) {
        // #62: propagate the user's parallel-jobs pref into the queue each
        // frame. Pref is read at job-start time, so live updates just apply
        // on the next spawn without disturbing in-flight jobs.
        self.state.render_queue.max_parallel =
            self.prefs.export.parallel_render_jobs.max(1);
        let events = self.state.render_queue.poll();
        let idle = self.renders_idle();
        self.export_dialog.banding.poll(idle);
        // The project as the control socket sees it.
        if let Ok(mut project) = self.state.control_shared.project.lock() {
            project.recent_projects = self.prefs.general.recent_projects.iter().map(crate::prefs::RecentProject::control_summary).collect();
            project.path = self.state.project_path.clone();
            project.dirty = self.state.unsaved;
            project.undo = self.state.undo_label();
            project.redo = self.state.redo_label();
            project.undo_depth = self.state.undo_depth();
            project.redo_depth = self.state.redo_depth();
            let (undo_scope, redo_scope) = self.state.undo_scopes();
            (project.undo_scope, project.redo_scope) = (undo_scope.map(str::to_owned), redo_scope.map(str::to_owned));
            (project.sequences, project.active_sequence) = self.state.sequence_summaries();
        }
        self.publish_control_others();
        // And where the user is, so a frame read can put the playhead back.
        // Not while a seek is on its way: the playhead has not reached it
        // yet, and this would publish where it was before.
        if !self.state.awaiting_seek_frame {
            self.publish_transport(None);
        }
        // The queue as the control socket sees it, refreshed every frame
        // the queue is polled; cheap, the queue is short.
        if let Ok(mut exports) = self.state.control_shared.exports.lock() {
            exports.clear();
            exports.extend(self.state.render_queue.jobs.iter().map(|j| klipp_core::ExportStatus {
                job_id: j.id.to_string(),
                path: j.output_path.clone(),
                status: match j.status {
                    klipp_export::RenderJobStatus::Queued => "queued",
                    klipp_export::RenderJobStatus::Rendering => "rendering",
                    klipp_export::RenderJobStatus::Complete => "complete",
                    klipp_export::RenderJobStatus::Failed => "failed",
                    klipp_export::RenderJobStatus::Cancelled => "cancelled",
                }
                .to_string(),
                progress: j.progress,
                frames_done: j.frames_done,
                total_frames: j.total_frames,
                stage: j.render_stage.clone(),
                error: j.error.clone(),
                loudness: j.loudness_result.clone(),
                hardware: j.hw_info.clone(),
                fps: j.started_at.map(|t| t.elapsed().as_secs_f32()).filter(|s| *s > 2.0 && j.frames_done > 0).map(|s| j.frames_done as f32 / s),
                eta_secs: j.started_at
                    .map(|t| t.elapsed().as_secs_f32())
                    .filter(|s| *s > 2.0 && j.frames_done > 0 && j.status == klipp_export::RenderJobStatus::Rendering)
                    .map(|s| ((j.total_frames.saturating_sub(j.frames_done)) as f32 * s / j.frames_done as f32) as u64),
                projected_bytes: j.projected_bytes(),
                encoder: j.timings.as_ref().and_then(|t| t.video_encoder.clone()),
                audio_encoder: j.timings.as_ref().and_then(|t| t.audio_encoder.clone()),
                compositing: j.timings.as_ref().and_then(|t| t.compositing()),
                timings: j.timings.clone(),
                banding: self.export_dialog.banding.state_for(j.id, &j.settings).map(str::to_string),
                note: self.export_dialog.banding.note_for(j.id, &j.settings),
            }));
        }
        for event in events {
            match event {
                klipp_export::RenderQueueEvent::Complete { id, output_path } => {
                    tracing::info!("Render job {} complete: {}", id, output_path.display());
                    // The credits of what it played, beside the file, now
                    // that there is one (#1604).
                    let heard = self.state.render_queue.jobs.iter().find(|j| j.id == id).map(|j| j.heard_media.clone()).unwrap_or_default();
                    match klipp_audio_search::write_credits(&output_path, &heard) {
                        Ok(Some((file, count))) => tracing::info!("credits for {count} file(s) written to {}", file.display()),
                        Ok(None) => {}
                        Err(e) => tracing::warn!("the credits list beside {} could not be written: {e}", output_path.display()),
                    }
                    // #63 Tier-1: open output folder if the pref is enabled.
                    if self.prefs.export.open_folder_after_export {
                        if let Some(parent) = output_path.parent() {
                            if let Err(e) = crate::ui_util::open_external(parent) {
                                tracing::warn!(
                                    "open_folder_after_export: xdg-open failed for {}: {}",
                                    parent.display(),
                                    e
                                );
                            }
                        }
                    }
                    // F-163: run post-export shell command with {file}/{dir}
                    // substitution. Previously fire-and-forget — spawn()
                    // returned immediately and any command failure was
                    // invisible. Now we pipe stdout + stderr, wait for
                    // the process in a detached thread, and log the exit
                    // code plus a tail of stderr on non-zero exit so
                    // users can diagnose a broken hook instead of
                    // wondering why the file never appeared on their
                    // rsync target.
                    // The paths go in environment variables, never into
                    // the command's text (#820, crate::export_hook).
                    if let Some(mut hook) = crate::export_hook::command(&self.prefs.export.post_export_command, &output_path) {
                        let cmd = format!("{} (file {})", self.prefs.export.post_export_command.trim(), output_path.display());
                        match hook
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                        {
                            Ok(child) => {
                                tracing::info!("post_export_command launched: {cmd}");
                                // Reap the process off the UI thread so
                                // a long-running hook doesn't block the
                                // event loop; the exit code is logged
                                // when it terminates.
                                let cmd_for_log = cmd.clone();
                                std::thread::spawn(move || {
                                    match child.wait_with_output() {
                                        Ok(out) if out.status.success() => {
                                            tracing::info!(
                                                "post_export_command completed (exit 0): {cmd_for_log}"
                                            );
                                        }
                                        Ok(out) => {
                                            // Trim stderr to a sane tail — a
                                            // verbose tool could dump MB of
                                            // text to the log otherwise.
                                            let stderr = String::from_utf8_lossy(&out.stderr);
                                            let tail: String = stderr.lines().rev().take(10)
                                                .collect::<Vec<_>>().into_iter().rev()
                                                .collect::<Vec<_>>().join("\n");
                                            tracing::warn!(
                                                "post_export_command exited with {} ({cmd_for_log})\n--- stderr tail ---\n{}\n--- end stderr ---",
                                                out.status, tail,
                                            );
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "post_export_command wait failed ({cmd_for_log}): {e}"
                                            );
                                        }
                                    }
                                });
                            }
                            Err(e) => tracing::warn!(
                                "post_export_command failed to launch ({cmd}): {e}"
                            ),
                        }
                    }
                }
                klipp_export::RenderQueueEvent::Failed { id, error } => {
                    tracing::error!("Render job {} failed: {}", id, error);
                }
                klipp_export::RenderQueueEvent::Progress { .. } => {}
            }
        }
    }
}

impl KlippApp {
    pub(super) fn show_render_queue_window(&mut self, ctx: &egui::Context) {
        if !self.state.show_render_queue {
            return;
        }
        // The Export workspace shows the queue in its column; no window
        // on top of it.
        if self.state.active_workspace == crate::state::Workspace::Export {
            self.state.show_render_queue = false;
            return;
        }
        let actions = crate::panels::render_queue::RenderQueuePanel::show(
            ctx,
            &self.state.render_queue,
            &self.theme,
        );
        self.apply_render_queue_actions(actions);
    }

    pub(super) fn apply_render_queue_actions(&mut self, actions: Vec<crate::panels::render_queue::RenderQueueAction>) {
        for action in actions {
            match action {
                crate::panels::render_queue::RenderQueueAction::CancelJob(id) => {
                    self.state.render_queue.cancel_job(id);
                }
                crate::panels::render_queue::RenderQueueAction::RemoveJob(id) => {
                    self.state.render_queue.remove_job(id);
                }
                crate::panels::render_queue::RenderQueueAction::ClearCompleted => {
                    self.state.render_queue.clear_completed();
                }
                crate::panels::render_queue::RenderQueueAction::Close => {
                    self.state.show_render_queue = false;
                }
                crate::panels::render_queue::RenderQueueAction::Play(path) => {
                    if let Err(e) = crate::ui_util::open_external(&path) {
                        self.state.toast.show(format!("Could not open {}: {e}", path.display()));
                    }
                }
                crate::panels::render_queue::RenderQueueAction::ShowInFolder(path) => {
                    self.send_commands(vec![AppCommand::RevealInFileManager { path }]);
                }
                crate::panels::render_queue::RenderQueueAction::RenderAgain(id) => self.render_job_again(id),
            }
        }
    }

    /// A finished, failed or cancelled job queued again (#717): its
    /// settings, file and frames, from its sequence as it is now, through
    /// the same checks as an export from the dialog. It takes the old
    /// job's place on the list.
    fn render_job_again(&mut self, id: uuid::Uuid) {
        let Some(job) = self.state.render_queue.jobs.iter().find(|j| j.id == id) else { return };
        let (settings, label, range, sequence_id) = (job.settings.clone(), job.label.clone(), job.export_range, job.sequence_id);
        let Some(seq_idx) = self.state.sequences.iter().position(|s| s.id == sequence_id) else {
            self.state.toast.show("The sequence this was rendered from is gone; export it again from the dialog");
            return;
        };
        let seq = &self.state.sequences[seq_idx];
        let offline = seq.tracks.iter().flat_map(|t| t.clips.iter()).filter_map(|c| c.media_id()).filter(|m| self.state.media_pool.get(m).is_some_and(|i| i.offline)).count();
        if offline > 0 {
            self.state.toast.show(format!("{offline} clip{} in it {} offline; relink before rendering again", if offline == 1 { "" } else { "s" }, if offline == 1 { "is" } else { "are" }));
            return;
        }
        if let Some(name) = self.find_export_overwrite_conflict(&settings.output_path) {
            self.state.toast.show(format!("{} is source media in this project; rendering over it would destroy it", name));
            return;
        }
        if !matte_gaps(&self.state.sequences[seq_idx], &self.state.media_pool).is_empty() {
            self.state.toast.show("A tracked subject matte in it isn't finished; track the rest before rendering again");
            return;
        }
        let snapshot = klipp_export::RenderSnapshot::from_sequence_with_all(
            &self.state.sequences[seq_idx],
            &self.state.sequences,
            &self.state.media_pool,
            &settings,
            self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
            false,
            self.state.project_working_colorspace,
        );
        match snapshot {
            Ok(mut snap) => {
                snap.sequence_snapshot.export_range = range;
                snap.sequence_snapshot.video.global_lut_opacity = self.state.global_lut_opacity;
                self.state.render_queue.remove_job(id);
                self.state.render_queue.add_job(settings.output_path.clone(), settings, label, snap);
            }
            Err(e) => self.state.toast.show(format!("Could not render again: {e}")),
        }
    }
}

impl KlippApp {
    pub(super) fn handle_export_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        if self.handle_render_commands(cmd) {
            return true;
        }
        match cmd {
            AppCommand::CaptureWindow { path, max_width } => {
                self.state.window_capture = Some((path.clone(), *max_width, false));
                true
            }
            AppCommand::ShowPreferences => {
                self.state.show_preferences = true;
                true
            }
            AppCommand::OpenClipProperties { media_id } => {
                self.state.clip_properties_media_id = Some(*media_id);
                self.state.clip_properties_tab = crate::state::ClipPropertiesTab::default();
                self.state.show_clip_properties = true;
                true
            }
            AppCommand::GradeTrackInColour { track_id } => {
                self.state.grade_track_in_colour(*track_id);
                true
            }
            AppCommand::ShowPanel { workspace, inspector_tab } => {
                if super::main_layout::show_panel(&mut self.state, &mut self.layout, workspace.as_deref(), inspector_tab.as_deref()) {
                    self.layout_dirty = true;
                }
                true
            }
            AppCommand::ApplyBuiltinWorkspaceLayout { index } => {
                let builtins = crate::state::builtin_workspace_layouts();
                if let Some(layout) = builtins.get(*index).cloned() {
                    self.apply_workspace_layout(&layout);
                    self.state.toast.show(format!("Applied workspace: {}", layout.name));
                }
                true
            }
            AppCommand::ApplyCustomWorkspaceLayout { name } => {
                if let Some(layout) = self.state.custom_workspaces.iter()
                    .find(|l| l.name == *name)
                    .cloned()
                {
                    self.apply_workspace_layout(&layout);
                    self.state.toast.show(format!("Applied workspace: {}", layout.name));
                }
                true
            }
            AppCommand::DeleteWorkspaceLayout { name } => {
                let before = self.state.custom_workspaces.len();
                self.state.custom_workspaces.retain(|l| l.name != *name);
                if self.state.custom_workspaces.len() != before {
                    if self.state.workspace_manager_selected.as_deref() == Some(name.as_str()) {
                        self.state.workspace_manager_selected = None;
                    }
                    persist_workspaces(&self.state.custom_workspaces);
                    self.state.toast.show("Workspace deleted".to_string());
                }
                true
            }
            AppCommand::RenameWorkspaceLayout { old_name, new_name } => {
                let trimmed = new_name.trim().to_string();
                if !trimmed.is_empty()
                    && !self.state.custom_workspaces.iter().any(|l| l.name == trimmed)
                {
                    if let Some(l) = self.state.custom_workspaces
                        .iter_mut()
                        .find(|l| l.name == *old_name)
                    {
                        l.name = trimmed.clone();
                        if self.state.workspace_manager_selected.as_deref() == Some(old_name.as_str()) {
                            self.state.workspace_manager_selected = Some(trimmed);
                        }
                        persist_workspaces(&self.state.custom_workspaces);
                    }
                }
                true
            }
            AppCommand::JogFrames { frames } => {
                // Reuse the existing SeekRelative(i64) plumbing — the
                // transport-bar step buttons already dispatch ±1 through
                // this path, so `JogFrames { frames: N }` is identical
                // except it bundles the N samples the wheel widget has
                // accumulated since its last emit. Clamping + playhead
                // math lives in the SeekRelative handler, no duplication.
                if *frames != 0 {
                    let _ = self.cmd_tx.send(AppCommand::SeekRelative(*frames as i64));
                }
                true
            }
            AppCommand::ImportSubtitles { path } => {
                let path = path.clone();
                // EBU-STL is binary; the rest are text (#1516).
                let is_stl = path.extension().and_then(|s| s.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("stl"));
                let read = std::fs::read(&path).and_then(|bytes| {
                    if is_stl {
                        Ok((String::new(), bytes))
                    } else {
                        String::from_utf8(bytes).map(|s| (s, Vec::new())).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
                    }
                });
                match read {
                    Ok((content, bytes)) => {
                        // Detect format by extension (case-insensitive). Unknown
                        // extensions fall back to the SRT/VTT parser; TTML,
                        // DFXP, IMSC and iTT read as TTML, SCC as CEA-608 and
                        // STL as EBU Tech 3264 (#1516).
                        let ext = path
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_ascii_lowercase())
                            .unwrap_or_default();
                        let is_ass = ext == "ass" || ext == "ssa";
                        let is_ttml = matches!(ext.as_str(), "ttml" | "dfxp" | "xml" | "itt");
                        let mut unreadable = None;

                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                        let fps_rate = seq.fps.rate();
                        // One new track per imported file, named after the file stem.
                        let track_name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Subtitles")
                            .to_string();
                        let new_track_id = seq.add_subtitle_track(track_name);
                        let sub_track_idx = seq
                            .tracks
                            .iter()
                            .position(|t| t.id == new_track_id)
                            .unwrap();
                        let track = &mut seq.tracks[sub_track_idx];
                        track.source_file = Some(path.clone());
                        // The language the file names, where its format
                        // says one (#2223).
                        track.language = klipp_captions::file_language(&path, if is_stl { &bytes } else { content.as_bytes() });

                        let imported_count;
                        if is_ass {
                            let (entries, styles) = parse_ass(&content);
                            imported_count = entries.len();
                            for entry in &entries {
                                let timeline_in = klipp_core::RationalTime::from_secs_f64(entry.start_secs, fps_rate);
                                let timeline_out = klipp_core::RationalTime::from_secs_f64(entry.end_secs, fps_rate);
                                let dur = timeline_out - timeline_in;
                                let style = styles.get(&entry.style_name);

                                let (font_name, font_size, color, outline, shadow, font_style) = match style {
                                    Some(s) => {
                                        let fs = match (s.bold, s.italic) {
                                            (false, false) => klipp_timeline::FontStyle::Regular,
                                            (true, false) => klipp_timeline::FontStyle::Bold,
                                            (false, true) => klipp_timeline::FontStyle::Italic,
                                            (true, true) => klipp_timeline::FontStyle::BoldItalic,
                                        };
                                        let outline = if s.outline > 0.0 {
                                            Some(klipp_timeline::TextOutline { color: [0, 0, 0, 255], width: s.outline, enabled: true, width_keyframes: Vec::new() })
                                        } else { None };
                                        let shadow = if s.shadow > 0.0 {
                                            Some(klipp_timeline::TitleShadow {
                                                color: [0, 0, 0, 180],
                                                offset_x: s.shadow,
                                                offset_y: s.shadow,
                                                blur: s.shadow,
                                                enabled: true,
                                                offset_x_keyframes: Vec::new(),
                                                offset_y_keyframes: Vec::new(),
                                                blur_keyframes: Vec::new(),
                                            })
                                        } else { None };
                                        (
                                            if s.font_name.is_empty() { String::new() } else { s.font_name.clone() },
                                            if s.font_size > 0.0 { s.font_size } else { 32.0 },
                                            s.primary_color,
                                            outline,
                                            shadow,
                                            fs,
                                        )
                                    }
                                    None => (
                                        String::new(),
                                        32.0,
                                        [255, 255, 255, 255],
                                        Some(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: 2.0, enabled: true, width_keyframes: Vec::new() }),
                                        None,
                                        klipp_timeline::FontStyle::Regular,
                                    ),
                                };

                                let mut tb = klipp_timeline::TextBox {
                                    text: entry.text.clone(),
                                    font_size,
                                    font_name,
                                    font_style,
                                    color,
                                    alignment: klipp_timeline::TextAlignment::Center,
                                    ..Default::default()
                                };
                                let mut fx = klipp_timeline::LayerFx::default();
                                if let Some(o) = outline { fx.outlines.push(o); }
                                if let Some(s) = shadow { fx.shadows.push(s); }
                                tb.ass_style_name = if entry.style_name.is_empty() { None } else { Some(entry.style_name.clone()) };
                                // Its override tags' styling and placement (#1515).
                                cue_style_runs(&mut tb, &entry.spans);
                                let mut title_data = klipp_timeline::TitleData::single_text_fx(tb, fx);
                                place_cue(&mut title_data, entry.placement);
                                let clip = klipp_timeline::Clip {
                                    id: klipp_core::ClipId::new(),
                                    kind: klipp_timeline::ClipKind::Title(title_data),
                                    timeline_in,
                                    timeline_out,
                                    source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                                    source_out: dur,
                                    ..klipp_timeline::Clip::gap(timeline_in, timeline_out)
                                };
                                track.clips.push(clip);
                            }
                        } else {
                            let read = if is_ttml {
                                Some(klipp_captions::ttml::parse_ttml(&content))
                            } else if ext == "scc" {
                                Some(klipp_captions::scc::parse_scc(&content))
                            } else if is_stl {
                                Some(klipp_captions::stl::parse_stl(&bytes))
                            } else {
                                None
                            };
                            let subs = if let Some(read) = read {
                                read.unwrap_or_else(|e| {
                                    unreadable = Some(e);
                                    Vec::new()
                                })
                            } else {
                                parse_srt_vtt(&content)
                            };
                            imported_count = subs.len();
                            for sub in &subs {
                                let timeline_in = klipp_core::RationalTime::from_secs_f64(sub.start_secs, fps_rate);
                                let timeline_out = klipp_core::RationalTime::from_secs_f64(sub.end_secs, fps_rate);
                                let dur = timeline_out - timeline_in;
                                let mut tb = klipp_timeline::TextBox {
                                    text: sub.text.clone(),
                                    font_size: 32.0,
                                    font_name: String::new(),
                                    alignment: klipp_timeline::TextAlignment::Center,
                                    ..Default::default()
                                };
                                // Its tags' italics, bold, underline and colour (#1515).
                                cue_style_runs(&mut tb, &sub.spans);
                                let mut fx = klipp_timeline::LayerFx::default();
                                fx.outlines.push(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: 2.0, enabled: true, width_keyframes: Vec::new() });
                                let mut title_data = klipp_timeline::TitleData::single_text_fx(tb, fx);
                                place_cue(&mut title_data, sub.placement);
                                let clip = klipp_timeline::Clip {
                                    id: klipp_core::ClipId::new(),
                                    kind: klipp_timeline::ClipKind::Title(title_data),
                                    timeline_in,
                                    timeline_out,
                                    source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                                    source_out: dur,
                                    ..klipp_timeline::Clip::gap(timeline_in, timeline_out)
                                };
                                track.clips.push(clip);
                            }
                        }

                        if imported_count == 0 {
                            // Remove the newly-created empty track and warn.
                            let tid = track.id;
                            let _ = seq.remove_track(tid);
                            self.state.toast.show(unreadable.unwrap_or_else(|| "No subtitles found in file".to_string()));
                        } else {
                            track.sort_clips();
                            self.state.toast.show(format!(
                                "Imported {} subtitle(s) into \"{}\"",
                                imported_count,
                                seq.tracks[sub_track_idx].name,
                            ));
                            self.state.dirty = true;
                        }
                    }
                    Err(e) => {
                        self.state.toast.show(format!("Failed to read file: {e}"));
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::ImportKdenlive { path } | AppCommand::ImportFcpxml { path }
            | AppCommand::ImportOtio { path } | AppCommand::ImportEdl { path } => {
                self.import_timeline_report(path, uuid::Uuid::new_v4());
                true
            }
            AppCommand::ImportTimeline { path, request_id } => {
                self.import_timeline_report(path, *request_id);
                true
            }
            AppCommand::OpenFromAnotherEditor { path, request_id } => {
                self.open_from_another_editor(path, *request_id);
                true
            }
            AppCommand::ConsolidateProject {
                dest_dir,
                include_proxies,
                include_skims,
                create_zip,
                delete_folder_after_zip,
            } => {
                self.start_consolidation(
                    dest_dir.clone(),
                    *include_proxies,
                    *include_skims,
                    *create_zip,
                    *delete_folder_after_zip,
                    klipp_project::consolidate::ConsolidateMode::Collect,
                );
                true
            }
            AppCommand::AddSubtitleAtPlayhead { timeline_in, duration_frames } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let fps_rate = seq.fps.rate();
                let timeline_in = timeline_in.rescale_to(fps_rate);
                let duration = klipp_core::RationalTime::from_frames(
                    *duration_frames as i64, fps_rate,
                );
                let timeline_out = timeline_in + duration;

                // Find or create a Subtitle track (prefer existing Subtitle, then Text)
                let track_idx = seq.tracks.iter()
                    .position(|t| t.kind == klipp_timeline::TrackKind::Subtitle)
                    .or_else(|| seq.tracks.iter()
                        .position(|t| t.kind == klipp_timeline::TrackKind::Text))
                    .unwrap_or_else(|| {
                        let id = seq.add_subtitle_track("Subtitles".into());
                        seq.tracks.iter().position(|t| t.id == id).unwrap()
                    });

                let tb = klipp_timeline::TextBox {
                    text: String::new(),
                    font_size: 32.0,
                    font_name: String::new(),
                    alignment: klipp_timeline::TextAlignment::Center,
                    ..Default::default()
                };
                let mut fx = klipp_timeline::LayerFx::default();
                fx.outlines.push(klipp_timeline::TextOutline {
                    color: [0, 0, 0, 200],
                    width: 2.0,
                    enabled: true,
                    width_keyframes: Vec::new(),
                });
                let mut title_data = klipp_timeline::TitleData::single_text_fx(tb, fx);
                if let Some(o) = title_data.objects.first_mut() {
                    o.transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };
                }
                // The track's caption style, when it has one (#70).
                if let Some(style) = &seq.tracks[track_idx].caption_style {
                    title_data = klipp_document::ops::apply_caption_style(&title_data, style);
                }

                let new_clip = klipp_timeline::Clip {
                    id: klipp_core::ClipId::new(),
                    kind: klipp_timeline::ClipKind::Title(title_data),
                    timeline_in,
                    timeline_out,
                    source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                    source_out: duration,
                    ..klipp_timeline::Clip::gap(timeline_in, timeline_out)
                };
                let new_clip_id = new_clip.id;
                seq.tracks[track_idx].clips.push(new_clip);
                seq.tracks[track_idx].sort_clips();
                self.state.selected_clip = Some(new_clip_id);
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::SaveCaptionStyle { track_id, apply_to_all } => {
                // The draft the subtitle editor holds for this track (#70);
                // saving it, and restyling the captions with it, is one
                // undo step (tracks_undo).
                let Some((_, style)) = self.state.subtitle_editor.style_draft.take().filter(|(id, _)| id == track_id) else { return true };
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(track) = seq.tracks.iter_mut().find(|t| t.id == *track_id) {
                    if *apply_to_all {
                        for clip in &mut track.clips {
                            if let klipp_timeline::ClipKind::Title(td) = &mut clip.kind {
                                *td = klipp_document::ops::apply_caption_style(td, &style);
                            }
                        }
                    }
                    track.caption_style = Some(style);
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                }
                true
            }
            AppCommand::ClearCaptionStyle { track_id } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(track) = seq.tracks.iter_mut().find(|t| t.id == *track_id) {
                    track.caption_style = None;
                    self.state.dirty = true;
                }
                self.state.subtitle_editor.style_draft = None;
                true
            }
            AppCommand::ExportSubtitles { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                // A file a subtitle track, or one for them all (#2223).
                let files = klipp_document::subtitle_export::files(seq, path);
                let brief = seq.brief.as_ref().and_then(|b| b.language.as_deref());
                let mut written = Vec::new();
                for file in &files {
                    let mut subs = Vec::new();
                    for clip in file.tracks.iter().flat_map(|&i| &seq.tracks[i].clips) {
                        if let Some(td) = clip.title_data() {
                            // With its inline styling and placement (#1515).
                            subs.push(cue_of(clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64(), td));
                        }
                    }
                    subs.sort_by(|a, b| a.start_secs.partial_cmp(&b.start_secs).unwrap_or(std::cmp::Ordering::Equal));
                    // The track's language, else the brief's; WebVTT's
                    // header names only a track's own.
                    let language = file.language.as_deref().or(brief).unwrap_or("");
                    let content: Vec<u8> = match ext.as_str() {
                        // EBU-STL for teletext, at the sequence's frame rate
                        // family (#1516).
                        "stl" => klipp_captions::stl::generate_stl(
                            &subs,
                            if (seq.fps.as_f64().round() as u32).is_multiple_of(30) { 30 } else { 25 },
                            language,
                            &seq.name,
                        ),
                        "vtt" => klipp_captions::generate_vtt_in(&subs, file.language.as_deref()).into_bytes(),
                        "ass" | "ssa" => generate_ass(&subs).into_bytes(),
                        // IMSC 1.1 Text (#1516).
                        "ttml" | "dfxp" | "xml" | "itt" => klipp_captions::ttml::generate_imsc(&subs, language).into_bytes(),
                        // CEA-608 pop-on captions on CC1 (#1516).
                        "scc" => klipp_captions::scc::generate_scc(&subs).into_bytes(),
                        _ => generate_srt(&subs).into_bytes(),
                    };
                    if let Err(e) = std::fs::write(&file.path, &content) {
                        self.state.toast.show(format!("Failed to write {}: {e}", file.path.display()));
                        return true;
                    }
                    written.push((subs.len(), &file.path));
                }
                match &written[..] {
                    [(n, p)] => self.state.toast.show(format!("Exported {n} subtitles to {}", p.display())),
                    _ => self.state.toast.show(format!(
                        "Exported {} subtitle files: {}",
                        written.len(),
                        written.iter().map(|(_, p)| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect::<Vec<_>>().join(", ")
                    )),
                }
                true
            }
            AppCommand::ExportReviewCopy { path } => {
                // The sequence with its review line burned in, rendered as
                // any export is, and the comment file beside it (#2214).
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let round = klipp_document::review::next_round(seq);
                let review = klipp_document::review::overlay(seq, round);
                let template = klipp_document::review::comment_template(seq, round);
                let settings = klipp_core::ExportSettings { output_path: path.clone(), burn_in_subtitles: true, embed_subtitles: false, ..Default::default() };
                let snapshot = klipp_export::RenderSnapshot::from_sequence_with_all(
                    &review,
                    &self.state.sequences,
                    &self.state.media_pool,
                    &settings,
                    self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
                    false,
                    self.state.project_working_colorspace,
                );
                match snapshot {
                    Ok(snap) => {
                        let label = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Review copy".into());
                        self.queue_job(path.clone(), settings, label, snap);
                        self.state.show_render_queue = true;
                        let comments = path.with_file_name(format!("{}-comments.csv", path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()));
                        match std::fs::write(&comments, template) {
                            Ok(()) => self.state.toast.show(format!("Review copy, round {round}, queued; comments go in {}", comments.display())),
                            Err(e) => self.state.toast.show(format!("Review copy queued, but the comment file could not be written: {e}")),
                        }
                    }
                    Err(e) => self.state.toast.show(format!("Review copy not made: {e}")),
                }
                true
            }
            AppCommand::ExportYouTubeChapters { path } => {
                // TODO: add clipboard copy variant (AppCommand::CopyYouTubeChapters)
                //       once an `arboard`-style helper lives in the workspace.
                let path = path.clone();
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                // Sequence markers only — auto-sorted by position on add.
                let mut entries: Vec<(f64, String)> = seq.markers.iter()
                    .map(|m| (m.position.to_secs_f64(), m.label.clone()))
                    .collect();
                // YouTube requires the first chapter at 0:00. If the earliest
                // marker is more than half a second in (or there are no
                // markers at all), prepend a synthetic "Intro" at 0:00.
                let needs_intro = entries.first().map(|(s, _)| *s > 0.5).unwrap_or(true);
                if needs_intro {
                    entries.insert(0, (0.0, "Intro".to_string()));
                }
                let content = generate_youtube_chapters(&entries);
                match std::fs::write(&path, &content) {
                    Ok(_) => {
                        let chapter_count = entries.len();
                        let plural = if chapter_count == 1 { "" } else { "s" };
                        let mut msg = format!("Exported {chapter_count} chapter{plural}.");
                        if chapter_count < 3 {
                            msg.push_str(
                                " Note: YouTube requires at least 3 chapters to display the chapter list.",
                            );
                        }
                        self.state.toast.show(msg);
                    }
                    Err(e) => {
                        self.state.toast.show(format!("Failed to export chapters: {e}"));
                    }
                }
                true
            }
            AppCommand::ExportFcpxml { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::fcpxml::export_fcpxml(seq, &self.state.media_pool, path) {
                    Ok(dropped) => self.state.toast.show(interchange_written("FCPXML", path, &dropped)),
                    Err(e) => self.state.toast.show(format!("FCPXML export failed: {e}")),
                }
                true
            }
            AppCommand::ExportKdenlive { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::mlt_export::export_kdenlive(seq, &self.state.media_pool, path) {
                    Ok(dropped) => self.state.toast.show(interchange_written("Kdenlive project", path, &dropped)),
                    Err(e) => self.state.toast.show(format!("Kdenlive export failed: {e}")),
                }
                true
            }
            AppCommand::ExportEdl { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::fcpxml::export_edl(seq, &self.state.media_pool, path) {
                    Ok(dropped) => self.state.toast.show(interchange_written("EDL", path, &dropped)),
                    Err(e) => self.state.toast.show(format!("EDL export failed: {e}")),
                }
                true
            }
            AppCommand::ExportOtio { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::otio_export::export_otio(seq, &self.state.media_pool, path) {
                    Ok(warnings) => self.state.toast.show(interchange_written("OTIO", path, &warnings)),
                    Err(e) => self.state.toast.show(format!("OTIO export failed: {e}")),
                }
                true
            }
            AppCommand::ExportAaf { path } => {
                // OTIO first, beside nothing: a scratch file the converter
                // reads and that goes when it is done.
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let otio = std::env::temp_dir().join(format!("klipp-aaf-{}-{stamp}.otio", std::process::id()));
                match klipp_project::otio_export::export_otio(seq, &self.state.media_pool, &otio) {
                    Ok(warnings) => {
                        let path = path.clone();
                        self.state.toast.show(format!("Writing AAF to {}…", path.display()));
                        // The converter is a Python program and a long
                        // timeline takes it a while: run off the UI thread.
                        crate::file_dialog::spawn(move || {
                            let converted = klipp_project::otio_export::convert(&otio, &path);
                            let _ = std::fs::remove_file(&otio);
                            vec![AppCommand::ShowToast { text: aaf_outcome(&path, converted, warnings.len()) }]
                        });
                    }
                    Err(e) => self.state.toast.show(format!("AAF export failed writing its OTIO: {e}")),
                }
                true
            }
            AppCommand::ShowToast { text } => {
                self.state.toast.show(text.clone());
                true
            }
            _ => false,
        }
    }
}

/// What an AAF export says when its conversion is done (#89): where the
/// file went and what OTIO could not carry, or what to install.
fn aaf_outcome(path: &std::path::Path, converted: Result<(), klipp_project::otio_export::ConvertError>, dropped: usize) -> String {
    use klipp_project::otio_export::ConvertError;
    let install = klipp_project::otio_export::AAF_INSTALL;
    match converted {
        Ok(()) if dropped == 0 => format!("AAF exported to {}", path.display()),
        Ok(()) => format!("AAF exported to {}: {dropped} thing(s) OTIO cannot carry were left out (Export OpenTimelineIO lists them)", path.display()),
        Err(ConvertError::NoConverter) => format!("AAF export needs OpenTimelineIO's otioconvert: {install}"),
        Err(ConvertError::NoAdapter(_)) => format!("AAF export needs OpenTimelineIO's AAF adapter: {install}"),
        Err(e) => format!("AAF export failed: {e}"),
    }
}

pub(super) fn channel_config_to_u32(c: klipp_timeline::ChannelConfig) -> u32 {
    match c {
        klipp_timeline::ChannelConfig::Default => 0,
        klipp_timeline::ChannelConfig::MonoDownmix => 1,
        klipp_timeline::ChannelConfig::SwapLR => 2,
        klipp_timeline::ChannelConfig::Surround51 => 3,
        klipp_timeline::ChannelConfig::Surround71 => 4,
        klipp_timeline::ChannelConfig::LeftOnly => 5,
        klipp_timeline::ChannelConfig::RightOnly => 6,
    }
}

/// Megabits per second a codec writes at a given pixel count, where
/// that is a fixed property of the codec rather than a choice.
///
/// The intra-frame masters have published rates: every frame is coded
/// on its own at a set quality, so the file size follows from the
/// resolution, the frame rate and the running time and nothing else.
/// Scaled from the profile's quoted 1920x1080 rate by pixel count and
/// by frame rate, which is how the published tables are built.
///
/// `None` for anything whose size depends on the picture — H.264 and
/// H.265 under CRF spend what the shot needs, and no arithmetic here
/// can know that.
pub(super) fn intra_frame_mbps(codec: klipp_core::ExportVideoCodec, px: f64, fps: f64) -> Option<f64> {
    use klipp_core::ExportVideoCodec as C;
    // At 1920x1080 and 29.97 fps, from Apple's and Avid's tables.
    let at_1080p29 = match codec {
        C::ProRes422 => 147.0,
        C::ProRes422HQ => 220.0,
        C::ProRes4444 => 330.0,
        C::DNxHD115 => 115.0,
        C::DNxHD145 => 145.0,
        _ => return None,
    };
    let reference_px = 1920.0 * 1080.0;
    Some(at_1080p29 * (px / reference_px) * (fps / 29.97))
}

/// What a quality (CRF) encode spends, low and high, in Mbps: from a
/// calm, still picture to busy handheld footage. At 1080p30 and the
/// encoder's default quality H.264 spends roughly 3 to 12 Mbps; six
/// steps of CRF halve or double it; HEVC, VP9 and AV1 need less for the
/// same picture. Pixels count for less than linearly (a 4K frame has
/// more redundancy) and frame rate for less again (frames repeat
/// themselves). `None` for what has no CRF to speak of (#19).
pub(super) fn crf_mbps_range(codec: klipp_core::ExportVideoCodec, crf: u32, px: f64, fps: f64) -> Option<(f64, f64)> {
    use klipp_core::ExportVideoCodec as C;
    // The codec's own default quality, where its CRF scale sits.
    let (default_crf, efficiency) = match codec {
        C::H264 => (23.0, 1.0),
        C::H265 => (28.0, 0.6),
        C::Vp9 => (31.0, 0.65),
        C::Av1 => (35.0, 0.5),
        _ => return None,
    };
    let scale = efficiency
        * (px / (1920.0 * 1080.0)).powf(0.75)
        * (fps.max(1.0) / 30.0).powf(0.6)
        * 2f64.powf((default_crf - crf as f64) / 6.0);
    Some((3.0 * scale, 12.0 * scale))
}

/// What an interchange export says once written. A format cannot carry a
/// title's look, a grade or an effect: the editor is told how much was
/// left behind, and the detail goes beside the file — the same shape the
/// importer uses for what it drops, so neither side of an interchange is
/// a surprise.
fn interchange_written(format: &str, path: &std::path::Path, dropped: &[String]) -> String {
    if dropped.is_empty() {
        return format!("{format} exported to {}", path.display());
    }
    let note = path.with_extension("export_warnings.txt");
    let n = dropped.len();
    match std::fs::write(&note, dropped.join("\n")) {
        Ok(()) => format!(
            "{format} exported to {} — {n} thing(s) the format cannot carry, listed in {}",
            path.display(),
            note.file_name().unwrap_or_default().to_string_lossy(),
        ),
        Err(e) => format!("{format} exported to {} — {n} thing(s) dropped (could not write the list: {e})", path.display()),
    }
}

#[cfg(test)]
mod export_prefs_tests {
    use super::*;
    use crate::prefs::{ExportPrefs, OverwriteMode};

    /// A folder of its own under the temp dir, removed when dropped.
    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("klipp-export-prefs-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// #1816: the Export dialog's folder is the preferences' while it is
    /// there; unset, or a folder since removed, it is the default.
    #[test]
    fn a_new_export_goes_to_the_preferences_folder() {
        let dir = Dir::new();
        let mut prefs = ExportPrefs::default();
        assert_eq!(export_folder(&prefs), default_export_dir(), "unset: the default");
        prefs.default_output_dir = Some(dir.path().to_path_buf());
        assert_eq!(export_folder(&prefs), dir.path(), "set: that folder");
        prefs.default_output_dir = Some(dir.path().join("gone"));
        assert_eq!(export_folder(&prefs), default_export_dir(), "a folder not there: the default");
    }

    /// #1816: with the file already there, Ask keeps the name and asks,
    /// Always keeps it and does not ask, Never moves to the next free
    /// name; with no file there every mode keeps the name.
    #[test]
    fn the_overwrite_mode_decides_the_name_and_the_question() {
        let dir = Dir::new();
        let taken = dir.path().join("cut.mp4");
        std::fs::write(&taken, b"x").unwrap();
        let taken = taken.to_string_lossy().to_string();
        assert_eq!(apply_overwrite_mode(OverwriteMode::Ask, &taken), (taken.clone(), false));
        assert_eq!(apply_overwrite_mode(OverwriteMode::Always, &taken), (taken.clone(), true));
        let (free, asks) = apply_overwrite_mode(OverwriteMode::Never, &taken);
        assert_eq!(free, dir.path().join("cut_1.mp4").to_string_lossy());
        assert!(!asks);
        let new = dir.path().join("new.mp4").to_string_lossy().to_string();
        for mode in [OverwriteMode::Ask, OverwriteMode::Never] {
            assert_eq!(apply_overwrite_mode(mode, &new).0, new, "{mode:?}: no file, no change");
        }
    }
}

impl super::KlippApp {
    pub(super) fn import_timeline_report(&mut self, path: &std::path::Path, request_id: uuid::Uuid) {
        if self.read_aaf_aside(path, request_id, false) {
            return;
        }
        let result = klipp_project::import_report::load(path, self.state.sequence().fps, &self.state.media_pool);
        self.take_import(path, result, request_id, false);
    }

    /// A timeline from another editor opened as a new project (#2083). The
    /// person's own request with unsaved changes asks about them first; a
    /// socket's is refused, since nobody is there to answer.
    pub(super) fn open_from_another_editor(&mut self, path: &std::path::Path, request_id: uuid::Uuid) {
        if self.is_unsaved() {
            if request_id.is_nil() {
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::OpenFromAnotherEditor(path.to_path_buf());
            } else {
                let error = "the open project has unsaved changes: save it first, or import the timeline into it without new_project";
                *self.state.control_shared.import_outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(serde_json::json!({"request_id":request_id,"error":error}));
            }
            return;
        }
        self.open_from_another_editor_now(path, request_id);
    }

    /// [`Self::open_from_another_editor`] with the open project's changes
    /// saved or let go. The file is read before the project is closed, so
    /// one that does not open leaves it as it was.
    pub(super) fn open_from_another_editor_now(&mut self, path: &std::path::Path, request_id: uuid::Uuid) {
        // An AAF with no converter to read it: the person is asked whether
        // to install one (#2086), and it opens when that is done.
        if self.read_aaf_aside(path, request_id, true) {
            return;
        }
        let result = klipp_project::import_report::load(path, self.state.sequence().fps, &self.state.media_pool);
        if result.is_ok() {
            self.do_new_project();
        }
        self.take_import(path, result, request_id, true);
    }

    /// An AAF taken aside from the reading in this frame (#2086): while
    /// the converter installs or reads another, refused as busy (the
    /// socket answered so); else read on a thread of its own, its outcome
    /// taken in when it is done ([`Self::poll_aaf_reading`]). Whether it
    /// was an AAF so taken.
    fn read_aaf_aside(&mut self, path: &std::path::Path, request_id: uuid::Uuid, new_project: bool) -> bool {
        if !matches!(klipp_project::import_report::detect(path), Ok(klipp_project::import_report::Format::Aaf)) {
            return false;
        }
        if self.state.aaf_install.as_ref().is_some_and(|i| i.is_running()) || self.state.aaf_reading.is_some() {
            self.take_import(path, Err("the AAF converter is busy with another file: try again when it has finished".into()), request_id, new_project);
            return true;
        }
        self.state.aaf_reading = Some(crate::aaf_install::AafReading::start(path.to_path_buf(), request_id, new_project));
        true
    }

    /// An AAF read aside, taken in once it is done. With no converter, the
    /// person's own request asks to install one; a socket's is answered
    /// that it isn't there.
    pub(super) fn poll_aaf_reading(&mut self, ctx: &egui::Context) {
        let Some(reading) = self.state.aaf_reading.as_ref() else { return };
        match reading.take() {
            None => crate::aaf_install::show_reading(ctx, reading),
            Some(result) => {
                let reading = self.state.aaf_reading.take().expect("reading");
                if reading.request_id.is_nil() && result.as_ref().err().is_some_and(|e| e == klipp_project::import_report::AAF_NEEDS_CONVERTER) {
                    self.state.aaf_install = Some(crate::aaf_install::AafInstall::ask(reading.path, reading.new_project));
                    return;
                }
                if reading.new_project && result.is_ok() {
                    self.do_new_project();
                }
                self.take_import(&reading.path, result, reading.request_id, reading.new_project);
            }
        }
    }

    /// An import's outcome taken in: its sequence added and opened, in
    /// place of the fresh project's own when `replace`, its media with it,
    /// and its report shown and published for the socket.
    fn take_import(&mut self, path: &std::path::Path, result: Result<klipp_project::mlt_import::ImportResult, String>, request_id: uuid::Uuid, replace: bool) {
        let outcome = match result {
            Ok(result) => {
                // Into an open project, its files and names are the
                // project's; a fresh one has none to meet (#2472).
                let names: Vec<&str> = if replace { Vec::new() } else { self.state.sequences.iter().map(|s| s.name.as_str()).collect() };
                let result = klipp_project::import_report::fit_into(result, &self.state.media_pool, &names);
                let report = klipp_project::import_report::ImportReport::from_import(path, &result);
                for item in result.media_pool.all().cloned() { self.state.media_pool.add(item); }
                if replace {
                    self.state.fps = result.sequence.fps;
                    self.state.sequences = vec![result.sequence];
                } else {
                    self.state.sequences.push(result.sequence);
                }
                self.state.active_sequence_idx = self.state.sequences.len() - 1;
                self.state.dirty = true;
                self.state.show_import_report = true;
                self.state.last_import_report = Some(report.clone());
                self.state.offline_media_ids = self.state.media_pool.all().filter(|m| m.offline).map(|m| m.id).collect();
                // Media it names that isn't here: the relink flow opened at
                // once, already searching beside the imported file (#2086).
                if !report.missing_media.is_empty() {
                    if let Some(dir) = klipp_project::import_report::relink_search_dir(path) {
                        self.relink_panel.request_scan(dir);
                    }
                    self.state.show_relink_dialog = true;
                }
                self.sync_media_pool_to_engine();
                self.sync_sequence_to_engine();
                serde_json::json!({"request_id":request_id,"report":report})
            }
            Err(error) => {
                self.state.toast.show(format!("Import failed: {error}"));
                serde_json::json!({"request_id":request_id,"error":error})
            }
        };
        *self.state.control_shared.import_outcome.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
    }
}

/// #2650: picking a delivery spec sets the export from it, and that
/// export, checked against the spec, meets it where its settings can.
#[cfg(test)]
mod delivery_spec_tests {
    use super::*;
    use klipp_core::delivery_spec::{builtin_specs, check, export_facts, Status};

    #[test]
    fn every_specs_own_settings_meet_it() {
        for spec in builtin_specs() {
            let mut dialog = ExportDialogState::default();
            apply_spec_to_dialog(&mut dialog, &spec);
            let settings = dialog_settings(&dialog);
            assert_eq!((settings.format, settings.video_codec), (spec.export.format, spec.export.video_codec), "{}", spec.name);
            assert_eq!(settings.target_lufs, spec.export.target_lufs, "{}", spec.name);
            // A 1080p 25 fps sequence with captions, at the spec's frame
            // size where the spec sets one.
            let facts = export_facts(&settings, dialog.hdr_mode, klipp_core::Resolution { width: 1920, height: 1080 }, 25.0, true, None);
            let failed: Vec<_> = check(&spec, &facts).into_iter().filter(|f| f.status == Status::Fail && !matches!(f.what.as_str(), "Frame shape")).collect();
            assert!(failed.is_empty(), "{}: its own settings fail {failed:#?}", spec.name);
        }
    }
}

/// A preset with a custom loudness target and a true-peak ceiling of
/// its own (one imported, or written by hand) exports under that
/// ceiling, not the default.
#[cfg(test)]
mod custom_ceiling_tests {
    use super::*;

    #[test]
    fn a_custom_target_keeps_its_ceiling() {
        let settings = ExportPresetSettings { target_lufs: Some(-27.0), true_peak_dbtp: Some(-2.0), ..klipp_core::builtin_presets()[0].settings.clone() };
        let mut dialog = ExportDialogState::default();
        apply_preset_to_dialog(&mut dialog, &settings);
        assert_eq!(dialog_target_lufs(&dialog), Some(-27.0));
        assert_eq!(dialog_true_peak(&dialog), Some(-2.0), "the preset's own ceiling");
        // One with no ceiling of its own takes the default, as before.
        let plain = ExportPresetSettings { true_peak_dbtp: None, ..settings };
        let mut dialog = ExportDialogState::default();
        apply_preset_to_dialog(&mut dialog, &plain);
        assert_eq!(dialog_true_peak(&dialog), None);
    }
}
