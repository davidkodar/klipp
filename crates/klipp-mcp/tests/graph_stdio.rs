//! Graph calls retain their vocabulary and indexed failures through stdio.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use serde_json::{Value, json};

#[test]
fn graph_tools_forward_paths_and_return_indexed_refusals_as_errors() {
    let socket = std::path::PathBuf::from(format!("/tmp/klipp-graph-mcp-{}.sock", uuid::Uuid::new_v4()));
    let listener = UnixListener::bind(&socket).unwrap();
    let (sent, received) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for (index, connection) in listener.incoming().take(3).enumerate() {
            let mut stream = connection.unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            sent.send(request.clone()).unwrap();
            let reply = if index == 2 {
                json!({"id":request["id"],"ok":true,"result":{"accepted":true,"applied":true,"revision":18}})
            } else if request["method"] == "graph_ops" {
                json!({"id":request["id"],"ok":false,"op_index":1,"error":"mix cannot be saved yet","result":{"accepted":false,"applied":false,"op_index":1,"why":"mix cannot be saved yet"}})
            } else { json!({"id":request["id"],"ok":true,"result":{"nodes":[],"edges":[],"revision":17}}) };
            writeln!(stream,"{reply}").unwrap();
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_klipp-mcp")).env("KLIPP_CONTROL_SOCKET", &socket)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let (reply_tx, reply_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            reply_tx.send(serde_json::from_str::<Value>(&line).unwrap()).unwrap();
        }
    });
    let path = json!({"root":{"clip":uuid::Uuid::nil()},"nodes":[]});
    let ops = json!([{"op":"add","after":path,"type":"klipp.fx.gaussian_blur"},{"op":"mix","path":{"root":{"clip":uuid::Uuid::nil()},"nodes":[{"new":0}]},"mix":0.75}]);
    // Transport coverage uses an applied socket reply; real model acceptance
    // is covered by control_roundtrip's picture-truth-gated tests.
    let effect = json!({"root":{"clip":uuid::Uuid::nil()},"nodes":[uuid::Uuid::from_u128(1)]});
    let mask = json!({"root":{"clip":uuid::Uuid::nil()},"nodes":[uuid::Uuid::from_u128(2)]});
    let effect_ops = json!([
        {"op":"mix","path":effect,"mix":0.25},
        {"op":"mask_channel","path":effect,"channel":"alpha"},
        {"op":"connect","from":mask,"from_port":"matte","to":effect,"to_port":"mask"}
    ]);
    for (id, name, arguments) in [
        (1,"klipp_graph_read",json!({"path":path,"depth":2})),
        (2,"klipp_graph",json!({"ops":ops,"expected_revision":17,"expected_project_identity":8,"idempotency_key":"graph-retry"})),
        (3,"klipp_graph",json!({"ops":effect_ops,"expected_revision":17})),
    ] {
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})).unwrap();
        input.flush().unwrap();
        let request = received.recv_timeout(Duration::from_secs(5)).unwrap();
        let reply = reply_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if id == 1 {
            assert_eq!(request["method"],"graph_read");
            assert_eq!(request["path"],path);
            assert_eq!(request["depth"],2);
        } else if id == 2 {
            assert_eq!(request["method"],"graph_ops");
            assert_eq!(request["ops"],ops);
            assert_eq!(request["expected_revision"],17);
            assert_eq!(request["expected_project_identity"],8);
            assert_eq!(request["idempotency_key"],"graph-retry");
            assert_eq!(reply["result"]["isError"],true);
            let error: Value = serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(error["op_index"],1);
            assert_eq!(error["applied"],false);
        } else {
            assert_eq!(request["method"],"graph_ops");
            assert_eq!(request["ops"],effect_ops);
            assert_ne!(reply["result"]["isError"],true);
            let applied: Value = serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(applied["applied"],true);
            assert_eq!(applied["revision"],18);
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    server.join().unwrap();
    std::fs::remove_file(socket).unwrap();
}
