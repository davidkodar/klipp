//! End-to-end exercise of the control socket.
//!
//! Builds a `ControlContext` over synthetic state, starts the real
//! server on a temp socket, and drives it the way `klipp-mcp` does — a
//! JSON line in, a JSON line out. Covers both halves that can go wrong
//! independently: reads must reflect the shared state, and writes must
//! actually reach the `AppCommand` channel.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, RwLock};

use klipp_control::{protocol, ControlContext};
use serde_json::{json, Value};

/// The server reads the socket path from a process-wide variable, so
/// two harnesses at once would bind each other's path: one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Harness {
    _serial: std::sync::MutexGuard<'static, ()>,
    stream: UnixStream,
    frame_buffer: Arc<Mutex<klipp_engine::FrameBuffer>>,
    reader: BufReader<UnixStream>,
    cmd_rx: crossbeam_channel::Receiver<klipp_core::AppCommand>,
    /// Every envelope the stand-in UI took: (batch, command count).
    envelopes: Arc<Mutex<Vec<(bool, usize)>>>,
    edits_seen: Arc<Mutex<Vec<klipp_document::history::EditCommand>>>,
    shared: Arc<klipp_core::ControlShared>,
    media_pool: Arc<RwLock<klipp_media::MediaPool>>,
    sequence: Arc<RwLock<klipp_timeline::Sequence>>,
    log: std::path::PathBuf,
    socket: std::path::PathBuf,
    next_id: u64,
    /// Two clips, each with its own Corrector, and a node nested inside
    /// a Compound on the first — what the id checks are checked against.
    ids: Ids,
}

struct Ids {
    clip_a: String,
    corrector_a: String,
    clip_b: String,
    corrector_b: String,
    nested_in_a: String,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(&self.log);
    }
}

impl Harness {
    fn call(&mut self, request: Value) -> Value {
        self.next_id += 1;
        let mut req = request;
        req["id"] = json!(self.next_id);
        let mut text = req.to_string();
        text.push('\n');
        self.stream.write_all(text.as_bytes()).expect("write request");
        self.stream.flush().ok();
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read response");
        serde_json::from_str(&line).expect("response is json")
    }

    /// Unwraps the `result`, failing loudly with the server's own error
    /// text — a bare unwrap here would hide exactly what went wrong.
    fn ok(&mut self, request: Value) -> Value {
        let reply = self.call(request);
        assert_eq!(
            reply["ok"], json!(true),
            "expected success, got error: {}",
            reply["error"],
        );
        reply["result"].clone()
    }
}

