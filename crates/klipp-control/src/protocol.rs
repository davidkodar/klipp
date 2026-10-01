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

fn strict_motion_fields(value: &serde_json::Value) -> Result<(), String> {
    let Some(object) = value.as_object() else { return Ok(()) };
    for key in object.keys() {
        if !["kind", "duration_secs", "delay_secs", "easing", "stagger", "stagger_secs", "distance"].contains(&key.as_str()) {
            return Err(format!("unknown motion field {key:?}"));
        }
    }
    Ok(())
}

fn strict_brief<'de, D: serde::Deserializer<'de>>(d: D) -> Result<klipp_timeline::Brief, D::Error> {
    brief_from_value(serde_json::Value::deserialize(d)?).map_err(serde::de::Error::custom)
}

/// A brief from JSON, its type styles' motion held to the known fields:
/// what `set_brief` reads, and what `update_brief` reads once merged.
pub(crate) fn brief_from_value(value: serde_json::Value) -> Result<klipp_timeline::Brief, String> {
    if let Some(styles) = value.get("type_styles").and_then(serde_json::Value::as_array) {
        for style in styles {
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

// One of these is deserialised per request off a socket, used, and
// dropped. It is never stored in a collection and never on a hot path,
// so the gap between the smallest variant and the largest costs one
// stack frame per command. Boxing a variant to close it would put an
// allocation and an indirection in the way, and — on an enum whose
// shape *is* the wire format — risk the JSON for nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
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
    RelinkMedia {
        media_id: String,
        path: String,
        #[serde(default)]
        force: bool,
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
    ImportMedia {
        path: String,
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
    /// Source patching: whether `append_clip` also brings the media's
    /// audio. The timeline toolbar's V+A / V only button, and the same
    /// session-scoped setting, so it shows there when set from here.
    SetAppendAudio { enabled: bool },
    SelectClip { clip_id: Option<String> },
    /// Timeline → Detect Scene Cuts for a clip (#616): its picture changes
    /// found in the background and the clip split there with its linked
    /// sound, one undoable edit. Returns at once; the split shows on the
    /// timeline when the detection is done.
    DetectSceneCuts { clip_id: String },

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
    /// Set one numeric grade parameter, e.g. param "temperature".
    SetNodeParam {
        clip_id: String,
        node_id: String,
        param: String,
        value: f32,
    },
    ToggleNodeEnabled { clip_id: String, node_id: String },
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
        #[serde(default)]
        apply: bool,
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
    /// `pause_secs` is the silence between sentences.
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
    Record {
        secs: f64,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        fps: Option<u32>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        import: Option<bool>,
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
    },
    /// The sound of a range of a media file (at most sixty seconds) as
    /// 16 kHz mono WAV: what a model that can hear is given.
    AudioExcerpt {
        media_id: String,
        from_secs: f64,
        to_secs: f64,
        #[serde(default)]
        path: Option<String>,
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
    /// settings, the container from the path's extension. Refused when a
    /// clip's media is offline or the path is one of the project's media
    /// files. Asynchronous: poll `export_status` for the job.
    Export {
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
        /// Whether captions and titles are burnt into the picture. Left
        /// out, they are, which is what the app's own export does.
        #[serde(default)]
        burn_captions: Option<bool>,
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
    /// Write the sequence's subtitle and text clips as an SRT file.
    ExportSubtitles { path: String },
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
    /// A scope image of a frame: waveform, parade, vectorscope or
    /// histogram, GPU-drawn from the frame at `secs`, written to `path`.
    /// Measures the frame without disturbing the viewer's own scopes.
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
    },
    /// Search royalty-free sound effects (Freesound) by keyword. A read;
    /// needs a Freesound token in the user's key config.
    SearchSfx {
        query: String,
        #[serde(default)]
        limit: Option<u32>,
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
    },
    /// The tempo and beat grid of a clip's sound: BPM, a confidence, and
    /// the beat times, for cutting to the music.
    Beats { media_id: String },
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
    /// Start an empty project `name` in the folder `location` (made
    /// `<location>/<name>.klipp`), with its first sequence at `width` x
    /// `height` and `fps` (a rate like 25, 29.97, 30, 59.94; left out,
    /// both follow the first clip placed, as the New Project dialog's
    /// Automatic does). Refused over unsaved changes unless
    /// `discard_changes`.
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
    },
    /// Add a sequence to the project and open it: `width` x `height` at
    /// `fps` (a vertical 1080x1920 at 30 for a phone delivery).
    NewSequence {
        name: String,
        width: u32,
        height: u32,
        fps: f64,
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
    /// since it renders frames.
    Check {
        #[serde(default)]
        color: bool,
    },
    /// Save the project: to `path` (a `.klipp` folder, made if needed),
    /// or where it was last saved when `path` is left out. Refused for a
    /// project never saved when no path is given.
    SaveProject {
        #[serde(default)]
        path: Option<String>,
    },
    /// Open the project at `path`, replacing the one that is open.
    /// Refused while the open one has unsaved changes, unless
    /// `discard_changes`: nobody is there to answer a dialog, so the
    /// caller says which it wants.
    OpenProject {
        path: String,
        #[serde(default)]
        discard_changes: bool,
    },
    /// What a colourist would read off the scopes for the frame at
    /// `secs`: the black, mid and white luma points, the RGB balance,
    /// the saturation, how much is clipped, and where skin tones sit —
    /// so a grade is set to numbers, not by eye. Seeks there and back
    /// like frame_at.
    FrameStats {
        secs: f64,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// The frame at `secs`, as a PNG, without leaving the playhead where
    /// it was: seeks there, waits for the frame, captures, seeks back.
    FrameAt {
        secs: f64,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        max_width: Option<u32>,
        #[serde(default)]
        timeout_ms: Option<u64>,
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
}

impl Response {
    pub fn ok(id: u64, result: serde_json::Value) -> Self {
        Self { id, ok: true, result: Some(result), error: None }
    }
    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, ok: false, result: None, error: Some(error.into()) }
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
    std::env::temp_dir().join(format!("klipp-control-{uid}.sock"))
}

// Avoids a `libc` dependency for one call.
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

fn default_analyze() -> bool { true }

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
        assert_eq!(none, std::env::temp_dir().join("klipp-control-1000.sock"), "no runtime directory at all: /tmp, as before");
    }
}

#[cfg(test)]
mod tests {
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
