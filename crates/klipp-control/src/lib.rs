//! Control surface for a running Klipp instance.
//!
//! Starts a Unix-socket listener that accepts [`protocol::ControlCommand`]
//! requests, maps them onto the app's existing `AppCommand` channel and
//! shared state, and answers with JSON.
//!
//! The point is to make Klipp drivable without a GUI in the loop:
//! by an MCP server, by a script, by a test. Reads (status, clip list,
//! grade graph, screenshot, perf, log) go straight at the shared state;
//! writes are handed to the UI's command router (`KlippApp::send_commands`)
//! exactly as a button click is, so there is one code path for mutation
//! and the undo stack, autosave and engine caches all behave as they
//! would for a human.
//!
//! That routing is load-bearing, not a nicety. `AppCommand` is split
//! between two consumers: the UI handles colour, clip, media and most
//! editing commands itself and forwards only the remainder to the
//! engine. Writing straight to the engine's channel therefore made
//! transport work while every grade edit silently vanished.
//!
//! Enabled by default; `KLIPP_CONTROL=0` turns it off. The socket lives
//! in `$XDG_RUNTIME_DIR`, which is user-owned and mode 0700, so it is
//! reachable by the user running Klipp and nobody else. Socket mode 0600
//! and peer-credential checks enforce the same boundary for override paths
//! and the /tmp fallback before any project paths are returned.

pub mod protocol;
pub mod graph_wire;
mod graph_model;
mod graph_masks;
mod graph_prepare;
pub mod recipes;
mod graph_read;
mod graph_request;
mod envelope;
mod grading;
mod plans;
mod project_access;
mod local_peer;
mod cuts;
mod listening;
mod looks;
mod frames;
mod transport;
mod timeline_view;
mod lookups;
mod operations;
mod cut_prep;
mod jobs;
mod json_path;
/// Project and media text inside a reply's words: one quoted value.
pub(crate) use klipp_core::quote::{quoted, word};

use envelope::*;
use grading::*;
use plans::*;
pub use cuts::*;
use listening::*;
pub use looks::*;
pub use frames::*;
use transport::*;
use timeline_view::*;
pub use lookups::*;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, RwLock};

use klipp_core::{AppCommand, ControlShared};
use klipp_document::control::{ControlAck, ControlEnvelope};
use protocol::{ControlCommand, Listen, MusicPick, Request, Response};
use serde_json::json;

/// Everything the server needs to read state and send commands. Built
/// in `main.rs` from the same handles the UI and engine already share.
#[derive(Clone)]
pub struct ControlContext {
    /// Into the UI's command router, *not* the engine's channel — see the
    /// module docs for why the difference matters.
    pub commands: crossbeam_channel::Sender<ControlEnvelope>,
    /// The document revision and the render queue, as the UI publishes
    /// them.
    pub shared: Arc<ControlShared>,
    /// Wakes the UI so it drains `commands`. egui repaints reactively; an
    /// idle window would otherwise sit on a queued command until the
    /// mouse next moved.
    pub wake_ui: std::sync::Arc<dyn Fn() + Send + Sync>,
    pub sequence: Arc<klipp_timeline::SharedSequence>,
    /// The project's other sequences, published by the UI, which
    /// `edit_from_selects` copies from (#2385).
    pub others: klipp_document::ops::SharedSelects,
    pub media_pool: Arc<RwLock<klipp_media::MediaPool>>,
    pub frame_buffer: Arc<Mutex<klipp_engine::FrameBuffer>>,
    pub perf_ring: klipp_engine::perf::SharedPerfRing,
    /// Path of the app's debug log, for `log_tail`.
    pub log_path: std::path::PathBuf,
}

/// Start the control server unless `KLIPP_CONTROL=0`.
///
/// Returns the socket path on success. Never fatal: a failure here
/// leaves the app fully functional, just not remotely drivable, so it
/// logs and returns `None` rather than taking the process down.
pub fn spawn(ctx: ControlContext) -> Option<std::path::PathBuf> {
    if std::env::var("KLIPP_CONTROL").ok().as_deref() == Some("0") {
        tracing::info!("control socket disabled by KLIPP_CONTROL=0");
        return None;
    }
    let (listener, path) = match claim_socket(&protocol::socket_path(), std::process::id()) {
        Ok(claimed) => claimed,
        Err(e) => {
            tracing::warn!("control socket unavailable: {e}");
            return None;
        }
    };
    tracing::info!("control socket listening on {}", path.display());

    let thread_path = path.clone();
    std::thread::Builder::new()
        .name("klipp-control".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let ctx = ctx.clone();
                        // One thread per connection. Connections are few
                        // (an MCP server, a script) and long-lived, so a
                        // pool would be ceremony for no gain.
                        let _ = std::thread::Builder::new()
                            .name("klipp-control-conn".into())
                            .spawn(move || serve_connection(s, ctx));
                    }
                    Err(e) => {
                        tracing::warn!("control socket accept failed: {e}");
                        break;
                    }
                }
            }
            let _ = std::fs::remove_file(&thread_path);
        })
        .ok()?;
    Some(path)
}

/// Bind the control socket at `path`, or beside it when another Klipp
/// owns it.
///
/// `dir` as this user's private folder: made 0700 when it is not there,
/// and refused when it is a link, someone else's, or open to others, so
/// no other user can have put a socket in it.
fn private_dir(dir: &std::path::Path, uid: u32) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if let Err(e) = std::fs::DirBuilder::new().mode(0o700).create(dir) {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(format!("{}: {e}", dir.display()));
        }
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(format!("{} is not this user's private folder: the control socket is not opened there", dir.display()));
    }
    Ok(())
}

/// Whether the other end of `stream` runs as this user: what a client
/// checks before it sends anything to a socket.
pub fn peer_is_same_user(stream: &UnixStream) -> bool {
    local_peer::same_user(stream)
}

/// A socket file left by a crashed instance would make bind fail
/// forever, so a file there used to be removed unconditionally. When a
/// Klipp was still running, that unlinked its live socket: the second
/// instance took every client over, and when it quit, left a dead file,
/// so the first was unreachable until it restarted (#400). Now the file
/// is connected to first. Refused, or gone: it is stale and replaced. A
/// live Klipp answers: its socket is left alone, and this instance binds
/// `<name>-<pid>.sock` in the same directory instead — reachable by
/// setting `KLIPP_CONTROL_SOCKET` to it.
fn claim_socket(path: &std::path::Path, pid: u32) -> Result<(UnixListener, std::path::PathBuf), String> {
    if let Some(parent) = path.parent() {
        if parent == protocol::fallback_socket_dir(protocol::current_uid()) {
            private_dir(parent, protocol::current_uid())?;
        } else {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let path = if path.exists() && UnixStream::connect(path).is_ok() {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "klipp-control".into());
        let own = path.with_file_name(format!("{stem}-{pid}.sock"));
        tracing::warn!(
            "another Klipp is listening on {}; this one listens on {} (set KLIPP_CONTROL_SOCKET to reach it)",
            path.display(),
            own.display()
        );
        own
    } else {
        path.to_path_buf()
    };
    // Stale: nobody answered on it (or this pid's own leftover).
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("could not restrict control socket permissions: {e}"));
    }
    Ok((listener, path))
}

#[cfg(test)]
mod socket_claim_tests {
    use super::claim_socket;
    use std::os::unix::net::UnixStream;