fn harness() -> Harness {
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Linux Unix sockets have a 108-byte pathname limit. The gate's
    // disk-backed TMPDIR can exceed it before the filename is appended.
    // Only this socket inode lives in /tmp; logs and media still use TMPDIR.
    let sock = std::path::Path::new("/tmp")
        .join(format!("klipp-ctl-{}-{stamp}.sock", std::process::id()));
    let log = std::env::temp_dir().join(format!("klipp-control-test-{stamp}.log"));
    std::fs::write(&log, "line one\nline two\nline three\n").expect("write log");

    // The server reads this to decide where to bind.
    std::env::set_var("KLIPP_CONTROL_SOCKET", &sock);
    std::env::remove_var("KLIPP_CONTROL");

    // A stand-in for the UI's inbox: takes each envelope, forwards its
    // commands one by one for the assertions below, advances the
    // revision as a recorded edit would, and acknowledges.
    let (env_tx, env_rx) = crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<klipp_core::AppCommand>();
    let envelopes: Arc<Mutex<Vec<(bool, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let edits_seen: Arc<Mutex<Vec<klipp_document::history::EditCommand>>> = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::new(klipp_core::ControlShared::default());
    let media_pool = Arc::new(RwLock::new(klipp_media::MediaPool::new()));
    {
        let (envelopes, shared, edits_seen) = (Arc::clone(&envelopes), Arc::clone(&shared), Arc::clone(&edits_seen));
        let media_pool = media_pool.clone();
        std::thread::spawn(move || {
            for env in env_rx {
                envelopes.lock().unwrap().push((env.batch, env.commands.len() + env.edits.len()));
                // A plan made for another revision is refused, as the UI does.
                let refused = match env.expected_revision {
                    Some(r) if r != shared.revision() => Some(format!("revision moved from {r}")),
                    _ => None,
                };
                if refused.is_none() {
                    for c in env.commands {
                        if let klipp_core::AppCommand::ImportMediaWithAnalysis { path, analyze: false } = &c {
                            let bytes = std::fs::read(path).expect("synthesized WAV exists before import");
                            assert_eq!(&bytes[..4], b"RIFF");
                            assert!(bytes[44..].iter().any(|b| *b != 0), "the synthesized audio is not silent");
                            let mut item = klipp_media::MediaItem::synthetic_for_test(path.clone(), None);
                            item.video_codec.clear();
                            item.resolution.width = 0;
                            item.resolution.height = 0;
                            item.duration = klipp_core::Timecode::from_frames(((bytes.len() - 44) / 2) as i64, 48000);
                            item.has_audio = true;
                            media_pool.write().unwrap_or_else(|e| e.into_inner()).add(item);
                        }

                        // A save or an open reports back as the UI does: the
                        // path, no unsaved changes, the generation moved. A
                        // path with "broken" in it fails, to see that said.
                        if let klipp_core::AppCommand::SaveProjectAs { path } | klipp_core::AppCommand::OpenProjectNow { path } = &c {
                            let mut p = shared.project.lock().unwrap();
                            p.generation += 1;
                            if path.to_string_lossy().contains("broken") {
                                p.last_error = Some("could not save: the disk said no".into());
                            } else {
                                p.path = Some(path.clone());
                                p.dirty = false;
                                p.last_error = None;
                            }
                        }
                        let _ = cmd_tx.send(c);
                    }
                    for e in env.edits {
                        edits_seen.lock().unwrap().push(e);
                    }
                }
                let revision = if refused.is_none() { shared.bump_revision() } else { shared.revision() };
                if let Some(ack) = env.ack {
                    let _ = ack.send(klipp_document::control::ControlAck { revision, refused });
                }
            }
        });
    }
    // A viewer frame with a recognisable first pixel, so the screenshot
    // is checked for content and not merely for existing.
    let mut fb = klipp_engine::FrameBuffer::new(64, 32);
    fb.data[0] = 10;
    fb.data[1] = 20;
    fb.data[2] = 30;
    fb.data[3] = 255;

    // Two clips on the timeline, each with its own grade graph. Both
    // start out as Source, Corrector, Output — which is the point: the
    // two Correctors are indistinguishable except by id, so a caller
    // reaching for the wrong one is an easy mistake to make and used to
    // be an impossible one to notice.
    let (sequence, ids) = sequence_with_two_clips();
    let sequence = Arc::new(RwLock::new(sequence));
    let frame_buffer = Arc::new(Mutex::new(fb));
    let ctx = ControlContext {
        commands: env_tx,
        shared: Arc::clone(&shared),
        wake_ui: Arc::new(|| {}),
        sequence: sequence.clone(),
        media_pool: media_pool.clone(),
        frame_buffer: frame_buffer.clone(),
        perf_ring: klipp_engine::perf::shared(),
        log_path: log.clone(),
    };

    let path = klipp_control::spawn(ctx).expect("control server starts");
    assert_eq!(path, sock, "server should bind the path we asked for");

    // Bind happens before the accept thread starts, so connect can race
    // it on a cold machine. A short retry is cheaper than a sleep.
    let mut last_err = None;
    for _ in 0..50 {
        match UnixStream::connect(&sock) {
            Ok(stream) => {
                let reader = BufReader::new(stream.try_clone().expect("clone"));
                return Harness { _serial: serial, stream, frame_buffer, reader, cmd_rx, envelopes, edits_seen, shared, media_pool, sequence, log, socket: sock, next_id: 0, ids };
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    panic!("could not connect to {}: {last_err:?}", sock.display());
}

/// A sequence holding two graded clips, and the ids to address them by.
fn sequence_with_two_clips() -> (klipp_timeline::Sequence, Ids) {
    use klipp_nodes::{Node, NodeGraph, NodeId, NodeKind};

    let clip = |with_compound: bool| {
        let mut graph = NodeGraph::default();
        let corrector = graph.ensure_corrector();
        let nested = NodeId::new_v4();
        if with_compound {
            // A Compound's children are addressable too, so the check
            // has to look inside one rather than only at the top level.
            let mut sub = NodeGraph::default();
            sub.nodes.push(Node {
                id: nested,
                kind: NodeKind::Exposure,
                label: "Exposure".into(),
                enabled: true,
                params: Default::default(),
                keyframes: Default::default(),
                output_link: None,
                input_links: Vec::new(),
            });
            let params = klipp_nodes::params::NodeParams {
                compound_subgraph: Some(sub),
                ..Default::default()
            };
            graph.nodes.push(Node {
                id: NodeId::new_v4(),
                kind: NodeKind::Compound,
                label: "Compound".into(),
                enabled: true,
                params,
                keyframes: Default::default(),
                output_link: None,
                input_links: Vec::new(),
            });
        }
        let mut c = klipp_timeline::Clip::gap(
            klipp_core::RationalTime::from_secs_f64(0.0, 24),
            klipp_core::RationalTime::from_secs_f64(4.0, 24),
        );
        c.node_graph = graph;
        (c, corrector.to_string(), nested.to_string())
    };

    let (clip_a, corrector_a, nested_in_a) = clip(true);
    let (clip_b, corrector_b, _) = clip(false);
    let ids = Ids {
        clip_a: clip_a.id.0.to_string(),
        corrector_a,
        clip_b: clip_b.id.0.to_string(),
        corrector_b,
        nested_in_a,
    };
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip_a, clip_b];
    seq.tracks = vec![track];
    (seq, ids)
}

/// One test, not several: the socket path comes from a process-global
/// env var, so parallel tests in this binary would fight over it.
#[test]
fn control_socket_reads_state_and_forwards_commands() {
    let mut h = harness();

    // ── Reads ──────────────────────────────────────────────────────
    let pong = h.ok(json!({ "method": "ping" }));
    assert_eq!(pong["pong"], json!(true));
    assert_eq!(pong["pid"], json!(std::process::id()));

    let status = h.ok(json!({ "method": "status" }));
    assert_eq!(status["viewer"]["width"], json!(64));
    assert_eq!(status["viewer"]["height"], json!(32));
    assert!(status["tracks"].as_u64().is_some(), "status reports track count");
    assert!(status["build"]["commit"].is_string(), "status reports the build: {status}");

    let clips = h.ok(json!({ "method": "list_clips" }));
    assert!(clips["tracks"].is_array(), "list_clips returns a track array");

    let media = h.ok(json!({ "method": "list_media" }));
    assert_eq!(media["items"].as_array().map(|a| a.len()), Some(0));

    let log = h.ok(json!({ "method": "log_tail", "lines": 2 }));
    let lines: Vec<&str> = log["lines"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(lines, vec!["line two", "line three"], "tail keeps file order");

    // ── Screenshot ─────────────────────────────────────────────────
    let shot_path = std::env::temp_dir().join(format!(
        "klipp-control-test-shot-{}.png",
        std::process::id(),
    ));
    let shot = h.ok(json!({
        "method": "screenshot",
        "path": shot_path.to_string_lossy(),
    }));
    assert_eq!(shot["width"], json!(64));
    assert_eq!(shot["height"], json!(32));
    let bytes = std::fs::read(&shot_path).expect("screenshot file exists");
    assert!(bytes.starts_with(b"\x89PNG"), "screenshot is a real PNG");
    let _ = std::fs::remove_file(&shot_path);

    // Downscaling is what keeps a 4K frame from being shipped whole.
    let small = h.ok(json!({
        "method": "screenshot",
        "path": shot_path.to_string_lossy(),
        "max_width": 16,
    }));
    assert_eq!(small["width"], json!(16));
    assert_eq!(small["height"], json!(8), "aspect ratio is preserved");
    let _ = std::fs::remove_file(&shot_path);

    // ── Writes reach the AppCommand channel ────────────────────────
    h.ok(json!({ "method": "play" }));
    assert!(
        matches!(h.cmd_rx.try_recv(), Ok(klipp_core::AppCommand::Play)),
        "play must forward AppCommand::Play",
    );
    h.ok(json!({ "method": "pause" }));
    assert!(matches!(h.cmd_rx.try_recv(), Ok(klipp_core::AppCommand::Pause)));

    h.ok(json!({ "method": "seek", "secs": 2.5 }));
    match h.cmd_rx.try_recv() {
        Ok(klipp_core::AppCommand::SeekTo(tc)) => {
            assert!(
                (tc.to_secs_f64() - 2.5).abs() < 0.05,
                "seek landed at {}s, expected 2.5s", tc.to_secs_f64(),
            );
        }
        other => panic!("expected SeekTo, got {other:?}"),
    }

    let (clip, node) = (h.ids.clip_a.clone(), h.ids.corrector_a.clone());
    h.ok(json!({
        "method": "set_node_param",
        "clip_id": clip, "node_id": node,
        "param": "temperature", "value": 0.25,
    }));
    match h.cmd_rx.try_recv() {
        Ok(klipp_core::AppCommand::SetNodeParam { param, value, .. }) => {
            assert_eq!(param, "temperature");
            assert!((value - 0.25).abs() < 1e-6);
        }
        other => panic!("expected SetNodeParam, got {other:?}"),
    }

    // ── An id that names nothing is refused, not accepted and dropped ─
    // A write used to be answered `accepted` whatever the ids were: the
    // command went down the channel and the UI's `find` quietly failed
    // to match anything. The grade then read back as identity with no
    // indication why. These are the four ways to get that wrong.
    let (clip_a, clip_b) = (h.ids.clip_a.clone(), h.ids.clip_b.clone());
    let (corr_a, corr_b) = (h.ids.corrector_a.clone(), h.ids.corrector_b.clone());
    let nested = h.ids.nested_in_a.clone();
    let refused = |h: &mut Harness, req: Value, expect: &str| {
        let reply = h.call(req);
        assert_eq!(reply["ok"], json!(false), "expected a refusal, got {reply}");
        let msg = reply["error"].as_str().unwrap_or_default();
        assert!(
            msg.contains(expect),
            "error should mention {expect:?} so the caller can fix it; got {msg:?}",
        );
        assert!(
            h.cmd_rx.try_recv().is_err(),
            "a refused command must not reach the app: it would change the wrong thing or nothing",
        );
    };

    // Another clip's node. The bug that prompted this: both clips carry
    // a Corrector, the ids look alike, and nothing said a word.
    refused(
        &mut h,
        json!({
            "method": "set_node_param",
            "clip_id": clip_a, "node_id": corr_b,
            "param": "temperature", "value": -9.0,
        }),
        "has no node",
    );
    // The error has to name the nodes the clip does have, or the caller
    // is left guessing at uuids.
    let reply = h.call(json!({
        "method": "set_node_param",
        "clip_id": clip_a, "node_id": corr_b,
        "param": "temperature", "value": -9.0,
    }));
    let msg = reply["error"].as_str().unwrap_or_default();
    assert!(msg.contains(corr_a.as_str()), "error lists the clip's own nodes: {msg}");
    assert!(msg.contains("Corrector"), "error names each node's kind: {msg}");
    let _ = h.cmd_rx.try_recv();

    // A node id that is no node at all.
    refused(
        &mut h,
        json!({
            "method": "set_node_param",
            "clip_id": clip_a, "node_id": uuid::Uuid::new_v4().to_string(),
            "param": "contrast", "value": 0.1,
        }),
        "has no node",
    );
    // A clip id that is no clip.
    refused(
        &mut h,
        json!({
            "method": "set_node_param",
            "clip_id": uuid::Uuid::new_v4().to_string(), "node_id": corr_a,
            "param": "contrast", "value": 0.1,
        }),
        "no clip with id",
    );
    // The same check guards the other node-addressing commands.
    refused(
        &mut h,
        json!({
            "method": "toggle_node_enabled",
            "clip_id": clip_a, "node_id": corr_b,
        }),
        "has no node",
    );
    refused(
        &mut h,
        json!({
            "method": "remove_node",
            "clip_id": clip_a, "node_id": corr_b,
        }),
        "has no node",
    );

    // A parameter name the grade does not carry is the same silent
    // no-op one level down: `apply_override` would drop it on its
    // fall-through and the caller would see a grade that did nothing.
    refused(
        &mut h,
        json!({
            "method": "set_node_param",
            "clip_id": clip_a, "node_id": corr_a,
            "param": "temperatur", "value": -9.0,
        }),
        "no grade parameter called",
    );

    // ── What the check must NOT refuse ────────────────────────────────
    // The qualifier's window, which is how a secondary is built over
    // this socket.
    h.ok(json!({
        "method": "set_node_param",
        "clip_id": clip_a, "node_id": corr_a,
        "param": "qual_hue_center", "value": 40.0,
    }));
    assert!(matches!(
        h.cmd_rx.try_recv(),
        Ok(klipp_core::AppCommand::SetNodeParam { .. })
    ));
    // A node inside a Compound is addressable, so the lookup recurses.
    h.ok(json!({
        "method": "set_node_param",
        "clip_id": clip_a, "node_id": nested,
        "param": "exposure", "value": 0.5,
    }));
    assert!(
        matches!(h.cmd_rx.try_recv(), Ok(klipp_core::AppCommand::SetNodeParam { .. })),
        "a node nested in a Compound must still be writable",
    );
    // The phantom Corrector id is not a node — it is the request to
    // create one on a clip that has none — so it passes through.
    h.ok(json!({
        "method": "set_node_param",
        "clip_id": clip_b,
        "node_id": klipp_nodes::PHANTOM_CORRECTOR_ID.to_string(),
        "param": "saturation", "value": 1.1,
    }));
    assert!(
        matches!(h.cmd_rx.try_recv(), Ok(klipp_core::AppCommand::SetNodeParam { .. })),
        "the phantom Corrector id must reach the app, which materialises the node",
    );

    // ── Change-then-look waits for the frame that reflects the change ─
    // Writes report the generation current when they were queued.
    let queued = h.ok(json!({ "method": "pause" }));
    let _ = h.cmd_rx.try_recv();
    let gen = queued["generation"].as_u64().expect("writes report a generation");

    // Nothing newer exists yet, so a bounded wait must fail loudly rather
    // than hand back the old frame as though it were the new one.
    let stale = h.call(json!({
        "method": "screenshot",
        "path": shot_path.to_string_lossy(),
        "min_generation": gen + 1,
        "timeout_ms": 50,
    }));
    assert_eq!(stale["ok"], json!(false), "must not return a stale frame");
    assert!(
        stale["error"].as_str().unwrap().contains("did not trigger a re-render"),
        "timeout should say what it means, got {}", stale["error"],
    );

    // The engine renders a frame after the command lands; the screenshot
    // waits for it and returns *that* frame, not the one before it.
    let fb = h.frame_buffer.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut pixels = vec![0u8; 64 * 32 * 4];
        pixels[0] = 200;
        fb.lock().unwrap().write_frame(pixels, 64, 32);
    });
    let fresh = h.ok(json!({
        "method": "screenshot",
        "path": shot_path.to_string_lossy(),
        "min_generation": gen + 1,
        "timeout_ms": 3000,
    }));
    writer.join().unwrap();
    assert!(
        fresh["generation"].as_u64().unwrap() > gen,
        "captured generation {} must be newer than {gen}", fresh["generation"],
    );
    let _ = std::fs::remove_file(&shot_path);

    let rel = h.call(json!({ "method": "screenshot", "path": "relative.png" }));
    assert_eq!(rel["ok"], json!(false), "relative screenshot paths must be refused");
    assert!(!std::path::Path::new("relative.png").exists(), "and nothing written");

    // ── Errors are reported, not swallowed ─────────────────────────
    let bad = h.call(json!({ "method": "get_grade", "clip_id": "not-a-uuid" }));
    assert_eq!(bad["ok"], json!(false));
    assert!(
        bad["error"].as_str().unwrap().contains("uuid"),
        "error should name the problem, got {}", bad["error"],
    );

    let missing = h.call(json!({
        "method": "import_media",
        "path": "/definitely/not/here.mov",
    }));
    assert_eq!(missing["ok"], json!(false));
    assert!(missing["error"].as_str().unwrap().contains("not found"));
    // A file that is there but not a video or a sound: refused with why,
    // not an ok with nothing imported (#654).
    let junk = std::env::temp_dir().join(format!("klipp-junk-{}.mp4", std::process::id()));
    std::fs::write(&junk, b"this is not a video file at all").unwrap();
    let refused = h.call(json!({ "method": "import_media", "path": junk.to_string_lossy() }));
    let _ = std::fs::remove_file(&junk);
    assert_eq!(refused["ok"], json!(false), "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("could not be imported"), "{}", refused["error"]);

    // An unparseable line must still get an answer rather than hang the
    // client waiting for one.
    h.stream.write_all(b"{ this is not json }\n").expect("write junk");
    h.stream.flush().ok();
    let mut line = String::new();
    h.reader.read_line(&mut line).expect("server answers bad json");
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("bad request"));

    let _ = std::fs::remove_file(protocol::socket_path());
}

/// A batch reaches the UI as one envelope, recorded as one undo step, or
/// not at all: a bad command anywhere in it refuses the whole batch
/// before anything is sent. Writes answer with the document revision
/// after them, and the timeline reads in seconds and frames.
#[test]
fn a_batch_is_one_envelope_or_nothing_and_writes_answer_with_the_revision() {
    let mut h = harness();
    let before = h.shared.revision();

    // A single write: applied, and the revision moved.
    let r = h.ok(json!({ "method": "seek", "secs": 1.0 }));
    assert_eq!(r["applied"], json!(true));
    assert_eq!(r["revision"], json!(before + 1));
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(1)), Ok(klipp_core::AppCommand::SeekTo(_))));
    assert_eq!(h.envelopes.lock().unwrap().last(), Some(&(false, 1)));

    // A batch of two: one envelope, both commands, one revision step.
    let r = h.ok(json!({ "method": "batch", "commands": [
        { "method": "seek", "secs": 2.0 },
        { "method": "play" },
    ]}));
    assert_eq!(r["applied"], json!(true));
    assert_eq!(r["commands"], json!(2));
    assert_eq!(r["revision"], json!(before + 2));
    assert_eq!(h.envelopes.lock().unwrap().last(), Some(&(true, 2)));
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(1)), Ok(klipp_core::AppCommand::SeekTo(_))));
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(1)), Ok(klipp_core::AppCommand::Play)));

    // A bad command inside a batch refuses it whole: nothing sent.
    let sent = h.envelopes.lock().unwrap().len();
    let reply = h.call(json!({ "method": "batch", "commands": [
        { "method": "play" },
        { "method": "set_node_param", "clip_id": "not-a-clip", "node_id": "x", "param": "exposure", "value": 1.0 },
    ]}));
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("batch command 1"), "{}", reply["error"]);
    assert_eq!(h.envelopes.lock().unwrap().len(), sent, "a refused batch sends nothing");
    assert!(h.cmd_rx.try_recv().is_err());

    // A read inside a batch is refused too.
    let reply = h.call(json!({ "method": "batch", "commands": [ { "method": "status" } ] }));
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("cannot be part of a batch"));

    // Undo is a write like any other.
    let r = h.ok(json!({ "method": "undo" }));
    assert_eq!(r["applied"], json!(true));
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(1)), Ok(klipp_core::AppCommand::Undo)));

    // Reads carry the revision and editorial units.
    let status = h.ok(json!({ "method": "status" }));
    assert_eq!(status["revision"], json!(h.shared.revision()));
    assert!(status.get("playhead_secs").is_some());
    assert!(status.get("duration_secs").is_some());
    let clips = h.ok(json!({ "method": "list_clips" }));
    let first = &clips["tracks"][0]["clips"][0];
    assert!(first["timeline_in_frame"].is_i64(), "{first}");
    assert_eq!(first["kind"], json!("gap"), "the fixture's clips are gaps with a grade");
    assert!(clips["fps"].is_f64());
}

