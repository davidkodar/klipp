//! Wire types for the control socket.
//!
//! One JSON object per line, request and response both. Line-delimited
//! rather than length-prefixed so a socket can be driven by hand with
//! `socat` or `nc` while debugging.
//!
//! `ControlCommand` is deliberately *not* `AppCommand`. That enum has
//! 566 variants, is internal, and changes shape whenever the UI grows a
//! feature; serialising it onto a public socket would make every
//! refactor a breaking change for MCP clients and scripts. This
//! vocabulary is small and stable, and the server maps it into
//! `AppCommand` on the inside.

use serde::{Deserialize, Serialize};

fn graph_read_depth() -> usize { 1 }
fn default_true() -> bool { true }

fn strict_motion_fields(value: &serde_json::Value) -> Result<(), String> {
    let Some(object) = value.as_object() else { return Ok(()) };
    for key in object.keys() {
        if !["kind", "duration_secs", "delay_secs", "easing", "stagger", "stagger_secs", "distance", "scale", "anchor"].contains(&key.as_str()) {
            return Err(format!("unknown motion field {key:?}"));
        }
    }
    Ok(())
}

fn strict_brief<'de, D: serde::Deserializer<'de>>(d: D) -> Result<klipp_timeline::Brief, D::Error> {
    brief_from_value(serde_json::Value::deserialize(d)?).map_err(serde::de::Error::custom)
}