    /// A directory of the test's own (#2006). Named by the process and a
    /// count, never by the clock: two tests starting together read the
    /// same nanoseconds under load, shared one directory, and the stale
    /// test found the live one's socket in it. A leftover of an earlier
    /// run with the same pid is cleared first.
    fn dir() -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // A socket's path must fit in sun_path (108 bytes); a build's scratch
        // TMPDIR can be longer, the runtime directory sockets live in is not.
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).filter(|d| d.is_dir()).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("klipp-claim-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn project_access_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d=dir();
        let (_,path)=claim_socket(&d.join("control.sock"),123).unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777,0o600);
        std::fs::remove_dir_all(d).unwrap();
    }

    /// #2006: each call is a directory of its own, however close
    /// together: called back to back, as two tests starting at once do.
    #[test]
    fn each_test_has_a_directory_of_its_own() {
        let dirs: Vec<_> = (0..64).map(|_| dir()).collect();
        let mut unique = dirs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), dirs.len(), "{dirs:?}");
        for d in dirs {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// #400: a second instance leaves a live Klipp's socket alone and
    /// binds its own beside it; both stay reachable.
    #[test]
    fn a_live_socket_is_not_taken_over() {
        let d = dir();
        let path = d.join("klipp-control.sock");
        let (_first, p1) = claim_socket(&path, 100).unwrap();
        assert_eq!(p1, path);
        let (_second, p2) = claim_socket(&path, 200).unwrap();
        assert_eq!(p2, d.join("klipp-control-200.sock"));
        assert!(UnixStream::connect(&path).is_ok(), "the first instance lost its socket");
        assert!(UnixStream::connect(&p2).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A socket file nobody listens on — a crashed instance's — is
    /// replaced, as before.
    #[test]
    fn a_stale_socket_is_replaced() {
        let d = dir();
        let path = d.join("klipp-control.sock");
        // A concurrent spawn can inherit a listening stream until exec,
        // keeping connect() successful after our drop (#2075). A datagram
        // leaves the same stale socket file but never accepts a stream.
        drop(std::os::unix::net::UnixDatagram::bind(&path).unwrap());
        let (exists, connect) = (path.exists(), UnixStream::connect(&path));
        assert!(exists && connect.is_err(), "a dead socket file: exists {exists}, connect {connect:?}");
        let (_l, p) = claim_socket(&path, 300).unwrap();
        assert_eq!(p, path);
        assert!(UnixStream::connect(&path).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }
}

fn serve_connection(stream: UnixStream, ctx: ControlContext) {
    // Also protects override paths and /tmp fallback, including a peer
    // that connected in the interval between bind and chmod.
    if !local_peer::same_user(&stream) {
        tracing::warn!("refused control connection from a different or unverifiable user");
        return;
    }
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("control connection clone failed: {e}");
            return;
        }
    });
    let mut writer = stream;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let field_error = serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|raw| {
            let method = raw["method"].as_str()?;
            graph_wire::validate_fields(&raw, method, true).err().map(|why| (raw["id"].as_u64().unwrap_or(0), why))
        });
        let response = if let Some((id, why)) = field_error {
            Response::err(id, why)
        } else { match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                let id = req.id;
                let key = req.idempotency_key;
                let fingerprint = serde_json::to_value(&req.command).unwrap_or_default();
                let graph_write = matches!(&req.command, ControlCommand::GraphOps { .. });
                let run = || match handle(&ctx, req.command, &mut None) {
                    Ok(v) if graph_write && v["accepted"] == false => Response::graph_refused(id, v),
                    Ok(v) => Response::ok(id, with_project_generation(&ctx, v)),
                    Err(e) => Response::err(id, e),
                };
                if let Some(key) = key {
                    if key.is_empty() || key.len() > 256 {
                        Response::err(id, "idempotency_key must be 1 to 256 bytes")
                    } else {
                        operations::with_idempotency_key(&key, fingerprint, id, run)
                    }
                } else {
                    run()
                }
            }
            // Parse failures answer on id 0 rather than dropping the
            // line: a client that sent malformed JSON should be told so,
            // not left waiting.
            Err(e) => {
                let id = serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|v| v["id"].as_u64()).unwrap_or(0);
                Response::err(id, explain_bad_request(&line, &e))
            }
        }};
        let mut text = match serde_json::to_string(&response) {
            Ok(t) => t,
            Err(e) => format!(r#"{{"id":0,"ok":false,"error":"serialise failed: {e}"}}"#),
        };
        text.push('\n');
        if writer.write_all(text.as_bytes()).is_err() {
            break;
        }
        let _ = writer.flush();
    }
}

// The labelled corpus the review checks' thresholds are tuned against (#1994).
#[cfg(test)]
#[path = "qc_corpus_tests.rs"]
mod qc_corpus_tests;

#[cfg(test)]
mod made_media_tests {
    /// #730: downloaded music, effects and narration land in the kept
    /// made-media folder, never the cache, so a project's save can adopt
    /// them and a cache clean cannot take them.
    #[test]
    fn downloads_land_in_the_kept_made_media_folder() {
        assert_eq!(klipp_audio_search::download_dir(), klipp_media::made::made_dir("audio"));
        assert!(!klipp_audio_search::download_dir().starts_with(klipp_media::disk_cache::cache_base_dir()));
    }
}

#[cfg(test)]
mod next_step_tests {
    use super::*;

    fn placed() -> Vec<klipp_document::ops::Op> {
        let place: klipp_document::ops::Op = serde_json::from_value(json!({ "op": "place", "media_id": "00000000-0000-0000-0000-000000000000", "track": "video", "at": { "secs": 0 } })).unwrap();
        vec![place.clone(), place]
    }

    /// #743: the next step after the selects follows the brief: a piece
    /// that is not captioned is not told to caption or to cover joins.
    #[test]
    fn the_next_step_reads_the_brief() {
        let ops = placed();
        let trail = Trail { pictures_placed: true, ..Default::default() };
        let told = next_after_ops(&ops, None, trail).unwrap();
        assert!(told.contains("captions"), "{told}");
        let brief = klipp_timeline::Brief { captions: false, music: true, ..Default::default() };
        let told = next_after_ops(&ops, Some(&brief), trail).unwrap();
        assert!(!told.contains("captions") && !told.contains("jump cut") && told.contains("beat"), "{told}");
        // Without music, nothing about a beat (#758 review).
        let silent = klipp_timeline::Brief { captions: false, music: false, ..Default::default() };
        let told = next_after_ops(&ops, Some(&silent), trail).unwrap();
        assert!(!told.contains("captions") && !told.contains("beat"), "{told}");
    }

