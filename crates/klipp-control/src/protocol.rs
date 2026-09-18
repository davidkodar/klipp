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

/// A request. `id` is echoed back on the response so a client can have
/// several in flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: u64,
    #[serde(flatten)]
    pub command: ControlCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ControlCommand {
    /// Liveness plus version, so a client can tell a stale socket from a
    /// running app.
    Ping,
    /// Transport state, playhead, resolution, fps, current clip.
    Status,

    // ── Transport ──────────────────────────────────────────────────
    Play,
    Pause,
    Stop,
    /// Absolute seek, in seconds from the start of the timeline.
    Seek { secs: f64 },

    // ── Timeline ───────────────────────────────────────────────────
    /// Every track with its clips: ids, kinds, timings, media.
    ListClips,
    /// The media pool.
    ListMedia,
    /// Import a file into the media pool. Asynchronous: the app probes
    /// the file on a worker, so poll `list_media` for the result.
    ImportMedia { path: String },
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
    },
    /// Segments of every analysed item's transcript containing `query`,
    /// case-insensitively, with where they are in the file.
    SearchTranscripts {
        query: String,
        #[serde(default)]
        max_hits: Option<usize>,
    },
    /// Where every requested item stands.
    AnalysisStatus,
    /// What is known about a media item, as text: the shot list, the
    /// loudness, the silences. Cut at `max_chars` (default 8000).
    MediaAnalysis {
        media_id: String,
        #[serde(default)]
        max_chars: Option<usize>,
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
        /// Integrated loudness target in LUFS; none leaves the mix as it is.
        #[serde(default)]
        target_lufs: Option<f32>,
        /// Whether captions and titles are burnt into the picture. Left
        /// out, they are, which is what the app's own export does.
        #[serde(default)]
        burn_captions: Option<bool>,
    },
    /// Every render job the app knows: status, progress, result.
    ExportStatus,
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
    /// Apply a creative look — a bundled 3D LUT chosen by name — to a
    /// picture clip, as one node after its grade. opacity fades it in;
    /// remove clears it. Names resolve against the bundled LUT library.
    Look {
        clip_id: String,
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
    },
    /// Cut a long take down to its selects: the runs of speech between
    /// the real pauses, so the good takes are kept and the dead air and
    /// filler dropped. A read; changes nothing.
    Selects {
        media_id: String,
        #[serde(default)]
        min_gap_secs: Option<f64>,
    },
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
    /// Name the active sequence: what the timeline is called in the app
    /// and in the project. A new project's sequence keeps a default name
    /// until someone names it.
    RenameSequence { name: String },
    /// Everything a session needs to know where it is, in one call: the
    /// build, the project and whether it is saved, the brief, every media
    /// item with its analysis state, the sequence, and what to do next.
    /// The first thing to call.
    Orient,
    /// The sequence's brief: what the piece is for.
    Brief,
    /// Replace the brief, as one undo step. Fields left out ask for nothing.
    SetBrief { brief: klipp_timeline::Brief },
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
    if let Ok(dir) = std::env::var("KLIPP_CONTROL_SOCKET") {
        return std::path::PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("klipp-control.sock");
        }
    }
    let uid = unsafe { libc_getuid() };
    std::env::temp_dir().join(format!("klipp-control-{uid}.sock"))
}

// Avoids a `libc` dependency for one call.
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}