/// An export is refused where it would go wrong, and otherwise reaches
/// the UI as the app's own export command; the queue reads back through
/// the shared state.
#[test]
fn export_is_refused_where_it_would_go_wrong_and_status_reads_the_queue() {
    let mut h = harness();
    let no_jobs = h.call(json!({ "method": "export_status", "wait": true }));
    assert_eq!(no_jobs["ok"], false);
    // The fixture's clips are gaps: nothing to export (#655).
    let empty = h.call(json!({ "method": "export", "path": "/tmp/klipp-control-test-out.mp4" }));
    assert_eq!(empty["ok"], json!(false));
    assert!(empty["error"].as_str().unwrap().contains("empty"), "{}", empty["error"]);
    // Pictures on it from here.
    let media = h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/export-fixture.mov".into(), None));
    for c in h.sequence.write().unwrap().tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        c.kind = klipp_timeline::ClipKind::Media { media_id: media };
    }
    let reply = h.call(json!({ "method": "export", "path": "relative.mp4" }));
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("absolute"));
    let reply = h.call(json!({ "method": "export", "path": "/tmp/out.avi" }));
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("container"));

    let r = h.ok(json!({ "method": "export", "path": "/tmp/klipp-control-test-out.mp4", "target_lufs": -14.0, "crf": 20 }));
    assert_eq!(r["applied"], json!(true));
    match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(1)) {
        Ok(klipp_core::AppCommand::ExportSequence { settings }) => {
            assert_eq!(settings.output_path, std::path::PathBuf::from("/tmp/klipp-control-test-out.mp4"));
            assert_eq!(settings.target_lufs, Some(-14.0));
            assert_eq!(settings.crf, 20);
            assert!(matches!(settings.format, klipp_core::ExportFormat::Mp4));
        }
        other => panic!("expected ExportSequence, got {other:?}"),
    }

    h.shared.exports.lock().unwrap().push(klipp_core::ExportStatus {
        job_id: "j1".into(),
        path: "/tmp/klipp-control-test-out.mp4".into(),
        status: "rendering".into(),
        progress: 0.5,
        frames_done: 50,
        total_frames: 100,
        stage: "Encoding".into(),
        error: None,
        loudness: None,
        hardware: "Decode: CUDA (NVDEC) | Encode: h264_nvenc".into(),
        fps: Some(28.0),
        eta_secs: Some(2),
    });
    let st = h.ok(json!({ "method": "export_status" }));
    assert_eq!(st["jobs"][0]["status"], json!("rendering"));
    assert_eq!(st["jobs"][0]["progress"], json!(0.5));
    let timeout = h.ok(json!({ "method": "export_status", "wait": true, "job_id": "j1", "timeout_ms": 1 }));
    assert_eq!(timeout["finished"], false);
    assert_eq!(timeout["timed_out"], true);
    let shared = h.shared.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut jobs = shared.exports.lock().unwrap_or_else(|e| e.into_inner());
        jobs[0].status = "complete".into();
        jobs[0].progress = 1.0;
        jobs[0].frames_done = 100;
        // A later job must not extend a wait for the captured job.
        let mut later = jobs[0].clone();
        later.job_id = "j2".into();
        later.status = "rendering".into();
        jobs.push(later);
    });
    let done = h.ok(json!({ "method": "export_status", "wait": true, "timeout_ms": 2000 }));
    worker.join().unwrap();
    assert_eq!(done["finished"], true);
    assert_eq!(done["succeeded"], true);
    assert_eq!(done["timed_out"], false);
    assert_eq!(done["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(done["jobs"][0]["frames_done"], 100);
    for terminal in ["failed", "cancelled"] {
        h.shared.exports.lock().unwrap_or_else(|e| e.into_inner())[0].status = terminal.into();
        let failed = h.ok(json!({ "method": "export_status", "wait": true, "job_id": "j1" }));
        assert_eq!(failed["finished"], true);
        assert_eq!(failed["succeeded"], false);
    }
    let missing = h.call(json!({ "method": "export_status", "wait": true, "job_id": "missing" }));
    assert_eq!(missing["ok"], false);

}

/// The text view carries what a model needs to plan and respects its
/// budget on a line boundary.
#[test]
fn the_timeline_reads_as_text_within_a_budget() {
    let mut h = harness();
    let v = h.ok(json!({ "method": "timeline" }));
    let text = v["text"].as_str().unwrap();
    assert_eq!(v["cut"], json!(false));
    assert!(text.contains("Sequence \""), "{text}");
    assert!(text.contains("Tracks, top first"), "{text}");
    assert!(text.contains(" f]"), "ranges carry frames: {text}");
    assert!(text.contains(&format!("id {}", h.ids.clip_a)), "clip ids are in the view: {text}");
    assert!(text.contains("grade"), "{text}");

    let v = h.ok(json!({ "method": "timeline", "max_chars": 200 }));
    assert_eq!(v["cut"], json!(true));
    let text = v["text"].as_str().unwrap();
    assert!(text.chars().count() <= 200 + 40, "cut near the budget: {}", text.chars().count());
    assert!(text.contains("cut at 200 chars"));
}

/// The analysis reads answer without analysed media: nothing found,
/// nothing wrong.
#[test]
fn analysis_queries_answer_empty_without_analysed_media() {
    let mut h = harness();
    let v = h.ok(json!({ "method": "find_shots", "framing": "wide" }));
    assert_eq!(v["hits"].as_array().map(|a| a.len()), Some(0));
    let v = h.ok(json!({ "method": "search_transcripts", "query": "anything" }));
    assert_eq!(v["hits"].as_array().map(|a| a.len()), Some(0));
    let v = h.ok(json!({ "method": "analysis_status" }));
    assert!(v["items"].is_array());
    let empty = h.ok(json!({ "method": "analyze", "wait": true, "timeout_ms": 1000 }));
    assert_eq!(empty["finished"], true);
    assert_eq!(empty["succeeded"], true);
    let missing = h.call(json!({ "method": "analysis_status", "wait": true, "media_id": "00000000-0000-0000-0000-000000000000" }));
    assert_eq!(missing["ok"], false);

}

/// A plan is resolved against the timeline and applied as one envelope
/// of edits; a refused op applies nothing; a plan made for an older
/// revision is refused by the host.
#[test]
fn plans_are_dry_runs_and_applies_are_one_envelope_of_edits() {
    let mut h = harness();
    let clip = h.ids.clip_a.clone();
    // The plan is its text; the steps as JSON come when asked for.
    let brief = h.ok(json!({ "method": "plan", "ops": [ { "op": "split", "clip_id": clip, "at": { "secs": 1.0 } } ] }));
    assert!(brief.get("steps").is_none() && brief["steps_resolved"] == json!(1) && brief["text"].as_str().unwrap().starts_with("1. "), "{brief}");
    let v = h.ok(json!({ "method": "plan", "detail": true, "ops": [ { "op": "split", "clip_id": clip, "at": { "secs": 1.0 } } ] }));
    assert_eq!(v["applied"], json!(false));
    assert!(v["text"].as_str().unwrap().contains("1. split"), "{}", v["text"]);
    assert_eq!(v["steps"][0]["after"].as_array().map(|a| a.len()), Some(2));
    assert!(h.edits_seen.lock().unwrap().is_empty(), "a plan applies nothing");

    let reply = h.call(json!({ "method": "plan", "ops": [ { "op": "split", "clip_id": clip, "at": { "secs": 99.0 } } ] }));
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("not inside"), "{}", reply["error"]);

    let before = h.shared.revision();
    let v = h.ok(json!({ "method": "apply", "label": "split for the test", "ops": [ { "op": "split", "clip_id": clip, "at": { "secs": 1.0 } } ] }));
    assert!(v["next"].as_str().unwrap().contains("check"), "apply points at the next step: {v}");
    assert_eq!(v["applied"], json!(true));
    assert_eq!(v["revision"], json!(before + 1));
    assert_eq!(h.envelopes.lock().unwrap().last(), Some(&(true, 1)));
    let edits = h.edits_seen.lock().unwrap();
    assert!(matches!(edits.as_slice(), [klipp_document::history::EditCommand::LegacyBladeClip { .. }]));
}