    /// #785: sound effects placed are not the selects, and what the
    /// timeline already has (titles, a grade) is not advised again.
    #[test]
    fn the_next_step_reads_the_timeline() {
        let ops = placed();
        let brief = klipp_timeline::Brief { captions: false, music: true, ..Default::default() };
        let sound = next_after_ops(&ops, Some(&brief), Trail::default()).unwrap();
        assert!(!sound.contains("selects") && sound.contains("Hear it"), "{sound}");
        let fresh = next_after_ops(&ops, Some(&brief), Trail { pictures_placed: true, ..Default::default() }).unwrap();
        assert!(fresh.ends_with("then titles in the brief's type styles, the grade, and check."), "{fresh}");
        let titled = next_after_ops(&ops, Some(&brief), Trail { pictures_placed: true, titled: true, ..Default::default() }).unwrap();
        assert!(!titled.contains("titles") && titled.ends_with("then the grade and check."), "{titled}");
        let titled = Trail { pictures_placed: true, titled: true, ..Default::default() };
        let done = next_after_ops(&ops, Some(&brief), Trail { graded: true, ..titled }).unwrap();
        assert!(done.ends_with("(klipp_beats), then check."), "{done}");
        // Music on a track and the cut on its beats: not advised again.
        let scored = next_after_ops(&ops, Some(&brief), Trail { graded: true, music_on_track: true, cut_to_beat: true, ..titled }).unwrap();
        assert!(!scored.contains("beat") && scored.ends_with("(frame_at across the cut), then check."), "{scored}");
        // Music placed but the cut not on it yet (the bed laid first, the
        // selects after): that is exactly when the beat is the next step.
        let unscored = next_after_ops(&ops, Some(&brief), Trail { graded: true, music_on_track: true, ..titled }).unwrap();
        assert!(unscored.contains("cut them to the music's beat"), "{unscored}");
    }

    /// #785: cuts are on the bed's beats when most fall within 0.12 s of
    /// one, the beats carried to the timeline through where the bed sits.
    #[test]
    fn cuts_are_on_the_beat_when_most_land_on_one() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let shot = klipp_core::MediaId::new();
        let song = klipp_core::MediaId::new();
        let mut seq = klipp_timeline::Sequence::new_default();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        seq.tracks[v].clips = [(0.0, 2.0), (2.0, 4.0), (4.0, 6.0), (6.0, 8.0)]
            .iter()
            .map(|&(a, b)| klipp_timeline::Clip::media(shot, tc(a), tc(b), tc(10.0 + a), tc(10.0 + b)))
            .collect();
        // The bed sits 0.75 s into the timeline and plays its file from
        // 0.25 s in; its beats fall every second of the file from 0.5 s, so
        // on the timeline at 1, 2, 3 s… only when both offsets are taken
        // into account. Ignoring either, or both, puts them a quarter or a
        // half second off the cuts at 2, 4 and 6 s.
        let bed = klipp_timeline::Clip::media(song, tc(0.75), tc(8.75), tc(0.25), tc(8.25));
        let beats = |first: f64| klipp_analysis::beats::BeatData { bpm: 60.0, confidence: 0.9, beat_times: (0..9).map(|i| first + i as f64).collect() };
        assert!(cuts_on_beat(&seq, &bed, &beats(0.5)));
        // The same beats half a second later: off.
        assert!(!cuts_on_beat(&seq, &bed, &beats(1.0)));
        // A read the detector was unsure of says nothing.
        assert!(!cuts_on_beat(&seq, &bed, &klipp_analysis::beats::BeatData { confidence: 0.2, ..beats(0.5) }));
        // Two cuts are too few to call a cut to the music.
        seq.tracks[v].clips.truncate(3);
        assert!(!cuts_on_beat(&seq, &bed, &beats(0.5)), "both cuts on the beat, but two are too few");
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use klipp_core::{Timecode, TransportStatus};

    #[test]
    fn the_published_playhead_wins_over_a_stale_presented_frame() {
        let held = held_from(
            TransportStatus { playhead: Some(Timecode::from_frames(150, 30)), playing: false },
            Some(Timecode::from_frames(5307, 30)),
        );
        assert_eq!(held.at, Some(Timecode::from_frames(150, 30)));
    }

    #[test]
    fn with_nothing_published_the_presented_frame_is_used() {
        let held = held_from(TransportStatus::default(), Some(Timecode::from_frames(30, 30)));
        assert_eq!(held.at, Some(Timecode::from_frames(30, 30)));
    }

    #[test]
    fn an_empty_buffer_is_not_a_picture_but_a_black_frame_is() {
        assert!(!has_picture(&[0u8; 16]));
        assert!(has_picture(&[0, 0, 0, 255, 0, 0, 0, 255]), "an opaque black frame is a real picture");
    }

    #[test]
    fn a_read_during_playback_seeks_back_and_plays_on() {
        let t = Timecode::from_frames(90, 30);
        let cmds = restore_commands(HeldTransport { at: Some(t), playing: true });
        assert!(matches!(cmds.as_slice(), [AppCommand::SeekTo(s), AppCommand::Play] if *s == t), "{cmds:?}");
    }
}

#[cfg(test)]
mod timeline_view_tests {
    use super::*;

    /// #796: one track and one stretch of the timeline can be read alone,
    /// so a long cut stays readable inside the reply's budget.
    #[test]
    fn the_timeline_reads_a_track_and_a_stretch() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let scene = klipp_timeline::Sequence::new_default();
        seq.tracks[0].clips = vec![
            klipp_timeline::Clip::sequence(scene.id, tc(0.0), tc(2.0)),
            klipp_timeline::Clip::sequence(scene.id, tc(2.0), tc(4.0)),
        ];
        let (first, second) = (seq.tracks[0].clips[0].id.0.to_string(), seq.tracks[0].clips[1].id.0.to_string());
        let pool = klipp_media::MediaPool::new();
        let all = timeline_text(&seq, &pool, None, 1, TimelineView::default());
        assert!(all.contains(&first) && all.contains(&second) && all.contains("[1] Audio"));
        let late = timeline_text(&seq, &pool, None, 1, TimelineView { track: Some(0), from_secs: Some(2.5), to_secs: None });
        assert!(!late.contains(&first) && late.contains(&second), "{late}");
        assert!(!late.contains("[1] Audio") && late.contains("Showing track 0 only, what plays from 2.50 s on."), "{late}");
    }

    /// #2758: a clip's placed recipe is on its line, with its controls
    /// and what each drives, as a placed template's are.
    #[test]
    fn the_timeline_shows_a_clips_recipe_and_its_controls() {
        use klipp_document::ops::{resolve, ClipRef, Op};
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let id = clip.id;
        seq.tracks[0].clips = vec![clip];
        let pool = klipp_media::MediaPool::new();
        let op = Op::Recipe { clip_id: ClipRef::Id(id), name: "Green screen".into(), slots: [("screen".to_string(), serde_json::json!("#00ff00"))].into(), controls: Default::default(), build: None };
        let plan = resolve(&seq, &pool, &crate::plans::with_recipes(&[op], None, &klipp_media::MediaPool::new())).unwrap_or_else(|r| panic!("{r:?}"));
        let klipp_document::history::EditCommand::PictureEdit { edit, .. } = &plan.steps[0].edit else { panic!("a picture step") };
        seq.tracks[0].clips[0].restore_picture(&edit.after);
        let text = timeline_text(&seq, &pool, None, 1, TimelineView::default());
        assert!(text.contains("recipe \"Green screen\"  control \"tolerance\" = 0.2: node:0.tolerance 0..1 linear;"), "{text}");
    }
}

#[cfg(test)]
mod listen_tests {
    use super::*;

