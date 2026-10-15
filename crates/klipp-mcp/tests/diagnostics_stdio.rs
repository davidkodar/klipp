//! The catalog and a live status identify the same serving MCP process.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::Duration;
use serde_json::{json, Value};

#[test]
fn advertised_catalog_is_verified_against_the_serving_process() {
    let socket = std::path::PathBuf::from(format!("/tmp/klipp-diag-{}.sock", uuid::Uuid::new_v4()));
    let listener = UnixListener::bind(&socket).unwrap();
    let server = std::thread::spawn(move || {
        for connection in listener.incoming().take(2) {
            let mut stream = connection.unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "status");
            let reply = json!({"id":request["id"],"ok":true,"result":{
                "text":"Klipp is running.", "revision":12,
                "build":{"version":"0.1.0","commit":"different-app-build"}}});
            writeln!(stream, "{reply}").unwrap();
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_klipp-mcp")).env("KLIPP_CONTROL_SOCKET", &socket)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let output = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            tx.send(serde_json::from_str::<Value>(&line).unwrap()).unwrap();
        }
    });
    let mut call = |id, method: &str, params: Value| {
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).unwrap();
        input.flush().unwrap();
        let reply = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(reply["id"], id);
        reply["result"].clone()
    };
    let init = call(1, "initialize", json!({"protocolVersion":"2025-06-18"}));
    let catalog = call(2, "tools/list", json!({}));
    assert_eq!(init["_meta"]["catalog_fingerprint"], catalog["_meta"]["catalog_fingerprint"]);
    let expected = catalog["_meta"]["catalog_fingerprint"].clone();
    let status = call(3, "tools/call", json!({"name":"klipp_status","arguments":{"expected_catalog_fingerprint":expected}}));
    let raw = &status["structuredContent"];
    assert_eq!(raw["revision"], 12);
    assert_eq!(raw["mcp"]["mcp_build"], catalog["_meta"]["mcp_build"]);
    assert_eq!(raw["mcp"]["catalog_fingerprint"], expected);
    assert_eq!(raw["mcp"]["tool_count"], catalog["tools"].as_array().unwrap().len());
    assert!(raw["text"].as_str().unwrap().contains("capabilities may differ"));
    let stale = call(4, "tools/call", json!({"name":"klipp_status","arguments":{"expected_catalog_fingerprint":"stale"}}));
    assert!(stale["structuredContent"]["text"].as_str().unwrap().contains("catalog differs"));
    let unknown = call(5, "tools/call", json!({"name":"klipp_missing_from_cached_catalog","arguments":{}}));
    assert_eq!(unknown["isError"], true);
    assert!(unknown["content"][0]["text"].as_str().unwrap().contains("Reconnect"));
    drop(input);
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    server.join().unwrap();
    std::fs::remove_file(socket).unwrap();
}