/// A type style's fields, as the style writes them: every one, the ones
/// left out when empty included.
fn type_style_fields() -> Vec<String> {
    let motion = klipp_timeline::TitleMotionSpec { kind: "fade".into(), ..Default::default() };
    let shadow = klipp_timeline::ShadowSpec { x: None, y: None, blur: None, color: None, opacity: None };
    let full = klipp_timeline::TypeStyle { enter: Some(motion.clone()), exit: Some(motion), shadow: Some(shadow), ..Default::default() };
    match serde_json::to_value(full) {
        Ok(serde_json::Value::Object(fields)) => fields.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// The brief's field names, every one, the ones left out when empty
/// included (`language`): what `set_brief` and `update_brief` both hold
/// a brief to (#1914, #1915).
pub(crate) fn brief_fields() -> Vec<String> {
    let full = klipp_timeline::Brief { language: Some("en".into()), ..Default::default() };
    match serde_json::to_value(full) {
        Ok(serde_json::Value::Object(fields)) => fields.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// `name` refused as a field of the brief, with the field it likely
/// means and the ones there are.
pub(crate) fn unknown_brief_field(name: &str, known: &[String]) -> String {
    let meant = match name {
        "target_duration_secs" | "target_duration" | "duration_secs" | "duration" | "length_secs" | "length" => Some("target_secs"),
        "target_lufs" | "lufs" | "loudness" | "integrated_lufs" => Some("loudness_lufs"),
        "lang" | "speech_language" | "transcript_language" => Some("language"),
        "colors" | "colours" | "brand_colours" => Some("brand_colors"),
        _ => None,
    };
    let hint = meant.map_or(String::new(), |m| format!(" (did you mean {m:?}?)"));
    format!("{name:?} is not a field of the brief{hint}; it has {}", known.join(", "))
}

/// A brief from JSON, its type styles and their motion held to the known
/// fields (a field the style doesn't keep is refused by name, never
/// dropped): what `set_brief` reads, and what `update_brief` reads once
/// merged.
pub(crate) fn brief_from_value(value: serde_json::Value) -> Result<klipp_timeline::Brief, String> {
    // A field the brief doesn't keep is refused by name, as an update
    // refuses it, never dropped (#1914).
    if let Some(fields) = value.as_object() {
        let known = brief_fields();
        if let Some(name) = fields.keys().find(|k| !known.contains(k)) {
            return Err(unknown_brief_field(name, &known));
        }
    }
    if let Some(styles) = value.get("type_styles").and_then(serde_json::Value::as_array) {
        let known = type_style_fields();
        for style in styles {
            if let Some(fields) = style.as_object() {
                if let Some(key) = fields.keys().find(|k| !known.contains(k)) {
                    return Err(format!("unknown type style field {key:?}"));
                }
            }
            for side in ["enter", "exit"] {
                if let Some(motion) = style.get(side) {
                    strict_motion_fields(motion)?;
                }
            }
        }
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn strict_edit_plan<'de, D: serde::Deserializer<'de>>(d: D) -> Result<klipp_timeline::EditPlan, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    if let Some(segments) = value.get("segments").and_then(serde_json::Value::as_array) {
        for segment in segments {
            for side in ["enter", "exit"] {
                if let Some(motion) = segment.get("title").and_then(|title| title.get(side)) {
                    strict_motion_fields(motion).map_err(serde::de::Error::custom)?;
                }
            }
        }
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}

/// A request. `id` is echoed back on the response so a client can have
/// several in flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: u64,
    /// Reuse this key when retrying the exact same write after a lost reply.
    /// The first reply, including an error, is replayed for up to one hour
    /// in this app process. After an error, inspect Klipp before deciding
    /// whether to send a new request with a new key.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(flatten)]
    pub command: ControlCommand,
}

/// Which search results to listen to (#795): the first this-many, or
/// those with these ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Listen {
    First(u32),
    Ids(Vec<String>),
}

impl Listen {
    /// Whether the result at `index` with `id` is to be heard; at most
    /// five in all, counted by the caller.
    pub fn picks(&self, index: usize, id: &str) -> bool {
        match self {
            Listen::First(n) => index < (*n).min(5) as usize,
            Listen::Ids(ids) => ids.iter().any(|w| w.trim() == id),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Listen::First(0)) || matches!(self, Listen::Ids(v) if v.is_empty())
    }
}

/// One track for `add_music`, as a search result gives it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MusicPick {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub track_id: String,
    #[serde(default)]
    pub download_url: String,
    #[serde(default)]
    pub preview_url: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub license_url: String,
    #[serde(default)]
    pub page_url: String,
    /// A Freesound sound's file: "original" (signed in), or "preview".
    /// Left out, the original when signed in, else the preview (#1894).
    #[serde(default)]
    pub quality: Option<String>,
    /// Only the first this many seconds of an MP3 (a preview, or a
    /// library's MP3 download), for an ambience bed out of a long
    /// recording (#2359); an original comes whole.
    #[serde(default)]
    pub max_secs: Option<f64>,
}

impl ControlCommand {
    /// Import one file: what the tools that make a file and bring it in
    /// send.
    pub fn import_one(path: String, analyze: bool) -> Self {
        Self::ImportMedia { path, paths: Vec::new(), folder: None, analyze }
    }
}

/// One output of a delivery (#2724): its aspect ("16:9", "9:16", "1:1"
/// or "4:5"), the file it is written to, and its preset and loudness.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeliveryAsk {
    pub format: String,
    pub path: String,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub target_lufs: Option<f32>,
}

/// A moment to make a short of: where it runs in the file, and the line
/// it opens on, its name and hook title.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ShortMomentAsk {
    pub from_secs: f64,
    pub to_secs: f64,
    #[serde(default)]
    pub hook: Option<String>,
}

// One of these is deserialised per request off a socket, used, and
// dropped. It is never stored in a collection and never on a hot path,
// so the gap between the smallest variant and the largest costs one
// stack frame per command. Boxing a variant to close it would put an
// allocation and an indirection in the way, and — on an enum whose
// shape *is* the wire format — risk the JSON for nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlCommand {
    /// Liveness plus version, so a client can tell a stale socket from a
    /// running app.
    Ping,
    /// Latest known UI outcome for a queued write. Completed outcomes are
    /// retained for a bounded period in this app process only.
    OperationStatus { operation_id: String },
    /// Unified render and analysis jobs. With `wait`, return when the
    /// requested job changes or a bounded long-poll expires.
    JobStatus {
        #[serde(default)] job_id: Option<String>,
        #[serde(default)] wait: bool,
        #[serde(default)] timeout_ms: Option<u64>,
    },
    /// Stop a matching render or one queued analysis item. Running analysis
    /// and import/sync probes cannot currently be interrupted safely.
    CancelJob { job_id: String },
    /// Transport state, playhead, resolution, fps, current clip.
    Status,

    // ── Transport ──────────────────────────────────────────────────
    Play,
    Pause,
    Stop,
    /// Absolute seek, in seconds from the start of the timeline.
    Seek { secs: f64 },

    // ── Timeline ───────────────────────────────────────────────────
    /// Every track with its clips: ids, kinds, timings, media. Compact,
    /// one short object a clip, unless `full`; narrowed to a `track`
    /// (index or name), the clips over `from_secs`..`to_secs`, or those
    /// of one `media_id` (#507).
    ListClips {
        #[serde(default)]
        full: bool,
        #[serde(default)]
        track: Option<serde_json::Value>,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        #[serde(default)]
        media_id: Option<String>,
    },
    /// The media pool.
    ListMedia,
    /// Point a media item at `path` (an offline one found, or a better
    /// copy): judged against what the project knew of it (reel,
    /// timecode, length, rate, size) and relinked, the answer saying
    /// whether it is the same clip. A file that is probably not the
    /// clip is refused unless `force`: a relink is not an undo step.
    /// With `folder` instead of `path`, every offline item (or the one
    /// `media_id` names) is looked for under it by content, name, reel,
    /// timecode and length, and relinked where one file clearly is it;
    /// the rest are named with why (#2480).
    RelinkMedia {
        #[serde(default)]
        media_id: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        folder: Option<String>,
        #[serde(default)]
        force: bool,
    },
    /// Make proxies, the light copies playback can read in place of the
    /// originals, for `media_ids` (every picture file on the timeline
    /// when empty), as the media pool's own Generate Proxy does: a
    /// `proxies` job, followed with job_status. `use_proxies` sets
    /// whether playback reads them (#2478).
    MakeProxies {
        #[serde(default)]
        media_ids: Vec<String>,
        #[serde(default)]
        use_proxies: Option<bool>,
    },
    /// The whole window as the person sees it (panels, dialogs, the
    /// viewer through its monitor LUT and overlays) to a PNG at `path`,
    /// or a temporary file, no wider than `max_width`. For checking a
    /// layout or a dialog; measure colour from `screenshot`, not this.
    WindowScreenshot {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        max_width: Option<u32>,
    },
    /// A media item's tags: the words that sort the pool (a person, a
    /// place, "b-roll", "hero"). `add` and `remove` are tag words.
    TagMedia {
        media_id: String,
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Import a file into the media pool. Asynchronous: the app probes
    /// the file on a worker, so poll `list_media` for the result.
    /// `paths`, a `folder` (its media files, not its subfolders) or both
    /// import many at once, each answered on its own (#1587).
    ImportMedia {
        #[serde(default)]
        path: String,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        folder: Option<String>,
        /// Queue automatic analysis when the file arrives (default true).
        #[serde(default = "default_analyze")]
        analyze: bool,
    },
    /// Append a pool item to a track as a new clip. Never prompts: a clip
    /// at a different frame rate from the project is added without
    /// changing the project's rate (a remote client cannot answer the
    /// dialog the UI would show).
    AppendClip {
        media_id: String,
        #[serde(default)]
        track: Option<usize>,
    },
    RemoveClip { clip_id: String },
    /// Give a clip the audio it should have come with, linked, at its
    /// own position. Repairs a timeline built before appending brought
    /// the audio along — re-appending those clips would get the audio
    /// but throw away their grades.
    AttachAudio { clip_id: String },
    /// Bring clips to a loudness (LUFS) or peak (dBFS) target by their
    /// gain, each on its own or `group`ed as one, never past
    /// `ceiling_dbtp` (#1843). The source media is not changed. Measured
    /// here; `apply` false answers what it would set without setting it.
    NormalizeClips {
        clip_ids: Vec<String>,
        #[serde(default)]
        mode: klipp_core::normalize::NormalizeMode,
        target: Option<f64>,
        ceiling_dbtp: Option<f64>,
        #[serde(default)]
        group: bool,
        apply: Option<bool>,
    },
    /// Source patching: whether `append_clip` also brings the media's
    /// audio. The timeline toolbar's V+A / V only button, and the same
    /// session-scoped setting, so it shows there when set from here.
    SetAppendAudio { enabled: bool },
    SelectClip { clip_id: Option<String> },
    /// A built-in effect command as the inspector sends it, by place in
    /// the clip's list: for live checks of the UI's effect writers and
    /// M3's inspector parity checks. Test hooks only (`test-hooks`), off by
    /// default and never in a release build.
    #[cfg(feature = "test-hooks")]
    EffectCommand {
        clip_id: String,
        action: EffectAction,
        #[serde(default)] index: usize,
        /// The place a `reorder` moves it to.
        #[serde(default)] to: usize,
        /// The effect an `add`, `update` or drag step carries, as saved.
        #[serde(default)] effect: Option<serde_json::Value>,
    },
    /// Show a workspace and an inspector tab, the inspector made visible:
    /// the layout only, never the document, for a harness that opens every
    /// clip's inspector (M3 step 18). `workspace`: `edit`, `colour`,
    /// `export`; `inspector_tab`: `video`, `audio`, `colour`, `effects`,
    /// `title`, `generator`, `info`. Test hooks only (`test-hooks`).
    #[cfg(feature = "test-hooks")]
    ShowPanel {
        #[serde(default)] workspace: Option<String>,
        #[serde(default)] inspector_tab: Option<String>,
    },
    /// Timeline → Detect Scene Cuts for a clip (#616): its picture changes
    /// found in the background and the clip split there with its linked
    /// sound, one undoable edit. Returns at once; the split shows on the
    /// timeline when the detection is done.
    DetectSceneCuts { clip_id: String },
    /// Follow a flat surface in a clip, a phone screen, a sign, a monitor,
    /// in perspective through the clip (#684), from four `corners` (top
    /// left, top right, bottom right, bottom left, fractions of the frame)
    /// as they are at timeline second `at_secs` (the playhead when left
    /// out). Runs in the background; the surface shows on the clip in the
    /// timeline readout when done, one undoable edit. With `surface` (an
    /// id or name), that surface is tracked again from these corners and
    /// what is pinned to it follows.
    TrackSurface {
        clip_id: String,
        /// The four corners; or leave them out and give `click`.
        #[serde(default)]
        corners: Option<[[f32; 2]; 4]>,
        /// A point inside the surface, fractions of the frame: its corners
        /// are found from the edges round it, and tracked from.
        #[serde(default)]
        click: Option<[f32; 2]>,
        /// The area followed, when it isn't the corners (#2754): four
        /// corners of a larger part of the same plane with more on it (the
        /// boat's side for its canopy, the whole wall for a sign).
        #[serde(default)]
        track_corners: Option<[[f32; 2]; 4]>,
        /// Parts of the area left out (#2754): polygons, each three or
        /// more points in fractions of the frame, that are on the area but
        /// not of the plane (a screen playing in a wall, a reflection).
        /// Left out, a surface tracked again keeps its own; `[]` clears
        /// them.
        #[serde(default)]
        exclude: Option<Vec<Vec<[f32; 2]>>>,
        #[serde(default)]
        at_secs: Option<f64>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        surface: Option<String>,
        /// "forward" or "backward" follows only later or only earlier
        /// frames from these corners and lays that over the surface's
        /// track, keeping the rest: a track lost partway, continued from
        /// a frame where the corners were put right. Left out, both ways,
        /// replacing it.
        #[serde(default)]
        direction: Option<String>,
    },
    /// Measure the water under a clip's reflection again (#2795).
    MeasureWater { clip_id: String },
    /// Correct a tracked surface on one frame (#2754): `corners` are where
    /// it is at `at_secs` (fractions of the frame, as `track_surface`
    /// takes them). That frame is held as given and the track either side
    /// is solved again from it as far as the next corrections, keeping
    /// the rest.
    SurfaceKey {
        clip_id: String,
        surface: String,
        corners: [[f32; 2]; 4],
        #[serde(default)]
        at_secs: Option<f64>,
    },

    // ── Colour / grade ─────────────────────────────────────────────
    /// The clip's node graph, as the serialised `NodeGraph`.
    GetGrade { clip_id: String },
    /// Add a grade node. `kind` is a `NodeKind` name, e.g. "ColorBalance".
    AddNode {
        clip_id: String,
        kind: String,
        #[serde(default)]
        x: f32,
        #[serde(default)]
        y: f32,
    },
    RemoveNode { clip_id: String, node_id: String },
    ConnectNodes {
        clip_id: String,
        from: String,
        #[serde(default)]
        from_port: u8,
        to: String,
        #[serde(default)]
        to_port: u8,
    },
    /// Set one numeric grade parameter, e.g. param "temperature". With
    /// `at` (seconds on the timeline, within the clip) it keys it there
    /// instead (#891): the value at that time, the move into and out of the
    /// key `interpolation` ("linear", "ease_in", "ease_out", "ease_both",
    /// "bezier", "hold"); `remove` takes the key at `at` away. One undo
    /// step either way.
    SetNodeParam {
        clip_id: String,
        node_id: String,
        param: String,
        #[serde(default)]
        value: Option<f32>,
        #[serde(default)]
        at: Option<f64>,
        #[serde(default)]
        interpolation: Option<String>,
        #[serde(default)]
        remove: bool,
    },
    ToggleNodeEnabled { clip_id: String, node_id: String },
    /// Give a grade node a name of its own (#893): "skin", "sky", "look".
    RenameNode { clip_id: String, node_id: String, name: String },
    /// Tell Klipp what camera log (or HDR) encoding a clip's media was
    /// recorded in — for footage whose file metadata does not say. Applies
    /// to the media, so every clip of the same file follows. `format` is a
    /// `LogFormat` name: "SLog3", "VLog", "LogC3", …, or "Srgb" for
    /// ordinary display-referred video.
    SetLogFormat { clip_id: String, format: String },

    // ── Observation ────────────────────────────────────────────────
    /// Write the current viewer frame to a PNG. Returns the path and
    /// dimensions. Without `path`, writes into the system temp dir.
    /// While playing, it is the frame on screen, read back from the
    /// viewer's GPU texture (`playing_frame: true` in the reply, with
    /// the `secs` and `frame` of the timeline it shows).
    Screenshot {
        #[serde(default)]
        path: Option<String>,
        /// Downscale so the longest edge of the PNG is at most this
        /// wide. A 4K frame is 33 MB of RGBA; anything inspecting it
        /// visually wants a fraction of that.
        #[serde(default)]
        max_width: Option<u32>,
        /// Wait until the engine has produced a frame with at least this
        /// generation before capturing. Every write replies with the
        /// generation current when it was queued; pass that plus one to
        /// capture the frame that actually reflects the change. Without
        /// this, "change then look" races the render and captures the old
        /// frame.
        #[serde(default)]
        min_generation: Option<u64>,
        /// How long to wait for `min_generation`, default 3000 ms.
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Latest performance sample plus a short rolling average.
    Perf,

    /// The timeline as text a model reads: tracks top first, every clip
    /// with its range in seconds and frames, what it is, its grade and
    /// effects in a word, the markers. Cut at `max_chars` (default
    /// 12000) on a line boundary, and says so.
    Timeline {
        #[serde(default)]
        max_chars: Option<usize>,
        /// One track only, by index (#796).
        #[serde(default)]
        track: Option<usize>,
        /// Only what plays between these timeline seconds (#796).
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
    },

    // ── Analysis ──────────────────────────────────────────────────
    /// Analyse a media item, or every item in the pool, in the
    /// background: shot boundaries, loudness, silence. Returns at once
    /// with each item's state; results already stored cost nothing.
    Analyze {
        #[serde(default)]
        media_id: Option<String>,
        /// Fetch the models the analysers lack first (the transcription
        /// model, 148 MB); nothing is downloaded otherwise, and an
        /// analyser without its model is skipped.
        #[serde(default)]
        download_models: bool,
        /// Hold the response until the requested analyses end.
        #[serde(default)]
        wait: bool,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Remove an AI matte from the clip’s Output alpha in one undo step.
    RestoreBackground { clip_id: String },
    /// Track the subject through a clip (#239): a matte that follows it
    /// frame by frame, made in the background and bound to the clip's
    /// grade (gating its correction) in one undo step. Its job is
    /// `<media id>:subject_matte`, read with job_status and stopped with
    /// cancel_job; frames already made are kept.
    ///
    /// With `transition_id` in place of `clip_id`, the subject a piece
    /// transition cuts round (#985 phase 3): the pass over the frames it
    /// reads, by the model the transition names, bound to no grade.
    TrackSubject {
        #[serde(default)]
        clip_id: Option<String>,
        #[serde(default)]
        transition_id: Option<String>,
        /// "subject" (one detected subject, followed) or "foreground"
        /// (the whole foreground); "subject" when left out.
        #[serde(default)]
        model: Option<String>,
        /// Fetch the model first when it isn't there; refused otherwise.
        #[serde(default)]
        download_model: bool,
        /// "grade" (the default: the matte gates the clip's correction) or
        /// "cut_out" (the clip cut out round the subject, #73), or
        /// "whole" (restore background without starting/fetching a model). A
        /// transition's subject takes neither.
        #[serde(default)]
        into: Option<String>,
    },
    /// Segments of every analysed item's transcript containing `query`,
    /// case-insensitively, with where they are in the file.
    SearchTranscripts {
        query: String,
        #[serde(default)]
        max_hits: Option<usize>,
    },
    /// What was on the screen of a media item and when: the lines read
    /// by OCR, folded into spans, between `from_secs` and `to_secs`.
    ScreenText {
        media_id: String,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        /// Every line read, not the summary of what mattered.
        #[serde(default)]
        all: bool,
    },
    /// Where on-screen text matching `query` was shown, across every
    /// media item.
    SearchScreenText {
        query: String,
        #[serde(default)]
        max_hits: Option<usize>,
    },
    /// Stretches of footage whose picture looks like `query`, a plain
    /// description, across every analysed media item (#2212), best first.
    SearchVisual {
        query: String,
        #[serde(default)]
        max_hits: Option<usize>,
        /// The least similarity (cosine, about 0.2 to 0.35 for a match)
        /// a stretch needs; 0.2 when left out.
        #[serde(default)]
        min_score: Option<f32>,
    },
    /// Hide everything the screen text found sensitive, wherever the
    /// timeline shows it: a redact op per finding, resolved as a plan;
    /// applied as one undo step with `apply`.
    AutoRedact {
        #[serde(default)]
        apply: bool,
    },
    /// Sound design from the cut: a whoosh on every cut of the top
    /// picture track, a click on every reveal the beat sheet found, a
    /// riser into the last cut, from the effects given by media id
    /// (search_sfx brings them in). Resolved as a plan; applied as one
    /// undo step with `apply`.
    AutoSfx {
        #[serde(default)]
        whoosh: Option<klipp_core::MediaId>,
        #[serde(default)]
        click: Option<klipp_core::MediaId>,
        #[serde(default)]
        riser: Option<klipp_core::MediaId>,
        /// Which cuts take a whoosh: cuts (every one, the default),
        /// transitions, or stylised (#791).
        #[serde(default)]
        on: Option<String>,
        #[serde(default)]
        apply: bool,
    },
    /// The mix's loudness over a stretch of the timeline (#792): the
    /// sound as the export mixes it, before loudness normalisation,
    /// metered every `step_secs` (0.5 s): its short-term loudness and the
    /// loudest 400 ms in the step, where a whoosh or a hit shows. A read.
    MixLoudness {
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        #[serde(default)]
        step_secs: Option<f64>,
    },
    /// The cut as an editor scrubs it: a draft render of the whole
    /// timeline at `max_width` (640), a contact sheet of it (`frames`
    /// tiles in `cols` columns), and the check with the colour pass.
    /// `path` names the sheet; the draft sits beside it.
    Review {
        #[serde(default)]
        max_width: Option<u32>,
        #[serde(default)]
        frames: Option<u32>,
        #[serde(default)]
        cols: Option<u32>,
        #[serde(default)]
        path: Option<String>,
    },
    /// Narration: `text` spoken by the `voice` (the manifest's Piper
    /// voice, downloaded with `download_models`) through the piper
    /// program, imported, and with `apply` placed on a Voice track at
    /// `at_secs` as dialogue. Without apply the file is made and
    /// imported only. `speed` above 1 is quicker, below 1 slower;
    /// `pause_secs` is the silence between sentences; `fit_secs` the
    /// length the take is made to last.
    Narrate {
        text: String,
        #[serde(default)]
        at_secs: Option<f64>,
        #[serde(default)]
        voice: Option<String>,
        #[serde(default)]
        speed: Option<f32>,
        #[serde(default)]
        pause_secs: Option<f32>,
        /// How long the take must last, seconds: said again at another
        /// speed until it lands within a frame of it (#112).
        #[serde(default)]
        fit_secs: Option<f32>,
        /// How much a Piper voice varies its delivery, 0 flat to 2, 1 as
        /// the voice does (#112).
        #[serde(default)]
        variation: Option<f32>,
        #[serde(default)]
        download_models: bool,
        #[serde(default)]
        apply: bool,
        #[serde(default)]
        path: Option<String>,
    },
    /// Record the screen for `secs` with the recorder on the machine
    /// (gpu-screen-recorder, else wf-recorder): `target` "focused"
    /// (the focused window), "screen", a monitor's name, or a region
    /// "WxH+X+Y"; `fps` 60. Imported into the pool unless `import` is
    /// false.
    ///
    /// With `wait` false it returns at once with a `recording_id`, `secs`
    /// then the most it records; `action` "status" with that id says how
    /// it goes (`ready` once the recorder writes), and "stop" ends it,
    /// imports it and says what was captured (#1903). `codec` and
    /// `quality` are the capture's (#1904).
    Record {
        /// How long to record; with `wait` false, the most.
        #[serde(default)]
        secs: Option<f64>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        fps: Option<u32>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        import: Option<bool>,
        /// Record the whole length before answering (the default), or
        /// start and answer with an id.
        #[serde(default)]
        wait: Option<bool>,
        /// "start" (the default), "status" or "stop".
        #[serde(default)]
        action: Option<String>,
        /// The recording a status or stop is about.
        #[serde(default)]
        recording_id: Option<String>,
        /// h264 (the default), hevc, av1, vp9, hevc_10bit or av1_10bit.
        #[serde(default)]
        codec: Option<String>,
        /// medium, high (the default), very_high or ultra
        /// (gpu-screen-recorder).
        #[serde(default)]
        quality: Option<String>,
    },
    /// Skin on the band: measure the faces in a clip's frame, and with
    /// `apply` add a skin secondary that turns their hue onto it,
    /// measured again after each round (up to three). Refused when the
    /// scene's neutrals carry the same cast — that is a white balance.
    SkinBalance {
        clip_id: String,
        #[serde(default)]
        apply: bool,
    },
    /// The LV2 and CLAP audio plug-ins installed here (#2485), those whose
    /// name or id holds `query` when given: each id, name and count of
    /// parameters, and the parameters with their keys and ranges (those
    /// whose name or key holds `param`) when the query names one plug-in,
    /// what clip_audio and track_audio take.
    AudioPlugins {
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        param: Option<String>,
    },
    /// A file's sound as frequency against time: where a hum starts,
    /// when a hiss creeps in, which stretch has the air conditioning.
    AudioSpectrum {
        media_id: String,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
    },
    /// Take a look's numbers off a reference — a file in the pool, or
    /// the timeline's own best shot — and make them the target every
    /// colour judgement is held to. A preset is a fallback; a reference
    /// is what a grade is actually asked for.
    AdoptLook {
        /// A reference file in the media pool.
        #[serde(default)]
        media_id: Option<String>,
        /// Or the timeline itself: its best-graded shot becomes the one
        /// the rest are held to.
        #[serde(default)]
        from_timeline: bool,
        #[serde(default)]
        apply: bool,
    },
    /// Group the timeline's shots into scenes by how they were lit,
    /// pick the best shot in each as the one the others are matched to,
    /// and report how far each still is from it.
    MatchScenes {
        #[serde(default)]
        apply: bool,
        #[serde(default)]
        rounds: Option<u32>,
        /// Only these clips, each matched to the hero of its scene among
        /// them (#897: the Colour page's Match to Hero on a selection).
        /// All the picture track's shots when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip_ids: Option<Vec<String>>,
    },
    /// A clip's grade chain in the order the picture flows through it,
    /// each node named with the stage it belongs to, and whatever the
    /// chain does out of a colourist's order.
    GradeChain {
        #[serde(default)]
        clip_id: Option<String>,
    },
    /// Send a published colour chart through the render path and read
    /// it back, to find out whether the path itself moves colour before
    /// trusting anything it is asked to measure.
    #[serde(rename = "colour_selfcheck")]
    ColourSelfCheck {
        #[serde(default)]
        size: Option<u32>,
    },
    /// One region of the frame read on its own: a face, a sky, a
    /// product. `x`,`y`,`w`,`h` are fractions of the frame.
    MeasureRegion {
        secs: f64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Grade every shot of the top picture track (or one clip) to the
    /// brief's look: measure, move the primaries toward the target,
    /// measure again, up to three rounds each; every round one undo
    /// step. Without `apply`, the readings and the first step only.
    GradeToTarget {
        #[serde(default)]
        clip_id: Option<String>,
        /// Several clips, graded as one pass and one undo step (auto
        /// colour on a selection, #2758); with `clip_id`, both.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        clips: Vec<String>,
        /// The look to grade toward where the piece has no brief, by
        /// preset name (auto colour's starting grade, #2758): exposure,
        /// contrast and white balance, which the unbriefed target leaves
        /// to the scene. A brief's or a grade group's look still wins.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        look_if_unbriefed: Option<String>,
        #[serde(default)]
        apply: bool,
        #[serde(default)]
        rounds: Option<u32>,
        /// Grade a clip that looks like camera log with no input
        /// transform anyway. Off: such a clip is left alone and named,
        /// because grading log as if it were a finished picture throws
        /// away the latitude the format exists to keep.
        #[serde(default)]
        force: bool,
        /// How far each knob may move from where the shot started:
        /// gentle (the default), normal or bold (#742).
        #[serde(default)]
        strength: Option<String>,
    },
    /// Where every requested item stands.
    AnalysisStatus {
        #[serde(default)]
        wait: bool,
        #[serde(default)]
        media_id: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// What is known about a media item, as text: the shot list, the
    /// loudness, the silences. Cut at `max_chars` (default 8000).
    MediaAnalysis {
        media_id: String,
        #[serde(default)]
        max_chars: Option<usize>,
    },
    /// The screencast beat sheet of a media item: where the picture is
    /// settled versus changing, the candidate cut points (cut to a
    /// settle, hold until readable), and the title/hold windows. Reads
    /// the stored analysis; run `analyze` first.
    BeatSheet {
        media_id: String,
    },

    /// Shots across every analysed item matching what is asked: a
    /// framing word (close-up, medium close-up, medium, wide, very
    /// wide, no people), at least so many people, an object the
    /// detector named, a motion word (static, gentle, moving, busy),
    /// whether anything is said in the shot, a minimum length. Every
    /// condition given must hold; none given lists every shot.
    FindShots {
        #[serde(default)]
        framing: Option<String>,
        #[serde(default)]
        min_people: Option<u32>,
        #[serde(default)]
        object: Option<String>,
        #[serde(default)]
        motion: Option<String>,
        #[serde(default)]
        with_speech: Option<bool>,
        #[serde(default)]
        min_secs: Option<f64>,
        #[serde(default)]
        max_hits: Option<usize>,
    },
    /// A contact sheet of a range of a media file: `frames` frames
    /// evenly spaced between `from_secs` and `to_secs` (the whole file
    /// when omitted), `cols` to a row, each `tile_width` wide, their
    /// time in the file burnt in, as a JPEG. What a model that can see
    /// is shown before it plans.
    ContactSheet {
        media_id: String,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        #[serde(default)]
        frames: Option<u32>,
        #[serde(default)]
        cols: Option<u32>,
        #[serde(default)]
        tile_width: Option<u32>,
        #[serde(default)]
        path: Option<String>,
        /// The picture in the reply as well as at `path`. Left out, it is
        /// inline only when no `path` was given, so a sheet written where
        /// it was asked costs no context (#796).
        #[serde(default)]
        inline: Option<bool>,
    },
    /// The sound of a range of a media file (at most sixty seconds) as
    /// 16 kHz mono WAV: what a model that can hear is given. With no
    /// `media_id` (or `sequence`), the range is the timeline's, and the
    /// sound is the sequence's mix as the export mixes it (#1577).
    AudioExcerpt {
        #[serde(default)]
        media_id: Option<String>,
        from_secs: f64,
        to_secs: f64,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        sequence: bool,
    },
    /// Play sound for the person at the machine to hear, through Klipp's
    /// output (#1891, #1927): a pool file's stretch or a clip's at the
    /// source's own rate and channels, the timeline's mix, or search
    /// previews before import, in turn. `action` "stop" or "status".
    Audition {
        #[serde(default)]
        action: Option<String>,
        #[serde(default)]
        media_id: Option<String>,
        #[serde(default)]
        clip_id: Option<String>,
        #[serde(default)]
        sequence: bool,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        #[serde(default)]
        preview_urls: Vec<String>,
    },

    // ── Operations ────────────────────────────────────────────────
    /// Resolve editorial operations against the timeline and say what
    /// they would do, applying nothing: the dry run. Each op is one of
    /// place, trim, split, remove, move (see `klipp_document::ops::Op`);
    /// anchors are `{"secs": s}`, `{"frame": n}`, `{"clip_start": id}`,
    /// `{"clip_end": id}`, `{"marker": label}` or
    /// `{"offset": {"from": anchor, "secs": s}}`. Ops run in order, each
    /// seeing what the ones before did; the first that cannot be done
    /// refuses the plan with why and what would help.
    Plan {
        ops: Vec<klipp_document::ops::Op>,
        /// Every step in full, and each touched clip's range as JSON.
        /// Without it the reply is the plan as text, long runs folded.
        #[serde(default)]
        detail: bool,
    },
    /// The same, then applied as one undo step — unless the document
    /// changed since it was resolved, in which case nothing is applied.
    Apply {
        ops: Vec<klipp_document::ops::Op>,
        /// The revision returned by plan or another timeline read. Refuses
        /// an edit prepared for an older document before resolving it.
        #[serde(default)]
        expected_revision: Option<u64>,
        /// The project_identity returned with plan, when the caller has it.
        /// This counter moves when projects change within one app launch;
        /// it starts again after Klipp restarts. Check it alongside a
        /// revision for stronger protection against a different project.
        #[serde(default)]
        expected_project_identity: Option<u64>,
        #[serde(default)]
        label: Option<String>,
        /// As for `plan`.
        #[serde(default)]
        detail: bool,
    },

    // ── Edit history ──────────────────────────────────────────────
    /// One step back in the active sequence's history.
    Undo,
    Redo,
    /// Several commands applied in one frame, recorded as one undo step:
    /// what an agent's action is. Every command is checked before any is
    /// sent, so a bad one refuses the whole batch and nothing happens.
    /// Reads inside a batch are refused; a batch is for writes.
    Batch { commands: Vec<ControlCommand> },

    // ── Delivery ──────────────────────────────────────────────────
    /// Render the active sequence to `path` with the app's default export
    /// settings, the container from the path's extension. With In and Out
    /// points set it covers their range, else the whole timeline; the
    /// reply's `export.range` says which. Refused when a clip's media is
    /// offline or the path is one of the project's media files.
    /// Asynchronous: poll `export_status` for the job.
    Export {
        #[serde(default)]
        path: String,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        fps: Option<f64>,
        #[serde(default)]
        crf: Option<u32>,
        /// Bits a channel: 8, or 10 for a delivery that has to hold a
        /// gradient. Eight bits give 256 steps, and a dark, shallow ramp
        /// crosses so few of them that the encoder quantises it into
        /// visible bands; ten give 1024 and the bands go away. Measured
        /// on a film whose frame is mostly one dark gradient, as the
        /// share of 8x8 blocks the encoder flattened completely: 8-bit
        /// at crf 20, 84%; 10-bit at the same bitrate, 47%; 10-bit at
        /// crf 14, 18%, which is the compositor's own figure, at a sixth
        /// of the bitrate 8-bit needed to reach it. Left out, 8.
        #[serde(default)]
        bit_depth: Option<u8>,
        /// Integrated loudness target in LUFS; none leaves the mix as it is.
        #[serde(default)]
        target_lufs: Option<f32>,
        /// The true-peak ceiling in dBTP for a normalised delivery: -1 when
        /// left out, which EBU R128 and streaming ask; -2 for ATSC A/85.
        #[serde(default)]
        true_peak_dbtp: Option<f32>,
        /// Whether captions (subtitle tracks) are burnt into the picture.
        /// Left out, they are, which is what the app's own export does.
        /// Titles are part of the picture either way (#2426).
        #[serde(default)]
        burn_captions: Option<bool>,
        /// The subtitle track as CEA-608 closed captions inside the video
        /// stream (#1663), as broadcast deliveries want; h264 and hevc.
        #[serde(default)]
        captions_608: Option<bool>,
        /// HDR10+ dynamic metadata (ST 2094-40) per shot (#468); hevc
        /// with a PQ delivery.
        #[serde(default)]
        hdr10plus: Option<bool>,
        /// The output transfer (#2477): sdr, hlg or pq. HDR is written at
        /// 10 bits in hevc, h264 or ProRes, as the export dialog allows.
        #[serde(default)]
        hdr: Option<String>,
        /// The mastering peak of a PQ delivery, cd/m²: 1000, 2000 or 4000
        /// as the dialog offers, 100 to 10000 in all (default 1000).
        #[serde(default)]
        peak_nits: Option<f32>,
        /// One of the app's built-in export presets, by name ("ProRes 422",
        /// "YouTube 4K", ...) or id: its codecs, size, quality and
        /// loudness, under any field given here as well (#1583). The
        /// path's extension must be the preset's container.
        #[serde(default)]
        preset: Option<String>,
        /// The video codec: h264, hevc, prores, prores_hq, prores_4444,
        /// dnxhd_115, dnxhd_145, av1 or vp9; the container must hold it.
        #[serde(default)]
        codec: Option<String>,
        /// The audio codec: aac, mp3, flac, opus, pcm16 or pcm24.
        #[serde(default)]
        audio_codec: Option<String>,
        /// A target video bitrate in kbps instead of `crf` (VBR). ProRes
        /// and DNxHD have rates of their own.
        #[serde(default)]
        bitrate_kbps: Option<u32>,
        /// How long the encoder takes over each frame: fast, medium, slow
        /// or veryslow (#1899). Slower is smaller at the same quality.
        /// Apart from `preset`, which is a delivery; given with one, it
        /// overrides the preset's.
        #[serde(default)]
        encoder_speed: Option<String>,
        /// Several formats in one delivery (#2724), each from the open
        /// sequence's version at its aspect: with these, `path` and the
        /// fields above are left out.
        #[serde(default)]
        outputs: Vec<DeliveryAsk>,
        /// Replace a file already at the path (or at an output's). Left
        /// out, an existing file is refused, as timeline export does.
        #[serde(default)]
        overwrite: bool,
    },
    /// Every render job the app knows: status, progress, result.
    ExportStatus {
        /// Wait for these jobs to finish, fail or be cancelled.
        #[serde(default)]
        wait: bool,
        /// Omit to capture all jobs known when the request arrives.
        #[serde(default)]
        job_id: Option<String>,
        /// Maximum wait, default one hour. Timeout does not cancel work.
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Write the sequence's subtitle clips (its text clips where it has
    /// none) as a subtitle file, in the format the path's extension names:
    /// SRT, WebVTT, ASS/SSA, TTML (IMSC 1.1 Text; .ttml, .dfxp, .xml or
    /// .itt), SCC (CEA-608) or EBU-STL. `format`, when given, must be that
    /// one.
    ExportSubtitles {
        path: String,
        #[serde(default)]
        format: Option<String>,
    },
    /// Read an SRT, WebVTT, ASS/SSA, TTML (DFXP, IMSC, iTT), SCC or EBU-STL
    /// file onto a new subtitle track of the active sequence, as the app's
    /// Import Subtitles does (#1516).
    ImportSubtitles { path: String },
    /// The captions on subtitle track `track` (the first one with
    /// captions when left out) translated from `from` (English when left
    /// out) into `to`, ISO 639-1 codes, sentence by sentence, onto a new
    /// subtitle track under it at the same times, as one undo step
    /// (#2213). The pair's model downloads only with `download_models`.
    TranslateCaptions {
        #[serde(default)]
        track: Option<usize>,
        to: String,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        download_models: bool,
        /// Terms to translate a set way, source term to its translation
        /// (#2429); the brief's names and vocabulary are kept as written
        /// without being named here.
        #[serde(default)]
        glossary: std::collections::BTreeMap<String, String>,
    },
    /// Append a supported interchange timeline; omit path to read the last
    /// outcome. With `new_project`, open it as a new project instead
    /// (#2083), refused while the open one has unsaved changes.
    ImportTimeline {
        path: Option<String>,
        #[serde(default)]
        new_project: bool,
    },
    /// Write the open sequence to an interchange file at the absolute
    /// `path`, in the format its extension names: .fcpxml, .edl, .otio,
    /// .aaf (through OpenTimelineIO's otioconvert and its AAF adapter) or
    /// .kdenlive. `format`, if given, must match it. An existing file is
    /// refused unless `overwrite`. Answers with what the format could not
    /// carry, where it says. The counterpart of `import_timeline`.
    ExportTimeline {
        path: String,
        #[serde(default)]
        format: Option<String>,
        #[serde(default)]
        overwrite: bool,
    },
    /// What is said in a media file between two times, with when: the
    /// segments, and with `words` every word and its start and end, in
    /// the file's seconds. For setting a cut on the edge of a word.
    Transcript {
        media_id: String,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        #[serde(default)]
        words: bool,
    },
    /// Line a clip's sound up with a reference clip's by cross-correlating
    /// their audio — dual-system sound, a separate recorder against the
    /// camera's scratch track. Moves `clip_id` so it plays in sync with
    /// `reference_clip_id`; refuses when the two share no common sound.
    AlignAudio {
        clip_id: String,
        reference_clip_id: String,
        #[serde(default)]
        max_offset_secs: Option<f64>,
    },
    /// Apply a creative look — a bundled 3D LUT chosen by name — to a
    /// picture clip, as one node after its grade. opacity fades it in;
    /// remove clears it. Names resolve against the bundled LUT library.
    Look {
        /// The clip; not needed with `on`.
        #[serde(default)]
        clip_id: Option<String>,
        /// The timeline, or a grade group's grade, instead of a clip
        /// (#237): "timeline", or {"group": name, "stage": "pre"|"post"}.
        #[serde(default)]
        on: Option<klipp_document::ops::GradeOn>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        opacity: Option<f32>,
        #[serde(default)]
        remove: bool,
    },
    /// The font families titles can be set in: bundled with Klipp, and
    /// installed on this machine (#738); with `query`, those whose name
    /// holds it (#2492).
    Fonts {
        #[serde(default)]
        query: Option<String>,
        /// A catalogue family to fetch (#2732).
        #[serde(default)]
        fetch: Option<String>,
    },
    /// The title templates, built-in (#1724) and saved (#1139), each with
    /// the fields it keeps editable: what a `template` op places and sets.
    TitleTemplates,
    /// A title on the timeline saved as a template (#2757): `fields` by
    /// label (its defaults and Duration when left out), in `category`;
    /// a template of that name is replaced only with `replace`.
    SaveTemplate {
        /// A title, saved as a title template; or, with `clip_ids`, left
        /// out.
        #[serde(default)]
        clip_id: Option<String>,
        /// Clips saved together as a composition template (#2757): its
        /// fields are the texts, colours, fonts, pictures and media slots
        /// they can expose, by label.
        #[serde(default)]
        clip_ids: Option<Vec<String>>,
        /// Nodes of `clip_id`'s picture saved together as a recipe
        /// (#2757 step 6), by id; `controls` publish their numbers, each
        /// drive's target `node:<id>.<param>`.
        #[serde(default)]
        nodes: Option<Vec<String>>,
        name: String,
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        fields: Option<Vec<String>>,
        /// Macro controls of clips saved together (#2757): each a slider
        /// driving numbers of their layers, named by label.
        #[serde(default)]
        controls: Vec<klipp_timeline::composition_template::ControlSpec>,
        #[serde(default)]
        replace: bool,
    },
    /// The user's templates (title templates, composition templates and
    /// recipes) named in `templates` (all when left out) written as the
    /// pack `name` to `path` (#2757); each picture's licence by its file
    /// name in `licences`, or a `.licence.txt` beside it.
    ExportTemplatePack {
        path: String,
        name: String,
        #[serde(default)]
        templates: Option<Vec<String>>,
        #[serde(default)]
        licences: std::collections::BTreeMap<String, String>,
        /// Who made it, its version and its licence, as the pack says.
        #[serde(default)]
        author: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        licence: String,
    },
    /// The pack at `path` installed as a folder of its own under the
    /// packs (#2757), read-only beside the user's templates: one of that
    /// name already installed refused unless `replace` (an update, which
    /// takes the old one away first); what is missing named, with what is
    /// drawn instead.
    ImportTemplatePack {
        path: String,
        #[serde(default)]
        replace: bool,
    },
    /// The installed template packs (#2757).
    TemplatePacks,
    /// The brand kits (#2722): the user's and the starters.
    BrandKits,
    /// The brand kit `name`, every field.
    BrandKit { name: String },
    /// Keep `kit` (a brand kit's fields) as the user's, replacing one of
    /// its name; `files` are the paths of the logo or intro/outro clips it
    /// names, copied into its folder.
    SaveBrandKit {
        kit: serde_json::Value,
        #[serde(default)]
        files: Vec<String>,
    },
    /// The user's brand kit `name` removed, its folder with it.
    RemoveBrandKit { name: String },
    /// The installed pack `name` removed: its folder, nothing else.
    RemoveTemplatePack { name: String },
    /// The node types the registry describes, with their ports and
    /// parameters, optionally of one category (#1086).
    NodeTypes {
        #[serde(default)]
        category: Option<String>,
    },
    /// Read the lifted graph by the same paths used for edits.
    GraphRead {
        path: crate::graph_wire::GraphPath,
        #[serde(default = "graph_read_depth")]
        depth: usize,
    },
    /// One graph root, committed atomically by the app.
    GraphOps {
        ops: Vec<crate::graph_wire::GraphWireOp>,
        expected_revision: Option<u64>,
        expected_project_identity: Option<u64>,
        label: Option<String>,
    },
    /// The bundled looks (3D LUTs) an agent can apply, by category, so it
    /// knows what names `look` will take.
    Looks {
        #[serde(default)]
        category: Option<String>,
    },
    /// Match a picture clip's grade to a reference clip's: read a frame
    /// of each through the scopes and set the target's exposure, lift,
    /// gain, temperature and tint to bring its black, mid and white
    /// points and its balance toward the reference. A first pass to
    /// verify with the colour check and refine, not a final grade.
    MatchGrade {
        clip_id: String,
        reference_clip_id: String,
        /// The target's frame to read, in timeline seconds; by default
        /// the middle of the clip's longest detected shot.
        #[serde(default)]
        secs: Option<f64>,
        /// The reference's frame, likewise.
        #[serde(default)]
        reference_secs: Option<f64>,
    },
    /// Cut a long take down to its selects: the runs of speech between
    /// the real pauses, so the good takes are kept and the dead air and
    /// filler dropped. A read; changes nothing.
    Selects {
        media_id: String,
        #[serde(default)]
        min_gap_secs: Option<f64>,
    },
    /// The moments of a long talk, podcast, stream or vlog that stand on
    /// their own as short clips (#2720), ranked, each with why. A read;
    /// changes nothing.
    FindMoments {
        media_id: String,
        /// The length aimed for, seconds (30).
        #[serde(default)]
        length: Option<f64>,
        /// How many (5).
        #[serde(default)]
        count: Option<usize>,
        /// Words one of which a moment must hold.
        #[serde(default)]
        include: Vec<String>,
        /// Words no moment may hold.
        #[serde(default)]
        exclude: Vec<String>,
    },
    /// A scope image of a frame: waveform, parade, vectorscope or
    /// histogram, GPU-drawn from the frame at `secs`, written to `path`.
    /// Measures the frame without disturbing the viewer's own scopes:
    /// the picture clip there alone, before titles and clips above it and
    /// without its fill, shadow or glow (#2475).
    Scope {
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        secs: Option<f64>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Search royalty-free music (Jamendo) by mood or genre words. A
    /// read; needs a Jamendo client id in the user's key config.
    SearchMusic {
        query: String,
        #[serde(default)]
        limit: Option<u32>,
        /// Only tracks whose licence allows commercial use.
        #[serde(default)]
        commercial_only: bool,
        /// Only instrumental tracks.
        #[serde(default)]
        instrumental: bool,
        /// Only tracks whose licence allows cutting and fitting: no ND
        /// (#1931).
        #[serde(default)]
        edits_only: bool,
        /// Only these licence families: "CC0", "CC BY", "CC BY-SA",
        /// "CC BY-NC", "CC BY-NC-SA", "CC BY-ND", "CC BY-NC-ND" ("by-sa"
        /// as well). Empty: any (#1931).
        #[serde(default)]
        licences: Vec<String>,
        /// Listen to results' previews (at most 5): the first this-many,
        /// or those with these track ids (#795). Tempo, how sure the beat
        /// is, and the energy over the track, read without importing
        /// anything (#739).
        #[serde(default)]
        listen: Option<Listen>,
    },
    /// Search royalty-free sound effects (Freesound) by keyword. A read;
    /// needs a Freesound token in the user's key config.
    SearchSfx {
        query: String,
        #[serde(default)]
        limit: Option<u32>,
        /// Only sounds this long or shorter, seconds (#795).
        #[serde(default)]
        max_secs: Option<f64>,
        /// Only sounds whose licence allows commercial use (#1573).
        #[serde(default)]
        commercial_only: bool,
        /// Listen to results' previews, the first this-many or those with
        /// these ids (at most 5): each one's length, when it peaks and how
        /// loud it is, which is what placing it needs (#795).
        #[serde(default)]
        listen: Option<Listen>,
    },
    /// Download a searched track into the media pool, with its licence and
    /// attribution kept beside it. Pass the fields from a search result.
    AddMusic {
        #[serde(default)]
        title: String,
        #[serde(default)]
        artist: String,
        #[serde(default)]
        track_id: String,
        #[serde(default)]
        download_url: String,
        #[serde(default)]
        preview_url: String,
        #[serde(default)]
        license: String,
        #[serde(default)]
        license_url: String,
        #[serde(default)]
        page_url: String,
        /// Many at once, each as the fields above, each answered on its
        /// own (#1587): a batch can't hold a download.
        #[serde(default)]
        tracks: Vec<MusicPick>,
        /// A Freesound sound's file: "original" or "preview" (#1894).
        #[serde(default)]
        quality: Option<String>,
        /// Only the first this many seconds of each MP3 (#2359).
        #[serde(default)]
        max_secs: Option<f64>,
    },
    /// Sign in to the user's own Freesound account, for original
    /// recordings (#1894): opens the browser at the approval page and
    /// takes Freesound's redirect itself; asked again, says how it went.
    /// `code` is the one Freesound showed, where its redirect can't come
    /// back; `sign_out` forgets the sign-in.
    FreesoundSignIn {
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        sign_out: bool,
    },
    /// The tempo and beat grid of a clip's sound: BPM, a confidence, and
    /// the beat times, for cutting to the music; and for music, its
    /// sections and energy bar by bar (#503). With `target_secs`, a fit of
    /// the track to that length by whole bars, as place ops on `track`
    /// (the first audio track when left out) from `at_secs` (0).
    Beats {
        media_id: String,
        #[serde(default)]
        target_secs: Option<f64>,
        /// With `target_secs`: leave the intro out and fit from where it
        /// ends (#1576).
        #[serde(default)]
        skip_intro: bool,
        #[serde(default)]
        at_secs: Option<f64>,
        #[serde(default)]
        track: Option<String>,
    },
    /// A reference video's editing style as facts: its cut cadence and
    /// whether it cuts on the beat, its tempo, its look measured off the
    /// picture, and its motion — so an agent can build a clip in the same
    /// feel from other footage. The reference must be imported and
    /// analysed. Reads only; nothing is copied.
    StyleProfile { media_id: String },
    /// The clean places to cut in a piece of media: the measured pauses,
    /// the sentence boundaries from the transcript, and the shot changes,
    /// ranked. With `near_secs`, it also says whether that moment is
    /// clean, mid-sentence or mid-word, and the nearest clean point to
    /// move to — so a cut never lands through a word or a sentence.
    CutPoints {
        media_id: String,
        #[serde(default)]
        near_secs: Option<f64>,
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
    },
    /// Revision-bound, frame-exact cut evidence in a bounded timeline
    /// range. All times are in sequence space; source evidence is mapped
    /// through each clip's trim, speed and direction.
    PrepareCut {
        from_secs: f64,
        to_secs: f64,
        #[serde(default)]
        near_secs: Option<f64>,
        #[serde(default)]
        intent: Option<String>,
        #[serde(default)]
        max_candidates: Option<usize>,
    },
    /// Name the active sequence: what the timeline is called in the app
    /// and in the project. A new project's sequence keeps a default name
    /// until someone names it.
    RenameSequence { name: String },
    /// Remove one of the project's sequences, as its tab's Delete does: by
    /// `index` or `sequence_id`, as `status` lists them. Refused for the
    /// project's only sequence and for one held as a compound clip in
    /// another. It can't be undone, so it is refused, naming the sequence
    /// and what it holds, until `confirm` is true, as the app asks first.
    RemoveSequence {
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        sequence_id: Option<String>,
        #[serde(default)]
        confirm: bool,
    },
    /// Copy one of the project's sequences, as its tab's Duplicate does: by
    /// `index` or `sequence_id`, the open one when neither is given. The
    /// copy is called `name` ("<name> copy" left out), goes last, and is
    /// opened unless `open` is false: a second version of a cut, such as
    /// a vertical one, starts from it. Not part of a batch.
    DuplicateSequence {
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        sequence_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default = "default_true")]
        open: bool,
    },
    /// Open another of the project's sequences, as clicking its tab does:
    /// by `index` or `sequence_id`, the `index` and `id` `status`'s
    /// `sequences` lists (`id` itself names the request). It records the
    /// playhead of the one left, so it counts as an edit.
    SwitchSequence {
        #[serde(default)]
        index: Option<usize>,
        #[serde(default)]
        sequence_id: Option<String>,
    },
    /// Start an empty project `name` in the folder `location` (made
    /// `<location>/<name>.klipp`), with its first sequence at `width` x
    /// `height` and `fps` (a rate like 25, 29.97, 30, 59.94; left out,
    /// both follow the first clip placed, as the New Project dialog's
    /// Automatic does). Refused over unsaved changes unless
    /// `discard_changes`. With `template` (a project template's name) the
    /// project starts from it instead: its sequences, tracks, bins, colour
    /// setup and starter content (#618).
    NewProject {
        name: String,
        location: String,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        #[serde(default)]
        fps: Option<f64>,
        #[serde(default)]
        sequence_name: Option<String>,
        #[serde(default)]
        discard_changes: bool,
        #[serde(default)]
        template: Option<String>,
    },
    /// The user's project templates (#618): each one's name, what it holds
    /// and needs. With `save`, the open project kept as one first.
    ProjectTemplates {
        #[serde(default)]
        save: Option<TemplateSave>,
    },
    /// Add a sequence to the project and open it: `width` x `height` at
    /// `fps` (a vertical 1080x1920 at 30 for a phone delivery).
    NewSequence {
        name: String,
        width: u32,
        height: u32,
        fps: f64,
    },
    /// A version at another aspect made again from its master's cut
    /// (#2724): the sequence by id, else the open one. Each clip still in
    /// the cut keeps the framing given it in the version; the new ones
    /// are reframed. One undo step.
    RebuildVersion {
        #[serde(default)]
        sequence_id: Option<String>,
    },
    /// A short clip for each of `moments` of a media file (#2720), each
    /// in a sequence of its own, 1080x1920: its speech placed on clean
    /// word edges, tightened when `tighten` (the default), filling the
    /// frame and following the speaker, every other piece pushed in,
    /// captioned, its hook as a title when `hook_title`, its sound to
    /// `platform`'s loudness.
    MakeShorts {
        media_id: String,
        moments: Vec<ShortMomentAsk>,
        #[serde(default)]
        platform: Option<String>,
        #[serde(default)]
        tighten: Option<bool>,
        #[serde(default)]
        hook_title: bool,
    },
    /// A marker on the timeline at `at_secs` (a range with
    /// `duration_secs`): a label, a note, a colour (red, orange, yellow,
    /// green, blue, purple) and a kind (chapter, todo, comment,
    /// best_take, section). Answers with its id.
    AddMarker {
        at_secs: f64,
        #[serde(default)]
        duration_secs: Option<f64>,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        kind: Option<String>,
    },
    /// Every marker on the timeline, in time order, with its id.
    ListMarkers,
    /// Change what is given of a marker, by id.
    UpdateMarker {
        marker_id: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        kind: Option<String>,
    },
    RemoveMarker { marker_id: String },
    /// Sync pool items onto one clock and lay them out as a multicam at
    /// the playhead (seek first), each angle on tracks of its own: what
    /// Create Synced Multicam does. `sync` is "timecode", "audio" or
    /// "timecode_then_audio" (the default). The sync runs in the
    /// background; `status` and `timeline` show the group when it lands.
    CreateMulticam {
        media_ids: Vec<String>,
        #[serde(default)]
        sync: Option<String>,
    },
    /// Cut the multicam group in play to `angle` (counted from 1, as
    /// keys 1-9 are) at `at_secs`, or at the playhead: what the keys, the
    /// angle bar and the grid do, one undo step each.
    SwitchAngle {
        angle: usize,
        #[serde(default)]
        at_secs: Option<f64>,
    },
    /// Choose whose sound the multicam group at `at_secs` (or the
    /// playhead) plays: an angle counted from 1 ("2"), "follow" for the
    /// angle on screen, "off" for none (sound from a recorder outside the
    /// group). One undo step.
    SetMulticamSound {
        sound: String,
        #[serde(default)]
        at_secs: Option<f64>,
    },
    /// Everything a session needs to know where it is, in one call: the
    /// build, the project and whether it is saved, the brief, every media
    /// item with its analysis state, the sequence, and what to do next.
    /// The first thing to call.
    Orient,
    /// The sequence's brief: what the piece is for.
    Brief,
    /// Replace the brief, as one undo step. Fields left out ask for nothing.
    SetBrief {
        #[serde(deserialize_with = "strict_brief")]
        brief: klipp_timeline::Brief,
    },
    /// Change the named fields of the brief, as one undo step: every
    /// field not given stays as it is, and one given as null goes back to
    /// empty. Held to the same checks as `set_brief`.
    UpdateBrief { fields: serde_json::Value },
    /// The edit plan: the paper edit, proposed and approved before it is
    /// built, with the ops it lowers to.
    EditPlan,
    /// Replace the edit plan, as one undo step. It is stored unapproved:
    /// approval is the user's, given through `approve_edit_plan`.
    SetEditPlan {
        #[serde(deserialize_with = "strict_edit_plan")]
        plan: klipp_timeline::EditPlan,
    },
    /// Mark the edit plan approved, on the user's word. One undo step.
    ApproveEditPlan,
    /// Build the approved edit plan onto the timeline, after whatever is
    /// down: a dry run that says what would be built, or with `apply`
    /// the build itself as one undo step. `detail` adds the steps as JSON.
    BuildEditPlan {
        #[serde(default)]
        apply: bool,
        #[serde(default)]
        detail: bool,
    },
    /// The check before delivery: the timeline read against the craft's
    /// rules and the brief, every finding with where, what and what
    /// would put it right. Changes nothing. With `color`, it also
    /// measures a frame of each shot for blown highlights, crushed
    /// blacks, over-saturation and skin off the skin-tone line — slower,
    /// since it renders frames. With `loudness`, it also meters the mix
    /// for stretches well under the rest of the piece — slower, since it
    /// mixes the whole piece's sound.
    Check {
        #[serde(default)]
        color: bool,
        #[serde(default)]
        loudness: bool,
        /// The least severity listed: error, warning or note (all, the
        /// default). The counts count every finding (#2492).
        #[serde(default)]
        min_severity: Option<String>,
    },
    /// Save a decision about one of check's findings, by the id check
    /// gave it (#1932): `accepted` keeps it on purpose and needs a
    /// `rationale`, `to_fix` notes it, `clear` takes the decision back.
    /// One undo step. A decision holds until what the finding judged
    /// changes, which gives the finding a new id.
    ReviewFinding {
        finding_id: String,
        decision: String,
        #[serde(default)]
        rationale: Option<String>,
    },
    /// Save the project: to `path` (a `.klipp` folder, made if needed),
    /// or where it was last saved when `path` is left out. Refused for a
    /// project never saved when no path is given.
    SaveProject {
        #[serde(default)]
        path: Option<String>,
    },
    /// Open by exactly one of absolute path or recent-project name.
    /// Unsaved changes require save or discard_changes, never both.
    OpenProject {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        save: bool,
        #[serde(default)]
        discard_changes: bool,
        /// What to do with an auto-save newer than the project's last save
        /// (#2789): "restore" it or "keep" the saved version. Left out when
        /// there is one, nothing opens and the reply says so.
        #[serde(default)]
        recovery: Option<String>,
    },
    /// List the existing recent projects without searching the filesystem.
    RecentProjects,
    /// The delivery specifications Klipp knows (#2650): the built-in
    /// ones, with the pages their numbers come from, and the person's
    /// own. A read.
    DeliverySpecs,
    /// Check against the delivery spec `spec` (an id or a name): a
    /// finished `file` (read with ffprobe, its loudness measured), or,
    /// with none, the timeline as an export with the spec's settings
    /// would make it. Every requirement as pass, warn or fail, with what
    /// to change. A read.
    CheckDelivery {
        spec: String,
        #[serde(default)]
        file: Option<String>,
    },
    /// Return to the project page; refuse unsaved changes unless resolved.
    CloseProject {
        #[serde(default)]
        save: bool,
        #[serde(default)]
        discard_changes: bool,
    },
    /// What a colourist would read off the scopes for the frame at
    /// `secs`: the black, mid and white luma points, the RGB balance,
    /// the saturation, how much is clipped, and where skin tones sit —
    /// so a grade is set to numbers, not by eye. Seeks there and back
    /// like frame_at. Read from the picture clip there alone, as the
    /// scope is.
    FrameStats {
        secs: f64,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// The frame at `secs`, as a PNG, without leaving the playhead where
    /// it was: seeks there, waits for the frame, captures, seeks back.
    /// A contact sheet of the timeline (#733): one tile for each stretch
    /// between cuts on the visible picture tracks (each shot once), or
    /// `frames` tiles evenly spaced, over `from_secs`..`to_secs`, as one
    /// PNG, with each tile's time and what is on it in the reply.
    TimelineSheet {
        #[serde(default)]
        from_secs: Option<f64>,
        #[serde(default)]
        to_secs: Option<f64>,
        #[serde(default)]
        frames: Option<usize>,
        #[serde(default)]
        cols: Option<u32>,
        #[serde(default)]
        tile_width: Option<u32>,
        #[serde(default)]
        path: Option<String>,
        /// The picture in the reply as well as at `path`. Left out, it is
        /// inline only when no `path` was given, so a sheet written where
        /// it was asked costs no context (#796).
        #[serde(default)]
        inline: Option<bool>,
    },
    FrameAt {
        secs: f64,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        max_width: Option<u32>,
        #[serde(default)]
        timeout_ms: Option<u64>,
        /// One channel of the picture in grey (#2758): red, green, blue,
        /// alpha or luma. Left out, the picture the viewer shows.
        #[serde(default)]
        channel: Option<String>,
        /// A node of a clip's picture to show in place of the finished
        /// frame (#2758), by its path as graph_read gives it.
        #[serde(default)]
        node: Option<crate::graph_wire::GraphPath>,
    },
    /// Tail of the app's debug log.
    LogTail {
        #[serde(default)]
        lines: Option<usize>,
    },
    /// Move a queued item to the front and/or cancel other queued items.
    /// Running work is not interrupted; prioritization respects decoder limits.
    AnalysisQueue {
        #[serde(default)]
        media_id: Option<String>,
        #[serde(default)]
        cancel_pending: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_index: Option<usize>,
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self { id, ok: true, result: Some(result), error: None, op_index: None }
    }
    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, ok: false, result: None, error: Some(error.into()), op_index: None }
    }
    pub(crate) fn graph_refused(id: u64, result: serde_json::Value) -> Self {
        let error = result["why"].as_str().unwrap_or("graph edit refused").to_owned();
        let op_index = result["op_index"].as_u64().and_then(|i| usize::try_from(i).ok());
        Self { id, ok: false, result: Some(result), error: Some(error), op_index }
    }
}

/// Where the socket lives.
///
/// `$XDG_RUNTIME_DIR` is the right home for it: user-owned, 0700, and
/// cleared on logout. Falls back to the temp dir with the uid in the
/// name so two users on one machine cannot collide.
pub fn socket_path() -> std::path::PathBuf {
    socket_path_from(
        std::env::var("KLIPP_CONTROL_SOCKET").ok().as_deref(),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        unsafe { libc_getuid() },
        &|p| p.is_dir(),
    )
}

/// [`socket_path`] from its inputs: the override, the session's runtime
/// directory, the user, and whether a directory exists.
///
/// Without `XDG_RUNTIME_DIR` the socket is still in the user's runtime
/// directory when there is one, `/run/user/<uid>`, where the app put it:
/// an assistant may start the MCP server with that variable left out —
/// Codex does — and the server then looked in `/tmp` for a socket that
/// was never there, so Klipp read as not running (#625).
fn socket_path_from(over: Option<&str>, runtime: Option<&str>, uid: u32, is_dir: &dyn Fn(&std::path::Path) -> bool) -> std::path::PathBuf {
    if let Some(path) = over.filter(|p| !p.is_empty()) {
        return std::path::PathBuf::from(path);
    }
    if let Some(dir) = runtime.filter(|d| !d.is_empty()) {
        return std::path::PathBuf::from(dir).join("klipp-control.sock");
    }
    let run = std::path::PathBuf::from(format!("/run/user/{uid}"));
    if is_dir(&run) {
        return run.join("klipp-control.sock");
    }
    fallback_socket_dir(uid).join("klipp-control.sock")
}

/// The user's own folder for the socket when there is no runtime folder:
/// `klipp-<uid>` in the temp folder, made private (0700) by the app, so
/// nobody else can put a socket where a client looks.
pub fn fallback_socket_dir(uid: u32) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("klipp-{uid}"))
}

/// The current user's id.
pub fn current_uid() -> u32 {
    unsafe { libc_getuid() }
}

// Avoids a `libc` dependency for one call.
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

fn default_analyze() -> bool { true }

/// The open project kept as a project template (#618): under `name`, with
/// the sequences `starter_sequences` names (by name) kept whole as starter
/// content, their media copied in with `package_media`, else linked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateSave {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub starter_sequences: Vec<String>,
    #[serde(default)]
    pub package_media: bool,
}

#[cfg(test)]
mod socket_path_tests {
    use super::socket_path_from;
    use std::path::{Path, PathBuf};

    /// #625: the app and the MCP server find the same socket whether or
    /// not the server was started with `XDG_RUNTIME_DIR`: Codex leaves it
    /// out, and the server looked in /tmp.
    #[test]
    fn the_socket_is_the_same_with_or_without_the_runtime_variable() {
        let has_run = |p: &Path| p == Path::new("/run/user/1000");
        let with = socket_path_from(None, Some("/run/user/1000"), 1000, &has_run);
        let without = socket_path_from(None, None, 1000, &has_run);
        assert_eq!(with, PathBuf::from("/run/user/1000/klipp-control.sock"));
        assert_eq!(without, with, "found without the variable");
        assert_eq!(socket_path_from(Some("/x/test.sock"), None, 1000, &has_run), PathBuf::from("/x/test.sock"), "the override wins");
        let none = socket_path_from(None, None, 1000, &|_| false);
        assert_eq!(none, std::env::temp_dir().join("klipp-1000").join("klipp-control.sock"), "no runtime directory at all: the user's own folder in /tmp");
    }
}

#[cfg(test)]
mod tests {
    /// A request's field the method doesn't take is refused with the ones
    /// it does (#2581); `id` and `idempotency_key` sit beside any method.
    #[test]
    fn a_method_refuses_a_field_it_does_not_take() {
        let read = |v: serde_json::Value| serde_json::from_value::<super::Request>(v).map(|_| ()).map_err(|e| e.to_string());
        assert_eq!(read(serde_json::json!({"id": 7, "idempotency_key": "k", "method": "seek", "secs": 1.0})), Ok(()));
        let err = read(serde_json::json!({"id": 7, "method": "seek", "secs": 1.0, "sec": 2.0})).unwrap_err();
        assert!(err.contains("sec") && err.contains("secs"), "{err}");
        assert_eq!(read(serde_json::json!({"method": "ping"})), Ok(()));
    }

    use super::Request;
    use serde_json::json;

    #[test]
    fn update_brief_arrives_as_a_field_object() {
        let r: Request = serde_json::from_value(json!({ "method": "update_brief", "fields": { "notes": "n" } })).unwrap();
        assert!(matches!(r.command, super::ControlCommand::UpdateBrief { .. }), "{r:?}");
    }

    #[test]
    fn incoming_brief_and_plan_motion_reject_unknown_fields() {
        let brief = json!({
            "method": "set_brief",
            "brief": {"type_styles": [{"name": "bad", "enter": {"kind": "rise", "delai_secs": 1.0}}]}
        });
        assert!(serde_json::from_value::<Request>(brief).is_err());

        let plan = json!({
            "method": "set_edit_plan",
            "plan": {"segments": [{"media_id": uuid::Uuid::new_v4(), "source_in_secs": 0.0, "source_out_secs": 1.0,
                "title": {"text": "Hi", "exit": {"kind": "fade", "durations_secs": 1.0}}}]}
        });
        assert!(serde_json::from_value::<Request>(plan).is_err());
    }

    /// #1914: a brief given whole is held to its fields as a merge is: a
    /// misspelt one is refused, naming the field it likely means, not
    /// dropped. #1915: `language` is one of them.
    #[test]
    fn a_whole_brief_refuses_a_field_it_doesnt_keep() {
        let brief = |b: serde_json::Value| serde_json::from_value::<Request>(json!({"method": "set_brief", "brief": b}));
        let err = brief(json!({"title": "Swansong", "target_duration_secs": 60})).unwrap_err().to_string();
        assert!(err.contains("target_duration_secs") && err.contains("\"target_secs\""), "{err}");
        let err = brief(json!({"target_lufs": -14})).unwrap_err().to_string();
        assert!(err.contains("\"loudness_lufs\""), "{err}");
        let kept = brief(json!({"target_secs": 60, "loudness_lufs": -14, "language": "sv"})).unwrap();
        let super::ControlCommand::SetBrief { brief: b } = kept.command else { panic!("a set_brief") };
        assert_eq!((b.target_secs, b.loudness_lufs, b.language.as_deref()), (Some(60.0), Some(-14.0), Some("sv")));
        // Every field the brief writes is one it reads.
        let all = super::brief_fields();
        assert!(all.iter().any(|f| f == "language") && all.iter().any(|f| f == "look"), "{all:?}");
    }

    /// #1567: a type style keeps its shadow, and a field it doesn't keep
    /// is refused by name, never dropped.
    #[test]
    fn a_type_style_keeps_its_shadow_and_refuses_what_it_cant_keep() {
        let brief = |style: serde_json::Value| serde_json::from_value::<Request>(json!({"method": "set_brief", "brief": {"type_styles": [style]}}));
        let kept = brief(json!({"name": "chapter", "shadow": {"x": 0, "y": 4, "blur": 12, "opacity": 0.5}})).unwrap();
        let super::ControlCommand::SetBrief { brief: b } = kept.command else { panic!("a set_brief") };
        let shadow = b.type_styles[0].shadow.as_ref().expect("the shadow kept");
        assert_eq!((shadow.y, shadow.blur, shadow.opacity), (Some(4.0), Some(12.0), Some(0.5)));
        let err = brief(json!({"name": "chapter", "drop_shadow": true})).unwrap_err().to_string();
        assert!(err.contains("drop_shadow"), "{err}");
        assert!(brief(json!({"name": "chapter", "shadow": {"spread": 2}})).is_err(), "a shadow's own fields are held too");
    }

    /// The multicam requests as a client sends them: create with or
    /// without a sync method, switch with or without a time.
    #[test]
    fn multicam_requests_parse() {
        let create: Request = serde_json::from_value(json!({"id": 1, "method": "create_multicam", "media_ids": ["a", "b"], "sync": "audio"})).unwrap();
        assert!(matches!(create.command, super::ControlCommand::CreateMulticam { ref media_ids, sync: Some(ref s) } if media_ids.len() == 2 && s == "audio"));
        let bare: Request = serde_json::from_value(json!({"id": 1, "method": "create_multicam", "media_ids": ["a", "b"]})).unwrap();
        assert!(matches!(bare.command, super::ControlCommand::CreateMulticam { sync: None, .. }));
        let switch: Request = serde_json::from_value(json!({"id": 1, "method": "switch_angle", "angle": 2, "at_secs": 6.0})).unwrap();
        assert!(matches!(switch.command, super::ControlCommand::SwitchAngle { angle: 2, at_secs: Some(t) } if t == 6.0));
        let now: Request = serde_json::from_value(json!({"id": 1, "method": "switch_angle", "angle": 1})).unwrap();
        assert!(matches!(now.command, super::ControlCommand::SwitchAngle { angle: 1, at_secs: None }));
        let sound: Request = serde_json::from_value(json!({"id": 1, "method": "set_multicam_sound", "sound": "follow"})).unwrap();
        assert!(matches!(sound.command, super::ControlCommand::SetMulticamSound { ref sound, at_secs: None } if sound == "follow"));
    }

    /// The marker requests as a client sends them.
    #[test]
    fn marker_requests_parse() {
        let add: Request = serde_json::from_value(json!({"id": 1, "method": "add_marker", "at_secs": 2.0, "note": "why", "kind": "todo"})).unwrap();
        assert!(matches!(add.command, super::ControlCommand::AddMarker { at_secs, note: Some(ref n), kind: Some(ref k), label: None, .. } if at_secs == 2.0 && n == "why" && k == "todo"));
        let list: Request = serde_json::from_value(json!({"id": 1, "method": "list_markers"})).unwrap();
        assert!(matches!(list.command, super::ControlCommand::ListMarkers));
        let update: Request = serde_json::from_value(json!({"id": 1, "method": "update_marker", "marker_id": "x", "color": "green"})).unwrap();
        assert!(matches!(update.command, super::ControlCommand::UpdateMarker { color: Some(_), label: None, .. }));
        let remove: Request = serde_json::from_value(json!({"id": 1, "method": "remove_marker", "marker_id": "x"})).unwrap();
        assert!(matches!(remove.command, super::ControlCommand::RemoveMarker { .. }));
    }
    /// new_project and new_sequence as a client sends them.
    #[test]
    fn project_and_sequence_requests_parse() {
        let np: Request = serde_json::from_value(json!({"id": 1, "method": "new_project", "name": "a", "location": "/tmp", "fps": 25.0})).unwrap();
        assert!(matches!(np.command, super::ControlCommand::NewProject { fps: Some(_), width: None, discard_changes: false, .. }));
        let ns: Request = serde_json::from_value(json!({"id": 1, "method": "new_sequence", "name": "V", "width": 1080, "height": 1920, "fps": 30})).unwrap();
        assert!(matches!(ns.command, super::ControlCommand::NewSequence { width: 1080, height: 1920, .. }));
    }
}

/// What an `effect_command` does: the inspector's effect commands, a
/// drag's begin, steps, end and abandon included.
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectAction {
    Add,
    Remove,
    Toggle,
    Update,
    Reorder,
    DragBegin,
    DragStep,
    DragEnd,
    DragAbandon,
}

#[cfg(feature = "test-hooks")]
#[cfg(not(debug_assertions))]
compile_error!("klipp-control's test-hooks are never in a release build");