    /// #795: listen names results by count or by id, five at most.
    #[test]
    fn listen_picks_by_count_or_id() {
        let req: ControlCommand = serde_json::from_value(json!({ "method": "search_sfx", "query": "whoosh", "listen": ["42", "7"], "max_secs": 3.0 })).unwrap();
        let ControlCommand::SearchSfx { listen: Some(listen), max_secs: Some(m), .. } = req else { panic!("{req:?}") };
        assert_eq!(m, 3.0);
        assert!(listen.picks(9, "7") && listen.picks(0, "42") && !listen.picks(0, "8"));
        // #1573: the licence filter reaches the search.
        let req: ControlCommand = serde_json::from_value(json!({ "method": "search_sfx", "query": "whoosh", "commercial_only": true })).unwrap();
        let ControlCommand::SearchSfx { commercial_only: true, .. } = req else { panic!("{req:?}") };
        let req: ControlCommand = serde_json::from_value(json!({ "method": "search_music", "query": "folk", "listen": 9 })).unwrap();
        let ControlCommand::SearchMusic { listen: Some(listen), .. } = req else { panic!() };
        assert!(listen.picks(4, "x") && !listen.picks(5, "x"), "at most five");
        assert!(Listen::First(0).is_none() && Listen::Ids(Vec::new()).is_none());
    }

    /// A short sound's length, when it peaks and how loud: silence, then
    /// a burst at 0.40 s.
    #[test]
    fn a_sound_effect_is_heard_for_where_it_peaks() {
        let sr = 22_050u32;
        let mut pcm = vec![0.001f32; sr as usize];
        for (i, x) in pcm.iter_mut().enumerate().skip((0.40 * sr as f32) as usize).take((0.05 * sr as f32) as usize) {
            *x = 0.8 * ((i as f32) * 0.3).sin();
        }
        let l = SfxListen::from_pcm(&pcm, sr);
        assert!((l.secs - 1.0).abs() < 1e-3, "{l:?}");
        assert!((l.peak_secs - 0.45).abs() < 0.06, "{l:?}");
        assert!(l.peak_db > -3.0 && l.peak_db < 0.0, "{l:?}");
        assert!(l.describe().contains("loudest at"));
    }

    /// No more than five previews are fetched, whatever is asked.
    #[test]
    fn at_most_five_previews_are_heard() {
        let urls: Vec<Option<String>> = (0..8).map(|i| Some(format!("https://example.invalid/{i}"))).collect();
        let heard = hear_previews(urls, |_| Ok::<u8, String>(1));
        assert_eq!(heard.iter().filter(|h| h.is_some()).count(), 5);
        assert!(heard[5..].iter().all(|h| h.is_none()));
    }
}

#[cfg(test)]
mod look_name_tests {
    use super::pick_look;

    /// #656: a look named as `looks` lists it, category/name, is found;
    /// words still work; an exact category/name settles a tie.
    #[test]
    fn a_look_is_found_by_its_listed_name() {
        let look = |c: &str, s: &str| (c.to_string(), s.to_string(), std::path::PathBuf::from(format!("/luts/{c}/{s}.cube")));
        let looks = vec![look("film-print", "Kodak 2383"), look("film-print", "Kodak 2383 D60"), look("cinematic", "Teal Orange")];
        assert_eq!(pick_look(&looks, "cinematic/Teal Orange").unwrap().1, "Teal Orange");
        assert_eq!(pick_look(&looks, "teal orange").unwrap().1, "Teal Orange");
        assert_eq!(pick_look(&looks, "film-print/Kodak 2383").unwrap().1, "Kodak 2383", "exact settles the tie with D60");
        assert!(pick_look(&looks, "film-print/kodak").unwrap_err().contains("matches 2 looks"));
        assert!(pick_look(&looks, "nothing/here").is_err());
    }
}

#[cfg(test)]
mod offline_frame_tests {
    use super::offline_picture_at;

    /// #657: a picture clip whose file is missing is named at once; one
    /// whose file is there is not.
    #[test]
    fn a_missing_file_under_the_time_is_named() {
        let mut pool = klipp_media::MediaPool::new();
        let gone = pool.add(klipp_media::MediaItem::synthetic_for_test("/nowhere/gone.mov".into(), None));
        let here_path = std::env::temp_dir().join(format!("klipp-here-{}.mov", std::process::id()));
        std::fs::write(&here_path, b"x").unwrap();
        let here = pool.add(klipp_media::MediaItem::synthetic_for_test(here_path.clone(), None));
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v.clips = vec![
            klipp_timeline::Clip::media(gone, tc(0.0), tc(2.0), tc(0.0), tc(2.0)),
            klipp_timeline::Clip::media(here, tc(2.0), tc(4.0), tc(0.0), tc(2.0)),
        ];
        seq.tracks = vec![v];
        let said = offline_picture_at(&seq, &pool, tc(1.0)).expect("the missing file is named");
        assert!(said.contains("/nowhere/gone.mov") && said.contains("offline"), "{said}");
        assert_eq!(offline_picture_at(&seq, &pool, tc(3.0)), None);
        // An online clip on a track above the gone one draws: the frame is
        // not refused for the layer under it (#660 review).
        let mut top = klipp_timeline::Track::new("V2", klipp_timeline::TrackKind::Video);
        top.clips = vec![klipp_timeline::Clip::media(here, tc(0.0), tc(2.0), tc(0.0), tc(2.0))];
        seq.tracks.insert(0, top);
        assert_eq!(offline_picture_at(&seq, &pool, tc(1.0)), None);
        std::fs::remove_file(&here_path).ok();
    }

