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

struct Harness {
    stream: UnixStream,
    frame_buffer: Arc<Mutex<klipp_engine::FrameBuffer>>,
    reader: BufReader<UnixStream>,
    cmd_rx: crossbeam_channel::Receiver<klipp_core::AppCommand>,
    _log: std::path::PathBuf,
    next_id: u64,
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

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    // A viewer frame with a recognisable first pixel, so the screenshot
    // is checked for content and not merely for existing.
    let mut fb = klipp_engine::FrameBuffer::new(64, 32);
    fb.data[0] = 10;
    fb.data[1] = 20;
    fb.data[2] = 30;
    fb.data[3] = 255;

    let frame_buffer = Arc::new(Mutex::new(fb));
    let ctx = ControlContext {
        commands: cmd_tx,
        wake_ui: Arc::new(|| {}),
        sequence: Arc::new(RwLock::new(klipp_timeline::Sequence::new_default())),
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
                return Harness { stream, frame_buffer, reader, cmd_rx, _log: log, next_id: 0 };
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    panic!("could not connect to {}: {last_err:?}", sock.display());
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

    let node = uuid::Uuid::new_v4().to_string();
    let clip = uuid::Uuid::new_v4().to_string();
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
