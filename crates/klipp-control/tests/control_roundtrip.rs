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
    shared: Arc<klipp_core::ControlShared>,
    _log: std::path::PathBuf,
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
    let sock = std::env::temp_dir().join(format!("klipp-control-test-{stamp}.sock"));
    let log = std::env::temp_dir().join(format!("klipp-control-test-{stamp}.log"));
    std::fs::write(&log, "line one\nline two\nline three\n").expect("write log");

    // The server reads this to decide where to bind.
    std::env::set_var("KLIPP_CONTROL_SOCKET", &sock);
    std::env::remove_var("KLIPP_CONTROL");

    // A stand-in for the UI's inbox: takes each envelope, forwards its
    // commands one by one for the assertions below, advances the
    // revision as a recorded edit would, and acknowledges.
    let (env_tx, env_rx) = crossbeam_channel::unbounded::<klipp_core::ControlEnvelope>();
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<klipp_core::AppCommand>();
    let envelopes: Arc<Mutex<Vec<(bool, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let shared = Arc::new(klipp_core::ControlShared::default());
    {
        let (envelopes, shared) = (Arc::clone(&envelopes), Arc::clone(&shared));
        std::thread::spawn(move || {
            for env in env_rx {
                envelopes.lock().unwrap().push((env.batch, env.commands.len()));
                for c in env.commands {
                    let _ = cmd_tx.send(c);
                }
                let revision = shared.bump_revision();
                if let Some(ack) = env.ack {
                    let _ = ack.send(klipp_core::ControlAck { revision });
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
    let frame_buffer = Arc::new(Mutex::new(fb));
    let ctx = ControlContext {
        commands: env_tx,
        shared: Arc::clone(&shared),
        wake_ui: Arc::new(|| {}),
        sequence: Arc::new(RwLock::new(sequence)),
        media_pool: Arc::new(RwLock::new(klipp_media::MediaPool::new())),
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
                return Harness { _serial: serial, stream, frame_buffer, reader, cmd_rx, envelopes, shared, _log: log, next_id: 0, ids };
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
    });
    let st = h.ok(json!({ "method": "export_status" }));
    assert_eq!(st["jobs"][0]["status"], json!("rendering"));
    assert_eq!(st["jobs"][0]["progress"], json!(0.5));
}