    /// #2474: a file under the time that cannot be decoded is named with
    /// why; a still that decodes is not, at any time into its clip.
    #[test]
    fn an_undecodable_file_under_the_time_is_named() {
        use super::undecodable_picture_at;
        let dir = std::env::temp_dir().join(format!("klipp-undecodable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken_path = dir.join("broken.mov");
        std::fs::write(&broken_path, b"not a video").unwrap();
        let still_path = dir.join("still.jpg");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "color=c=gray:s=64x48", "-frames:v", "1"])
            .arg(&still_path)
            .status()
            .unwrap();
        assert!(made.success());
        let mut pool = klipp_media::MediaPool::new();
        let broken = pool.add(klipp_media::MediaItem::synthetic_for_test(broken_path.clone(), None));
        let mut still_item = klipp_media::MediaItem::synthetic_for_test(still_path, None);
        still_item.is_still = true;
        let still = pool.add(still_item);
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        v.clips = vec![
            klipp_timeline::Clip::media(broken, tc(0.0), tc(2.0), tc(0.0), tc(2.0)),
            klipp_timeline::Clip::media(still, tc(2.0), tc(6.0), tc(0.0), tc(4.0)),
        ];
        seq.tracks = vec![v];
        let said = undecodable_picture_at(&seq, &pool, tc(1.0)).expect("the broken file is named");
        assert!(said.contains("broken.mov") && said.contains("could not be decoded"), "{said}");
        assert_eq!(undecodable_picture_at(&seq, &pool, tc(5.0)), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod preview_tests {
    use super::{energy_shape, listen_to_preview};

    /// #767 review: only an https stream is handed to ffmpeg.
    #[test]
    fn only_an_https_preview_is_listened_to() {
        for url in ["file:///etc/passwd", "/etc/passwd", "http://example.com/a.mp3", "pipe:0"] {
            assert!(listen_to_preview(url).is_err_and(|e| e.contains("https")), "{url}");
        }
    }

    fn tone(levels: &[f32]) -> Vec<f32> {
        levels.iter().flat_map(|a| (0..1000).map(move |i| a * ((i as f32) * 0.3).sin())).collect()
    }

    /// #739: a track's course read from its level over eighths.
    #[test]
    fn energy_shapes_are_named() {
        assert_eq!(energy_shape(&tone(&[0.5; 8])).1, "steady");
        assert_eq!(energy_shape(&tone(&[0.05, 0.05, 0.1, 0.2, 0.3, 0.4, 0.6, 0.6])).1, "builds");
        assert_eq!(energy_shape(&tone(&[0.6, 0.6, 0.4, 0.3, 0.2, 0.1, 0.05, 0.05])).1, "falls away");
        assert_eq!(energy_shape(&tone(&[0.05, 0.05, 0.2, 0.6, 0.6, 0.2, 0.05, 0.05])).1, "swells in the middle");
    }
}

/// What a batch gathers: the writes that are app commands, and the ones
/// that are document ops (a look), resolved together when it is sent.
#[derive(Default)]
struct Collected {
    commands: Vec<AppCommand>,
    ops: Vec<klipp_document::ops::Op>,
}

/// The `send` a request's arm writes with: inside a batch (`collect` is
/// `Some`) the command is gathered, otherwise it goes to the app now. A
/// macro, not a function, so the closure borrows `collect` only until its
/// last use, as it did when it was a local in `handle`.
macro_rules! sender {
    ($ctx:ident, $collect:ident) => {
        |c: AppCommand| -> Result<serde_json::Value, String> {
            if let Some(buf) = $collect.as_mut() {
                buf.commands.push(c);
                return Ok(json!({ "queued": true }));
            }
            send_envelope($ctx, vec![c], false)
        }
    };
}

mod requests;
pub use requests::output::{export_presets, ENCODER_SPEEDS};
/// The licence families a music search narrows to (#1931).
pub use klipp_audio_search::LICENCE_FAMILIES;

// A read inside a batch would answer before the writes it follows
// are applied, which is a lie; refuse it.
fn read_in_batch(what: &str) -> Result<serde_json::Value, String> {
    Err(format!("{what} cannot be part of a batch; a batch is for writes"))
}

/// Answer one request in this process, as the socket would, without
/// the socket (#897): the app's own buttons for the measured colour
/// tools run the same code an assistant does, whether or not the
/// assistant is on. `request` is the socket's JSON, `{"method": ...}`.
/// Blocks until the request is done, which for a tool that grades in
/// rounds is seconds, so it belongs on a worker thread.
pub fn respond_local(ctx: &ControlContext, request: serde_json::Value) -> Result<serde_json::Value, String> {
    if let Some(method) = request["method"].as_str() { graph_wire::validate_fields(&request, method, true)?; }
    let req: Request = serde_json::from_value(request).map_err(|e| format!("not a request Klipp knows: {e}"))?;
    handle(ctx, req.command, &mut None)
}

#[cfg(test)]
mod respond_local_tests {
    use super::protocol::{ControlCommand, Request};
    use serde_json::json;

    /// The requests the app's Colour Check buttons send (#897,
    /// `colour_tool_request` in klipp-ui) are ones the protocol reads,
    /// with the fields they mean.
    #[test]
    fn the_colour_buttons_requests_parse() {
        let parse = |v: serde_json::Value| serde_json::from_value::<Request>(v).expect("a request").command;
        let ids = vec!["a".to_string(), "b".to_string()];
        assert!(matches!(parse(json!({ "method": "match_scenes", "apply": true, "clip_ids": ids })), ControlCommand::MatchScenes { apply: true, clip_ids: Some(c), .. } if c.len() == 2));
        assert!(matches!(parse(json!({ "method": "match_scenes", "apply": true })), ControlCommand::MatchScenes { clip_ids: None, .. }));
        assert!(matches!(parse(json!({ "method": "skin_balance", "clip_id": "x", "apply": true })), ControlCommand::SkinBalance { apply: true, .. }));
        assert!(matches!(parse(json!({ "method": "grade_to_target", "clip_id": "x", "apply": true })), ControlCommand::GradeToTarget { apply: true, clip_id: Some(_), .. }));
        // One file, or a folder to search for the offline media (#2480).
        assert!(matches!(parse(json!({ "method": "relink_media", "media_id": "m", "path": "/a.mov" })), ControlCommand::RelinkMedia { media_id: Some(_), path: Some(_), folder: None, .. }));
        assert!(matches!(parse(json!({ "method": "relink_media", "folder": "/footage" })), ControlCommand::RelinkMedia { media_id: None, path: None, folder: Some(_), .. }));
        assert!(matches!(parse(json!({ "method": "adopt_look", "from_timeline": true, "apply": true })), ControlCommand::AdoptLook { from_timeline: true, apply: true, .. }));
        assert!(matches!(parse(json!({ "method": "adopt_look", "media_id": "m", "apply": true })), ControlCommand::AdoptLook { media_id: Some(_), .. }));
    }
}

/// `collect` is `Some` inside a batch: writes are gathered there instead
/// of being sent, so the batch goes to the UI whole or not at all.
fn handle(
    ctx: &ControlContext,
    cmd: ControlCommand,
    collect: &mut Option<Collected>,
) -> Result<serde_json::Value, String> {
    match cmd {
        cmd @ (ControlCommand::Ping
            | ControlCommand::OperationStatus { .. }
            | ControlCommand::JobStatus { .. }
            | ControlCommand::CancelJob { .. }
            | ControlCommand::Status
            | ControlCommand::Play
            | ControlCommand::Pause
            | ControlCommand::Stop
            | ControlCommand::Seek { .. }
            | ControlCommand::ListClips { .. }
        ) => requests::session::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::Analyze { .. }
            | ControlCommand::TrackSubject { .. }
            | ControlCommand::RestoreBackground { .. }
            | ControlCommand::SearchTranscripts { .. }
            | ControlCommand::ScreenText { .. }
            | ControlCommand::SearchScreenText { .. }
            | ControlCommand::SearchVisual { .. }
            | ControlCommand::AutoRedact { .. }
            | ControlCommand::AutoSfx { .. }
            | ControlCommand::MixLoudness { .. }
            | ControlCommand::AudioSpectrum { .. }
            | ControlCommand::AudioPlugins { .. }
            | ControlCommand::AnalysisQueue { .. }
            | ControlCommand::AnalysisStatus { .. }
            | ControlCommand::MediaAnalysis { .. }
            | ControlCommand::BeatSheet { .. }
            | ControlCommand::FindShots { .. }
            | ControlCommand::ContactSheet { .. }
            | ControlCommand::AudioExcerpt { .. }
            | ControlCommand::Audition { .. }
            | ControlCommand::Transcript { .. }
            | ControlCommand::AlignAudio { .. }
        ) => requests::analysis::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::AdoptLook { .. }
            | ControlCommand::MatchScenes { .. }
            | ControlCommand::GradeChain { .. }
            | ControlCommand::ColourSelfCheck { .. }
            | ControlCommand::MeasureRegion { .. }
            | ControlCommand::SkinBalance { .. }
            | ControlCommand::GradeToTarget { .. }
            | ControlCommand::GetGrade { .. }
            | ControlCommand::AddNode { .. }
            | ControlCommand::RemoveNode { .. }
            | ControlCommand::ConnectNodes { .. }
            | ControlCommand::SetNodeParam { .. }
            | ControlCommand::ToggleNodeEnabled { .. }
            | ControlCommand::RenameNode { .. }
            | ControlCommand::SetLogFormat { .. }
            | ControlCommand::Looks { .. }
            | ControlCommand::Look { .. }
            | ControlCommand::MatchGrade { .. }
            | ControlCommand::Scope { .. }
        ) => requests::colour::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::Review { .. }
            | ControlCommand::Narrate { .. }
            | ControlCommand::Record { .. }
            | ControlCommand::WindowScreenshot { .. }
            | ControlCommand::Screenshot { .. }
        ) => requests::review::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::Timeline { .. }
            | ControlCommand::RelinkMedia { .. }
            | ControlCommand::MakeProxies { .. }
            | ControlCommand::TagMedia { .. }
            | ControlCommand::ListMedia
            | ControlCommand::ImportMedia { .. }
            | ControlCommand::CreateMulticam { .. }
            | ControlCommand::SwitchAngle { .. }
            | ControlCommand::SetMulticamSound { .. }
            | ControlCommand::AddMarker { .. }
            | ControlCommand::ListMarkers
            | ControlCommand::UpdateMarker { .. }
            | ControlCommand::RemoveMarker { .. }
        ) => requests::media::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::NewProject { .. }
            | ControlCommand::ProjectTemplates { .. }
            | ControlCommand::NewSequence { .. }
            | ControlCommand::MakeShorts { .. }
            | ControlCommand::RebuildVersion { .. }
            | ControlCommand::RenameSequence { .. }
            | ControlCommand::SwitchSequence { .. }
            | ControlCommand::DuplicateSequence { .. }
            | ControlCommand::RemoveSequence { .. }
            | ControlCommand::AppendClip { .. }
            | ControlCommand::RemoveClip { .. }
            | ControlCommand::AttachAudio { .. }
            | ControlCommand::NormalizeClips { .. }
            | ControlCommand::SetAppendAudio { .. }
            | ControlCommand::DetectSceneCuts { .. }
            | ControlCommand::TrackSurface { .. }
            | ControlCommand::SurfaceKey { .. }
            | ControlCommand::MeasureWater { .. }
            | ControlCommand::SelectClip { .. }
            | ControlCommand::Plan { .. }
            | ControlCommand::Apply { .. }
            | ControlCommand::Fonts { .. }
            | ControlCommand::TitleTemplates
            | ControlCommand::SaveTemplate { .. }
            | ControlCommand::ExportTemplatePack { .. }
            | ControlCommand::ImportTemplatePack { .. }
            | ControlCommand::TemplatePacks
            | ControlCommand::RemoveTemplatePack { .. }
            | ControlCommand::BrandKits
            | ControlCommand::BrandKit { .. }
            | ControlCommand::SaveBrandKit { .. }
            | ControlCommand::RemoveBrandKit { .. }
            | ControlCommand::ExportBrandKit { .. }
            | ControlCommand::ImportBrandKit { .. }
            | ControlCommand::NodeTypes { .. }
            | ControlCommand::GraphRead { .. }
            | ControlCommand::GraphOps { .. }
        ) => requests::editing::respond(ctx, cmd, collect),
        #[cfg(feature = "test-hooks")]
        cmd @ ControlCommand::EffectCommand { .. } => requests::editing::respond(ctx, cmd, collect),
        #[cfg(feature = "test-hooks")]
        cmd @ ControlCommand::ShowPanel { .. } => requests::editing::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::Selects { .. }
            | ControlCommand::FindMoments { .. }
            | ControlCommand::SearchMusic { .. }
            | ControlCommand::SearchSfx { .. }
            | ControlCommand::AddMusic { .. }
            | ControlCommand::FreesoundSignIn { .. }
            | ControlCommand::Beats { .. }
            | ControlCommand::StyleProfile { .. }
            | ControlCommand::CutPoints { .. }
            | ControlCommand::PrepareCut { .. }
        ) => requests::cutting::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::Orient
            | ControlCommand::Brief
            | ControlCommand::EditPlan
            | ControlCommand::SetEditPlan { .. }
            | ControlCommand::ApproveEditPlan
            | ControlCommand::BuildEditPlan { .. }
            | ControlCommand::UpdateBrief { .. }
            | ControlCommand::SetBrief { .. }
            | ControlCommand::ApplyBrandKit { .. }
            | ControlCommand::Check { .. }
            | ControlCommand::ReviewFinding { .. }
            | ControlCommand::Batch { .. }
            | ControlCommand::Undo
            | ControlCommand::Redo
        ) => requests::planning::respond(ctx, cmd, collect),
        cmd @ (ControlCommand::Export { .. }
            | ControlCommand::ExportStatus { .. }
            | ControlCommand::ExportSubtitles { .. }
            | ControlCommand::ImportSubtitles { .. }
            | ControlCommand::TranslateCaptions { .. }
            | ControlCommand::ImportTimeline { .. }
            | ControlCommand::ExportTimeline { .. }
            | ControlCommand::SaveProject { .. }
            | ControlCommand::OpenProject { .. }
            | ControlCommand::CloseProject { .. }
            | ControlCommand::RecentProjects
            | ControlCommand::DeliverySpecs
            | ControlCommand::CheckDelivery { .. }
            | ControlCommand::FrameStats { .. }
            | ControlCommand::TimelineSheet { .. }
            | ControlCommand::FrameAt { .. }
            | ControlCommand::Perf
            | ControlCommand::LogTail { .. }
        ) => requests::output::respond(ctx, cmd, collect),
    }
}