/// A project is saved and opened over the socket with nobody to answer
/// a dialog: a save names its path the first time, an open refuses to
/// throw unsaved work away unless told to, and a failure comes back as
/// one.
#[test]
fn a_project_is_saved_and_opened_without_a_dialog() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-control-project-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let at = |name: &str| dir.join(name).to_string_lossy().to_string();

    // Never saved: status says so, and a save with no path is refused.
    let st = h.ok(json!({ "method": "status" }));
    assert_eq!(st["project"]["path"], json!(null));
    let r = h.call(json!({ "method": "save_project" }));
    assert!(r["error"].as_str().unwrap().contains("give a path"), "{r}");
    let r = h.call(json!({ "method": "save_project", "path": "relative/cut" }));
    assert!(r["error"].as_str().unwrap().contains("absolute"), "{r}");

    // With a path: the folder gets the extension, and the next save needs none.
    let r = h.ok(json!({ "method": "save_project", "path": at("cut") }));
    assert!(r["path"].as_str().unwrap().ends_with("cut.klipp"), "{r}");
    assert!(matches!(h.cmd_rx.try_iter().last(), Some(klipp_core::AppCommand::SaveProjectAs { .. })));
    let r = h.ok(json!({ "method": "save_project" }));
    assert!(r["path"].as_str().unwrap().ends_with("cut.klipp"), "{r}");
    assert_eq!(h.ok(json!({ "method": "status" }))["project"]["unsaved_changes"], json!(false));

    // A save that fails says so.
    let r = h.call(json!({ "method": "save_project", "path": at("broken") }));
    assert!(r["error"].as_str().unwrap().contains("the disk said no"), "{r}");

    // Opening: the path has to exist, and unsaved work is not thrown away unasked.
    let r = h.call(json!({ "method": "open_project", "path": at("nowhere.klipp") }));
    assert!(r["error"].as_str().unwrap().contains("no project"), "{r}");
    std::fs::create_dir_all(dir.join("other.klipp")).unwrap();
    h.shared.project.lock().unwrap().dirty = true;
    let r = h.call(json!({ "method": "open_project", "path": at("other.klipp") }));
    assert!(r["error"].as_str().unwrap().contains("unsaved changes"), "{r}");
    let r = h.ok(json!({ "method": "open_project", "path": at("other.klipp"), "discard_changes": true }));
    assert_eq!(r["opened"], json!(true));
    assert!(matches!(h.cmd_rx.try_iter().last(), Some(klipp_core::AppCommand::OpenProjectNow { .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The brief is set as a recorded edit and read back, a bad one is
/// refused with why, and the check answers with findings.
#[test]
fn a_brief_is_set_and_the_piece_is_checked_against_it() {
    let mut h = harness();
    let r = h.ok(json!({ "method": "brief" }));
    assert_eq!(r["brief"], json!(null));
    assert!(r["text"].as_str().unwrap().contains("no brief"));

    let before = h.ok(json!({ "method": "status" }))["revision"].as_u64().unwrap();
    let r = h.ok(json!({ "method": "set_brief", "brief": {
        "title": "Why Omarchy is not afraid of AI code", "platform": "web", "target_secs": 90, "loudness_lufs": -14,
        "captions": true, "cast": [{ "name": "David Heinemeier Hansson", "role": "Creator of Omarchy" }] } }));
    assert_eq!(r["revision"], json!(before + 1));
    assert!(r["text"].as_str().unwrap().contains("Cast: David Heinemeier Hansson, Creator of Omarchy"), "{r}");
    assert!(matches!(h.edits_seen.lock().unwrap().last(), Some(klipp_document::history::EditCommand::SetBrief { new: Some(_), .. })));

    for (bad, why) in [
        (json!({ "loudness_lufs": 3 }), "not a delivery loudness"),
        (json!({ "width": 1080 }), "both width and height"),
        (json!({ "cast": [{ "name": "Ada" }, { "name": "ada " }] }), "twice"),
        (json!({ "target_secs": -5 }), "not a length"),
    ] {
        let r = h.call(json!({ "method": "set_brief", "brief": bad }));
        assert!(r["error"].as_str().unwrap().contains(why), "{r}");
    }

    // The stand-in UI does not apply edits, so the check sees no brief; it
    // still answers with a report and its findings.
    let r = h.ok(json!({ "method": "check" }));
    assert!(r["text"].as_str().unwrap().starts_with("Checked "), "{r}");
    assert!(r["next"].is_string(), "check points at the next step: {r}");
    assert!(r["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "no_brief"), "{r}");
}

/// The edit plan is proposed as a recorded edit, never approved by its
/// proposer, and neither approved nor built when there is none; a plan
/// that cannot be built is refused with which segment and why.
#[test]
fn an_edit_plan_is_proposed_unapproved_and_refused_when_it_cannot_build() {
    let mut h = harness();
    let r = h.ok(json!({ "method": "edit_plan" }));
    assert_eq!(r["plan"], json!(null));
    assert!(r["text"].as_str().unwrap().contains("no edit plan"), "{r}");
    for method in ["approve_edit_plan", "build_edit_plan"] {
        let r = h.call(json!({ "method": method }));
        assert!(r["error"].as_str().unwrap().contains("no edit plan"), "{r}");
    }

    let r = h.call(json!({ "method": "set_edit_plan", "plan": { "segments": [] } }));
    assert!(r["error"].as_str().unwrap().contains("at least one segment"), "{r}");
    // The harness's pool is empty: a segment's file must exist.
    let r = h.call(json!({ "method": "set_edit_plan", "plan": { "segments": [
        { "media_id": "00000000-0000-0000-0000-000000000001", "source_in_secs": 1.0, "source_out_secs": 2.0 } ] } }));
    assert!(r["error"].as_str().unwrap().contains("segment 1: no media item"), "{r}");
    assert!(h.edits_seen.lock().unwrap().is_empty(), "a refused plan records nothing");
    // A plan that says it is approved is stored unapproved all the same;
    // that is checked in the document tests, where the pool has a file.
}

/// One call tells a session where it stands and what to do next.
#[test]
fn orient_hands_over_the_whole_situation() {
    let mut h = harness();
    let r = h.ok(json!({ "method": "orient" }));
    assert!(r["build"]["commit"].is_string());
    assert_eq!(r["project"]["path"], json!(null));
    assert_eq!(r["brief"], json!(null));
    assert!(r["sequence"].as_str().unwrap().contains("Tracks"), "{r}");
    // With no footage and no brief, the next step is to import and set one.
    assert!(r["next"].as_str().unwrap().contains("Import the footage"), "{}", r["next"]);
    // Media and sequence come through the same call (the stand-in UI does
    // not apply edits, so the brief stays as the sequence holds it).
    assert!(r["media"].is_array() && r["clips"].is_number());
    assert!(r["guide"].as_str().unwrap().contains("klipp_guide"));
}

/// The scope readings measure a made frame: a mid-grey field reads
/// neutral at mid IRE; a warm, bright, low-contrast field reads a red
/// cast and high black point.
#[test]
fn frame_stats_reads_the_scopes_of_a_frame() {
    // frame_stats is a pure measurement; call it through the module.
    let grey: Vec<u8> = std::iter::repeat_n([128u8, 128, 128, 255], 320 * 180).flatten().collect();
    let v = klipp_control::frame_stats(&grey, 320, 180, 1.0);
    assert!((v["luma_ire"]["mid"].as_f64().unwrap() - 50.0).abs() < 2.0, "{v}");
    assert_eq!(v["rgb_balance"]["note"], "neutral");
    assert!(v["text"].as_str().unwrap().contains("mid 50"));

    // A grey wall under a warm lamp: the neutrals carry a red cast.
    let warm: Vec<u8> = std::iter::repeat_n([175u8, 150, 128, 255], 320 * 180).flatten().collect();
    let v = klipp_control::frame_stats(&warm, 320, 180, 2.0);
    assert!(v["rgb_balance"]["note"].as_str().unwrap().contains("red cast"), "{v}");
    assert!(v["luma_ire"]["black"].as_f64().unwrap() > 55.0, "a bright field lifts the black point: {v}");

    // #327: blue sky over a warm-lit grey wall. The frame's average is
    // blue, but the cast is read on the wall — warm — and says so, as
    // the Lab reading does.
    let (w, h) = (100usize, 100usize);
    let street: Vec<u8> = (0..w * h)
        .flat_map(|i| if i / w < 60 { [90u8, 150, 230, 255] } else { [158, 148, 134, 255] })
        .collect();
    let v = klipp_control::frame_stats(&street, w as u32, h as u32, 3.0);
    assert!(v["rgb_balance"]["blue"].as_f64().unwrap() > v["rgb_balance"]["red"].as_f64().unwrap(), "the average is the sky's: {v}");
    assert!(v["rgb_balance"]["note"].as_str().unwrap().contains("red cast"), "the cast is the wall's, not the sky's: {v}");

    // Nothing neutral in the frame: no cast is named.
    let grass: Vec<u8> = std::iter::repeat_n([60u8, 150, 50, 255], 320 * 180).flatten().collect();
    let v = klipp_control::frame_stats(&grass, 320, 180, 4.0);
    assert_eq!(v["rgb_balance"]["note"], "no neutral surface to read a cast from", "{v}");
}

/// The colour faults read a frame's stats: a blown, over-saturated,
/// dull, or skin-off frame each draws the right note; a clean one none.
#[test]
fn color_faults_flag_what_a_colourist_would() {
    let clean = klipp_control::frame_stats(&std::iter::repeat_n([195u8, 193, 190, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 1.0);
    assert!(klipp_control::color_faults(&clean, 1.0).is_empty(), "{clean}");
    // An over-saturated frame: pure saturated red at full. Its red is on
    // the rail but the picture is not white (luma 61 of 255): that is
    // saturation, not clipped white (#459).
    let hot = klipp_control::frame_stats(&std::iter::repeat_n([255u8, 10, 10, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 2.0);
    let faults = klipp_control::color_faults(&hot, 2.0);
    assert!(faults.iter().any(|f| f.1.contains("saturation is high")), "{faults:?}");
    assert!(!faults.iter().any(|f| f.1.contains("clipped white")), "a pinned red is not white: {faults:?}");
    // A blown frame: white.
    let blown = klipp_control::frame_stats(&std::iter::repeat_n([255u8, 255, 255, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 2.5);
    assert!(klipp_control::color_faults(&blown, 2.5).iter().any(|f| f.1.contains("clipped white")), "{blown}");
    // A dull frame: everything at 40/255 grey → white point ~16 IRE.
    let dull = klipp_control::frame_stats(&std::iter::repeat_n([40u8, 40, 40, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 3.0);
    assert!(klipp_control::color_faults(&dull, 3.0).iter().any(|f| f.1.contains("dull")), "{dull}");
}

/// cut_points ranks pauses over sentence ends over phrase breaks, and
/// tells the truth about a target that lands mid-word or mid-sentence.
#[test]
fn cut_points_find_the_clean_joins() {
    let words = vec![
        (0.0f64, 0.4, "So".into()),
        (0.5, 1.2, "anyway".into()),
        (2.0, 2.6, "right".into()),
    ];
    let segments = vec![
        (0.0f64, 1.2, "So anyway.".into()),   // ends a sentence
        (2.0, 2.6, "right".into()),           // no full stop → phrase
    ];
    let silences = vec![(1.2f64, 2.0)];       // a 0.8 s pause
    let shots = vec![3.0f64];
    let v = klipp_control::cut_points(&words, &segments, &silences, &shots, Some(1.0), (0.0, 4.0));

    // The pause is strength 3 and is the nearest clean point to 1.0 s.
    let points = v["points"].as_array().unwrap();
    let pause = points.iter().find(|p| p["kind"] == "silence").unwrap();
    assert_eq!(pause["strength"], 3);
    // 1.0 s lands inside the word "anyway" → mid-word, honestly flagged.
    assert_eq!(v["target"]["verdict"], "mid-word");
    assert!(v["target"]["why"].as_str().unwrap().contains("anyway"));
    let nearest = v["target"]["nearest_clean_secs"].as_f64().unwrap();
    assert!((nearest - 1.2).abs() < 0.01, "nearest clean is the full stop at 1.2, got {nearest}");

    // A target sitting inside "right" with no full stop is mid-sentence.
    let v2 = klipp_control::cut_points(&words, &segments, &silences, &shots, Some(2.3), (0.0, 4.0));
    assert_eq!(v2["target"]["verdict"], "mid-word");
    // The shot change is a clean strength-2 point.
    assert!(v2["points"].as_array().unwrap().iter().any(|p| p["kind"] == "shot_change" && p["strength"] == 2));
}

/// match_grade_knobs moves the right knobs in the right direction: a
/// target darker and cooler than the reference gets positive gain/lift
/// and a warm temperature; a matched pair moves nothing.
#[test]
fn match_grade_knobs_close_the_gap() {
    use klipp_control::match_grade_knobs;
    // A neutral reference: white 90, mid 50, black 5, balanced.
    let reference = klipp_control::frame_stats(&std::iter::repeat_n([230u8, 128, 12, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 0.0);
    // A darker, bluer target: everything lower, blue lifted.
    let target = klipp_control::frame_stats(&std::iter::repeat_n([120u8, 120, 170, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 1.0);
    let k = match_grade_knobs(&target, &reference);
    assert!(k.gain.unwrap() > 0.0, "target is darker at the top: raise gain, got {:?}", k.gain);
    assert!(k.temperature.unwrap() > 0.0, "target is cooler: warm it, got {:?}", k.temperature);

    // A clip matched to itself moves nothing.
    let same = match_grade_knobs(&reference, &reference);
    assert!(same.is_empty(), "no gap, no grade: {same:?}");
}

/// resolve_look picks a look by a distinctive word, takes an absolute
/// .cube path as-is, and says so when a name matches nothing.
#[test]
fn looks_resolve_by_name_and_path() {
    // A distinctive token picks exactly one look out of the library.
    let (path, name) = klipp_control::resolve_look("gyroflow").expect("gyroflow is one look");
    assert!(path.to_string_lossy().ends_with("GYROFLOW CONVERSION Legal to Full.cube"), "{path:?}");
    assert_eq!(name, "GYROFLOW CONVERSION Legal to Full");

    // A word in many looks asks the agent to be more specific.
    let ambiguous = klipp_control::resolve_look("normal");
    assert!(ambiguous.is_err(), "'normal' should match several");

    // Nothing matches → a helpful error, not a panic.
    let miss = klipp_control::resolve_look("no-such-look-zzz").unwrap_err();
    assert!(miss.contains("no look matches"), "{miss}");
}

/// suggest_selects splits speech at the real pauses and flags filler and
/// too-short runs to drop.
#[test]
fn selects_split_a_take_and_drop_the_filler() {
    let words = vec![
        (0.0f64, 0.4, "So".into()), (0.45, 0.9, "here".into()), (0.95, 1.4, "we".into()), (1.45, 1.9, "go".into()),
        // a long pause, then a filler-only run
        (3.0, 3.2, "um".into()),
        // another pause, then the real second select
        (5.0, 5.4, "the".into()), (5.45, 5.9, "point".into()), (5.95, 6.6, "stands".into()),
    ];
    let sel = klipp_control::suggest_selects(&words, 0.6, 0.35);
    assert_eq!(sel.len(), 3, "{sel:?}");
    assert!(!sel[0].filler && sel[0].text == "So here we go");
    assert!(sel[1].filler, "a lone 'um' is a drop: {:?}", sel[1]);
    assert!(!sel[2].filler && sel[2].text.contains("point stands"));
}

/// style_cut_summary reads pace from the shot lengths and finds how many
/// cuts land on the beat.
#[test]
fn style_cut_summary_reads_pace_and_beat_alignment() {
    use klipp_analysis::beats::BeatData;
    // Six 0.5 s shots → fast, cut every half second.
    let shots: Vec<(f64, f64)> = (0..6).map(|i| (i as f64 * 0.5, i as f64 * 0.5 + 0.5)).collect();
    let boundaries: Vec<f64> = (1..6).map(|i| i as f64 * 0.5).collect(); // 0.5,1.0,1.5,2.0,2.5
    // A 120 BPM grid: beats every 0.5 s — every cut lands on one.
    let beats = BeatData { bpm: 120.0, confidence: 0.9, beat_times: (0..20).map(|i| i as f64 * 0.5).collect() };
    let v = klipp_control::style_cut_summary(&shots, &boundaries, 3.0, &beats);
    assert_eq!(v["pace"], "fast");
    assert_eq!(v["shot_count"], 6);
    assert!((v["cut_on_beat_ratio"].as_f64().unwrap() - 1.0).abs() < 1e-6, "all cuts on the beat: {v}");

    // Off-beat cuts: a grid the cuts never touch → low ratio.
    let offbeat = BeatData { bpm: 120.0, confidence: 0.9, beat_times: (0..20).map(|i| i as f64 * 0.5 + 0.25).collect() };
    let v2 = klipp_control::style_cut_summary(&shots, &boundaries, 3.0, &offbeat);
    assert!(v2["cut_on_beat_ratio"].as_f64().unwrap() < 0.5, "few cuts on this grid: {v2}");
}

#[test]
fn auto_sfx_previews_without_assets_and_applies_synthesized_audio() {
    let mut h = harness();
    let cache = std::env::temp_dir().join(format!("klipp-sfx-test-{}", klipp_core::MediaId::new().0));
    let old_cache = std::env::var_os("XDG_CACHE_HOME");
    std::env::set_var("XDG_CACHE_HOME", &cache);
    {
        let mut seq = h.sequence.write().unwrap_or_else(|e| e.into_inner());
        seq.tracks.clear();
        let mut video = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        for n in 0..8 {
            let tc = |s| klipp_core::Timecode::from_secs_f64(s, 1000);
            video.clips.push(klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(n as f64 * 0.85), tc((n + 1) as f64 * 0.85), tc(0.0), tc(0.85)));
        }
        seq.tracks.push(video);
    }
    let preview = h.ok(json!({ "method": "auto_sfx" }));
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["placements"].as_array().unwrap().len(), 7);
    assert!(h.media_pool.read().unwrap_or_else(|e| e.into_inner()).all().next().is_none());
    assert!(!cache.exists(), "preview must not create audio");
    let applied = h.ok(json!({ "method": "auto_sfx", "apply": true }));
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["synthesized_whoosh"], true);
    assert_eq!(applied["placements"].as_array().unwrap().len(), 7);
    assert_eq!(h.edits_seen.lock().unwrap_or_else(|e| e.into_inner()).len(), 22);
    assert_eq!(h.media_pool.read().unwrap_or_else(|e| e.into_inner()).all().count(), 1);
    if let Some(old) = old_cache { std::env::set_var("XDG_CACHE_HOME", old); } else { std::env::remove_var("XDG_CACHE_HOME"); }
}

#[test]
fn native_generators_preview_without_edits_and_apply_as_one_undo_batch() {
    let mut h = harness();
    let data = json!({"LivingGradient": {"a": [22, 34, 78, 255], "b": [64, 24, 82, 255], "angle_deg": 20, "drift_deg_per_sec": 4, "glow": 0.35}});
    let ops = json!([{"op": "generator", "data": data, "at": {"secs": 0.0}, "secs": 1.7}]);
    let preview = h.ok(json!({"method": "plan", "ops": ops, "detail": true}));
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["steps_resolved"], 1);
    assert!(h.edits_seen.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    let apply = h.ok(json!({"method": "apply", "ops": ops}));
    assert_eq!(apply["applied"], true);
    assert_eq!(h.envelopes.lock().unwrap_or_else(|e| e.into_inner()).last(), Some(&(true, 1)));
    let edits = h.edits_seen.lock().unwrap_or_else(|e| e.into_inner());
    match &edits[0] {
        klipp_document::history::EditCommand::Batch(edits) => match &edits[1] {
            klipp_document::history::EditCommand::LegacyAppendClip { clip, .. } => {
                let klipp_timeline::ClipKind::Generator(native) = &clip.kind else { panic!("expected generator") };
                assert_eq!(*native, serde_json::from_value::<klipp_core::GeneratorData>(data).unwrap());
            }
            other => panic!("expected generator append, got {other:?}"),
        },
        other => panic!("expected bottom-track creation and generator, got {other:?}"),
    }
}

#[test]
fn media_analysis_returns_null_for_undecoded_bins_and_ignores_old_sentinel_cache() {
    let mut h = harness();
    struct CacheGuard { previous: Option<std::ffi::OsString>, path: std::path::PathBuf }
    impl Drop for CacheGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(old) => std::env::set_var("XDG_CACHE_HOME", old),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
    let cache = CacheGuard {
        previous: std::env::var_os("XDG_CACHE_HOME"),
        path: std::env::temp_dir().join(format!("klipp-motion-cache-test-{}", klipp_core::MediaId::new().0)),
    };
    std::fs::create_dir_all(&cache.path).unwrap();
    std::env::set_var("XDG_CACHE_HOME", &cache.path);
    let path = cache.path.join("synthetic.mkv");
    std::fs::write(&path, b"cache identity fixture; no decoding requested").unwrap();
    let key = klipp_analysis::store::key_for(&path).unwrap();
    let item = klipp_media::MediaItem::synthetic_for_test(path, None);
    let id = h.media_pool.write().unwrap_or_else(|e| e.into_inner()).add(item);
    for (version, bins) in [(1, json!([9.0,0.0,0.0])), (2, json!([9.0,-1.0,-1.0]))] {
        klipp_analysis::store::save(&key, "motion", version, &json!({"duration_secs":3.0,"per_second":bins,"mean":3.0,"p95":9.0})).unwrap();
        let old = h.ok(json!({"method":"media_analysis","media_id":id.0.to_string()}));
        assert!(old["motion"].is_null(), "v{version} must require re-analysis");
    }
    // One frame of 90 in the first second reads 3.0 at the reference
    // cadence: "gentle", so the measured shot cannot match the static
    // query below and only the unknown seconds are in question.
    let motion = klipp_analysis::motion::parse_metadata("frame:1 pts_time:0.5\nlavfi.signalstats.YDIF=90\n", 3.0);
    klipp_analysis::store::save(&key, "motion", klipp_analysis::motion::VERSION, &serde_json::to_value(motion).unwrap()).unwrap();
    let response = h.ok(json!({"method": "media_analysis", "media_id": id.0.to_string()}));
    let expected = 3.0_f32 as f64;
    assert!((response["motion"]["per_second"][0].as_f64().unwrap() - expected).abs() < 1e-6);
    assert!(response["motion"]["per_second"][1].is_null());
    assert!(response["motion"]["per_second"][2].is_null());
    assert!((response["motion"]["mean"].as_f64().unwrap() - expected).abs() < 1e-6);
    let shots = klipp_analysis::shots::Shots::from_boundaries(3.0, vec![1.0]);
    klipp_analysis::store::save(&key, "shots", klipp_analysis::shots::VERSION, &serde_json::to_value(shots).unwrap()).unwrap();
    let unknown = h.ok(json!({"method":"find_shots","motion":"static"}));
    assert!(!unknown["hits"].as_array().unwrap().iter().any(|v| v["media_id"] == id.0.to_string()), "unknown second must not match static");
    let measured = klipp_analysis::motion::Motion::from_samples(3.0,vec![Some(9.0),Some(0.0),Some(0.0)]);
    klipp_analysis::store::save(&key,"motion",klipp_analysis::motion::VERSION,&serde_json::to_value(measured).unwrap()).unwrap();
    let known = h.ok(json!({"method":"find_shots","motion":"static"}));
    assert!(known["hits"].as_array().unwrap().iter().any(|v|v["media_id"]==id.0.to_string() && v["start_secs"]==1.0));

}

#[test]
fn title_motion_reaches_plan_and_apply_and_invalid_styles_are_refused() {
    let mut h = harness();
    let ops = json!([{"op":"title","text":"Masked word stagger","at":{"secs":0},"secs":4,
        "enter":{"kind":"mask_rise","stagger":"word","stagger_secs":0.13,"easing":"out_quint"},
        "exit":{"kind":"mask_drop","duration_secs":0.4,"delay_secs":0.2}}]);
    let preview = h.ok(json!({"method":"plan","ops":ops,"detail":true}));
    assert_eq!(preview["applied"], false);
    assert!(h.edits_seen.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    let applied = h.ok(json!({"method":"apply","ops":ops}));
    assert_eq!(applied["applied"], true);
    fn title_motion(edit: &klipp_document::history::EditCommand) -> Option<klipp_core::title_motion::TitleMotion> {
        use klipp_document::history::EditCommand;
        match edit {
            EditCommand::Batch(edits) => edits.iter().find_map(title_motion),
            EditCommand::AddTitleClip { clip, .. } => {
                let object = clip.title_data()?.objects.first()?;
                Some(object.fx().motion)
            }
            _ => None,
        }
    }
    let edits = h.edits_seen.lock().unwrap_or_else(|e| e.into_inner());
    let motion = edits.iter().find_map(title_motion).expect("motion in delivered edit");
    assert_eq!(motion.enter.unwrap().kind, klipp_core::title_motion::MotionKind::MaskRise);
    assert_eq!(motion.enter.unwrap().stagger_secs, 0.13);
    assert_eq!(motion.exit.unwrap().kind, klipp_core::title_motion::MotionKind::MaskDrop);
    assert_eq!(motion.exit.unwrap().delay_secs, 0.2);
    drop(edits);
    let reply = h.call(json!({"method":"set_brief","brief":{"type_styles":[{"name":"bad","exit":{"kind":"fade","delay_secs":-1}}]}}));
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("delay_secs"));
}

/// #319: a frame read moves the viewer to the time it measures, then
/// puts the user back where the UI says the playhead is — not where
/// the last presented frame was, which can be minutes stale — and
/// plays on if it was playing.
#[test]
fn a_frame_read_puts_the_users_playhead_back_and_plays_on() {
    use klipp_core::{AppCommand, Timecode, TransportStatus};

    let mut h = harness();
    let rate = h.sequence.read().unwrap().fps.rate();
    let user_at = Timecode::from_secs_f64(5.0, rate);
    *h.shared.transport.lock().unwrap() = TransportStatus { playhead: Some(user_at), playing: true };
    h.frame_buffer.lock().unwrap().set_presented(Timecode::from_secs_f64(176.9, rate), true);

    // A stand-in engine: every seek presents a frame at that time.
    let seen: Arc<Mutex<Vec<AppCommand>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let (rx, fb, seen) = (h.cmd_rx.clone(), Arc::clone(&h.frame_buffer), Arc::clone(&seen));
        std::thread::spawn(move || {
            for c in rx {
                if let AppCommand::SeekTo(t) = &c {
                    let mut fb = fb.lock().unwrap();
                    fb.set_presented(*t, true);
                    fb.write_frame(vec![128; 64 * 32 * 4], 64, 32);
                }
                seen.lock().unwrap().push(c);
            }
        });
    }

    h.ok(json!({ "method": "frame_stats", "secs": 2.0 }));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while seen.lock().unwrap().len() < 4 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let seen = seen.lock().unwrap();
    let secs = |c: &AppCommand| match c { AppCommand::SeekTo(t) => Some(t.to_secs_f64()), _ => None };
    assert!(matches!(seen.first(), Some(AppCommand::Pause)), "{seen:?}");
    assert!((secs(&seen[1]).unwrap() - 2.0).abs() < 0.05, "measured at the wrong time: {seen:?}");
    assert!(
        (secs(&seen[2]).expect("a seek back after the read") - 5.0).abs() < 0.05,
        "the playhead went back to the stale presented frame, not the user's: {seen:?}",
    );
    assert!(matches!(seen.get(3), Some(AppCommand::Play)), "playback was not resumed: {seen:?}");
}

/// #344: a frame presented at the right time whose bytes are an empty
/// buffer is not measured as a black frame; the read waits for a
/// picture and, when none comes, says so.
#[test]
fn a_frame_read_does_not_measure_an_empty_buffer() {
    use klipp_core::AppCommand;

    let mut h = harness();
    {
        let (rx, fb) = (h.cmd_rx.clone(), Arc::clone(&h.frame_buffer));
        std::thread::spawn(move || {
            for c in rx {
                if let AppCommand::SeekTo(t) = &c {
                    let mut fb = fb.lock().unwrap();
                    fb.set_presented(*t, true);
                    fb.write_frame(vec![0; 64 * 32 * 4], 64, 32);
                }
            }
        });
    }
    let reply = h.call(json!({ "method": "frame_stats", "secs": 1.0, "timeout_ms": 300 }));
    assert_eq!(reply["ok"], json!(false), "an empty buffer was measured: {reply}");
    assert!(reply["error"].as_str().unwrap().contains("empty"), "{}", reply["error"]);
}

/// #458: one look on many clips is one batch — one envelope, one undo
/// step — and a batch that mixes looks with other writes is refused
/// rather than applied as two steps.
#[test]
fn looks_batch_into_one_step() {
    let mut h = harness();
    let media = h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/look-batch.mov".into(), None));
    {
        let mut seq = h.sequence.write().unwrap();
        for c in seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            c.kind = klipp_timeline::ClipKind::Media { media_id: media };
        }
    }
    let cube = std::env::temp_dir().join(format!("klipp-look-batch-{}.cube", std::process::id()));
    std::fs::write(&cube, "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").unwrap();
    let path = cube.to_string_lossy().to_string();
    let (a, b) = (h.ids.clip_a.clone(), h.ids.clip_b.clone());

    let sent = h.envelopes.lock().unwrap().len();
    let r = h.ok(json!({ "method": "batch", "commands": [
        { "method": "look", "clip_id": a, "name": path },
        { "method": "look", "clip_id": b, "name": path },
    ]}));
    assert_eq!(r["applied"], json!(true), "{r}");
    let envs = h.envelopes.lock().unwrap().clone();
    assert_eq!(envs.len(), sent + 1, "one envelope");
    assert_eq!(envs.last(), Some(&(true, 2)), "both looks in it");

    let sent = envs.len();
    let reply = h.call(json!({ "method": "batch", "commands": [
        { "method": "look", "clip_id": a, "name": path },
        { "method": "play" },
    ]}));
    assert_eq!(reply["ok"], json!(false));
    assert!(reply["error"].as_str().unwrap().contains("only looks"), "{}", reply["error"]);
    assert_eq!(h.envelopes.lock().unwrap().len(), sent, "a refused batch sends nothing");
    let _ = std::fs::remove_file(&cube);
}
