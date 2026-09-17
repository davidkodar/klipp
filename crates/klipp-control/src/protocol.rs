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
    },
    /// Every render job the app knows: status, progress, result.
    ExportStatus,
    /// Write the sequence's subtitle and text clips as an SRT file.
    ExportSubtitles { path: String },
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