#[cfg(test)]
mod tests {

    /// The fallback socket folder is this user's alone: made 0700, and
    /// one open to others or a link to elsewhere is refused. A client
    /// talks only to a peer of its own user.
    #[test]
    fn the_fallback_socket_folder_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let uid = protocol::current_uid();
        let root = std::env::temp_dir().join(format!("klipp-private-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fresh = root.join("fresh");
        private_dir(&fresh, uid).unwrap();
        assert_eq!(std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777, 0o700);
        let open = root.join("open");
        std::fs::create_dir_all(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&open, uid).is_err(), "open to others");
        let link = root.join("link");
        std::os::unix::fs::symlink(&fresh, &link).unwrap();
        assert!(private_dir(&link, uid).is_err(), "a link");
        assert!(private_dir(&fresh, uid.wrapping_add(1)).is_err(), "someone else's");
        let (a, _b) = UnixStream::pair().unwrap();
        assert!(peer_is_same_user(&a));
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    /// Exhaustive on purpose: a variant added to `LogFormat` stops this
    /// compiling until it is given a name in `LOG_FORMAT_NAMES` too.
    fn wire_name(f: klipp_core::LogFormat) -> &'static str {
        use klipp_core::LogFormat as L;
        match f {
            L::Srgb => "Srgb", L::Linear => "Linear", L::LogC3 => "LogC3", L::LogC4 => "LogC4",
            L::SLog2 => "SLog2", L::SLog3 => "SLog3", L::CanonLog => "CanonLog",
            L::CanonLog3 => "CanonLog3", L::VLog => "VLog", L::NLog => "NLog", L::FLog => "FLog",
            L::DJIDLog => "DJIDLog", L::GoProFlat => "GoProFlat", L::AppleLog => "AppleLog",
            L::LeicaLLog => "LeicaLLog", L::LeicaLLogBt709 => "LeicaLLogBt709", L::RedLog3G10 => "RedLog3G10",
            L::BmdFilmGen5 => "BmdFilmGen5", L::HLG => "HLG", L::PQ => "PQ",
        }
    }

