use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn socket_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("klipp-mcp-test-{}-{}.sock", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
}

#[test]
fn status_and_ping_respond_while_analysis_waits_on_the_same_session() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        loop {
            if stop_rx.try_recv().is_ok() { break; }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    std::thread::spawn(move || {
                        let mut line = String::new();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        reader.read_line(&mut line).unwrap();
                        let request: Value = serde_json::from_str(&line).unwrap();
                        if request["method"] == "analysis_status" { std::thread::sleep(Duration::from_millis(1500)); }
                        let result = if request["method"] == "status" {
                            json!({"text":"Klipp is ready.","revision":1})
                        } else { json!({"items":[],"finished":true}) };
                        let _ = writeln!(stream, "{}", json!({"id":request["id"],"ok":true,"result":result}));
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("socket accept failed: {error}"),
            }
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_klipp-mcp"))
        .env("KLIPP_CONTROL_SOCKET", &path)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (reply_tx, reply_rx) = mpsc::channel::<Value>();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str(&line) { let _ = reply_tx.send(value); }
        }
    });
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"klipp_analysis_status","arguments":{"wait":true}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"ping"}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"klipp_status","arguments":{}}}),
    ] { writeln!(input, "{request}").unwrap(); }
    input.flush().unwrap();
    let started = Instant::now();
    let mut prompt_ids = Vec::new();
    while prompt_ids.len() < 2 {
        let reply = reply_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        if reply["id"] == 3 || reply["id"] == 4 { prompt_ids.push(reply["id"].as_u64().unwrap()); }
    }
    assert!(started.elapsed() < Duration::from_millis(900), "ping and status were blocked by analysis");
    assert!(prompt_ids.contains(&3) && prompt_ids.contains(&4));
    let mut samples = Vec::new();
    for id in 10..30 {
        let sent = Instant::now();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":"klipp_status","arguments":{}}})).unwrap();
        input.flush().unwrap();
        loop {
            let reply = reply_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            if reply["id"] == id { break; }
        }
        samples.push(sent.elapsed());
    }
    samples.sort();
    assert!(samples[18] < Duration::from_millis(300), "status p95 exceeded 300 ms: {:?}", samples[18]);
    child.kill().unwrap();
    let _ = child.wait();
    drop(input);
    reader.join().unwrap();
    stop_tx.send(()).unwrap();
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn stalled_write_has_bounded_unknown_outcome_and_replay_key() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let (key_tx, key_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        key_tx.send(request["idempotency_key"].as_str().unwrap().to_owned()).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let _ = writeln!(stream, "{}", json!({"id":1,"ok":true,"result":{"applied":true}}));
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_klipp-mcp"))
        .env("KLIPP_CONTROL_SOCKET", &path).env("KLIPP_MCP_SOCKET_TIMEOUT_MS", "250")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let request = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{
        "name":"klipp_apply","arguments":{"ops":[{"op":"cut","from":{"frame":1},"to":{"frame":2}}]}}});
    let started = Instant::now();
    writeln!(input, "{request}").unwrap();
    input.flush().unwrap();
    let key = key_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (reply_tx, reply_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(output).read_line(&mut line).unwrap();
        reply_tx.send(serde_json::from_str::<Value>(&line).unwrap()).unwrap();
    });
    let reply = reply_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"]["isError"], true);
    let message = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("outcome is unknown") && message.contains(&key), "{message}");
    child.kill().unwrap();
    let _ = child.wait();
    drop(input);
    reader.join().unwrap();
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn concurrent_edit_requests_reach_the_socket_in_input_order() {
    use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
    let path = socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let server_active = Arc::clone(&active);
    let server_maximum = Arc::clone(&maximum);
    let server = std::thread::spawn(move || {
        let mut workers = Vec::new();
        for index in 0..22 {
            let (mut stream, _) = listener.accept().unwrap();
            let active = Arc::clone(&server_active);
            let maximum = Arc::clone(&server_maximum);
            workers.push(std::thread::spawn(move || {
                let count = active.fetch_add(1, Ordering::AcqRel) + 1;
                maximum.fetch_max(count, Ordering::AcqRel);
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], "apply");
                std::thread::sleep(Duration::from_millis(if index < 2 { 200 } else { 5 }));
                let _ = writeln!(stream, "{}", json!({"id":request["id"],"ok":true,"result":{
                    "text":"cut\n","applied":true,"revision":index+1,"operation_id":format!("op-{index}")}}));
                active.fetch_sub(1, Ordering::AcqRel);
            }));
        }
        for worker in workers { worker.join().unwrap(); }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_klipp-mcp"))
        .env("KLIPP_CONTROL_SOCKET", &path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    for id in [1, 2] {
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"klipp_apply","arguments":{"ops":[{"op":"cut","from":{"frame":1},"to":{"frame":2}}]}}})).unwrap();
    }
    input.flush().unwrap();
    for id in [1, 2] {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["isError"], false);
    }
    let mut acknowledgements = Vec::new();
    for id in 3..23 {
        let sent = Instant::now();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"klipp_apply","arguments":{"ops":[{"op":"cut","from":{"frame":1},"to":{"frame":2}}]}}})).unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        acknowledgements.push(sent.elapsed());
    }
    acknowledgements.sort();
    assert!(acknowledgements[18] < Duration::from_millis(250),
        "edit acknowledgement p95 exceeded 250 ms: {:?}", acknowledgements[18]);
    assert_eq!(maximum.load(Ordering::Acquire), 1, "writes overlapped at the socket");
    child.kill().unwrap();
    let _ = child.wait();
    drop(input);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn following_a_render_emits_progress_and_the_terminal_event() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || {
        for (index, progress) in [0.2, 0.7, 1.0].into_iter().enumerate() {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "job_status");
            let state = if index == 2 { "completed" } else { "running" };
            let _ = writeln!(stream, "{}", json!({"id":request["id"],"ok":true,"result":{"jobs":[{
                "job_id":"render-1","kind":"render","state":state,"phase":"encoding",
                "progress":progress,"artifact":if index == 2 {Some("/tmp/render.mp4")} else {None}
            }]}}));
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_klipp-mcp"))
        .env("KLIPP_CONTROL_SOCKET", &path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{
        "name":"klipp_follow_job","arguments":{"job_id":"render-1","max_wait_ms":3000},
        "_meta":{"progressToken":"render-progress"}}})).unwrap();
    input.flush().unwrap();
    let mut notifications = Vec::new();
    loop {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["method"] == "notifications/progress" { notifications.push(value); }
        else if value["id"] == 5 {
            assert_eq!(value["result"]["isError"], false);
            assert!(value["result"]["content"][0]["text"].as_str().unwrap().contains("completed"));
            break;
        }
    }
    assert!(notifications.len() >= 2, "initial and terminal progress must be visible");
    assert!(notifications.iter().all(|note| note["params"]["progressToken"] == "render-progress"));
    assert_eq!(notifications.last().unwrap()["params"]["progress"], 100);
    child.kill().unwrap();
    let _ = child.wait();
    drop(input);
    server.join().unwrap();
    std::fs::remove_file(path).unwrap();
}