    #[test]
    fn log_format_names_cover_the_enum_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for name in LOG_FORMAT_NAMES {
            let f = parse_log_format(name).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(wire_name(f), *name);
            assert!(seen.insert(*name), "{name} listed twice");
        }
        assert_eq!(seen.len(), 20, "wire_name has 20 arms; every one must be listed");
        assert!(parse_log_format("slog3").is_err(), "names are case-sensitive");
    }

    /// A video track above the footage holding no picture (a title, a
    /// gap) or hidden is passed over: the colour tools grade the track
    /// the footage is on (#513).
    #[test]
    fn the_picture_track_is_the_top_one_with_footage() {
        use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
        let tc = |s: i64| klipp_core::Timecode::from_frames(s * 25, 25);
        let media = || Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0), tc(2)) };
        let mut empty = Track::new("Credits", TrackKind::Video);
        empty.clips = vec![Clip::gap(tc(0), tc(2))];
        let mut hidden = Track::new("Hidden", TrackKind::Video);
        hidden.clips = vec![media()];
        hidden.visible = false;
        let mut footage = Track::new("Video 1", TrackKind::Video);
        footage.clips = vec![media()];
        let mut seq = Sequence::new_default();
        seq.tracks = vec![hidden, empty, footage, Track::new("Audio 1", TrackKind::Audio)];
        assert_eq!(picture_track(&seq).map(|t| t.name.as_str()), Some("Video 1"));
        seq.tracks.retain(|t| t.name != "Video 1");
        assert!(picture_track(&seq).is_none(), "a hidden track and a track of gaps hold nothing to grade");
    }

    /// An update changes what it names and nothing else, clears what it
    /// gives as null, and refuses a name the brief does not have (#498).
    #[test]
    fn an_update_changes_only_the_fields_it_names() {
        let current: klipp_timeline::Brief = serde_json::from_value(serde_json::json!({
            "title": "Wherever We Walk", "tone": "calm", "target_secs": 150.0,
            "cast": [{"name": "Tove", "role": "walking in Bangkok"}], "notes": "sparse voiceover",
        })).unwrap();
        let updated = merge_brief(&current, &serde_json::json!({ "notes": "stylised transitions at the chapter changes", "tone": null })).unwrap();
        assert_eq!(updated.notes, "stylised transitions at the chapter changes");
        assert_eq!(updated.tone, "", "null clears");
        assert_eq!(updated.title, "Wherever We Walk");
        assert_eq!(updated.target_secs, Some(150.0));
        assert_eq!(updated.cast.len(), 1);

        let err = merge_brief(&current, &serde_json::json!({ "notse": "x" })).unwrap_err();
        assert!(err.contains("\"notse\" is not a field of the brief"), "{err}");
        assert!(merge_brief(&current, &serde_json::json!(["notes"])).is_err());
        // The type styles' motion is held to its fields as in set_brief.
        assert!(merge_brief(&current, &serde_json::json!({
            "type_styles": [{"name": "place", "enter": {"kind": "rise", "delai_secs": 1.0}}]
        })).is_err());
        // #1915: the speech's language is a field of the brief, set and
        // cleared like the rest; #1914: a misspelt one names the field.
        let spoken = merge_brief(&current, &serde_json::json!({ "language": "en" })).unwrap();
        assert_eq!(spoken.language.as_deref(), Some("en"));
        assert_eq!(merge_brief(&spoken, &serde_json::json!({ "language": null })).unwrap().language, None);
        let err = merge_brief(&current, &serde_json::json!({ "target_lufs": -14 })).unwrap_err();
        assert!(err.contains("\"loudness_lufs\""), "{err}");
    }

    #[test]
    fn an_apply_without_a_label_is_named_after_its_ops() {
        use klipp_document::ops::{Anchor, ClipRef, Op};
        let cut = Op::Cut { from: Anchor::Secs(1.0), to: Anchor::Secs(2.0), tracks_of: Vec::new() };
        let split = Op::Split { clip_id: ClipRef::At { track: 0, secs: 0.5 }, at: Anchor::Secs(1.0) };
        assert_eq!(apply_label(std::slice::from_ref(&cut)), "Cut");
        assert_eq!(apply_label(&[cut, split.clone(), split]), "Cut and 2 more");
        assert_eq!(apply_label(&[]), "Apply");
    }
}

#[cfg(test)]
mod face_region_tests {
    use super::face_or_head_regions;
    use klipp_analysis::framing::{FaceBox, FrameSample, Framing};

    fn walking_away() -> FrameSample {
        FrameSample { secs: 1.0, people: vec![(0.4, 0.2, 0.2, 0.6)], faces: Vec::new() }
    }

    /// #326: the face detector looked, saw a person and no face — their
    /// back is to the camera. No head stands in for the face.
    #[test]
    fn a_person_with_no_face_in_view_gives_no_face() {
        let framing = Framing { face_model: Some("yunet_face_2023mar".into()), ..Default::default() };
        assert!(face_or_head_regions(&framing, &walking_away()).is_empty());
    }

    #[test]
    fn a_detected_face_is_the_face() {
        let framing = Framing { face_model: Some("yunet_face_2023mar".into()), ..Default::default() };
        let mut s = walking_away();
        s.faces.push(FaceBox { x: 0.45, y: 0.22, w: 0.1, h: 0.12, score: 0.9, landmarks: Vec::new() });
        assert_eq!(face_or_head_regions(&framing, &s), vec![[0.45, 0.22, 0.1, 0.12]]);
    }

    /// An analysis made without the face detector still reads the head,
    /// as before: it cannot tell a face from a back.
    #[test]
    fn without_a_face_detector_the_head_stands_in() {
        let framing = Framing::default();
        assert_eq!(face_or_head_regions(&framing, &walking_away()).len(), 1);
    }
}

#[cfg(test)]
mod fps_fraction_tests {
    #[test]
    fn rates_become_fractions() {
        assert_eq!(super::fps_fraction(25.0), Ok((25, 1)));
        assert_eq!(super::fps_fraction(29.97), Ok((30000, 1001)));
        assert_eq!(super::fps_fraction(23.976), Ok((24000, 1001)));
        assert_eq!(super::fps_fraction(59.94), Ok((60000, 1001)));
        assert!(super::fps_fraction(0.0).is_err());
        assert!(super::fps_fraction(25.5).is_err());
    }
}

#[cfg(test)]
mod png_size_tests {
    /// A whole PNG gives its size; a cut-off one, or not a PNG, nothing.
    #[test]
    fn a_whole_png_gives_its_size() {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&360u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        let whole = [png.clone(), vec![0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82]].concat();
        assert_eq!(super::png_size_if_whole(&whole), Some((640, 360)));
        assert_eq!(super::png_size_if_whole(&png), None, "no IEND yet");
        assert_eq!(super::png_size_if_whole(b"not a png at all, no, not one bit of it"), None);
    }
}

#[cfg(test)]
mod audio_fixes_tests {
    use super::AudioFixes;

    fn fx(t: &str, params: &[(&str, f32)]) -> klipp_core::AudioEffectInstance {
        klipp_core::AudioEffectInstance { effect_type: t.into(), enabled: true, wet_dry: 1.0, params: params.iter().map(|(k, v)| (k.to_string(), *v)).collect() }
    }

    /// #505: after clean_audio wind the rumble is not asked for again,
    /// a hum still is; a sound also heard where nothing cleaned it still
    /// asks for the fix.
    #[test]
    fn a_fix_on_every_clip_that_plays_the_sound_answers_its_fault() {
        let wind = AudioFixes::on(&[fx("denoiser", &[("strength", 0.6), ("wind", 0.8)])]);
        assert!(wind.covers("a strong low-frequency rumble — wind or handling — sits under the sound"));
        assert!(!wind.covers("a mains hum near 50/60 Hz runs under the sound"));
        assert!(AudioFixes::on(&[fx("hum_removal", &[])]).covers("a mains hum near 50/60 Hz runs under the sound"));
        let bare = AudioFixes::on(&[]);
        assert!(!wind.and(bare).covers("a strong low-frequency rumble — wind or handling — sits under the sound"));
        assert!(wind.and(AudioFixes::ALL).covers("a strong low-frequency rumble — wind or handling — sits under the sound"));
        let off = klipp_core::AudioEffectInstance { enabled: false, ..fx("voice_isolation", &[]) };
        assert!(!AudioFixes::on(&[off]).covers("high-frequency hiss or air is loud against the sound"));
    }
}

#[cfg(test)]
mod group_look_tests {
    use klipp_color::measure::LookTarget;
    use klipp_timeline::{Clip, GradeGroup, Sequence, Track, TrackKind};

    /// #793: a shot in a group with its own look is held to it, a shot in
    /// a group that leaves the look to faults only, any other to the brief.
    #[test]
    fn a_group_holds_its_shots_to_its_own_look() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 25);
        let id = klipp_core::MediaId::new();
        let mut seq = Sequence::new_default();
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = (0..3).map(|k| Clip::media(id, tc(k as f64 * 2.0), tc(k as f64 * 2.0 + 2.0), tc(0.0), tc(2.0))).collect();
        seq.tracks = vec![v];
        let mut south = GradeGroup::new("South");
        south.look = Some(LookTarget { name: "warm_interview".into(), ..Default::default() });
        let mut north = GradeGroup::new("North");
        north.look = Some(LookTarget::preset("cool_tech").unwrap());
        north.departs = true;
        seq.tracks[0].clips[0].grade_group = Some(south.id);
        seq.tracks[0].clips[1].grade_group = Some(north.id);
        seq.grade_groups = vec![south, north];

        let south_look = super::group_look(&seq, tc(1.0)).expect("the south group's look");
        assert_eq!(south_look, LookTarget::preset("warm_interview").unwrap(), "a look by name takes the preset's numbers");
        assert_eq!(super::group_look(&seq, tc(3.0)), Some(super::departing_look()), "leaving the look on purpose outranks its own look");
        assert_eq!(super::group_look(&seq, tc(5.0)), None, "a shot in no group goes by the brief");
    }

    /// #811 review: a frame warm on purpose in a group that leaves the look
    /// gets no cast finding, though the unbriefed target gives it one; a
    /// clipped frame there is still a fault.
    #[test]
    fn a_group_off_the_look_is_judged_for_faults_only() {
        use klipp_color::measure::{judge, FrameMeasure, Verdict};
        let warm = FrameMeasure { l_p1: 6.0, l_p50: 55.0, l_p99: 90.0, cast_a: 4.0, cast_b: 14.0, neutral_pct: 20.0, neutral_solid: true, ..FrameMeasure::default() };
        let rules = |m: &FrameMeasure, t: &LookTarget| judge(m, t).into_iter().filter(|f| f.verdict != Verdict::Pass).map(|f| f.rule).collect::<Vec<_>>();
        assert!(rules(&warm, &LookTarget::unbriefed()).contains(&"cast"), "the unbriefed target calls the warmth a cast");
        assert!(!rules(&warm, &super::departing_look()).contains(&"cast"), "{:?}", rules(&warm, &super::departing_look()));
        let clipped = FrameMeasure { clip_hi_pct: 6.0, ..warm.clone() };
        assert!(rules(&clipped, &super::departing_look()).contains(&"clipped_white"));
    }
}

#[cfg(test)]
mod scene_group_tests {
    /// #772 review: shots of two files that share a name are two scenes,
    /// and a file with one shot is no scene to group.
    #[test]
    fn same_name_different_files_are_two_scenes() {
        let (a, b) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
        let shot = |id: &str| (id.to_string(), 0.0, "IMG_0001.mov".to_string());
        let shots = vec![shot("1"), shot("2"), shot("3"), shot("4"), shot("5")];
        let media_of = [("1", a), ("2", a), ("3", b), ("4", b), ("5", klipp_core::MediaId::new())].into_iter().map(|(k, m)| (k.to_string(), m)).collect();
        let groups = super::scene_groups(&shots, &media_of);
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!(groups[0]["clips"], serde_json::json!(["1", "2"]));
        assert_eq!(groups[1]["clips"], serde_json::json!(["3", "4"]));
        assert_ne!(groups[0]["media_id"], groups[1]["media_id"]);
    }
}

#[cfg(test)]
mod grade_limit_tests {
    use super::{describe_step, GradeLimits};
    use klipp_color::measure::PrimaryStep;

    /// #742: a step is held within the strength's reach of where the shot
    /// started, however many rounds ask for more, and says which knobs
    /// were held.
    #[test]
    fn knobs_stay_within_reach_of_where_the_shot_started() {
        let start = PrimaryStep { saturation: 1.0, ..Default::default() };
        let greedy = PrimaryStep { saturation: 0.3, temperature: 9.0, exposure: 0.1, ..Default::default() };
        let gentle = GradeLimits::named("gentle").unwrap();
        let (one, held) = gentle.clamp(&start, &start, &greedy);
        assert!((one.saturation - 1.12).abs() < 1e-4 && held == vec!["saturation"], "{one:?} {held:?}");
        // A second round asking for the same is held where the first was.
        let (two, held) = gentle.clamp(&start, &one, &greedy);
        assert!((two.saturation - 1.12).abs() < 1e-4 && (two.temperature - 15.0).abs() < 1e-4, "{two:?}");
        assert_eq!(held, vec!["temperature", "saturation"]);
        let bold = GradeLimits::named("bold").unwrap();
        assert!((bold.clamp(&start, &start, &greedy).0.saturation - 1.3).abs() < 1e-4, "bold reaches the 1.3x gentle refused");
        assert!(GradeLimits::named("wild").is_err());
        assert_eq!(describe_step(&start, &one), "exposure +0.10, temperature +9.00, saturation x1.12");
    }
}

#[cfg(test)]
mod sound_role_tests {
    use super::{sound_json, sound_role};
    use klipp_core::{ClipRole, MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    /// A picture on V1 and its sound on A1, linked both ways.
    fn pair() -> Sequence {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 30);
        let media = MediaId::new();
        let mut picture = Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        let mut sound = Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        picture.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(picture.id);
        let mut seq = Sequence::new_default();
        let mut v = Track::new("Video 1", TrackKind::Video);
        v.clips.push(picture);
        let mut a = Track::new("Audio 1", TrackKind::Audio);
        a.clips.push(sound);
        seq.tracks = vec![v, a];
        seq
    }

    #[test]
    fn a_role_set_on_either_half_is_the_pairs_role() {
        let mut seq = pair();
        seq.tracks[1].clips[0].role = ClipRole::Music;
        let sound = &seq.tracks[1].clips[0];
        assert_eq!(sound_role(&seq, sound), ClipRole::Music, "set on the sound half");
        let mut v = serde_json::json!({});
        sound_json(&seq, sound, false, &mut v);
        assert_eq!(v["sound"]["role"], "music", "and reported: {v}");

        let mut seq = pair();
        seq.tracks[0].clips[0].role = ClipRole::Ambience;
        assert_eq!(sound_role(&seq, &seq.tracks[1].clips[0]), ClipRole::Ambience, "set on the picture");
    }
}
