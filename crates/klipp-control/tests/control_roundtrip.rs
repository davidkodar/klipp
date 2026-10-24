//! End-to-end exercise of the control socket.
//!
//! Builds a `ControlContext` over synthetic state, starts the real
//! server on a temp socket, and drives it the way `klipp-mcp` does — a
//! JSON line in, a JSON line out. Covers both halves that can go wrong
//! independently: reads must reflect the shared state, and writes must
//! actually reach the `AppCommand` channel.

use klipp_timeline::GradeAccess;
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
    /// Every command the stand-in UI took, in order.
    sent: Arc<Mutex<Vec<klipp_core::AppCommand>>>,
    edits_seen: Arc<Mutex<Vec<klipp_document::history::EditCommand>>>,
    /// The other sequences the socket sees, and every envelope's sources.
    others: klipp_document::ops::SharedSelects,
    sources_seen: Arc<Mutex<Vec<(klipp_core::SequenceId, u64)>>>,
    shared: Arc<klipp_core::ControlShared>,
    media_pool: Arc<RwLock<klipp_media::MediaPool>>,
    sequence: Arc<klipp_timeline::SharedSequence>,
    log: std::path::PathBuf,
    socket: std::path::PathBuf,
    next_id: u64,
    ack_delay_ms: Arc<std::sync::atomic::AtomicU64>,
    /// A picture write the stand-in UI says it refused, in its next ack.
    picture_refusal: Arc<Mutex<Option<String>>>,
    commit_change: Arc<std::sync::atomic::AtomicU8>,
    project_switch_delay_ms: Arc<std::sync::atomic::AtomicU64>,
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
    let sent: Arc<Mutex<Vec<klipp_core::AppCommand>>> = Arc::new(Mutex::new(Vec::new()));
    let edits_seen: Arc<Mutex<Vec<klipp_document::history::EditCommand>>> = Arc::new(Mutex::new(Vec::new()));
    let ack_delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let picture_refusal: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let commit_change = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let project_switch_delay_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let shared = Arc::new(klipp_core::ControlShared::default());
    let media_pool = Arc::new(RwLock::new(klipp_media::MediaPool::new()));
    let (sequence, ids) = sequence_with_two_clips();
    let sequence = Arc::new(klipp_timeline::SharedSequence::new(sequence));
    let others = klipp_document::ops::SharedSelects::default();
    let sources_seen = Arc::new(Mutex::new(Vec::new()));
    {
        let (envelopes, shared, edits_seen) = (Arc::clone(&envelopes), Arc::clone(&shared), Arc::clone(&edits_seen));
        let sources_seen = Arc::clone(&sources_seen);
        let sent = Arc::clone(&sent);
        let media_pool = media_pool.clone();
        let delay = Arc::clone(&ack_delay_ms);
        let picture_refused_next = Arc::clone(&picture_refusal);
        let sequence = Arc::clone(&sequence);
        let commit_change = Arc::clone(&commit_change);
        let project_switch_delay = Arc::clone(&project_switch_delay_ms);
        std::thread::spawn(move || {
            for env in env_rx {
                let ms = delay.load(std::sync::atomic::Ordering::Relaxed);
                if ms > 0 { std::thread::sleep(std::time::Duration::from_millis(ms)); }
                envelopes.lock().unwrap().push((env.batch, env.commands.len() + env.edits.len()));
                sources_seen.lock().unwrap().extend(env.expected_sources.iter().copied());
                // Inject a competing change after socket preflight, before
                // this stand-in app thread checks the envelope guards.
                match commit_change.swap(0, std::sync::atomic::Ordering::SeqCst) {
                    1 => { shared.bump_revision(); }
                    2 => { shared.project.lock().unwrap().identity += 1; }
                    _ => {}
                }
                // A plan made for another revision is refused, as the UI does.
                let mut refused_op = None;
                let mut ungated = Vec::new();
                let mut renamed = Vec::new();
                let mut refused = if let Some(want) = env.expected_project_identity {
                    if shared.project.lock().unwrap().identity != want {
                        Some(format!("project identity moved from {want}"))
                    } else {
                        None
                    }
                } else {
                    None
                }.or_else(|| match env.expected_revision {
                    Some(r) if r != shared.revision() => Some(format!("revision moved from {r}")),
                    _ => None,
                });
                if refused.is_none() {
                    for c in env.commands {
                        sent.lock().unwrap().push(c.clone());
                        if let klipp_core::AppCommand::GraphOps { ops, .. } = &c {
                            // Stand-in app thread: use the production clip adapter;
                            // the UI's own tests cover envelope exclusivity/undo dispatch.
                            let mut seq = sequence.write().unwrap();
                            let result = (|| {
                                let root = ops.first().ok_or((None, "empty batch".to_owned()))?.path().root;
                                // The sequence's own pictures commit through
                                // the same sequence adapters the UI uses (#1911).
                                let clip = match root {
                                    klipp_graph_format::path::Root::Clip { clip } => clip,
                                    klipp_graph_format::path::Root::Track { track, part: klipp_graph_format::path::TrackPart::Effects } => {
                                        let t = seq.tracks.iter().find(|t| t.id.0 == track).ok_or((None, "no track".to_owned()))?;
                                        let (id, old) = (t.id, t.settings());
                                        let (_, effects, written) = seq.apply_track_effect_ops(ops).map_err(|e| (None, e))?;
                                        renamed = written;
                                        let new = klipp_timeline::TrackSettings { effects, ..old.clone() };
                                        edits_seen.lock().unwrap().push(klipp_document::history::EditCommand::SetTrackSettings { changes: vec![(id, old, new)] });
                                        return Ok(());
                                    }
                                    _ => {
                                        let (old, new) = seq.apply_root_grade_ops(ops, None).map_err(|e| (None, e))?;
                                        edits_seen.lock().unwrap().push(klipp_document::history::EditCommand::SetRootGrade { description: "Graph Edit", root, old: old.map(Box::new), new: Some(Box::new(new)) });
                                        return Ok(());
                                    }
                                };
                                let clip = seq.tracks.iter_mut().flat_map(|t| &mut t.clips).find(|c| c.id.0 == clip).ok_or((None, "no clip".to_owned()))?;
                                let before = serde_json::to_value(&clip.picture_v2().unwrap().picture).unwrap();
                                match clip.apply_picture_ops_indexed(ops) {
                                    Ok(edit) => {
                                        let after = serde_json::to_value(&clip.picture_v2().unwrap().picture).unwrap();
                                        ungated = klipp_graph_lift::edit_planner::ungated_consumers(&klipp_graph_lift::ops::GraphEdit { root, before, after }, &klipp_timeline::RegistryCatalog).unwrap();
                                        edits_seen.lock().unwrap().push(klipp_document::history::EditCommand::PictureEdit { clip_id: clip.id, description: "Graph Edit", edit: Box::new(edit) });
                                        Ok(())
                                    }
                                    Err(error) => Err(error),
                                }
                            })();
                            // Match control_bridge: the index travels only in
                            // refused_op; prose must not name the expanded op.
                            if let Err((at, why)) = result { refused_op = at; refused = Some(why); }
                        }
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

                        if let klipp_core::AppCommand::ImportTimeline { path, request_id } | klipp_core::AppCommand::OpenFromAnotherEditor { path, request_id } = &c {
                            let pool = media_pool.read().unwrap();
                            let result = klipp_project::import_report::load(path, klipp_core::Fps::film_24(), &pool);
                            let outcome = match result {
                                Ok(result) => serde_json::json!({"request_id":request_id,"report":klipp_project::import_report::ImportReport::from_import(path, &result)}),
                                Err(error) => serde_json::json!({"request_id":request_id,"error":error}),
                            };
                            *shared.import_outcome.lock().unwrap() = Some(outcome);
                        }

                        let switching = matches!(&c, klipp_core::AppCommand::OpenProjectNow { .. } | klipp_core::AppCommand::CloseProjectNow { .. });
                        let delay = project_switch_delay.load(std::sync::atomic::Ordering::SeqCst);
                        if switching && delay > 0 {
                            {
                                let mut p = shared.project.lock().unwrap();
                                // A successful save, followed by an unrelated
                                // event, while the replacement is still pending.
                                p.generation += 2;
                                p.dirty = false;
                                p.last_error = None;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(delay));
                        }

                        // A save or an open reports back as the UI does: the
                        // path, no unsaved changes, the generation moved. A
                        // path with "broken" in it fails, to see that said.
                        if let klipp_core::AppCommand::SaveProjectAs { path } | klipp_core::AppCommand::OpenProjectNow { path, .. } = &c {
                            let mut p = shared.project.lock().unwrap();
                            p.generation += 1;
                            if path.to_string_lossy().contains("broken") {
                                p.last_error = Some(if matches!(&c, klipp_core::AppCommand::OpenProjectNow { .. }) { "could not open: late load failed" } else { "could not save: the disk said no" }.into());
                            } else {
                                p.path = Some(path.clone());
                                p.dirty = false;
                                p.last_error = None;
                            }
                        }
                        if let klipp_core::AppCommand::CloseProjectNow { save, discard_changes, expected_identity } = &c {
                            let mut p = shared.project.lock().unwrap();
                            let decision = p.replacement_save_path(*expected_identity,*save,*discard_changes);
                            p.generation += 1;
                            p.last_error = match decision {
                                Err(e) => Some(e),
                                Ok(Some(path)) if path.to_string_lossy().contains("broken") => Some("could not save: the disk said no".into()),
                                Ok(_) => { p.path=None; p.dirty=false; p.identity+=1; None }
                            };
                        }
                        if switching { refused = shared.project.lock().unwrap().last_error.clone(); }
                        let _ = cmd_tx.send(c);
                    }
                    for e in env.edits {
                        edits_seen.lock().unwrap().push(e);
                    }
                }
                let revision = if refused.is_none() { shared.bump_revision() } else { shared.revision() };
                if let Some(ack) = env.ack {
                    let picture_refused = picture_refused_next.lock().unwrap().take();
                    let _ = ack.send(klipp_document::control::ControlAck { revision, refused, refused_op, renamed, ungated, picture_refused });
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
        others: Arc::clone(&others),
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
                return Harness { _serial: serial, stream, frame_buffer, reader, cmd_rx, envelopes, sent, edits_seen, others, sources_seen, shared, media_pool, sequence, log, socket: sock, next_id: 0, ack_delay_ms, picture_refusal, commit_change, project_switch_delay_ms, ids };
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
        c.set_grade(graph);
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

#[test]
fn graph_batches_cross_the_socket_once_and_name_created_nodes() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let after = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let new = json!({"root":root,"nodes":[group,{"new":0}]});
    let before = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    let reply = h.ok(json!({"method":"graph_ops","expected_revision":graph["revision"],"expected_project_identity":graph["project_identity"],"ops":[
        {"op":"add","after":after,"type":"klipp.grade.exposure","params":{"exposure":0.5}},
        {"op":"set","path":new,"key":"exposure","value":1.25},
        {"op":"enable","path":new,"enabled":false}
    ]}));
    assert_eq!(reply["applied"], true);
    assert_eq!(reply["revision"].as_u64().unwrap(), graph["revision"].as_u64().unwrap() + 1);
    let path = reply["created"][0]["path"].clone();
    let read = h.ok(json!({"method":"graph_read","path":path}));
    assert_eq!(read["nodes"][0]["type"], "klipp.grade.exposure");
    assert_eq!(read["nodes"][0]["params"]["exposure"], 1.25);
    assert_eq!(read["nodes"][0]["enabled"], false);
    assert_eq!(h.edits_seen.lock().unwrap().len(), 1);
    let edit = h.edits_seen.lock().unwrap()[0].clone();
    let klipp_document::history::EditCommand::PictureEdit { edit, .. } = edit else { panic!("one picture edit") };
    let mut clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    let after = clip.picture_v2().unwrap().picture.clone();
    clip.restore_picture(&edit.before);
    assert_eq!(clip.picture_v2().unwrap().picture, before.picture_v2().unwrap().picture);
    clip.restore_picture(&edit.after);
    assert_eq!(clip.picture_v2().unwrap().picture, after);
}

#[test]
fn graph_add_beside_connects_a_matte_in_one_undoable_batch() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let at = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let new = json!({"root":root,"nodes":[group,{"new":0}]});
    let catalog = h.ok(json!({"method":"node_types","category":"Grade"}));
    let mask = catalog["types"].as_array().unwrap().iter().find(|t| t["tag"] == "Mask").unwrap();
    let port = mask["outputs"][0]["key"].clone();
    let shapes = json!({"masks":[{"shape":{"Rectangle":{"center_x":0.5,"center_y":0.5,"width":0.5,"height":0.5,"rotation":0}},"feather":0,"opacity":1,"invert":false,"enabled":true}]});
    assert!(mask["params"].as_array().unwrap().iter().any(|p| p["key"] == "masks" && p["value_format"].as_str().is_some_and(|s| s.contains("Rectangle"))));
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let result = h.ok(json!({"method":"graph_ops","ops":[
        {"op":"add_beside","at":at,"type":mask["id"],"label":"Window","params":{"masks":shapes}},
        {"op":"connect","from":new,"from_port":port,"to":at,"to_port":"mask"}
    ]}));
    assert_eq!(result["applied"], true);
    assert_eq!(result["revision"].as_u64().unwrap(), graph["revision"].as_u64().unwrap() + 1);
    let path = result["created"][0]["path"].clone();
    let made = h.ok(json!({"method":"graph_read","path":path}));
    assert_eq!(made["nodes"][0]["type"], mask["id"]);
    assert_eq!(made["nodes"][0]["label"], "Window");
    let stored = serde_json::to_value(serde_json::from_value::<klipp_core::MaskStack>(shapes).unwrap()).unwrap();
    assert_eq!(made["nodes"][0]["params"]["masks"], stored);
    let body = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[group]}}));
    assert!(body["edges"].as_array().unwrap().iter().any(|e| e["from"] == made["nodes"][0]["id"] && e["from_port"] == port && e["to"] == h.ids.corrector_a && e["to_port"] == "mask"));
    assert_eq!(h.edits_seen.lock().unwrap().len(), 1);
    let edit = h.edits_seen.lock().unwrap()[0].clone();
    let klipp_document::history::EditCommand::PictureEdit { edit, .. } = edit else { panic!("one picture edit") };
    let mut clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    let after = clip.picture_v2().unwrap().picture.clone();
    clip.restore_picture(&edit.before);
    assert_eq!(clip.picture_v2().unwrap().picture, before);
    clip.restore_picture(&edit.after);
    assert_eq!(clip.picture_v2().unwrap().picture, after);
}

/// The batch #1905 saw, inside the clip's grade (a group's body): a
/// corrector after the grade's corrector, a mask beside the new one, wired
/// into its mask. Each new node lands on a place of its own, clear of
/// every other placed card; Source stays put; the new corrector sits right of the old
/// on its row, the mask below it; and undoing the one edit puts every
/// node back, the ones moved along too.
#[test]
fn graph_adds_give_each_new_node_a_clear_place_undone_in_one_step() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let at = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let (first, second) = (json!({"root":root,"nodes":[group,{"new":0}]}), json!({"root":root,"nodes":[group,{"new":1}]}));
    let grade = h.sequence.read().unwrap().tracks[0].clips[0].grade().clone();
    assert!(!grade.node_positions.is_empty(), "the grade is laid out");
    let source = grade.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).unwrap().id;
    let result = h.ok(json!({"method":"graph_ops","ops":[
        {"op":"add","after":at,"type":"klipp.grade.corrector"},
        {"op":"add_beside","at":first,"type":"klipp.grade.mask"},
        {"op":"connect","from":second,"from_port":"out","to":first,"to_port":"mask"}
    ]}));
    assert_eq!(result["applied"], true, "{result}");
    let id = |i: usize| uuid::Uuid::parse_str(result["created"][i]["path"]["nodes"][1].as_str().unwrap()).unwrap();
    let (c2, mask) = (id(0), id(1));
    let after = h.sequence.read().unwrap().tracks[0].clips[0].grade().clone();
    assert!(after.node_positions.contains_key(&c2) && after.node_positions.contains_key(&mask), "the new nodes are placed");
    // Every node with a place (the fixture's compound has none).
    let spots: Vec<_> = after.nodes.iter().filter_map(|n| after.node_positions.get(&n.id).map(|p| (n.id, *p))).collect();
    for (i, (a, pa)) in spots.iter().enumerate() {
        for (b, pb) in &spots[i + 1..] {
            assert!((pa.x - pb.x).abs() >= 140.0 || (pa.y - pb.y).abs() >= 80.0, "{a} at {pa:?} covers {b} at {pb:?}");
        }
    }
    assert_eq!(after.node_positions.get(&source), grade.node_positions.get(&source), "Source stays");
    let corrector = uuid::Uuid::parse_str(&h.ids.corrector_a).unwrap();
    let (pc, p2) = (after.node_positions[&corrector], after.node_positions[&c2]);
    assert!(p2.x > pc.x && p2.y == pc.y, "the new corrector right of the old: {pc:?} {p2:?}");
    assert!(after.node_positions[&mask].y > p2.y, "the mask below its corrector");
    let edits = h.edits_seen.lock().unwrap().clone();
    assert_eq!(edits.len(), 1, "one edit");
    let klipp_document::history::EditCommand::PictureEdit { edit, .. } = edits[0].clone() else { panic!("one picture edit") };
    let mut clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    clip.restore_picture(&edit.before);
    assert_eq!(clip.grade().node_positions, grade.node_positions, "undo puts every node back");
}

#[test]
fn graph_mask_shapes_set_atomically_and_refuse_invalid_values() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let at = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let added = h.ok(json!({"method":"graph_ops","ops":[{"op":"add_beside","at":at,"type":"klipp.grade.mask"}]}));
    let path = added["created"][0]["path"].clone();
    let shapes = serde_json::to_value(klipp_core::MaskStack { masks: vec![klipp_core::Mask::default()] }).unwrap();
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let applied = h.ok(json!({"method":"graph_ops","ops":[{"op":"set","path":path,"key":"masks","value":shapes}]}));
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["revision"].as_u64().unwrap(), added["revision"].as_u64().unwrap()+1);
    assert_eq!(h.edits_seen.lock().unwrap().len(),2);
    let read = h.ok(json!({"method":"graph_read","path":path}));
    let stored: klipp_core::MaskStack = serde_json::from_value(read["nodes"][0]["params"]["masks"].clone()).unwrap();
    let expected: klipp_core::MaskStack = serde_json::from_value(shapes.clone()).unwrap();
    assert_eq!(stored, expected);
    let edit = h.edits_seen.lock().unwrap()[1].clone();
    let klipp_document::history::EditCommand::PictureEdit { edit, .. } = edit else { panic!("one mask edit") };
    let mut clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    let after = clip.picture_v2().unwrap().picture.clone();
    clip.restore_picture(&edit.before);
    assert_eq!(clip.picture_v2().unwrap().picture,before);
    clip.restore_picture(&edit.after);
    assert_eq!(clip.picture_v2().unwrap().picture,after);
    let mut oversized = shapes.clone(); oversized["masks"] = json!(vec![shapes["masks"][0].clone();17]);
    let mut nonfinite = shapes.clone(); nonfinite["masks"][0]["opacity"] = json!(1e100);
    let mut unknown = shapes.clone(); unknown["masks"][0]["shape"]["Circle"]["raduis"] = json!(0.5);
    for value in [oversized,nonfinite,unknown] {
        let refused = h.call(json!({"method":"graph_ops","ops":[
            {"op":"enable","path":path,"enabled":false},
            {"op":"set","path":path,"key":"masks","value":value}
        ]}));
        assert_eq!(refused["ok"],false,"{refused}");
        assert_eq!(refused["op_index"],1);
        assert_eq!(h.shared.revision(),applied["revision"].as_u64().unwrap());
        assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture,after);
    }
    assert_eq!(h.edits_seen.lock().unwrap().len(),2);
}

#[test]
fn graph_required_input_beside_nodes_validate_at_batch_end() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let at = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let new = json!({"root":root,"nodes":[group,{"new":1}]});
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let first = json!({"op":"set","path":at,"key":"exposure","value":0.2});
    let add = json!({"op":"add_beside","at":at,"type":"klipp.grade.exposure","params":{"exposure":0.5}});
    let refused = h.call(json!({"method":"graph_ops","ops":[first,add]}));
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["op_index"], 1);
    assert!(refused["error"].as_str().unwrap().contains("Unwired"));
    assert!(refused["error"].as_str().unwrap().contains("port: \"in\""));
    assert!(h.envelopes.lock().unwrap().is_empty());
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(), graph["revision"].as_u64().unwrap());
    assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);

    let applied = h.ok(json!({"method":"graph_ops","ops":[first,add,
        {"op":"connect","from":at,"from_port":"out","to":new,"to_port":"in"}
    ]}));
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["revision"].as_u64().unwrap(), graph["revision"].as_u64().unwrap() + 1);
    let node = h.ok(json!({"method":"graph_read","path":applied["created"][0]["path"]}));
    assert_eq!(node["nodes"][0]["params"]["exposure"], 0.5);
    assert_eq!(h.edits_seen.lock().unwrap().len(), 1);
}

#[test]
fn graph_preflight_refusal_and_stale_guards_leave_the_picture_unchanged() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let path = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let refused = h.call(json!({"method":"graph_ops","ops":[
        {"op":"set","path":path,"key":"exposure","value":1.0},
        {"op":"set","path":path,"key":"exposure","value":"wrong"}
    ]}));
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["op_index"], 1);
    assert!(refused["error"].as_str().unwrap().contains("Float"));
    for guard in ["expected_revision", "expected_project_identity"] {
        let mut request = json!({"method":"graph_ops","ops":[{"op":"enable","path":path,"enabled":false}]});
        request[guard] = json!(u64::MAX);
        assert_eq!(h.call(request)["ok"], false);
    }
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(), graph["revision"].as_u64().unwrap());
    assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);
}

#[test]
fn graph_commit_guards_are_sent_even_when_the_caller_omits_them() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let path = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    for change in [1,2] {
        let revision = h.shared.revision();
        h.commit_change.store(change, std::sync::atomic::Ordering::SeqCst);
        let refused = h.call(json!({"method":"graph_ops","ops":[{"op":"enable","path":path,"enabled":false}]}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert!(refused["result"]["operation_id"].is_string(), "the envelope reached the app thread");
        assert_eq!(h.shared.revision(), revision + u64::from(change == 1), "only the competing edit may bump the revision");
    }
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);
}

// Effect-node acceptance uses the production clip adapter and persistence
// boundary, including the supported route through a published grade matte.
fn graph_test_blur(h: &mut Harness) -> Value {
    // Seed an existing effect so each acceptance test reaches its own operation,
    // independently of the add operation's revision-1 path remapping.
    let seeded = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    h.sequence.write().unwrap().tracks[0].clips[0] = klipp_timeline::test_clip::TestClip::of(seeded).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 }).build();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let blur = graph["nodes"].as_array().unwrap().iter()
        .find(|n| n["type"] == "klipp.fx.gaussian_blur").unwrap()["id"].clone();
    json!({"root":root,"nodes":[blur]})
}

fn graph_test_edit_roundtrip(h: &mut Harness, ops: Value) {
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let revision = h.shared.revision();
    let count = h.edits_seen.lock().unwrap().len();
    let result = h.ok(json!({"method":"graph_ops","ops":ops}));
    assert_eq!(result["applied"], true);
    assert_eq!(h.shared.revision(), revision + 1);
    assert_eq!(h.edits_seen.lock().unwrap().len(), count + 1, "one undo edit per batch");
    let mut clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    let after = clip.picture_v2().unwrap().picture.clone();
    assert_ne!(before, after);
    // Project persistence stores the fields and picture separately; Clip serde
    // alone deliberately omits the truth. Exercise that same adoption boundary.
    let saved = serde_json::to_vec(&clip).unwrap();
    let stored_picture = serde_json::to_vec(&after).unwrap();
    let versions = clip.picture_v2().unwrap().versions.clone();
    let mut reopened: klipp_timeline::Clip = serde_json::from_slice(&saved).unwrap();
    reopened.adopt_stored_picture(klipp_timeline::LiftedPicture {
        picture: serde_json::from_slice(&stored_picture).unwrap(), versions,
    }).unwrap();
    assert_eq!(reopened.picture_v2().unwrap().picture, after, "stored picture retains the edit");
    let edit = h.edits_seen.lock().unwrap().last().unwrap().clone();
    let klipp_document::history::EditCommand::PictureEdit { edit, .. } = edit else { panic!("one picture edit") };
    clip.restore_picture(&edit.before);
    assert_eq!(clip.picture_v2().unwrap().picture, before);
    clip.restore_picture(&edit.after);
    assert_eq!(clip.picture_v2().unwrap().picture, after);
}

#[test]
fn graph_effect_mix_applies_reads_back_and_roundtrips() {
    let mut h = harness();
    let path = graph_test_blur(&mut h);
    for mix in [json!(0.25), Value::Null] {
        graph_test_edit_roundtrip(&mut h, json!([{"op":"mix","path":path,"mix":mix}]));
        let read = h.ok(json!({"method":"graph_read","path":path}));
        assert_eq!(read["nodes"][0]["mix"], mix);
        assert_eq!(read["nodes"][0]["id"], path["nodes"][0]);
    }
}

#[test]
fn graph_effect_mask_channel_applies_reads_back_and_roundtrips() {
    let mut h = harness();
    let path = graph_test_blur(&mut h);
    for channel in [json!("alpha"), json!("red"), Value::Null] {
        graph_test_edit_roundtrip(&mut h, json!([{"op":"mask_channel","path":path,"channel":channel}]));
        let read = h.ok(json!({"method":"graph_read","path":path}));
        assert_eq!(read["nodes"][0]["mask_channel"], if channel.is_null() { json!("red") } else { channel.clone() });
        assert_eq!(read["nodes"][0]["mask_channel_set"], !channel.is_null());
        assert_eq!(read["nodes"][0]["id"], path["nodes"][0]);
    }
}

#[test]
fn graph_effect_published_mask_connection_applies_reads_back_and_roundtrips() {
    let mut h = harness();
    let effect = graph_test_blur(&mut h);
    let types = h.ok(json!({"method":"node_types"}));
    let types = types["types"].as_array().unwrap();
    let mask = types.iter().find(|n| n["id"] == "klipp.grade.mask").unwrap();
    let blur = types.iter().find(|n| n["id"] == "klipp.fx.gaussian_blur").unwrap();
    let from_port = mask["outputs"].as_array().unwrap().iter().find(|p| p["type"] == "Matte").unwrap()["key"].clone();
    let to_port = blur["inputs"].as_array().unwrap().iter().find(|p| p["type"] == "Matte").unwrap()["key"].clone();
    let window = serde_json::to_value(klipp_core::MaskStack { masks: vec![klipp_core::Mask::default()] }).unwrap();
    let root = json!({"root":effect["root"],"nodes":[]});
    let graph = h.ok(json!({"method":"graph_read","path":root}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let at = json!({"root":effect["root"],"nodes":[group,h.ids.corrector_a]});
    let added = h.ok(json!({"method":"graph_ops","ops":[
        {"op":"add_beside","at":at,"type":mask["id"],"params":{"masks":window}}
    ]}));
    let maskpath = added["created"][0]["path"].clone();
    let key = "window_mask";
    let group_path = json!({"root":effect["root"],"nodes":[group]});
    graph_test_edit_roundtrip(&mut h, json!([
        {"op":"publish","path":group_path,"from":maskpath,"from_port":from_port,"key":key,"label":"Window"},
        {"op":"connect","from":group_path,"from_port":key,"to":effect,"to_port":to_port}
    ]));
    let group_read = h.ok(json!({"method":"graph_read","path":group_path}));
    let published = group_read["ports"].as_array().unwrap().iter().find(|p| p["key"] == key).unwrap();
    assert_eq!(published["direction"], "output");
    assert_eq!(published["ty"], "matte");
    assert_eq!(published["binding"]["node"], maskpath["nodes"][1]);
    assert_eq!(published["binding"]["port"], from_port);
    let read = h.ok(json!({"method":"graph_read","path":root}));
    let made = h.ok(json!({"method":"graph_read","path":maskpath}));
    assert_eq!(serde_json::from_value::<klipp_core::MaskStack>(made["nodes"][0]["params"]["masks"].clone()).unwrap(),
        serde_json::from_value::<klipp_core::MaskStack>(window).unwrap());
    assert!(read["edges"].as_array().unwrap().iter().any(|e|
        e["from"] == group && e["from_port"] == key
            && e["to"] == effect["nodes"][0] && e["to_port"] == to_port));
    graph_test_edit_roundtrip(&mut h, json!([{"op":"disconnect","to":effect,"port":to_port}]));
    let disconnected = h.ok(json!({"method":"graph_read","path":root}));
    assert!(!disconnected["edges"].as_array().unwrap().iter().any(|e|
        e["to"] == effect["nodes"][0] && e["to_port"] == to_port));
    assert_eq!(disconnected["nodes"], read["nodes"], "disconnect retains both nodes");
    let group_node = read["nodes"].as_array().unwrap().iter().find(|n| n["id"] == group).unwrap();
    assert_eq!(group_node["ports"], group_read["ports"]);
    graph_test_edit_roundtrip(&mut h, json!([{"op":"unpublish","path":group_path,"key":key}]));
    let restored = h.ok(json!({"method":"graph_read","path":group_path}));
    assert!(!restored["ports"].as_array().unwrap().iter().any(|p| p["key"] == key));
    assert!(restored["nodes"].as_array().unwrap().iter().any(|n| n["id"] == maskpath["nodes"][1]));
}

#[test]
fn graph_publication_refusals_are_atomic_and_new_children_are_supported() {
    let mut h = harness();
    let effect = graph_test_blur(&mut h);
    let root = json!({"root":effect["root"],"nodes":[]});
    let graph = h.ok(json!({"method":"graph_read","path":root}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let group_path = json!({"root":effect["root"],"nodes":[group]});
    let at = json!({"root":effect["root"],"nodes":[group,h.ids.corrector_a]});
    let new = json!({"root":effect["root"],"nodes":[group,{"new":0}]});
    let missing = json!({"root":effect["root"],"nodes":[group,uuid::Uuid::new_v4()]});
    let types = h.ok(json!({"method":"node_types"}));
    assert_eq!(types["publication"]["available"], true);
    assert_eq!(types["publication"]["publish"]["op"], "publish");
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let revision = h.shared.revision();
    let edits = h.edits_seen.lock().unwrap().len();
    for (from, port) in [(missing, "out"), (new.clone(), "missing"), (effect.clone(), "out")] {
        let refused = h.call(json!({"method":"graph_ops","ops":[
            {"op":"add_beside","at":at,"type":"klipp.grade.mask"},
            {"op":"publish","path":group_path,"from":from,"from_port":port,"key":"window_mask"}
        ]}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["op_index"], 1, "{refused}");
        assert_eq!(h.shared.revision(), revision);
        assert_eq!(h.edits_seen.lock().unwrap().len(), edits);
        assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);
    }
    graph_test_edit_roundtrip(&mut h, json!([
        {"op":"add_beside","at":at,"type":"klipp.grade.mask"},
        {"op":"publish","path":group_path,"from":new,"from_port":"out","key":"window_mask"},
        {"op":"connect","from":group_path,"from_port":"window_mask","to":effect,"to_port":"mask"}
    ]));
    // Removing a still-connected port must not leave a dangling reader.
    let refused = h.call(json!({"method":"graph_ops","ops":[{"op":"unpublish","path":group_path,"key":"window_mask"}]}));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(h.shared.revision(), revision + 1);
    graph_test_edit_roundtrip(&mut h, json!([
        {"op":"disconnect","to":effect,"port":"mask"},
        {"op":"unpublish","path":group_path,"key":"window_mask"}
    ]));
}

#[test]
fn graph_effect_mask_connection_applies_reads_back_and_roundtrips() {
    let mut h = harness();
    let effect = graph_test_blur(&mut h);
    let types = h.ok(json!({"method":"node_types"}));
    let types = types["types"].as_array().unwrap();
    let mask = types.iter().find(|n| n["id"] == "klipp.grade.mask").unwrap();
    let blur = types.iter().find(|n| n["id"] == "klipp.fx.gaussian_blur").unwrap();
    let from_port = mask["outputs"].as_array().unwrap().iter().find(|p| p["type"] == "Matte").unwrap()["key"].clone();
    let to_port = blur["inputs"].as_array().unwrap().iter().find(|p| p["type"] == "Matte").unwrap()["key"].clone();
    let window = serde_json::to_value(klipp_core::MaskStack { masks: vec![klipp_core::Mask::default()] }).unwrap();
    let new = json!({"root":effect["root"],"nodes":[{"new":0}]});
    graph_test_edit_roundtrip(&mut h, json!([
        {"op":"add_beside","at":effect,"type":mask["id"],"params":{"masks":window}},
        {"op":"connect","from":new,"from_port":from_port,"to":effect,"to_port":to_port}
    ]));
    let root = json!({"root":effect["root"],"nodes":[]});
    let read = h.ok(json!({"method":"graph_read","path":root}));
    let made = read["nodes"].as_array().unwrap().iter().find(|n| n["type"] == mask["id"]).unwrap();
    assert_eq!(serde_json::from_value::<klipp_core::MaskStack>(made["params"]["masks"].clone()).unwrap(),
        serde_json::from_value::<klipp_core::MaskStack>(window).unwrap());
    assert!(read["edges"].as_array().unwrap().iter().any(|e|
        e["from"] == made["id"] && e["from_port"] == from_port
            && e["to"] == effect["nodes"][0] && e["to_port"] == to_port));
    graph_test_edit_roundtrip(&mut h, json!([{"op":"disconnect","to":effect,"port":to_port}]));
    let disconnected = h.ok(json!({"method":"graph_read","path":root}));
    assert!(!disconnected["edges"].as_array().unwrap().iter().any(|e|
        e["to"] == effect["nodes"][0] && e["to_port"] == to_port));
    assert_eq!(disconnected["nodes"], read["nodes"], "disconnect retains both nodes");
}

#[test]
fn graph_effect_persistence_refusals_map_expanded_indices_and_change_nothing() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let display = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.colour.scene_to_display").unwrap()["id"].clone();
    let after = json!({"root":root,"nodes":[display]});
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    // Refused after the add expands (an effect's mix or mask makes the clip
    // picture-truth since M2 step 9c, so those go through).
    for refused_op in [json!({"op":"enable","path":after,"enabled":false}), json!({"op":"move_after","path":after,"after":{"new":0}})] {
        let preflight = refused_op["op"] == "move_after";
        let envelopes = h.envelopes.lock().unwrap().len();
        let refused = h.call(json!({"method":"graph_ops","ops":[
            {"op":"add","after":after,"type":"klipp.fx.gaussian_blur","params":{"radius":3.0}},
            refused_op
        ]}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["op_index"], 1, "expanded initial params must not shift the caller's index: {refused}");
        assert!(!refused["error"].as_str().unwrap().starts_with("op "), "the prose must not contradict the caller's index: {refused}");
        if preflight {
            assert!(refused["result"]["operation_id"].is_null(), "protected moves are refused before dispatch");
            assert_eq!(h.envelopes.lock().unwrap().len(), envelopes);
        } else {
            let operation = refused["result"]["operation_id"].as_str().expect("app-thread refusal");
            let status = h.ok(json!({"method":"operation_status","operation_id":operation}));
            assert_eq!(status["op_index"], 1);
            assert_eq!(status["state"], "refused");
            assert_eq!(status["reason"], refused["error"]);
        }
    }
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(), graph["revision"].as_u64().unwrap());
    assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);
}

#[test]
fn graph_commit_refusal_after_a_beside_node_names_the_actual_operation() {
    let mut h = harness();
    let seeded = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    h.sequence.write().unwrap().tracks[0].clips[0] = klipp_timeline::test_clip::TestClip::of(seeded).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 }).build();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let nodes = graph["nodes"].as_array().unwrap();
    let group = nodes.iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let at = json!({"root":root,"nodes":[group,h.ids.corrector_a]});
    let new = json!({"root":root,"nodes":[group,{"new":0}]});
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    // Refused (an effect's mix or mask makes the clip picture-truth since
    // M2 step 9c, so those go through).
    let display = nodes.iter().find(|n| n["type"] == "klipp.colour.scene_to_display").unwrap()["id"].clone();
    let display = json!({"root":root,"nodes":[display]});
    for bad in [json!({"op":"enable","path":display,"enabled":false}), json!({"op":"remove","path":display})] {
        let preflight = bad["op"] == "remove";
        let envelopes = h.envelopes.lock().unwrap().len();
        let refused = h.call(json!({"method":"graph_ops","ops":[
            {"op":"add_beside","at":at,"type":"klipp.grade.exposure","params":{"exposure":0.5}},
            {"op":"connect","from":at,"from_port":"out","to":new,"to_port":"in"},
            {"op":"enable","path":new,"enabled":true},
            bad
        ]}));
        assert_eq!(refused["ok"], false, "{refused}");
        assert_eq!(refused["op_index"], 3, "the incomplete add prefix must not hide the actual refusal: {refused}");
        if preflight {
            assert!(refused["result"]["operation_id"].is_null(), "protected removal is refused before dispatch");
            assert_eq!(h.envelopes.lock().unwrap().len(), envelopes);
        } else {
            let operation = refused["result"]["operation_id"].as_str().expect("app-thread refusal");
            let status = h.ok(json!({"method":"operation_status","operation_id":operation}));
            assert_eq!(status["op_index"], 3);
            assert_eq!(status["state"], "refused");
        }
    }
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(), graph["revision"].as_u64().unwrap());
    assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);
}

#[test]
fn graph_misspelled_top_level_fields_never_reach_the_app() {
    let mut h = harness();
    let path = json!({"root":{"clip":h.ids.clip_a},"nodes":[]});
    for (method, field) in [("graph_ops", "expected_revison"), ("graph_ops", "expected_project_identty"), ("graph_read", "depht")] {
        let mut request = if method == "graph_ops" {
            json!({"method":method,"ops":[{"op":"enable","path":path,"enabled":false}]})
        } else { json!({"method":method,"path":path}) };
        request[field] = json!(0);
        let refused = h.call(request);
        assert_eq!(refused["ok"],false);
        assert!(refused["error"].as_str().unwrap().contains(field), "{refused}");
    }
    assert!(h.envelopes.lock().unwrap().is_empty());
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(),0);
}

/// #1911: each of the sequence's own pictures takes a guarded write over
/// the socket as a clip does: the timeline grade, a grade group's stage, a
/// track's grade and its effects. Each write is one step of its kind, reads
/// back, and a stale revision changes nothing.
#[test]
fn graph_writes_reach_the_sequence_s_own_pictures() {
    use klipp_graph_format::path::{Root, TrackPart};
    use klipp_graph_format::lifted::Stage;
    let mut h = harness();
    let (sequence, group, track) = {
        let mut seq = h.sequence.write().unwrap();
        let group = klipp_timeline::GradeGroup::new("Night");
        let gid = group.id.0;
        seq.grade_groups.push(group);
        let tid = seq.tracks[0].id.0;
        seq.tracks[0].effects = serde_json::from_value(json!({ "video": [{ "type": "gaussian_blur", "radius": 3.0 }], "video_enabled": [true] })).unwrap();
        for root in [Root::TimelineGrade { sequence: seq.id.0 }, Root::GroupStage { group: gid, stage: Stage::Post }, Root::Track { track: tid, part: TrackPart::Grade }] {
            seq.set_root_grade(root, Some(klipp_nodes::NodeGraph::default())).unwrap();
        }
        (seq.id.0, gid, tid)
    };
    for (root, kind, key, value) in [
        (json!({"timeline_grade":sequence}), "klipp.grade.corrector", "exposure", 0.75),
        (json!({"group_stage":group,"stage":"post"}), "klipp.grade.corrector", "exposure", 0.5),
        (json!({"track":track,"part":"grade"}), "klipp.grade.corrector", "exposure", 0.25),
        (json!({"track":track,"part":"effects"}), "klipp.fx.gaussian_blur", "radius", 6.0),
    ] {
        let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
        let node = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == kind).unwrap_or_else(|| panic!("{root}: {graph}"))["id"].clone();
        let path = json!({"root":root,"nodes":[node]});
        let count = h.edits_seen.lock().unwrap().len();
        let mut stale = json!({"method":"graph_ops","ops":[{"op":"set","path":path,"key":key,"value":value}]});
        stale["expected_revision"] = json!(graph["revision"].as_u64().unwrap() + 1);
        assert_eq!(h.call(stale)["ok"], false, "{root}: a stale revision is refused");
        assert_eq!(h.edits_seen.lock().unwrap().len(), count, "{root}: and changes nothing");
        let reply = h.ok(json!({"method":"graph_ops","expected_revision":graph["revision"],"expected_project_identity":graph["project_identity"],"ops":[{"op":"set","path":path,"key":key,"value":value}]}));
        assert_eq!(reply["applied"], true, "{root}: {reply}");
        assert_eq!(reply["revision"].as_u64().unwrap(), graph["revision"].as_u64().unwrap() + 1);
        let seen = h.edits_seen.lock().unwrap();
        assert_eq!(seen.len(), count + 1, "{root}: one step");
        let step_kind = match &seen[count] {
            klipp_document::history::EditCommand::SetRootGrade { .. } => "grade",
            klipp_document::history::EditCommand::SetTrackSettings { .. } => "track",
            other => panic!("{root}: {other:?}"),
        };
        assert_eq!(step_kind, if key == "radius" { "track" } else { "grade" }, "{root}");
        drop(seen);
        let read = h.ok(json!({"method":"graph_read","path":path}));
        assert_eq!(read["nodes"][0]["params"][key], value, "{root}: reads back");
    }
    // An effect added to a track's list, which names its effects by place,
    // is answered with the path the written list gives it.
    let root = json!({"track":track,"part":"effects"});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let blur = graph["nodes"][0]["id"].clone();
    let reply = h.ok(json!({"method":"graph_ops","ops":[{"op":"add","after":{"root":root,"nodes":[blur]},"type":"klipp.fx.sharpen","params":{"amount":0.4}}]}));
    assert_eq!(reply["applied"], true, "{reply}");
    let read = h.ok(json!({"method":"graph_read","path":reply["created"][0]["path"]}));
    assert_eq!(read["nodes"][0]["type"], "klipp.fx.sharpen", "{reply}");
}

#[test]
fn graph_batch_refusals_precede_document_preflight() {
    let mut h = harness();
    let empty = h.call(json!({"method":"graph_ops","ops":[]}));
    assert_eq!(empty["ok"],false);
    assert!(empty["result"]["op_index"].is_null());
    let missing = json!({"root":{"clip":uuid::Uuid::nil()},"nodes":[uuid::Uuid::nil()]});
    let long_label = h.call(json!({"method":"graph_ops","label":"x".repeat(257),"ops":[{"op":"enable","path":missing,"enabled":false}]}));
    assert_eq!(long_label["ok"],false);
    assert!(long_label["error"].as_str().unwrap().contains("undo label"));
    assert!(long_label["result"]["op_index"].is_null());
    // A grade the sequence doesn't have has no node to name (#1911).
    let sequence = h.sequence.read().unwrap().id.0;
    let no_grade = h.call(json!({"method":"graph_ops","ops":[{"op":"enable","path":{"root":{"timeline_grade":sequence},"nodes":[uuid::Uuid::nil()]},"enabled":false}]}));
    assert_eq!(no_grade["ok"],false);
    assert!(no_grade["error"].as_str().unwrap().contains("no timeline grade"), "{no_grade}");
    assert!(h.envelopes.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(),0);
}

/// A matte spliced into the grade's chain is refused, changing nothing.
/// (An effect's label, refused here before, is kept since #1912:
/// `graph_an_effect_s_label_is_kept_with_its_entry`.)
#[test]
fn graph_add_refuses_matte_splices_without_partial_edits() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let nodes = graph["nodes"].as_array().unwrap();
    let group = nodes.iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let op = json!({"op":"add","after":{"root":root,"nodes":[group,h.ids.corrector_a]},"type":"klipp.grade.mask"});
    let refused = h.call(json!({"method":"graph_ops","ops":[op]}));
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["op_index"], 0);
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert_eq!(h.shared.revision(), graph["revision"].as_u64().unwrap());
}

#[test]
fn removing_a_mask_reports_named_readers_over_the_socket() {
    mask_notice_roundtrip(false);
}

#[test]
fn removing_a_mask_reports_named_readers_after_a_timeout() {
    mask_notice_roundtrip(true);
}

fn mask_notice_roundtrip(delayed: bool) {
    let mut h = harness();
    let (mask, reader) = {
        let mut seq = h.sequence.write().unwrap();
        let clip = &mut seq.tracks[0].clips[0];
        let mut grade = clip.grade().clone();
        let pair = grade.ensure_masked_secondary(klipp_nodes::NodeKind::Exposure);
        let source = grade.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Source).unwrap().id;
        grade.edges.iter_mut().filter(|e| e.to_node == pair.0 && e.to_port == 0).for_each(|e| e.from_node = source);
        grade.nodes.iter_mut().find(|n| n.id == pair.1).unwrap().label = "Face exposure".into();
        grade.nodes.iter_mut().find(|n| n.id.to_string() == h.ids.corrector_a).unwrap().label = "Primary correction".into();
        grade.edges.push(klipp_nodes::Edge { from_node: pair.0, from_port: 0, to_node: h.ids.corrector_a.parse().unwrap(), to_port: 1 });
        clip.set_grade(grade);
        pair
    };
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap()["id"].clone();
    let count = h.edits_seen.lock().unwrap().len();
    let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
    let revision = h.shared.revision();
    let refusal = h.call(json!({"method":"graph_ops","ops":[{"op":"remove","path":{"root":root,"nodes":[group,reader]}}]}));
    assert_eq!(refusal["ok"], false, "{refusal}");
    assert!(refusal["error"].as_str().unwrap().contains("This branch is shared. Open in node view"));
    assert_eq!(h.shared.revision(), revision);
    assert_eq!(h.edits_seen.lock().unwrap().len(), count);
    assert_eq!(h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture, before);
    if delayed { h.ack_delay_ms.store(3000, std::sync::atomic::Ordering::Relaxed); }
    let result = h.ok(json!({"method":"graph_ops","ops":[{"op":"remove","path":{"root":root,"nodes":[group,mask]}}]}));
    assert_eq!(result["applied"], !delayed, "{result}");
    let started = std::time::Instant::now();
    let status = loop {
        let status = h.ok(json!({"method":"operation_status","operation_id":result["operation_id"]}));
        if status["state"] != "queued" { break status; }
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "ack did not arrive");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(status["state"], "applied", "{status}");
    if !delayed { assert_eq!(status["ungated"], result["ungated"]); }
    let notices = status["ungated"].as_array().unwrap();
    assert_eq!(notices.len(), 2);
    assert!(notices.contains(&json!({"path":{"root":root,"nodes":[group,reader]},"label":"Face exposure"})), "{notices:?}");
    assert!(notices.contains(&json!({"path":{"root":root,"nodes":[group,h.ids.corrector_a]},"label":"Primary correction"})), "{notices:?}");
    assert_eq!(h.edits_seen.lock().unwrap().len(), count + 1);
}

#[test]
fn graph_wiring_reordering_and_removal_use_catalog_ports_and_one_edit_each() {
    let mut h = harness();
    let mask_id = uuid::Uuid::new_v4();
    {
        let mut seq = h.sequence.write().unwrap();
        let clip = &mut seq.tracks[0].clips[0];
        let mut grade = clip.grade().clone();
        grade.nodes.push(klipp_nodes::Node { id:mask_id, kind:klipp_nodes::NodeKind::Mask, label:"Window".into(), enabled:true, params:Default::default(), keyframes:Default::default(), output_link:None, input_links:Vec::new() });
        clip.set_grade(grade);
    }
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let group = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group").unwrap();
    let group_id = group["id"].clone();
    let input = group["body"]["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.group.input").unwrap()["id"].clone();
    let path = |id: Value| json!({"root":root,"nodes":[group_id,id]});
    let corrector = path(json!(h.ids.corrector_a));
    let mask = path(json!(mask_id));
    let types = h.ok(json!({"method":"node_types","category":"Grade"}));
    let port = types["types"].as_array().unwrap().iter().find(|n| n["tag"] == "Mask").unwrap()["outputs"][0]["key"].clone();
    let add = h.ok(json!({"method":"graph_ops","ops":[{"op":"add","after":corrector,"type":"klipp.grade.exposure"}]}));
    let added = add["created"][0]["path"].clone();
    for op in [
        json!({"op":"connect","from":mask,"from_port":port,"to":corrector,"to_port":"mask"}),
        json!({"op":"disconnect","to":corrector,"port":"mask"}),
        json!({"op":"move_after","path":added,"after":input}),
        json!({"op":"remove","path":added}),
    ] {
        let count = h.edits_seen.lock().unwrap().len();
        let revision = h.shared.revision();
        let before = h.sequence.read().unwrap().tracks[0].clips[0].picture_v2().unwrap().picture.clone();
        let result = h.ok(json!({"method":"graph_ops","ops":[op]}));
        assert_eq!(result["applied"], true);
        assert_eq!(h.shared.revision(), revision + 1);
        assert_eq!(h.edits_seen.lock().unwrap().len(), count + 1);
        let mut clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
        let after = clip.picture_v2().unwrap().picture.clone();
        assert_ne!(after, before, "the op changes the graph");
        let edit = h.edits_seen.lock().unwrap().last().unwrap().clone();
        let klipp_document::history::EditCommand::PictureEdit { edit, .. } = edit else { panic!("one picture edit") };
        clip.restore_picture(&edit.before);
        assert_eq!(clip.picture_v2().unwrap().picture, before);
        clip.restore_picture(&edit.after);
        assert_eq!(clip.picture_v2().unwrap().picture, after);
    }
    assert_eq!(h.call(json!({"method":"graph_read","path":added}))["ok"], false);
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
    refused(
        &mut h,
        json!({
            "method": "rename_node",
            "clip_id": clip_a, "node_id": corr_b, "name": "skin",
        }),
        "has no node",
    );
    refused(
        &mut h,
        json!({
            "method": "rename_node",
            "clip_id": clip_a, "node_id": corr_a, "name": "  ",
        }),
        "not blank",
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
    // An id the clip doesn't hold is refused, a made-up "make me a
    // Corrector" id among them: a node is made by path (#1527).
    refused(
        &mut h,
        json!({
            "method": "set_node_param",
            "clip_id": clip_b,
            "node_id": uuid::Uuid::from_u128(0xC0DE_C0DE_C0DE_C0DE_C0DE_C0DE_C0DE_C0DE).to_string(),
            "param": "saturation", "value": 1.1,
        }),
        "has no node",
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
    // Many at once (#1587): each file answered on its own, one that can't
    // be imported stopping none of the rest; a folder gives its media
    // files only, not a list beside them or a subfolder.
    let shots = std::env::temp_dir().join(format!("klipp-import-many-{}", std::process::id()));
    std::fs::create_dir_all(shots.join("sub")).unwrap();
    std::fs::write(shots.join("broken.mp4"), b"this is not a video file at all").unwrap();
    std::fs::write(shots.join("list.txt"), b"broken.mp4").unwrap();
    std::fs::write(shots.join("sub").join("deeper.mp4"), b"not reached").unwrap();
    let many = h.ok(json!({ "method": "import_media", "paths": ["/definitely/not/here.mov"], "folder": shots.to_string_lossy(), "analyze": false }));
    assert_eq!(many["failed"], 2, "{many}");
    assert_eq!(many["imported"], 0, "{many}");
    let results = many["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "the list and the subfolder are left out: {many}");
    assert!(results[0]["error"].as_str().unwrap().contains("not found"), "{many}");
    assert!(results[1]["path"].as_str().unwrap().ends_with("broken.mp4"), "{many}");
    assert!(results[1]["error"].as_str().unwrap().contains("could not be imported"), "{many}");
    let empty = h.call(json!({ "method": "import_media", "folder": shots.join("sub").join("none").to_string_lossy() }));
    assert_eq!(empty["ok"], json!(false), "a folder that isn't there is refused: {empty}");
    std::fs::remove_file(shots.join("broken.mp4")).unwrap();
    let none = h.call(json!({ "method": "import_media", "folder": shots.to_string_lossy() }));
    assert_eq!(none["ok"], json!(false), "a folder with no media is refused: {none}");
    assert!(none["error"].as_str().unwrap().contains("no media file"), "{}", none["error"]);
    let _ = std::fs::remove_dir_all(&shots);
    let nothing = h.call(json!({ "method": "import_media" }));
    assert_eq!(nothing["ok"], json!(false), "{nothing}");

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
    let clips = h.ok(json!({ "method": "list_clips", "full": true }));
    let first = &clips["tracks"][0]["clips"][0];
    assert!(first["timeline_in_frame"].is_i64(), "{first}");
    assert_eq!(first["kind"], json!("gap"), "the fixture's clips are gaps with a grade");
    assert!(clips["fps"].is_f64());
    // Short by default (#507): the range, graded only where it is, no frames.
    let identity = clips["tracks"][0]["clips"][0]["grade_is_identity"] == json!(true);
    let short = h.ok(json!({ "method": "list_clips" }));
    let first = &short["tracks"][0]["clips"][0];
    assert!(first["in_secs"].is_f64() && first.get("timeline_in_frame").is_none(), "{first}");
    assert_eq!(first.get("graded").is_some(), !identity, "graded only where it is: {first}");
    // Narrowed: a track by name, a stretch of time; an unknown track says
    // which there are.
    let name = short["tracks"][0]["name"].as_str().unwrap().to_string();
    let one = h.ok(json!({ "method": "list_clips", "track": name }));
    assert_eq!(one["tracks"].as_array().unwrap().len(), 1, "{one}");
    let none = h.ok(json!({ "method": "list_clips", "from_secs": 1.0e6 }));
    assert_eq!(none["tracks"], json!([]), "{none}");
    let err = h.call(json!({ "method": "list_clips", "track": "nowhere" }));
    assert!(err["error"].as_str().is_some_and(|e| e.contains("the tracks are")), "{err}");
}

/// #2481: an export takes the loudness the call names, else the preset's
/// own, else the brief's, and says where it came from; a preset with no
/// target of its own (a master) used to drop the brief's. A delivery not
/// at the brief's loudness says so.
#[test]
fn an_export_keeps_the_briefs_loudness_unless_told_otherwise() {
    let mut h = harness();
    let media = h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/export-loudness-fixture.mov".into(), None));
    for c in h.sequence.write().unwrap().tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        c.kind = klipp_timeline::ClipKind::Media { media_id: media };
    }
    h.sequence.write().unwrap().brief = Some(klipp_timeline::Brief { loudness_lufs: Some(-16.0), ..Default::default() });
    let sent = |h: &mut Harness, req: serde_json::Value| -> (serde_json::Value, klipp_core::ExportSettings) {
        let r = h.ok(req);
        match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(klipp_core::AppCommand::ExportSequence { settings }) => (r, settings),
            other => panic!("expected ExportSequence, got {other:?}"),
        }
    };
    // A master preset has no target: the brief's applies.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-loudness-master.mov", "preset": "prores 422" }));
    assert_eq!(s.target_lufs, Some(-16.0), "{r}");
    assert_eq!((r["export"]["loudness_from"].as_str(), r["export"]["loudness_warning"].is_null()), (Some("the brief"), true), "{r}");
    // A web preset's own -14 wins, and the reply says it isn't the brief's.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-loudness-web.mp4", "preset": "YouTube 1080p" }));
    assert_eq!(s.target_lufs, Some(-14.0));
    assert_eq!(r["export"]["loudness_from"], "the preset", "{r}");
    assert!(r["export"]["loudness_warning"].as_str().is_some_and(|w| w.contains("-14.0") && w.contains("brief's -16.0")), "{r}");
    // Named in the call: that.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-loudness-asked.mp4", "target_lufs": -23.0 }));
    assert_eq!((s.target_lufs, r["export"]["loudness_from"].as_str()), (Some(-23.0), Some("the call")), "{r}");
}

/// #2477: an export chooses its output transfer, under the dialog's
/// rules: HDR at 10 bits in hevc, h264 or ProRes, a mastering peak for PQ
/// only, and the reply says what it delivers.
#[test]
fn an_export_delivers_hdr_as_the_dialog_allows() {
    let mut h = harness();
    let media = h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/export-hdr-fixture.mov".into(), None));
    for c in h.sequence.write().unwrap().tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        c.kind = klipp_timeline::ClipKind::Media { media_id: media };
    }
    let sent = |h: &mut Harness, req: serde_json::Value| -> (serde_json::Value, klipp_core::ExportSettings) {
        let r = h.ok(req);
        match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(klipp_core::AppCommand::ExportSequence { settings }) => (r, settings),
            other => panic!("expected ExportSequence, got {other:?}"),
        }
    };
    use klipp_core::HdrMode;
    // PQ in hevc: ten bits set, the peak taken.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-hdr-pq.mp4", "codec": "hevc", "hdr": "pq", "peak_nits": 2000 }));
    assert_eq!((s.hdr_mode, s.bit_depth, s.mastering_peak_nits), (HdrMode::Pq, 10, 2000.0), "{r}");
    assert_eq!((r["export"]["hdr"].as_str(), r["export"]["peak_nits"].as_f64(), r["export"]["bit_depth"].as_i64()), (Some("pq"), Some(2000.0), Some(10)), "{r}");
    assert!(r["export"]["hdr_warning"].is_null(), "{r}");
    // HLG in h264: delivered, with the codec's warning.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-hdr-hlg.mp4", "hdr": "hlg" }));
    assert_eq!((s.hdr_mode, s.bit_depth), (HdrMode::Hlg, 10), "{r}");
    assert!(r["export"]["hdr_warning"].is_string(), "{r}");
    // ProRes keeps its own depth.
    let (_, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-hdr-prores.mov", "codec": "prores", "hdr": "hlg" }));
    assert_eq!(s.hdr_mode, HdrMode::Hlg);
    // SDR by default.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-hdr-sdr.mp4" }));
    assert_eq!((s.hdr_mode, r["export"]["hdr"].as_str(), r["export"]["peak_nits"].is_null()), (HdrMode::Sdr, Some("sdr"), true), "{r}");
    // Refused before anything is sent.
    for (req, says) in [
        (json!({ "method": "export", "path": "/tmp/klipp-hdr-x.mp4", "hdr": "pq", "bit_depth": 8 }), "10 bits"),
        (json!({ "method": "export", "path": "/tmp/klipp-hdr-x.webm", "hdr": "pq" }), "writes no HDR"),
        (json!({ "method": "export", "path": "/tmp/klipp-hdr-x.mp4", "hdr": "hlg", "peak_nits": 2000 }), "PQ delivery"),
        (json!({ "method": "export", "path": "/tmp/klipp-hdr-x.mp4", "hdr": "pq", "peak_nits": 50 }), "1000, 2000 or 4000"),
        (json!({ "method": "export", "path": "/tmp/klipp-hdr-x.mp4", "hdr": "dolby" }), "not an output transfer"),
    ] {
        let err = h.call(req);
        assert!(err["error"].as_str().is_some_and(|e| e.contains(says)), "{says}: {err}");
        assert!(h.cmd_rx.try_recv().is_err(), "nothing sent for {says}");
    }
}

/// #1583: an export over the socket chooses its codec. A built-in preset
/// by name gives its codecs and rate (fields beside it win); a codec, a
/// bit depth and a bitrate are taken as asked; and what the container
/// can't hold, or a rate a codec doesn't take, is refused before
/// anything is sent.
#[test]
fn export_takes_a_preset_a_codec_and_a_bitrate() {
    let mut h = harness();
    let media = h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/export-codec-fixture.mov".into(), None));
    for c in h.sequence.write().unwrap().tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        c.kind = klipp_timeline::ClipKind::Media { media_id: media };
    }
    let sent = |h: &mut Harness, req: serde_json::Value| -> (serde_json::Value, klipp_core::ExportSettings) {
        let r = h.ok(req);
        match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(klipp_core::AppCommand::ExportSequence { settings }) => (r, settings),
            other => panic!("expected ExportSequence, got {other:?}"),
        }
    };
    use klipp_core::{ExportAudioCodec as A, ExportVideoCodec as V};

    // A preset by name, case aside: a ProRes master in MOV, PCM sound.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-codec-master.mov", "preset": "prores 422" }));
    assert_eq!((s.video_codec, s.audio_codec), (V::ProRes422, A::Pcm24));
    assert_eq!(r["export"]["preset"], "ProRes 422", "{r}");
    // A field beside a preset wins.
    let (_, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-codec-web.mp4", "preset": "YouTube 1080p", "crf": 22 }));
    assert_eq!((s.video_codec, s.crf, s.target_lufs), (V::H264, 22, Some(-14.0)));
    // A codec, ten bits and a bitrate.
    let (r, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-codec-hevc.mp4", "codec": "hevc", "bit_depth": 10, "bitrate_kbps": 20000 }));
    assert_eq!((s.video_codec, s.bit_depth, s.rate_control, s.video_bitrate_kbps), (V::H265, 10, klipp_core::RateControl::Vbr, Some(20000)));
    assert_eq!((r["export"]["rate"].as_str(), r["export"]["bit_depth"].as_u64()), (Some("20000 kbps"), Some(10)), "{r}");
    let (_, s) = sent(&mut h, json!({ "method": "export", "path": "/tmp/klipp-codec-hq.mov", "codec": "ProRes HQ", "audio_codec": "pcm24" }));
    assert_eq!((s.video_codec, s.audio_codec), (V::ProRes422HQ, A::Pcm24));

    // Refused, and nothing sent.
    for (req, why) in [
        (json!({ "path": "/tmp/k.mov", "preset": "YouTube 4K" }), "give a path ending .mp4"),
        (json!({ "path": "/tmp/k.mp4", "preset": "Cinema" }), "the presets are"),
        (json!({ "path": "/tmp/k.mp4", "codec": "prores" }), "cannot hold"),
        (json!({ "path": "/tmp/k.mov", "codec": "prores", "bitrate_kbps": 50000 }), "rate of its own"),
        (json!({ "path": "/tmp/k.mp4", "crf": 20, "bitrate_kbps": 8000 }), "not both"),
        (json!({ "path": "/tmp/k.mov", "codec": "dnxhd_115", "bit_depth": 10 }), "8 bits"),
        (json!({ "path": "/tmp/k.mp4", "codec": "mpeg2" }), "not a video codec"),
        (json!({ "path": "/tmp/k.mp4", "audio_codec": "ac3" }), "not an audio codec"),
    ] {
        let mut req = req;
        req["method"] = json!("export");
        let reply = h.call(req.clone());
        assert_eq!(reply["ok"], json!(false), "{req}: {reply}");
        assert!(reply["error"].as_str().unwrap().contains(why), "{req}: {reply}");
    }
    assert!(h.cmd_rx.try_recv().is_err(), "a refused export sends nothing");
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

    let shared = h.shared.clone();
    let commands = h.cmd_rx.clone();
    let queue = std::thread::spawn(move || {
        match commands.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(klipp_core::AppCommand::ExportSequence { settings }) => {
                assert_eq!(settings.output_path, std::path::PathBuf::from("/tmp/klipp-control-test-out.mp4"));
                assert_eq!(settings.target_lufs, Some(-14.0));
                assert_eq!(settings.crf, 20);
                assert!(matches!(settings.format, klipp_core::ExportFormat::Mp4));
            }
            other => panic!("expected ExportSequence, got {other:?}"),
        }
        shared.exports.lock().unwrap().push(klipp_core::ExportStatus {
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
            projected_bytes: None,
            encoder: Some("h264_nvenc".into()),
            audio_encoder: Some("aac".into()),
            compositing: Some("GPU".into()),
            timings: Some(klipp_core::ExportTimings { frames: 50, render_ms: 33.2, decode_ms: 7.5, encode_ms: 3.5, audio_ms: 0.6, gpu_composited: 50, ..Default::default() }),
            banding: Some("bands".into()),
            note: Some(klipp_render::banding::SUGGESTION.into()),
        });
    });
    let r = h.ok(json!({ "method": "export", "path": "/tmp/klipp-control-test-out.mp4", "target_lufs": -14.0, "crf": 20 }));
    queue.join().unwrap();
    assert_eq!(r["applied"], json!(true));
    assert_eq!(r["job_id"], "j1");
    let st = h.ok(json!({ "method": "export_status" }));
    assert_eq!(st["jobs"][0]["status"], json!("rendering"));
    assert_eq!(st["jobs"][0]["progress"], json!(0.5));
    // Where the time goes and how frames were composited (#1924), apart
    // from the codec's acceleration.
    assert_eq!(st["jobs"][0]["encoder"], json!("h264_nvenc"));
    assert_eq!(st["jobs"][0]["audio_encoder"], json!("aac"));
    assert_eq!(st["jobs"][0]["compositing"], json!("GPU"));
    assert_eq!(st["jobs"][0]["timings"]["render_ms"], json!(33.2));
    // The 8-bit banding suggestion rides on the job (#2074).
    assert_eq!(st["jobs"][0]["note"], json!(klipp_render::banding::SUGGESTION));
    assert_eq!(st["jobs"][0]["banding"], json!("bands"));
    assert_eq!(st["jobs"][0]["timings"]["gpu_composited"], json!(50));
    let unified = h.ok(json!({ "method": "job_status", "job_id": "j1" }));
    assert_eq!(unified["jobs"][0]["kind"], "render");
    assert_eq!(unified["jobs"][0]["state"], "running");
    assert_eq!(unified["jobs"][0]["progress"], json!(0.5));
    assert_eq!(unified["jobs"][0]["completed_units"], 50);
    assert!(unified["jobs"][0]["first_seen_at_ms"].as_u64().is_some());
    let status = h.ok(json!({ "method": "status" }));
    assert!(status["active_jobs"].as_array().unwrap().iter().any(|job| job["job_id"] == "j1"));
    let timeout = h.ok(json!({ "method": "export_status", "wait": true, "job_id": "j1", "timeout_ms": 1 }));
    assert_eq!(timeout["finished"], false);
    assert_eq!(timeout["timed_out"], true);
    let shared = h.shared.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut jobs = shared.exports.lock().unwrap_or_else(|e| e.into_inner());
        jobs[0].progress = 0.75;
        jobs[0].frames_done = 75;
        drop(jobs);
        std::thread::sleep(std::time::Duration::from_millis(350));
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
    let progress = h.ok(json!({ "method": "job_status", "job_id": "j1", "wait": true, "timeout_ms": 2000 }));
    assert_eq!(progress["changed"], true);
    assert_eq!(progress["jobs"][0]["progress"], json!(0.75));
    let done = h.ok(json!({ "method": "export_status", "wait": true, "timeout_ms": 2000 }));
    worker.join().unwrap();
    let terminal = h.ok(json!({ "method": "job_status", "job_id": "j1" }));
    assert_eq!(terminal["jobs"][0]["state"], "completed");
    assert_eq!(terminal["jobs"][0]["artifact"], "/tmp/klipp-control-test-out.mp4");
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

#[test]
fn a_render_cancellation_names_the_job_it_stops() {
    let mut h = harness();
    let requested = uuid::Uuid::new_v4().to_string();
    let another = uuid::Uuid::new_v4().to_string();
    for id in [&requested, &another] {
        h.shared.exports.lock().unwrap().push(klipp_core::ExportStatus {
            job_id:id.clone(), path:"/tmp/render.mp4".into(), status:"rendering".into(),
            progress:0.4, frames_done:40, total_frames:100, stage:"Encoding".into(),
            error:None, loudness:None, hardware:String::new(), fps:None, eta_secs:None, projected_bytes:None,
            encoder:None, audio_encoder:None, compositing:None, timings:None, banding:None, note:None,
        });
    }
    let reply = h.ok(json!({"method":"cancel_job","job_id":requested}));
    assert_eq!(reply["job_id"], requested);
    assert_eq!(reply["cancel_requested"], true);
    match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(1)) {
        Ok(klipp_core::AppCommand::CancelExportJob { job_id }) => assert_eq!(job_id, requested),
        other => panic!("expected named cancellation, got {other:?}"),
    }
    h.shared.exports.lock().unwrap()[0].status = "cancelled".into();
    let status = h.ok(json!({"method":"job_status","job_id":requested}));
    assert_eq!(status["jobs"][0]["state"], "cancelled");
    assert_eq!(h.shared.exports.lock().unwrap()[1].status, "rendering");
}

#[test]
fn import_returns_a_trackable_job_and_its_media_result() {
    let mut h = harness();
    let path = std::env::temp_dir().join(format!("klipp-import-job-{}.wav", uuid::Uuid::new_v4()));
    let samples = 16_000u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples * 2).to_le_bytes());
    for frame in 0..samples {
        let sample: i16 = if frame % 32 < 16 { 1000 } else { -1000 };
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(&path, wav).unwrap();
    let imported = h.ok(json!({"method":"import_media","path":path.to_string_lossy().to_string(),"analyze":false}));
    assert!(imported["job_id"].as_str().is_some());
    assert!(imported["media_id"].as_str().is_some());
    let job = h.ok(json!({"method":"job_status","job_id":imported["job_id"]}));
    assert_eq!(job["jobs"][0]["kind"], "import");
    assert_eq!(job["jobs"][0]["state"], "completed");
    assert_eq!(job["jobs"][0]["result"]["media_id"], imported["media_id"]);
    let _ = std::fs::remove_file(path);
}

#[test]
fn sync_job_completes_only_after_its_multicam_group_appears() {
    let mut h = harness();
    let (first, second) = {
        let mut pool = h.media_pool.write().unwrap();
        (pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/sync-one.mov".into(), None)),
         pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/sync-two.mov".into(), None)))
    };
    let queued = h.ok(json!({"method":"create_multicam",
        "media_ids":[first.0.to_string(), second.0.to_string()]}));
    let job_id = queued["job_id"].as_str().expect("sync has a job ID").to_owned();
    let pending = h.ok(json!({"method":"job_status","job_id":job_id}));
    assert_eq!(pending["jobs"][0]["kind"], "sync");
    assert_eq!(pending["jobs"][0]["state"], "running");
    let group = klipp_core::GroupId::new();
    let time = |secs| klipp_core::Timecode::from_secs_f64(secs, 24);
    let mut clips = vec![
        klipp_timeline::Clip::media(first, time(0.0), time(2.0), time(0.0), time(2.0)),
        klipp_timeline::Clip::media(second, time(0.0), time(2.0), time(0.0), time(2.0)),
    ];
    for (angle, clip) in clips.iter_mut().enumerate() {
        clip.multicam_group = Some(group);
        clip.multicam_angle = angle as u8;
    }
    h.sequence.write().unwrap().tracks[0].clips = clips;
    let done = h.ok(json!({"method":"job_status","job_id":job_id}));
    assert_eq!(done["jobs"][0]["state"], "completed");
    assert!(done["jobs"][0]["result"]["group_id"].as_str().is_some());
}

/// #711: a transition reads back. The timeline text and `list_clips`
/// (both forms) show it on its cut: kind, length in seconds and frames,
/// the stretch it covers by its alignment, its two clips and its id. An
/// agent that made a dissolve could not see it before; only the saved
/// project held it.
#[test]
fn a_transition_reads_back_in_the_timeline_and_list_clips() {
    let mut h = harness();
    let fps = h.sequence.read().unwrap().fps;
    let time = |secs| klipp_core::Timecode::from_secs_f64(secs, fps.num);
    let (a, b) = (klipp_core::MediaId::new(), klipp_core::MediaId::new());
    let clips = vec![
        klipp_timeline::Clip::media(a, time(0.0), time(2.0), time(0.0), time(2.0)),
        klipp_timeline::Clip::media(b, time(2.0), time(4.0), time(2.0), time(4.0)),
    ];
    let (ca, cb) = (clips[0].id, clips[1].id);
    let tr = klipp_timeline::Transition {
        id: klipp_core::TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: time(0.5),
        clip_a_id: ca,
        clip_b_id: cb,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    };
    {
        let mut seq = h.sequence.write().unwrap();
        seq.tracks[0].clips = clips;
        seq.tracks[0].transitions = vec![tr];
    }
    let f = |secs: f64| (secs * fps.as_f64()).round() as i64;
    let text = h.ok(json!({ "method": "timeline" }))["text"].as_str().unwrap().to_string();
    let want = format!(
        "transition Cross Dissolve (CrossDissolve) 0.50 s ({} f)  [1.75-2.25 s | {}-{} f]  centred on the cut at 2.00 s  from {} to {}  id {}",
        f(0.5), f(1.75), f(2.25), ca.0, cb.0, tr.id.0,
    );
    assert!(text.contains(&want), "want {want:?} in:\n{text}");

    for full in [false, true] {
        let v = h.ok(json!({ "method": "list_clips", "full": full }));
        let t = &v["tracks"][0]["transitions"][0];
        assert_eq!(t["kind"], "CrossDissolve", "full {full}: {v}");
        assert_eq!((t["secs"].as_f64(), t["frames"].as_i64()), (Some(0.5), Some(f(0.5))));
        assert_eq!((t["alignment"].as_str(), t["cut_secs"].as_f64(), t["from_secs"].as_f64(), t["to_secs"].as_f64()), (Some("center"), Some(2.0), Some(1.75), Some(2.25)));
        assert_eq!((t["clip_a_id"].as_str(), t["clip_b_id"].as_str()), (Some(ca.0.to_string().as_str()), Some(cb.0.to_string().as_str())));
    }

    // Wholly before the cut: the stretch says so.
    h.sequence.write().unwrap().tracks[0].transitions[0].alignment = klipp_core::TransitionAlignment::EndAtCut;
    let t = h.ok(json!({ "method": "list_clips" }))["tracks"][0]["transitions"][0].clone();
    assert_eq!((t["alignment"].as_str(), t["from_secs"].as_f64(), t["to_secs"].as_f64()), (Some("end_at_cut"), Some(1.5), Some(2.0)));
    // A track without transitions carries no empty list.
    assert!(h.ok(json!({ "method": "list_clips" }))["tracks"][1].get("transitions").is_none());
}

/// The text view carries what a model needs to plan and respects its
/// budget on a line boundary.
/// `get_grade` names the track grade the render copy runs after the
/// clip's own (#1072), where it runs: on a picture track, for a clip that
/// is a picture of it, and not when it changes nothing.
#[test]
fn get_grade_reports_the_track_grade_where_it_runs() {
    let mut h = harness();
    let mut exposed = klipp_nodes::NodeGraph::default();
    exposed.ensure_corrector();
    if let Some(n) = exposed.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
        n.params.exposure = Some(0.4);
    }
    let set = |h: &Harness, kind: klipp_timeline::TrackKind, clip: klipp_timeline::ClipKind, grade: Option<klipp_nodes::NodeGraph>| {
        let mut seq = h.sequence.write().unwrap();
        let t = &mut seq.tracks[0];
        t.kind = kind;
        t.effects.grade_slot().set(grade);
        for c in &mut t.clips {
            c.kind = clip.clone();
        }
    };
    let media = klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() };
    let clip = h.ids.clip_a.clone();
    let read = |h: &mut Harness| h.ok(json!({ "method": "get_grade", "clip_id": clip }))["track_grade"].clone();

    set(&h, klipp_timeline::TrackKind::Video, media.clone(), Some(exposed.clone()));
    let tg = read(&mut h);
    assert_eq!(tg, serde_json::to_value(&exposed).unwrap(), "a graded video track's grade is reported");
    set(&h, klipp_timeline::TrackKind::Audio, media.clone(), Some(exposed.clone()));
    assert!(read(&mut h).is_null(), "a sound track grades no picture");
    set(&h, klipp_timeline::TrackKind::Video, klipp_timeline::ClipKind::Gap, Some(exposed.clone()));
    assert!(read(&mut h).is_null(), "a gap takes nothing of its track's");
    set(&h, klipp_timeline::TrackKind::Video, media, Some(klipp_nodes::NodeGraph::default()));
    assert!(read(&mut h).is_null(), "an identity track grade changes nothing");
}

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
    let v = h.ok(json!({ "method": "apply", "label": "split for the test", "detail": true, "expected_revision": before, "ops": [ { "op": "split", "clip_id": clip, "at": { "secs": 1.0 } } ] }));
    assert!(v["next"].as_str().unwrap().contains("check"), "apply points at the next step: {v}");
    assert_eq!(v["applied"], json!(true));
    assert_eq!(v["revision"], json!(before + 1));
    let status = h.ok(json!({ "method": "operation_status", "operation_id": v["operation_id"] }));
    assert_eq!(status["state"], "applied");
    assert_eq!(status["revision"], json!(before + 1));
    let changes = v["changes"].as_array().expect("cut apply reports exact clip changes");
    assert!(changes.iter().any(|change| change["removed"] == true && change["before"]["clip_id"] == clip));
    assert_eq!(changes.iter().filter(|change| change["created"] == true).count(), 2);
    assert!(changes.iter().filter(|change| change["created"] == true).all(|change| change["after"]["timeline_in_frame"].as_i64().is_some() && change["after"]["timeline_out_frame"].as_i64().is_some()));
    assert_eq!(h.envelopes.lock().unwrap().last(), Some(&(true, 1)));
    let edits = h.edits_seen.lock().unwrap();
    assert!(matches!(edits.as_slice(), [klipp_document::history::EditCommand::LegacyBladeClip { .. }]));
}

#[test]
fn delayed_apply_has_a_definitive_outcome_and_retry_does_not_cut_twice() {
    let mut h = harness();
    h.ack_delay_ms.store(3000, std::sync::atomic::Ordering::Relaxed);
    let request = json!({
        "method": "apply", "idempotency_key": "cut-once-test",
        "expected_revision": h.shared.revision(),
        "ops": [{ "op": "split", "clip_id": h.ids.clip_a.clone(), "at": { "secs": 1.0 } }],
    });
    let first = h.ok(request.clone());
    assert_eq!(first["accepted"], true);
    assert_eq!(first["applied"], false, "the UI reply was deliberately delayed: {first}");
    let id = first["operation_id"].as_str().unwrap().to_owned();
    let queued = h.ok(json!({ "method": "operation_status", "operation_id": id }));
    assert_eq!(queued["state"], "queued");
    assert_eq!(queued["progress"], Value::Null, "there is no invented percent for a UI wait");

    let retry = h.ok(request);
    assert_eq!(retry["operation_id"], id);
    let started = std::time::Instant::now();
    let final_status = loop {
        let status = h.ok(json!({ "method": "operation_status", "operation_id": id }));
        if status["state"] != "queued" { break status; }
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "ack did not arrive");
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(final_status["state"], "applied");
    assert_eq!(final_status["revision"], 1);
    assert_eq!(h.envelopes.lock().unwrap().len(), 1);
    assert_eq!(h.edits_seen.lock().unwrap().len(), 1);
}

#[test]
fn delayed_ui_refusal_is_reported_without_applying_an_edit() {
    let mut h = harness();
    h.ack_delay_ms.store(3000, std::sync::atomic::Ordering::Relaxed);
    let revision = h.shared.revision();
    let shared = Arc::clone(&h.shared);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        shared.bump_revision();
    });
    let first = h.ok(json!({
        "method": "apply", "expected_revision": revision,
        "ops": [{ "op": "split", "clip_id": h.ids.clip_a.clone(), "at": { "secs": 1.0 } }],
    }));
    assert_eq!(first["applied"], false);
    let id = first["operation_id"].as_str().unwrap();
    let started = std::time::Instant::now();
    loop {
        let status = h.ok(json!({ "method": "operation_status", "operation_id": id }));
        if status["state"] == "refused" {
            assert!(status["reason"].as_str().unwrap().contains("revision moved"));
            assert_eq!(status["revision"], 1);
            break;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "refusal did not arrive");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(h.edits_seen.lock().unwrap().is_empty());
}

#[test]
fn cut_preparation_is_revision_bound_and_does_not_invent_a_candidate() {
    let mut h = harness();
    let prepared = h.ok(json!({ "method": "prepare_cut", "from_secs": 0.0, "to_secs": 2.0,
        "intent": "remove the pause after this sentence", "max_candidates": 4 }));
    assert_eq!(prepared["revision"], h.shared.revision());
    assert!(prepared["project_identity"].as_u64().is_some());
    assert_eq!(prepared["range"]["fps_num"], 24);
    assert!(prepared["clips"].as_array().is_some_and(|clips| !clips.is_empty()));
    assert_eq!(prepared["candidates"], json!([]));
    assert_eq!(prepared["ambiguous"], true);
    assert!(prepared["next"].as_str().unwrap().contains("do not guess"));
    h.shared.bump_revision();
    let stale = h.call(json!({ "method": "apply", "expected_revision": prepared["revision"],
        "ops": [{ "op": "split", "clip_id": h.ids.clip_a.clone(), "at": { "secs": 1.0 } }] }));
    assert_eq!(stale["ok"], false);
    assert!(stale["error"].as_str().unwrap().contains("plan again"));
    assert!(h.edits_seen.lock().unwrap().is_empty());
    let wide = h.call(json!({ "method": "prepare_cut", "from_secs": 0.0, "to_secs": 301.0 }));
    assert_eq!(wide["ok"], false);
}

#[test]
fn a_ripple_cut_reports_linked_picture_and_sound_at_matching_frames() {
    let mut h = harness();
    {
        let mut sequence = h.sequence.write().unwrap();
        let mut picture = sequence.tracks[0].clips.remove(0);
        let mut sound = sequence.tracks[0].clips.remove(0);
        picture.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(picture.id);
        sequence.tracks[0].clips.push(picture);
        let mut audio = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        audio.clips.push(sound);
        sequence.tracks.push(audio);
    }
    h.shared.bump_revision();
    let result = h.ok(json!({ "method": "apply", "expected_revision": h.shared.revision(), "detail": true,
        "ops": [{ "op": "cut", "from": { "secs": 1.0 }, "to": { "secs": 2.0 } }] }));
    assert_eq!(result["applied"], true);
    let changes = result["changes"].as_array().unwrap();
    let after: Vec<_> = changes.iter().filter_map(|change| change.get("after")).filter(|after| !after.is_null()).collect();
    assert!(after.iter().any(|clip| clip["track"] == 0));
    assert!(after.iter().any(|clip| clip["track"] == 1));
    for picture in after.iter().filter(|clip| clip["track"] == 0) {
        let partner = after.iter().find(|sound| sound["clip_id"] == picture["linked_clip_id"])
            .expect("every picture half keeps a linked sound half");
        assert_eq!(partner["track"], 1);
        assert_eq!(partner["timeline_in_frame"], picture["timeline_in_frame"]);
        assert_eq!(partner["timeline_out_frame"], picture["timeline_out_frame"]);
    }
}

#[test]
fn ripple_reports_later_clips_as_one_bounded_shift() {
    let mut h = harness();
    {
        let mut sequence = h.sequence.write().unwrap();
        sequence.tracks[0].clips = (0..8).map(|index| {
            klipp_timeline::Clip::gap(
                klipp_core::RationalTime::from_secs_f64(index as f64 * 4.0, 24),
                klipp_core::RationalTime::from_secs_f64((index + 1) as f64 * 4.0, 24),
            )
        }).collect();
    }
    h.shared.bump_revision();
    let result = h.ok(json!({ "method": "apply", "expected_revision": h.shared.revision(), "detail": true,
        "ops": [{ "op": "cut", "from": { "frame": 24 }, "to": { "frame": 48 } }] }));
    assert_eq!(result["applied"], true);
    assert!(result["changes"].as_array().unwrap().len() <= 3, "{result}");
    assert_eq!(result["shifted"], json!([{ "track": 0, "clips": 7, "by_frames": -24 }]));
}

/// #1589: a split that also adds a track on top reports the clips that
/// only moved down an index as one count, not an entry each.
#[test]
fn clips_that_only_change_track_index_are_folded() {
    let mut h = harness();
    {
        let mut sequence = h.sequence.write().unwrap();
        sequence.tracks[0].clips = (0..6).map(|index| {
            klipp_timeline::Clip::gap(
                klipp_core::RationalTime::from_secs_f64(index as f64 * 4.0, 24),
                klipp_core::RationalTime::from_secs_f64((index + 1) as f64 * 4.0, 24),
            )
        }).collect();
    }
    h.shared.bump_revision();
    let result = h.ok(json!({ "method": "apply", "expected_revision": h.shared.revision(), "detail": true,
        "ops": [{ "op": "add_track", "kind": "video" }, { "op": "split", "clip_id": { "track": 1, "secs": 2.0 }, "at": { "secs": 2.0 } }] }));
    assert_eq!(result["applied"], true);
    assert!(result["changes"].as_array().unwrap().len() <= 3, "{result}");
    assert_eq!(result["retracked"], json!([{ "from_track": 0, "to_track": 1, "clips": 5 }]), "{result}");
}

#[test]
fn apply_refuses_a_plan_for_a_changed_revision_or_project() {
    let mut h = harness();
    // The position names the first clip at 1 s. A person replaces that
    // clip while the agent holds its plan; the same position now names a
    // different clip, so re-resolving the stale op would cut the wrong one.
    let op = json!({ "op": "split", "clip_id": { "track": 0, "secs": 1.0 }, "at": { "secs": 1.0 } });
    let plan = h.ok(json!({ "method": "plan", "ops": [op.clone()] }));
    let revision = plan["revision"].as_u64().unwrap();
    let identity = plan["project_identity"].as_u64().unwrap();
    let old_clip = h.sequence.read().unwrap().tracks[0].clips[0].id;
    {
        let mut sequence = h.sequence.write().unwrap();
        let old = &sequence.tracks[0].clips[0];
        let replacement = klipp_timeline::Clip::gap(old.timeline_in, old.timeline_out);
        sequence.tracks[0].clips[0] = replacement;
    }
    h.shared.bump_revision();
    let replacement_id = h.sequence.read().unwrap().tracks[0].clips[0].id;
    assert_ne!(old_clip, replacement_id);
    let resolved = {
        let sequence = h.sequence.read().unwrap();
        let pool = h.media_pool.read().unwrap();
        let operation = serde_json::from_value(op.clone()).unwrap();
        klipp_document::ops::resolve(&sequence, &pool, &[operation]).unwrap()
    };
    assert!(matches!(&resolved.steps[0].edit, klipp_document::history::EditCommand::LegacyBladeClip { original_clip, .. } if original_clip.id == replacement_id));

    let stale = h.call(json!({ "method": "apply", "ops": [op.clone()], "expected_revision": revision }));
    assert_eq!(stale["ok"], false);
    assert!(stale["error"].as_str().unwrap().contains("plan again"));
    assert!(h.edits_seen.lock().unwrap().is_empty());

    let current_revision = h.shared.revision();
    h.shared.project.lock().unwrap().identity += 1;
    let other_project = h.call(json!({ "method": "apply", "ops": [op], "expected_revision": current_revision, "expected_project_identity": identity }));
    assert_eq!(other_project["ok"], false);
    assert!(other_project["error"].as_str().unwrap().contains("project changed"));
    assert!(h.edits_seen.lock().unwrap().is_empty());
    assert!(h.envelopes.lock().unwrap().is_empty());
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

#[test]
fn project_access_recent_open_and_safe_close_over_socket() {
    let mut h=harness();
    let root=std::env::temp_dir().join(format!("klipp-project-access-{}",uuid::Uuid::new_v4()));
    let path=root.join("Example.klipp");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("project.json"),"{}").unwrap();
    h.shared.project.lock().unwrap().recent_projects=vec![klipp_core::RecentProjectSummary {
        path:path.clone(),name:"Example".into(),last_saved:None,duration_secs:Some(12.0),fps:Some("24".into()),resolution:Some("1920x1080".into()),missing_media:Some(false)
    }];
    assert_eq!(h.ok(json!({"method":"status"}))["recent_projects"][0]["name"],"Example");
    assert_eq!(h.ok(json!({"method":"recent_projects"}))["projects"][0]["duration_secs"],12.0);
    assert_eq!(h.ok(json!({"method":"open_project","name":"EXAMPLE"}))["opened"],true);
    assert_eq!(h.ok(json!({"method":"status"}))["recent_projects"],json!([]));
    h.shared.project.lock().unwrap().dirty=true;
    let before=h.sent.lock().unwrap().len();
    assert!(h.call(json!({"method":"close_project"}))["error"].as_str().unwrap().contains("unsaved"));
    assert_eq!(h.sent.lock().unwrap().len(),before);
    assert_eq!(h.ok(json!({"method":"close_project","save":true}))["closed"],true);
    assert!(h.shared.project.lock().unwrap().path.is_none());
    // A save failure must leave the project and its changes in memory.
    { let mut p=h.shared.project.lock().unwrap(); p.path=Some(root.join("broken.klipp"));p.dirty=true; }
    assert!(h.call(json!({"method":"close_project","save":true}))["error"].as_str().unwrap().contains("disk said no"));
    assert!(h.shared.project.lock().unwrap().dirty);
    assert_eq!(h.ok(json!({"method":"close_project","discard_changes":true}))["closed"],true);
    assert_eq!(h.ok(json!({"method":"close_project"}))["closed"],true);
    h.ok(json!({"method":"open_project","name":"Example"}));
    assert_eq!(h.ok(json!({"method":"close_project"}))["closed"],true);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_access_waits_past_intermediate_save_for_its_own_ack() {
    let mut h = harness();
    let root = std::env::temp_dir().join(format!("klipp-switch-delay-{}",uuid::Uuid::new_v4()));
    let original = root.join("original.klipp");
    let other = root.join("other.klipp");
    let broken = root.join("broken.klipp");
    for path in [&original,&other,&broken] { std::fs::create_dir_all(path).unwrap(); }
    // Longer than envelope::ACK_TIMEOUT (2 seconds): the first wait must
    // yield pending, then wait on this operation, not the saved generation.
    h.project_switch_delay_ms.store(2200,std::sync::atomic::Ordering::SeqCst);
    for target in [&other,&original,&broken] {
        { let mut p=h.shared.project.lock().unwrap(); p.path=Some(original.clone()); p.dirty=true; p.last_error=None; }
        let start=std::time::Instant::now();
        let result=h.call(json!({"method":"open_project","path":target,"save":true}));
        assert!(start.elapsed() >= std::time::Duration::from_millis(2200), "returned on the intermediate save: {result}");
        if target == &broken {
            assert!(result["error"].as_str().unwrap().contains("late load failed"),"{result}");
            assert_eq!(h.shared.project.lock().unwrap().path.as_ref(),Some(&original));
        } else {
            assert_eq!(result["result"]["opened"],true,"{result}");
            assert_eq!(h.shared.project.lock().unwrap().path.as_ref(),Some(target));
        }
    }
    { let mut p=h.shared.project.lock().unwrap();p.path=Some(original);p.dirty=true;p.last_error=None; }
    let start=std::time::Instant::now();
    assert_eq!(h.ok(json!({"method":"close_project","save":true}))["closed"],true);
    assert!(start.elapsed() >= std::time::Duration::from_millis(2200));
    assert!(h.shared.project.lock().unwrap().path.is_none());
    std::fs::remove_dir_all(root).unwrap();
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

/// #2074: check notes a dark gradient an 8-bit file will show in steps,
/// with the 10-bit or grain suggestion; with the gradient's default grain
/// on, which keeps it smooth there, it notes nothing.
#[test]
fn check_notes_a_gradient_that_will_band_at_eight_bits() {
    let mut h = harness();
    let banding = |h: &mut Harness, grain: f32| {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 30);
        let clip = klipp_timeline::Clip {
            kind: klipp_timeline::ClipKind::Generator(klipp_core::GeneratorData::LivingGradient { a: [12, 23, 32, 255], b: [29, 60, 65, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain }),
            source_in: tc(0.0),
            source_out: tc(2.0),
            ..klipp_timeline::Clip::gap(tc(0.0), tc(2.0))
        };
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![clip];
        h.sequence.write().unwrap().tracks = vec![track];
        let r = h.ok(json!({ "method": "check" }));
        r["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "banding_8bit").cloned()
    };
    let found = banding(&mut h, 0.0).expect("a gradient without grain bands");
    assert_eq!(found["severity"], "note", "advice, not a fault: {found}");
    assert_eq!(found["hint"], json!(klipp_render::banding::SUGGESTION));
    assert_eq!(banding(&mut h, klipp_core::DEFAULT_GENERATOR_GRAIN), None, "its default grain keeps it smooth");
    // While a render runs the check leaves the machine to it, and says so.
    h.shared.exports.lock().unwrap().push(klipp_core::ExportStatus {
        job_id: "busy".into(), path: "/tmp/render.mp4".into(), status: "rendering".into(),
        progress: 0.1, frames_done: 1, total_frames: 10, stage: "Encoding".into(),
        error: None, loudness: None, hardware: String::new(), fps: None, eta_secs: None, projected_bytes: None,
        encoder: None, audio_encoder: None, compositing: None, timings: None, banding: None, note: None,
    });
    assert_eq!(banding(&mut h, 0.0), None, "not sampled beside a render");
    let r = h.ok(json!({ "method": "check" }));
    assert!(r["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "banding_unchecked"), "{r}");
}

/// #1932: every finding of check carries an id; review_finding saves a
/// decision about one as an undo step, refusing an acceptance with no
/// reason and an id check never gave. Saved, an accepted finding leaves
/// the counts for a section of its own with its reason, and a review
/// whose finding is gone is listed as no longer holding.
#[test]
fn a_reviewed_finding_is_kept_apart_with_its_reason() {
    let mut h = harness();
    let r = h.ok(json!({ "method": "check" }));
    let finding = r["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "no_brief").unwrap_or_else(|| panic!("{r}")).clone();
    let id = finding["id"].as_str().unwrap_or_else(|| panic!("an id: {finding}")).to_string();
    assert!(id.starts_with("check/no_brief/"), "{id}");
    assert_eq!(h.ok(json!({ "method": "check" }))["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "no_brief").unwrap()["id"], json!(id), "the same id on every run");

    let refused = h.call(json!({ "method": "review_finding", "finding_id": id, "decision": "accepted" }));
    assert!(refused["error"].as_str().unwrap().contains("rationale"), "{refused}");
    let refused = h.call(json!({ "method": "review_finding", "finding_id": "no_brief", "decision": "accepted", "rationale": "x" }));
    assert!(refused["error"].as_str().unwrap().contains("not a finding id"), "{refused}");

    let v = h.ok(json!({ "method": "review_finding", "finding_id": id, "decision": "accepted", "rationale": "a test piece has no brief" }));
    assert!(v["text"].as_str().unwrap().contains("Accepted"), "{v}");
    let saved = match h.edits_seen.lock().unwrap().last() {
        Some(klipp_document::history::EditCommand::SetReviews { new, .. }) => new.clone(),
        other => panic!("a review edit: {other:?}"),
    };
    assert_eq!(saved.len(), 1);
    assert_eq!((saved[0].finding_id.as_str(), saved[0].decision, saved[0].rationale.as_str()), (id.as_str(), klipp_timeline::ReviewDecision::Accepted, "a test piece has no brief"));

    // The stand-in UI does not apply edits: put the decision, and one
    // whose finding is gone, where the app would.
    // A finding marked to fix stays counted and carries its review (#1978).
    let mut to_fix = saved[0].clone();
    to_fix.decision = klipp_timeline::ReviewDecision::ToFix;
    to_fix.rationale = "write a brief".into();
    h.sequence.write().unwrap().reviews = vec![to_fix];
    let r = h.ok(json!({ "method": "check" }));
    let f = r["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "no_brief").unwrap_or_else(|| panic!("{r}")).clone();
    assert_eq!(f["review"]["decision"], "to_fix", "{f}");
    assert_eq!(f["review"]["rationale"], "write a brief");

    let gone = klipp_timeline::FindingReview { finding_id: "check/flash/25/0123456789ab".into(), decision: klipp_timeline::ReviewDecision::Accepted, rationale: "a flash on the beat".into(), at_secs: Some(1.0), clip_id: None, revision: 1 };
    h.sequence.write().unwrap().reviews = vec![saved[0].clone(), gone];
    let r = h.ok(json!({ "method": "check" }));
    assert!(!r["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "no_brief"), "accepted, not among the findings: {r}");
    assert_eq!(r["accepted"][0]["id"], json!(id), "{r}");
    assert_eq!(r["accepted"][0]["rationale"], "a test piece has no brief");
    assert_eq!(r["lapsed_reviews"][0]["finding_id"], "check/flash/25/0123456789ab", "{r}");
    let text = r["text"].as_str().unwrap();
    assert!(text.contains("Accepted on purpose, not counted (1)") && text.contains("Why: a test piece has no brief"), "{text}");
    assert!(text.contains("[lapsed, was accepted]"), "{text}");
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
    // A sound file has no picture size or rate to report (#2492).
    let mut song = klipp_media::MediaItem::synthetic_for_test("/tmp/orient-song.wav".into(), None);
    song.video_codec = String::new();
    h.media_pool.write().unwrap().add(song);
    h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/orient-take.mov".into(), None));
    let r = h.ok(json!({ "method": "orient" }));
    let of = |name: &str| r["media"].as_array().unwrap().iter().find(|m| m["filename"] == name).cloned().unwrap();
    let (song, take) = (of("orient-song.wav"), of("orient-take.mov"));
    assert_eq!((song["width"].clone(), song["fps"].clone(), song["has_picture"].clone()), (json!(null), json!(null), json!(false)), "{song}");
    assert_eq!((take["width"].clone(), take["has_picture"].clone()), (json!(1920), json!(true)), "{take}");
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

/// #1897: a frame of warm stone with no person in it has plenty of
/// skin-coloured pixels. The scopes say that as a colour reading, never
/// as skin, and the colour faults ask for no white balance over it; the
/// same reading backed by a face does.
#[test]
fn skin_coloured_pixels_without_a_face_are_not_skin() {
    // Sandstone: bright, warm, off the skin-tone line's hue.
    let stone = klipp_control::frame_stats(&std::iter::repeat_n([210u8, 160, 150, 255], 320*180).flatten().collect::<Vec<u8>>(), 320, 180, 23.0);
    let note = stone["skin"].as_str().unwrap();
    assert!(note.contains("skin-coloured pixels") && note.contains("no face detected"), "{stone}");
    assert_eq!(stone["skin_from"], "colour");
    assert!(stone["skin_on_line_pct"].as_f64().unwrap() < 60.0, "{stone}");
    // Over most of the frame and off the line: said as colour only, to
    // check by eye, never as faces to fix.
    let said = klipp_control::color_faults(&stone, 23.0);
    assert!(said.iter().any(|f| f.1.starts_with("colour only, no face found")) && !said.iter().any(|f| f.2.contains("faces land")), "{said:?}");
    // A face beside a large warm rock: read inside the face, the share
    // follows the face alone.
    let (w, h) = (320usize, 180usize);
    let face = [0.7f32, 0.1, 0.2, 0.3];
    let scene: Vec<u8> = (0..w * h).flat_map(|i| {
        let (x, y) = ((i % w) as f32 / w as f32, (i / w) as f32 / h as f32);
        if x >= face[0] && x < face[0] + face[2] && y >= face[1] && y < face[1] + face[3] { [220u8, 170, 130, 255] } else { [210, 160, 150, 255] }
    }).collect();
    let whole = klipp_control::skin_line_share(&scene, w as u32, h as u32, &[[0.0, 0.0, 1.0, 1.0]]).unwrap();
    let in_face = klipp_control::skin_line_share(&scene, w as u32, h as u32, &[face]).unwrap();
    assert!(whole.0 < 60.0 && in_face.0 > 95.0, "the rock moves the frame's share, not the face's: {whole:?} {in_face:?}");
    let mut faced = stone.clone();
    faced["skin_from"] = serde_json::json!("faces");
    assert!(klipp_control::color_faults(&faced, 23.0).iter().any(|f| f.1.contains("skin-tone line")), "a face off the line is a fault");
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

/// #2476: match_grade stops short of clipping the shot it brightens, as
/// grade_to_target does, and says why; a shot with room is left alone,
/// and one already at white is not brightened at all.
#[test]
fn a_match_is_held_short_of_clipping() {
    use klipp_control::{hold_match_for_highlights, MatchKnobs};
    use klipp_color::measure::FrameMeasure;
    let up = MatchKnobs { exposure: Some(1.0), gain: Some(0.1), ..Default::default() };
    // Bright at the top: half a stop of room.
    let mut k = up;
    let why = hold_match_for_highlights(&mut k, &FrameMeasure { l_p95: 90.0, ..Default::default() }).expect("held");
    let e = k.exposure.unwrap();
    assert!((e - 7.0 / 15.0).abs() < 1e-3 && k.gain.is_none(), "{k:?}");
    assert!(why.contains("clip"), "{why}");
    // Plenty of room: the match goes through as it was.
    let mut k = up;
    assert_eq!(hold_match_for_highlights(&mut k, &FrameMeasure { l_p95: 50.0, ..Default::default() }), None);
    assert_eq!(k, up);
    // Already clipped: nothing that brightens, a darkening lift kept.
    let mut k = MatchKnobs { lift: Some(-0.05), ..up };
    let why = hold_match_for_highlights(&mut k, &FrameMeasure { l_p95: 99.0, clip_hi_pct: 19.0, ..Default::default() }).expect("held");
    assert!(k.exposure.is_none() && k.gain.is_none() && k.lift == Some(-0.05), "{k:?}");
    assert!(why.contains("19.0%"), "{why}");
    // Darkening is never held.
    let mut k = MatchKnobs { exposure: Some(-0.5), ..Default::default() };
    assert_eq!(hold_match_for_highlights(&mut k, &FrameMeasure { l_p95: 99.0, clip_hi_pct: 19.0, ..Default::default() }), None);
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

/// #2478: proxies are made over the socket as the media pool makes them:
/// the timeline's picture files by default, a job that follows each to its
/// proxy, a failure named, and status saying what playback reads.
#[test]
fn proxies_are_made_as_a_job_and_status_says_what_playback_reads() {
    let mut h = harness();
    let (a, b) = {
        let mut pool = h.media_pool.write().unwrap();
        (pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/proxy-a.mov".into(), None)), pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/proxy-b.mov".into(), None)))
    };
    {
        let mut seq = h.sequence.write().unwrap();
        let clips: Vec<&mut klipp_timeline::Clip> = seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).collect();
        for (k, c) in clips.into_iter().enumerate() {
            c.kind = klipp_timeline::ClipKind::Media { media_id: if k == 0 { a } else { b } };
        }
    }
    let reply = h.ok(json!({ "method": "make_proxies", "use_proxies": true }));
    let job = reply["job_id"].as_str().expect("a job").to_string();
    assert_eq!(reply["making"], 2, "{reply}");
    let sent = h.sent.lock().unwrap().clone();
    for id in [a, b] {
        assert!(sent.iter().any(|c| matches!(c, klipp_core::AppCommand::GenerateProxy { media_id } if *media_id == id)), "{sent:?}");
    }
    assert!(sent.iter().any(|c| matches!(c, klipp_core::AppCommand::SetUseProxies(true))));

    // Half of one made: running at a quarter.
    *h.shared.proxies.lock().unwrap() = klipp_core::ProxyStatus { use_proxies: true, making: vec![(a, 0.5)], failed: Vec::new() };
    let status = h.ok(json!({ "method": "job_status", "job_id": job }));
    assert_eq!((status["jobs"][0]["kind"].as_str(), status["jobs"][0]["state"].as_str()), (Some("proxies"), Some("running")), "{status}");
    assert_eq!(status["jobs"][0]["progress"], json!(0.25));
    // One made, the other failed: failed, naming it.
    h.media_pool.write().unwrap().get_mut(&a).unwrap().proxy_ready = true;
    *h.shared.proxies.lock().unwrap() = klipp_core::ProxyStatus { use_proxies: true, making: Vec::new(), failed: vec![(b, "ffmpeg exited with error".into())] };
    let status = h.ok(json!({ "method": "job_status", "job_id": job }));
    assert_eq!(status["jobs"][0]["state"], "failed", "{status}");
    assert!(status["jobs"][0]["error"].as_str().unwrap().contains("proxy-b.mov"), "{status}");
    let text = h.ok(json!({ "method": "status" }))["text"].as_str().unwrap().to_string();
    assert!(text.contains("Playback reads proxies: 1 of the 2"), "{text}");

    // A still has no moving picture to make a proxy of.
    let still = {
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/proxy-still.png".into(), None);
        item.is_still = true;
        h.media_pool.write().unwrap().add(item)
    };
    let refused = h.call(json!({ "method": "make_proxies", "media_ids": [still.0.to_string()] }));
    assert!(refused["error"].as_str().unwrap().contains("no moving picture"), "{refused}");
}

/// #2485: the installed audio plug-ins are listed with their ids and
/// parameters, and one goes on a clip by id through the ops.
#[test]
fn installed_audio_plugins_are_listed_and_one_goes_on_a_clip() {
    let mut h = harness();
    klipp_audio::lv2::set_plugin_registry(vec![klipp_audio::lv2::Lv2PluginInfo {
        uri: "urn:klipp-test:room".into(),
        name: "Test Room".into(),
        audio_inputs: 2,
        audio_outputs: 2,
        control_inputs: vec![klipp_audio::lv2::Lv2ControlPort { symbol: "size".into(), name: "Room size".into(), default: 2.0, min: Some(0.0), max: Some(10.0), index: 0 }],
    }]);
    let listed = h.ok(json!({ "method": "audio_plugins", "query": "room" }));
    assert_eq!(listed["plugins"][0]["id"], "lv2:urn:klipp-test:room", "{listed}");
    assert_eq!(listed["plugins"][0]["params"][0]["key"], "size");
    assert!(listed["text"].as_str().unwrap().contains("Room size 0..10"), "{listed}");
    let media = h.media_pool.write().unwrap().add(klipp_media::MediaItem::synthetic_for_test("/tmp/plugin-fixture.mov".into(), None));
    let clip = {
        let mut seq = h.sequence.write().unwrap();
        let c = seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).next().unwrap();
        c.kind = klipp_timeline::ClipKind::Media { media_id: media };
        c.linked_clip_id = None;
        c.id.0.to_string()
    };
    let plan = h.ok(json!({ "method": "plan", "ops": [{ "op": "clip_audio", "clip_id": clip, "effect": "lv2:urn:klipp-test:room", "params": { "Room size": 5.0 } }] }));
    assert!(plan["text"].as_str().unwrap().contains("lv2:urn:klipp-test:room (size 5)"), "{plan}");
    klipp_audio::lv2::set_plugin_registry(Vec::new());
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

/// #2489: a long take with a few words, or words whose times collapsed,
/// is too thin to cut by speech; its selects are its shots.
#[test]
fn a_take_with_next_to_no_speech_is_cut_by_its_picture() {
    let few = vec![(10.0f64, 10.4, "hello".into()), (40.0, 40.3, "okay".into())];
    assert!(klipp_control::speech_too_thin(&few, 60.0));
    let collapsed: Vec<(f64, f64, String)> = (0..7).map(|k| (20.0 + k as f64 * 0.01, 20.0 + k as f64 * 0.01 + 0.02, format!("w{k}"))).collect();
    assert!(klipp_control::speech_too_thin(&collapsed, 8.0), "seven words in a quarter second");
    let talk: Vec<(f64, f64, String)> = (0..40).map(|k| (k as f64 * 0.5, k as f64 * 0.5 + 0.4, "word".into())).collect();
    assert!(!klipp_control::speech_too_thin(&talk, 25.0));
    let shots = klipp_control::picture_selects(&[12.0, 30.0], 60.0);
    let spans: Vec<(f64, f64)> = shots.iter().map(|s| (s.in_secs, s.out_secs)).collect();
    assert_eq!(spans, [(0.0, 12.0), (12.0, 30.0), (30.0, 60.0)]);
    assert!(shots.iter().all(|s| !s.filler && s.text.contains("picture")));
    assert_eq!(klipp_control::picture_selects(&[], 60.0).len(), 1, "no shots found: the whole take");
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

    // #502: a 40 s take with no cut, walking for 20 s and then held still,
    // is one shot; the held moment inside it is found as static.
    let walk_path = cache.path.join("walk.mp4");
    std::fs::write(&walk_path, b"a long take; no decoding requested").unwrap();
    let walk_key = klipp_analysis::store::key_for(&walk_path).unwrap();
    let walk = h.media_pool.write().unwrap_or_else(|e| e.into_inner()).add(klipp_media::MediaItem::synthetic_for_test(walk_path, None));
    let bins: Vec<Option<f32>> = (0..40).map(|k| Some(if k < 20 { 7.0 } else { 0.5 })).collect();
    klipp_analysis::store::save(&walk_key, "motion", klipp_analysis::motion::VERSION, &serde_json::to_value(klipp_analysis::motion::Motion::from_samples(40.0, bins)).unwrap()).unwrap();
    klipp_analysis::store::save(&walk_key, "shots", klipp_analysis::shots::VERSION, &serde_json::to_value(klipp_analysis::shots::Shots::from_boundaries(40.0, vec![])).unwrap()).unwrap();
    let held = h.ok(json!({"method":"find_shots","motion":"static"}));
    let hit = held["hits"].as_array().unwrap().iter().find(|v| v["media_id"] == walk.0.to_string()).unwrap_or_else(|| panic!("the held moment is found: {held}"));
    assert_eq!(hit["moment"], json!(2), "{hit}");
    assert!((hit["start_secs"].as_f64().unwrap() - 20.0).abs() <= 1.0 && hit["end_secs"] == 40.0, "{hit}");
    assert_eq!(hit["steady"], json!(true));

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

/// #685 review: detect_scene_cuts refuses, with the reason, what the app
/// would refuse, rather than answering that it has started.
#[test]
fn detect_scene_cuts_refuses_what_the_app_would() {
    let mut h = harness();
    let clips = h.ok(json!({ "method": "list_clips" }));
    let gap = clips["tracks"][0]["clips"][0]["clip_id"].as_str().unwrap().to_string();
    let r = h.call(json!({ "method": "detect_scene_cuts", "clip_id": gap }));
    assert_eq!(r["ok"], json!(false), "{r}");
    assert!(r["error"].as_str().is_some_and(|e| e.contains("only a video clip")), "{r}");
}

/// #749 review: a clip-alone read that fails (no frame comes; here no
/// app answers at all) still puts the engine back on the whole picture,
/// and, with no playhead to return to, repaints: the solo clip is set,
/// then cleared, then a seek redraws the frame.
#[test]
fn a_failed_clip_alone_read_still_clears_and_repaints() {
    let mut h = harness();
    let fps = h.sequence.read().unwrap().fps;
    let time = |secs| klipp_core::Timecode::from_secs_f64(secs, fps.num);
    let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), time(0.0), time(2.0), time(0.0), time(2.0));
    let id = clip.id;
    {
        let mut seq = h.sequence.write().unwrap();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        seq.tracks[v].clips = vec![clip];
    }
    let reply = h.call(json!({ "method": "grade_to_target", "clip_id": id.0.to_string() }));
    assert_eq!(reply["ok"], json!(false), "no frame can come: {reply}");
    let mut sent = Vec::new();
    while let Ok(cmd) = h.cmd_rx.recv_timeout(std::time::Duration::from_millis(200)) {
        sent.push(cmd);
    }
    let set = sent.iter().position(|c| matches!(c, klipp_core::AppCommand::SetMeasureOnlyClip(Some(c)) if *c == id)).expect("the clip was set alone");
    let cleared = sent.iter().position(|c| matches!(c, klipp_core::AppCommand::SetMeasureOnlyClip(None))).expect("and cleared");
    assert!(cleared > set, "{sent:?}");
    assert!(sent[cleared..].iter().any(|c| matches!(c, klipp_core::AppCommand::SeekTo(_))), "and the frame redrawn: {sent:?}");
}

/// #741: undo and redo say what they took back or put back, by the name
/// the history gives the step, and say so when there is nothing to undo.
#[test]
fn undo_says_what_it_undid() {
    let mut h = harness();
    let r = h.ok(json!({ "method": "undo" }));
    assert_eq!(r["text"], "Nothing to undo.", "{r}");
    h.shared.project.lock().unwrap().undo = Some("Compose".into());
    let r = h.ok(json!({ "method": "undo" }));
    assert!(r["text"].as_str().unwrap().starts_with("Undid \u{201c}Compose\u{201d}."), "{r}");
    assert_eq!(r["undid"], "Compose");
}

/// #740: list_clips reads a clip's sound back, from the clip that holds
/// its settings: a sound linked to a picture keeps them on the picture,
/// and its audio clip shows them too.
#[test]
fn list_clips_reads_the_sound_back() {
    let mut h = harness();
    let fps = h.sequence.read().unwrap().fps;
    let time = |secs| klipp_core::Timecode::from_secs_f64(secs, fps.num);
    let media = klipp_core::MediaId::new();
    let mut picture = klipp_timeline::Clip::media(media, time(0.0), time(2.0), time(0.0), time(2.0));
    let mut sound = klipp_timeline::Clip::media(media, time(0.0), time(2.0), time(0.0), time(2.0));
    picture.linked_clip_id = Some(sound.id);
    sound.linked_clip_id = Some(picture.id);
    picture.volume = 10f32.powf(-14.0 / 20.0);
    picture.fade_out_secs = 0.5;
    picture.role = klipp_core::ClipRole::Ambience;
    picture.audio_effects = vec![klipp_core::AudioEffectInstance { effect_type: "denoiser".into(), enabled: true, wet_dry: 1.0, params: vec![("wind".into(), 1.0)] }];
    let sound_id = sound.id;
    {
        let mut seq = h.sequence.write().unwrap();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        if !seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Audio) {
            seq.tracks.push(klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio));
        }
        let a = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        seq.tracks[v].clips = vec![picture];
        seq.tracks[a].clips = vec![sound];
    }
    let r = h.ok(json!({ "method": "list_clips" }));
    let clip = r["tracks"].as_array().unwrap().iter().flat_map(|t| t["clips"].as_array().unwrap().iter()).find(|c| c["clip_id"] == sound_id.0.to_string()).unwrap_or_else(|| panic!("{r}")).clone();
    let s = &clip["sound"];
    assert_eq!((s["volume_db"].as_f64(), s["fade_out_secs"].as_f64(), s["role"].as_str()), (Some(-14.0), Some(0.5), Some("ambience")), "{clip}");
    assert_eq!(s["effects"], json!(["denoiser"]));
}

/// #729: an op that cannot be read is named by its step and op, with
/// what a clip is named by and where a group grade goes, not as a byte
/// offset in serde's words.
#[test]
fn an_unreadable_op_is_named_by_its_step() {
    let mut h = harness();
    let r = h.call(json!({ "method": "plan", "ops": [
        { "op": "trim", "clip_id": "00000000-0000-0000-0000-000000000000", "edge": "out", "to": { "secs": 1.0 } },
        { "op": "grade", "clip_id": { "group": "piece", "stage": "pre" }, "exposure": 0.1 },
    ] }));
    let e = r["error"].as_str().unwrap();
    assert!(e.starts_with("step 2 (grade) could not be read"), "{e}");
    assert!(e.contains("\"on\""), "{e}");
}

/// #738: a title or type style naming a font that is neither bundled nor
/// installed is warned about, and a bundled one is not.
#[test]
fn a_missing_font_is_warned_about() {
    let mut h = harness();
    let plan = |font: &str| json!({ "method": "plan", "ops": [{ "op": "title", "text": "Hi", "at": { "secs": 0.0 }, "secs": 1.0, "font": font }] });
    let r = h.ok(plan("Nowhere Grotesk Xyz"));
    assert!(r["warnings"][0].as_str().unwrap().contains("Nowhere Grotesk Xyz"), "{r}");
    assert!(r["text"].as_str().unwrap().contains("[warning] font"), "{r}");
    let r = h.ok(plan("Bebas Neue"));
    assert!(r.get("warnings").is_none(), "{r}");
    let fonts = h.ok(json!({ "method": "fonts" }));
    assert!(fonts["bundled"].as_array().unwrap().iter().any(|f| f == "Bebas Neue"), "{fonts}");
}

/// #723: a bass note in a music track is not mains hum under a voice.
/// The same sound — a tone in the voice band with a 115 Hz note in it —
/// reads as hum when it could be dialogue, and as music, with no noise
/// findings, when it is a library track or plays in the music role.
#[test]
fn audio_spectrum_does_not_judge_music_by_the_dialogue_rules() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-spectrum-music-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("piano.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=1000:duration=20",
               "-f", "lavfi", "-i", "sine=frequency=115:duration=20",
               "-filter_complex", "[1]volume=enable='between(t,10,12.5)':volume=6,volume=enable='not(between(t,10,12.5))':volume=0[b];[0][b]amix=inputs=2:normalize=0",
               "-ar", "48000", "-ac", "1"])
        .arg(&wav)
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut item = klipp_media::MediaItem::synthetic_for_test(wav.clone(), None);
    item.has_audio = true;
    item.duration = klipp_core::RationalTime::from_secs_f64(20.0, 30);
    let media = h.media_pool.write().unwrap().add(item);
    let read = |h: &mut Harness| h.ok(json!({ "method": "audio_spectrum", "media_id": media.0.to_string() }));

    let plain = read(&mut h);
    assert_eq!(plain["music"], json!(false));
    assert!(plain["findings"].as_array().unwrap().iter().any(|f| f["what"].as_str().unwrap().starts_with("hum")), "the note reads as hum when it could be dialogue: {plain}");

    // A library track: its credit beside it.
    std::fs::write(wav.with_extension("klipp-credit.txt"), "Piano by Someone, CC BY").unwrap();
    let library = read(&mut h);
    assert_eq!(library["music"], json!(true), "{library}");
    assert!(library["findings"].as_array().unwrap().is_empty(), "{library}");
    let text = library["text"].as_str().unwrap();
    assert!(text.contains("This is music (a library track)") && !text.contains("relative to the voice"), "{library}");
    assert!(!library["next"].as_str().unwrap().contains("clean_audio"), "{library}");

    // Without the credit, a clip of it in the music role.
    std::fs::remove_file(wav.with_extension("klipp-credit.txt")).unwrap();
    {
        let fps = h.sequence.read().unwrap().fps;
        let mut clip = klipp_timeline::Clip::media(media, klipp_core::Timecode::from_secs_f64(0.0, fps.num), klipp_core::Timecode::from_secs_f64(20.0, fps.num), klipp_core::Timecode::from_secs_f64(0.0, fps.num), klipp_core::Timecode::from_secs_f64(20.0, fps.num));
        clip.role = klipp_core::ClipRole::Music;
        let mut seq = h.sequence.write().unwrap();
        if !seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Audio) {
            seq.tracks.push(klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio));
        }
        let a = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        seq.tracks[a].clips = vec![clip];
    }
    let scored = read(&mut h);
    assert_eq!(scored["music"], json!(true), "{scored}");
    assert!(scored["text"].as_str().unwrap().contains("its clips play as music"), "{scored}");
    std::fs::remove_dir_all(&dir).ok();
}

/// #792: the mix metered over the timeline. A quiet tone with a loud
/// half-second burst at 4 s, written as float samples four times past
/// full scale (#812 review): the steps around it stand out from the bed,
/// read unclamped as the export's loudness pass reads them; the ones far
/// from it do not.
#[test]
fn mix_loudness_finds_the_hit_over_the_bed() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-mix-loudness-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("bed-and-hit.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi",
               "-i", "aevalsrc=exprs='if(between(t,4,4.5),3*sin(2*PI*1000*t),0.03*sin(2*PI*440*t))':s=48000:d=8",
               "-ac", "2", "-c:a", "pcm_f32le"])
        .arg(&wav)
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut item = klipp_media::MediaItem::synthetic_for_test(wav.clone(), None);
    item.has_audio = true;
    item.duration = klipp_core::RationalTime::from_secs_f64(8.0, 30);
    let media = h.media_pool.write().unwrap().add(item);
    {
        let fps = h.sequence.read().unwrap().fps;
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, fps.num);
        let clip = klipp_timeline::Clip::media(media, tc(0.0), tc(8.0), tc(0.0), tc(8.0));
        let mut seq = h.sequence.write().unwrap();
        if !seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Audio) {
            seq.tracks.push(klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio));
        }
        let a = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        seq.tracks[a].clips = vec![clip];
    }
    let r = h.ok(json!({ "method": "mix_loudness", "from_secs": 0.0, "to_secs": 8.0, "step_secs": 0.5 }));
    let steps = r["steps"].as_array().unwrap();
    let median = r["median_short_term_lufs"].as_f64().unwrap();
    let stands_out = |s: &serde_json::Value| s["momentary_max_lufs"].as_f64().unwrap() > median + 4.0;
    let at = |t: f64| steps.iter().find(|s| (s["secs"].as_f64().unwrap() - t).abs() < 0.01).unwrap();
    assert!(stands_out(at(4.0)) || stands_out(at(4.5)), "the hit at 4 s: {}", r["text"]);
    assert!(!stands_out(at(1.0)) && !stands_out(at(7.0)), "the bed alone does not: {}", r["text"]);
    assert!(r["text"].as_str().unwrap().contains("stands out"), "{}", r["text"]);
    // Past full scale, read unclamped: the burst meters +6.5 LUFS so, and
    // +2.2 clamped, near-square at full scale; the line sits between.
    let peak = [at(4.0), at(4.5)].iter().map(|s| s["momentary_max_lufs"].as_f64().unwrap()).fold(f64::MIN, f64::max);
    assert!(peak > 4.5, "the burst is metered over full scale, not clamped: {peak:.1} LUFS (+6.5 unclamped, +2.2 clamped)");
    std::fs::remove_dir_all(&dir).ok();
}

/// #503: a music track's sections and a fit to a length. A 120 BPM click
/// over a tone, quiet, then loud for two phrases, then quiet: the read
/// names the loud stretch, and a fit to 32 s gives place ops that put
/// the fitted track on the timeline, its start and end kept, and a
/// crossfade on the join (#1575).
#[test]
fn beats_reads_music_sections_and_fits_a_track_to_a_length() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-music-fit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("bed.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
               "aevalsrc='0.6*lt(mod(t\\,0.5)\\,0.02)*sin(2*PI*1000*t)+0.4*sin(2*PI*220*t)*if(between(t\\,16\\,32)\\,1\\,0.08)':s=22050:d=48",
               "-ac", "1"])
        .arg(&wav)
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut item = klipp_media::MediaItem::synthetic_for_test(wav.clone(), None);
    item.has_audio = true;
    item.video_codec = String::new();
    item.duration = klipp_core::RationalTime::from_secs_f64(48.0, 30);
    let media = h.media_pool.write().unwrap().add(item);
    {
        let mut seq = h.sequence.write().unwrap();
        if !seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Audio) {
            seq.tracks.push(klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio));
        }
    }
    let r = h.ok(json!({ "method": "beats", "media_id": media.0.to_string(), "target_secs": 32.0 }));
    assert!((r["bpm"].as_f64().unwrap() - 120.0).abs() < 6.0, "{r}");
    let sections = r["sections"].as_array().unwrap();
    assert!(sections.iter().any(|s| s["kind"] == "peak" && (s["start_secs"].as_f64().unwrap() - 16.0).abs() < 1.0), "the loud phrases are the peak: {r}");
    assert!((r["fit"]["length_secs"].as_f64().unwrap() - 32.0).abs() <= 2.0, "within a bar of the target: {r}");
    let ops = r["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 3, "{r}");
    assert_eq!(ops[0]["source_in"], json!(0.0), "it starts as it did: {r}");
    assert!(ops[2]["op"] == "transition" && ops[2]["clip_id"] == "@0", "the join crossfaded, on the first clip: {r}");
    // The ops apply as they are: two clips of the track, back to back,
    // crossfaded where they meet.
    let v = h.ok(json!({ "method": "apply", "ops": r["ops"] }));
    assert_eq!(v["applied"], json!(true), "{v}");
    let edits = h.edits_seen.lock().unwrap().clone();
    assert_eq!(edits.len(), 3, "two clips placed and a crossfade: {v}");
    assert!(format!("{v}").contains("Crossfade of 0.25 s"), "an audio track's transition is a crossfade: {v}");
    std::fs::remove_dir_all(&dir).ok();
}

/// #1584: a music bed with a section left 11 dB under the rest is a
/// loudness_drop warning where the section is, when the check is asked
/// to meter the mix; faded out there, it is a quiet moment meant as one
/// and nothing is said. A plain check doesn't meter, and says how to.
#[test]
fn check_warns_on_a_section_well_under_the_rest() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-loudness-drop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("bed.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
               "aevalsrc='0.5*sin(2*PI*440*t)*if(between(t\\,12\\,20)\\,0.28\\,1)':s=48000:d=30",
               "-ac", "2"])
        .arg(&wav)
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut item = klipp_media::MediaItem::synthetic_for_test(wav.clone(), None);
    item.has_audio = true;
    item.video_codec = String::new();
    item.duration = klipp_core::RationalTime::from_secs_f64(30.0, 30);
    let media = h.media_pool.write().unwrap().add(item);
    let time = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 30);
    let bed = |fade_out: f32| {
        let mut c = klipp_timeline::Clip::media(media, time(0.0), time(20.0), time(0.0), time(20.0));
        c.role = klipp_core::ClipRole::Music;
        c.fade_out_secs = fade_out;
        let mut track = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        track.clips = vec![c];
        track
    };
    h.sequence.write().unwrap().tracks = vec![klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video), bed(0.0)];
    let drops = |r: &serde_json::Value| r["findings"].as_array().unwrap().iter().filter(|f| f["rule"] == "loudness_drop").cloned().collect::<Vec<_>>();
    let plain = h.ok(json!({ "method": "check" }));
    assert!(drops(&plain).is_empty(), "a plain check doesn't mix the piece: {plain}");
    assert!(plain["findings"].as_array().unwrap().iter().any(|f| f["rule"] == "loudness_unmetered" && f["hint"].as_str().unwrap().contains("loudness: true")), "{plain}");
    let r = h.ok(json!({ "method": "check", "loudness": true }));
    let found = drops(&r);
    assert_eq!(found.len(), 1, "{r}");
    let at = found[0]["at_secs"].as_f64().unwrap();
    assert!((12.0..=15.0).contains(&at), "the drop is where the quiet section is: {r}");
    assert!(found[0]["what"].as_str().unwrap().contains("LU under the rest"), "{r}");

    // Faded out over the quiet section, it is meant.
    h.sequence.write().unwrap().tracks[1] = bed(8.0);
    let r = h.ok(json!({ "method": "check", "loudness": true }));
    assert!(drops(&r).is_empty(), "a fade there is meant: {r}");
    std::fs::remove_dir_all(&dir).ok();
}

/// #1577: the sequence's mix heard over the socket. A tone bed ducked
/// 12 dB from 4 to 6 s of a timeline, with a second tone clip from 8 s:
/// the excerpt of 2-10 s, with no media, is 8 s of 16 kHz mono whose
/// level follows the duck and rises where the second clip joins, as the
/// export mixes it. A file's excerpt and the timeline's at once is refused.
#[test]
fn audio_excerpt_hears_the_sequences_mix() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-mix-excerpt-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("tone.wav");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=12", "-ac", "2"])
        .arg(&wav)
        .status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut item = klipp_media::MediaItem::synthetic_for_test(wav.clone(), None);
    item.has_audio = true;
    item.video_codec = String::new();
    item.duration = klipp_core::RationalTime::from_secs_f64(12.0, 30);
    let media = h.media_pool.write().unwrap().add(item);
    let time = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 30);
    let mut bed = klipp_timeline::Clip::media(media, time(0.0), time(12.0), time(0.0), time(12.0));
    let low = 10f32.powf(-12.0 / 20.0);
    for (t, v) in [(3.5, 1.0), (4.0, low), (6.0, low), (6.5, 1.0)] {
        klipp_core::keyframe::upsert_keyframe(&mut bed.volume_keyframes, t, v);
    }
    let second = klipp_timeline::Clip::media(media, time(8.0), time(12.0), time(0.0), time(4.0));
    let track = |clips| {
        let mut t = klipp_timeline::Track::new("A", klipp_timeline::TrackKind::Audio);
        t.clips = clips;
        t
    };
    h.sequence.write().unwrap().tracks = vec![klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video), track(vec![bed]), track(vec![second])];

    let out = dir.join("mix.wav");
    let r = h.ok(json!({ "method": "audio_excerpt", "from_secs": 2.0, "to_secs": 10.0, "path": out.to_string_lossy() }));
    assert!(r["source"].as_str().unwrap().contains("mix"), "{r}");
    let bytes = std::fs::read(&out).unwrap();
    let data = bytes.windows(4).position(|w| w == b"data").expect("a data chunk") + 8;
    let samples: Vec<f32> = bytes[data..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    assert!((samples.len() as f64 - 8.0 * 16000.0).abs() < 160.0, "8 s at 16 kHz: {} samples", samples.len());
    // Seconds of the timeline, read from the excerpt that starts at 2 s.
    let rms = |a: f64, b: f64| {
        let s = &samples[((a - 2.0) * 16000.0) as usize..((b - 2.0) * 16000.0) as usize];
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    };
    let (before, ducked, after, both) = (rms(2.5, 3.4), rms(4.2, 5.8), rms(6.7, 7.8), rms(8.2, 9.8));
    assert!(before > 0.01, "the bed is heard: {before}");
    let ratio = ducked / before;
    assert!((ratio - low).abs() < 0.05, "ducked 12 dB, as the export mixes it: {ratio:.3} of the level, not {low:.3}");
    assert!((after / before - 1.0).abs() < 0.05, "back up after the duck: {after} against {before}");
    assert!(both > after * 1.5, "the second clip joins the mix at 8 s: {both} against {after}");

    let both_asked = h.call(json!({ "method": "audio_excerpt", "media_id": media.0.to_string(), "sequence": true, "from_secs": 0.0, "to_secs": 1.0 }));
    assert!(both_asked["error"].as_str().unwrap().contains("not both"), "{both_asked}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn add_node_refuses_an_unknown_kind_and_names_the_node_it_adds() {
    let mut h = harness();
    let clip = h.ids.clip_a.clone();

    // A kind no node has is refused with the kinds there are, and
    // nothing reaches the app (#1167).
    let reply = h.call(json!({ "method": "add_node", "clip_id": clip, "kind": "Glow" }));
    assert_eq!(reply["ok"], json!(false), "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("\"Glow\"") && error.contains("Corrector") && error.contains("RGBCurves"), "{error}");
    assert!(h.cmd_rx.try_recv().is_err(), "a refused add_node sends nothing");

    // A known kind is sent with the id the reply names (#1168).
    let result = h.ok(json!({ "method": "add_node", "clip_id": clip, "kind": "LumaKey", "x": 10.0 }));
    let said = result["node_id"].as_str().expect("the reply names the node").to_string();
    match h.cmd_rx.try_recv() {
        Ok(klipp_core::AppCommand::AddNode { kind, node_id, position, .. }) => {
            assert_eq!(kind, "LumaKey");
            assert_eq!(node_id.map(|n| n.to_string()), Some(said));
            assert_eq!(position, (10.0, 0.0));
        }
        other => panic!("expected AddNode, got {other:?}"),
    }
}

#[test]
fn perf_reports_the_presented_rate_only_while_pictures_change() {
    let mut h = harness();
    // The same picture time published again, as a paused viewer does:
    // no rate (#1169).
    for _ in 0..5 {
        h.frame_buffer.lock().unwrap().set_presented(klipp_core::RationalTime::from_frames(7, 25), true);
    }
    assert_eq!(h.ok(json!({ "method": "perf" }))["presented_fps"], Value::Null);
    for f in 8..20 {
        h.frame_buffer.lock().unwrap().set_presented(klipp_core::RationalTime::from_frames(f, 25), true);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let fps = h.ok(json!({ "method": "perf" }))["presented_fps"].as_f64().expect("a rate while pictures change");
    assert!(fps.is_finite() && fps > 0.0, "{fps}");
}

/// #1850: perf splits the viewer frame's time into its phases around the
/// render, averaged over the last frames, so a slow stretch can be read
/// apart on a running app.
#[test]
fn perf_gives_the_viewer_frames_phases() {
    let mut h = harness();
    let phases = h.ok(json!({ "method": "perf" }))["average_of_last_60"]["phases"].clone();
    for key in ["sync_ms", "snapshot_ms", "frame_snapshot_ms", "render_ms", "present_ms"] {
        assert!(phases[key].is_number(), "phases.{key}: {phases}");
    }
}

/// Perf's plan_effects says how many effect runs fell to the CPU, how many
/// masked effects were bypassed for a matte their run couldn't give, and
/// how many frames the GPU stack declined: cumulative counts a reader
/// takes deltas of (M2 step 9c-2, for #1212's acceptance run).
#[test]
fn perf_counts_cpu_fallbacks_bypassed_masks_and_stack_declines() {
    let mut h = harness();
    let plan = h.ok(json!({ "method": "perf" }))["plan_effects"].clone();
    for key in ["clips", "fallback_lists", "on_cpu", "on_cpu_kernels", "mask_unavailable", "stack_declines", "cold_lifts"] {
        assert!(plan[key].is_u64(), "plan_effects.{key}: {plan}");
    }
}

/// An edge joins nodes of one graph: a top-level node and one inside a
/// Compound are refused, with where each is, and nothing reaches the
/// app (#1179). Two nodes of one graph, top-level or inside the same
/// Compound, are sent.
#[test]
fn connect_nodes_refuses_nodes_of_different_graphs() {
    let mut h = harness();
    let (clip, top, nested) = (h.ids.clip_a.clone(), h.ids.corrector_a.clone(), h.ids.nested_in_a.clone());
    let reply = h.call(json!({ "method": "connect_nodes", "clip_id": clip, "from": top, "to": nested, "to_port": 0 }));
    assert_eq!(reply["ok"], json!(false), "{reply}");
    let error = reply["error"].as_str().unwrap_or_default();
    assert!(error.contains("one graph") && error.contains("top-level") && error.contains("Compound"), "{error}");
    assert!(h.cmd_rx.try_recv().is_err(), "a refused edge sends nothing");
    h.ok(json!({ "method": "connect_nodes", "clip_id": clip, "from": top, "to": top, "to_port": 1 }));
    assert!(matches!(h.cmd_rx.try_recv(), Ok(klipp_core::AppCommand::ConnectNodes { .. })));
    // Inside the same Compound: one graph, sent (the app edits that graph).
    h.ok(json!({ "method": "connect_nodes", "clip_id": clip, "from": nested, "to": nested, "to_port": 1 }));
    assert!(matches!(h.cmd_rx.try_recv(), Ok(klipp_core::AppCommand::ConnectNodes { .. })));
}

/// `status` reports the edit history's reach and the inspector's view as
/// the app publishes them (M3 step 18's harness reads them to check that
/// opening inspectors writes nothing), and reading it writes nothing.
#[test]
fn status_reports_history_and_the_inspector_view_and_writes_nothing() {
    let mut h = harness();
    {
        let mut p = h.shared.project.lock().unwrap();
        p.undo_depth = 3;
        p.redo_depth = 1;
        p.undo = Some("Add Effect".into());
        p.redo = Some("Remove Clip".into());
    }
    let before = h.shared.revision();
    let a = h.ok(json!({"method": "status"}));
    let b = h.ok(json!({"method": "status"}));
    assert_eq!(a["history"], json!({"undo_depth": 3, "redo_depth": 1, "next_undo": "Add Effect", "next_redo": "Remove Clip"}));
    assert_eq!(a["revision"], b["revision"]);
    assert_eq!(h.shared.revision(), before, "reading the status writes nothing");
    assert!(h.cmd_rx.try_recv().is_err(), "and sends the app nothing");
}


/// An agent's effect edit, planned from the socket's copy of a clip
/// whose track has effects, leaves the clip's own effects under the node
/// ids the document has, so a path read before the edit still names the
/// same node after it and after its undo (#1376). The socket plans from
/// that copy with its track's effects taken back out, which used to give
/// every effect a new id that the edit then wrote into the document.
#[test]
fn an_effect_op_on_a_clip_with_track_effects_keeps_its_effects_node_ids() {
    use klipp_effects::BuiltinEffect as E;
    let mut h = harness();
    let clip = h.ids.clip_a.clone();
    let mut doc: klipp_timeline::Sequence = (*h.sequence.read().unwrap()).clone();
    {
        let track = &mut doc.tracks[0];
        track.effects.set_video_effect_list(vec![E::Sharpen { amount: 0.5 }], vec![true]);
        let c = &mut track.clips[0];
        c.kind = klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() };
        *c = klipp_timeline::test_clip::TestClip::of(c.clone()).effect(E::GaussianBlur { radius: 4.0 }).build();
    }
    let blur = doc.tracks[0].clips[0].effect_id_list()[0];
    // What the socket reads, as the app hands it over: flattened, with
    // the track's effects baked onto its clips.
    *h.sequence.write().unwrap() = klipp_timeline::flatten::flatten_sequence(&doc, &[doc.clone()]);

    // List, and take the Blur's path from the reading.
    let read = h.ok(json!({"method":"graph_read","path":{"root":{"clip":clip},"nodes":[]}}));
    let listed = read["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.fx.gaussian_blur")
        .unwrap_or_else(|| panic!("the Blur is listed: {read}"))["id"].as_str().unwrap().to_owned();
    assert_eq!(listed, blur.as_uuid().to_string(), "the listed Blur is the document's node");

    // Edit by an op the socket plans from its copy, then play the edit
    // on the document, as the app does.
    h.ok(json!({"method":"apply","expected_revision":h.shared.revision(),"ops":[{"op":"effect","clip_id":clip,"kind":"grain"}]}));
    let edits = h.edits_seen.lock().unwrap().clone();
    assert_eq!(edits.len(), 1, "one edit: {edits:?}");
    let mut sequences = vec![doc];
    let mut pool = h.media_pool.read().unwrap().clone();
    let mut fps = sequences[0].fps;
    let ids = |s: &[klipp_timeline::Sequence]| s[0].tracks[0].clips[0].effect_id_list();
    {
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edits[0]);
    }
    let after = ids(&sequences);
    assert_eq!(klipp_timeline::test_clip::effects(&sequences[0].tracks[0].clips[0]).len(), 2, "the grain was added");
    assert_eq!(after[0], blur, "the Blur keeps its node id, so the path read before still names it");
    {
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_inverse(&edits[0]);
    }
    assert_eq!(ids(&sequences), vec![blur], "and its undo leaves the Blur as it was");
}

/// `status` lists every sequence, the open one marked, and
/// `switch_sequence` opens another by index or id, as its tab does (M3
/// step 18): a client can reach each sequence's clips.
#[test]
fn status_lists_the_sequences_and_switch_sequence_opens_one() {
    let mut h = harness();
    {
        let mut p = h.shared.project.lock().unwrap();
        p.sequences = vec![
            klipp_core::SequenceSummary { id: "a".into(), name: "Main".into(), clips: 3, used_in: None },
            klipp_core::SequenceSummary { id: "b".into(), name: "Social cut".into(), clips: 1, used_in: None },
        ];
        p.active_sequence = 0;
    }
    let status = h.ok(json!({"method": "status"}));
    assert_eq!(status["sequences"], json!([
        {"index": 0, "id": "a", "name": "Main", "clips": 3, "active": true},
        {"index": 1, "id": "b", "name": "Social cut", "clips": 1, "active": false},
    ]));
    h.ok(json!({"method": "switch_sequence", "index": 1}));
    h.ok(json!({"method": "switch_sequence", "sequence_id": "a"}));
    let switched: Vec<usize> = h.sent.lock().unwrap().iter().filter_map(|c| match c {
        klipp_core::AppCommand::SetActiveSequence { sequence_idx } => Some(*sequence_idx),
        _ => None,
    }).collect();
    assert_eq!(switched, [1, 0]);
    for (bad, why) in [
        (json!({"method": "switch_sequence", "index": 2}), "no such sequence"),
        (json!({"method": "switch_sequence", "sequence_id": "c"}), "no such sequence"),
        (json!({"method": "switch_sequence"}), "index or its sequence_id"),
        (json!({"method": "switch_sequence", "index": 0, "sequence_id": "a"}), "index or its sequence_id"),
    ] {
        let reply = h.call(bad.clone());
        assert_eq!(reply["ok"], json!(false), "{bad}");
        assert!(reply["error"].as_str().unwrap_or_default().contains(why), "{bad}: {}", reply["error"]);
    }
    assert_eq!(h.sent.lock().unwrap().len(), 2, "a refused switch sends nothing");
}

/// `edit_from_selects` (#2385) through `apply`: the socket plans it
/// against the other sequences the UI published, the edit is one step,
/// and the envelope names the sequence copied from and the content it
/// read, which the UI checks before applying. `plan` sees them too; an
/// edit plan's ops don't.
#[test]
fn edit_from_selects_names_the_sequence_and_content_it_copied() {
    let mut h = harness();
    let mut selects = klipp_timeline::Sequence::new_default();
    selects.name = "Selects".into();
    let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    let at = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    track.clips = vec![klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(0.0), at(2.0), at(10.0), at(12.0))];
    selects.tracks = vec![track];
    let content = klipp_document::clipboard::content_revision(&selects);
    *h.others.write().unwrap() = vec![klipp_document::ops::Selects { sequence: Arc::new(selects.clone()), content }];
    let first = selects.tracks.iter().flat_map(|t| &t.clips).min_by_key(|c| c.timeline_in).expect("a clip");
    let (a, b) = (first.timeline_in.to_secs_f64(), first.timeline_out.to_secs_f64());
    let op = json!([{"op": "edit_from_selects", "from": "Selects", "from_secs": a, "to_secs": b, "at": {"secs": 0.0}}]);
    let planned = h.ok(json!({"method": "plan", "ops": op}));
    assert!(planned["text"].as_str().unwrap_or_default().contains("from \"Selects\""), "{planned}");
    h.ok(json!({"method": "apply", "ops": op}));
    assert_eq!(h.edits_seen.lock().unwrap().len(), 1, "one step");
    assert_eq!(*h.sources_seen.lock().unwrap(), vec![(selects.id, content)]);
}

/// `remove_sequence` (#2366) removes one as its tab's Delete does, only
/// once confirmed: unconfirmed, it is refused naming the sequence and
/// what it holds. The project's only sequence, and one held as a
/// compound clip (which `status` says), are refused outright.
#[test]
fn remove_sequence_is_refused_until_confirmed_and_never_for_a_held_or_only_one() {
    let mut h = harness();
    let set = |h: &mut Harness, list: Vec<klipp_core::SequenceSummary>| h.shared.project.lock().unwrap().sequences = list;
    let summary = |id: &str, name: &str, clips, used_in: Option<&str>| klipp_core::SequenceSummary { id: id.into(), name: name.into(), clips, used_in: used_in.map(Into::into) };
    let (a, b, c, d) = ("00000000-0000-4000-8000-00000000000a", "00000000-0000-4000-8000-00000000000b", "00000000-0000-4000-8000-00000000000c", "00000000-0000-4000-8000-00000000000d");
    set(&mut h, vec![summary(a, "Main", 3, None), summary(b, "Demo", 2, None), summary(c, "Nest", 1, Some("Main")), summary(d, "Spare", 0, None)]);
    let refused = |h: &mut Harness, cmd: Value, why: &str| {
        let reply = h.call(cmd.clone());
        assert_eq!(reply["ok"], json!(false), "{cmd}");
        assert!(reply["error"].as_str().unwrap_or_default().contains(why), "{cmd}: {}", reply["error"]);
    };
    refused(&mut h, json!({"method": "remove_sequence", "sequence_id": b}), "\"Demo\" holds 2 clip(s)");
    refused(&mut h, json!({"method": "remove_sequence", "index": 2, "confirm": true}), "compound clip in \"Main\"");
    refused(&mut h, json!({"method": "remove_sequence", "index": 5, "confirm": true}), "no such sequence");
    assert_eq!(h.ok(json!({"method": "status"}))["sequences"][2]["used_in"], json!("Main"));
    assert!(h.sent.lock().unwrap().is_empty(), "a refused removal sends nothing");
    // Two in a batch: each by its id, so the second names the one meant
    // whatever the first moves.
    h.ok(json!({"method": "batch", "commands": [
        {"method": "remove_sequence", "index": 1, "confirm": true},
        {"method": "remove_sequence", "index": 3, "confirm": true},
    ]}));
    let id = |s: &str| klipp_core::SequenceId(s.parse().unwrap());
    let removed: Vec<_> = h.sent.lock().unwrap().iter().filter_map(|c| match c {
        klipp_core::AppCommand::DeleteSequence { sequence_id } => Some(*sequence_id),
        _ => None,
    }).collect();
    assert_eq!(removed, [id(b), id(d)]);
    set(&mut h, vec![summary(a, "Main", 3, None)]);
    refused(&mut h, json!({"method": "remove_sequence", "index": 0, "confirm": true}), "only sequence");
}

/// `export_timeline` (#2434) writes the open sequence as the extension
/// says, the counterpart of `import_timeline`; a relative path, a name no
/// format has, a format that is not the path's, a file there already
/// (until overwrite) and a batch are refused, writing nothing.
#[test]
fn export_timeline_writes_the_open_sequence_as_its_extension_says() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-export-timeline-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let clips = h.sequence.read().unwrap().tracks.iter().flat_map(|t| &t.clips).filter(|c| !c.kind.is_gap()).count();
    for (ext, format) in [("fcpxml", "fcpxml"), ("edl", "edl"), ("otio", "otio"), ("kdenlive", "kdenlive")] {
        let path = dir.join(format!("cut.{ext}"));
        let reply = h.ok(json!({"method": "export_timeline", "path": path, "format": format}));
        assert_eq!((reply["format"].clone(), reply["clips"].clone()), (json!(format), json!(clips)), "{reply}");
        assert!(std::fs::metadata(&path).is_ok_and(|m| m.len() > 0), "{ext} written");
    }
    let otio = dir.join("cut.otio");
    let refused = |h: &mut Harness, cmd: Value, why: &str| {
        let reply = h.call(cmd.clone());
        assert_eq!(reply["ok"], json!(false), "{cmd}");
        assert!(reply["error"].as_str().unwrap_or_default().contains(why), "{cmd}: {}", reply["error"]);
    };
    refused(&mut h, json!({"method": "export_timeline", "path": "cut.fcpxml"}), "absolute");
    refused(&mut h, json!({"method": "export_timeline", "path": dir.join("cut.mov")}), "names no interchange format");
    refused(&mut h, json!({"method": "export_timeline", "path": dir.join("other.edl"), "format": "fcpxml"}), "not the path's");
    refused(&mut h, json!({"method": "export_timeline", "path": otio}), "overwrite true");
    refused(&mut h, json!({"method": "batch", "commands": [{"method": "export_timeline", "path": dir.join("b.edl")}]}), "batch");
    assert!(!dir.join("other.edl").exists() && !dir.join("b.edl").exists(), "a refusal writes nothing");
    h.ok(json!({"method": "export_timeline", "path": otio, "overwrite": true}));
    assert!(h.sent.lock().unwrap().is_empty(), "an export sends the app nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// #2458: a write through a picture-truth clip's fields that the app
/// refused while applying a command is in that command's reply, the rest
/// of it applied.
#[test]
fn a_refused_picture_write_is_in_the_reply() {
    let mut h = harness();
    *h.picture_refusal.lock().unwrap() = Some("Shot 1: an effect's settings not done: its mask would be lost".into());
    let reply = h.ok(json!({"method": "rename_sequence", "name": "Cut"}));
    assert_eq!(reply["applied"], json!(true), "{reply}");
    assert_eq!(reply["picture_write_refused"], json!("Shot 1: an effect's settings not done: its mask would be lost"), "{reply}");
    let reply = h.ok(json!({"method": "rename_sequence", "name": "Cut 2"}));
    assert!(reply.get("picture_write_refused").is_none(), "only the envelope it was refused in: {reply}");
}

/// `duplicate_sequence` (#2433) copies the open sequence, or one by index
/// or id, as its tab's Duplicate does, named "<name> copy" unless named,
/// and opens the copy, which goes last; not in a batch.
#[test]
fn duplicate_sequence_copies_and_opens_the_copy() {
    let mut h = harness();
    let summary = |id: &str, name: &str| klipp_core::SequenceSummary { id: id.into(), name: name.into(), clips: 1, used_in: None };
    let (a, b) = ("00000000-0000-4000-8000-00000000000a", "00000000-0000-4000-8000-00000000000b");
    {
        let mut p = h.shared.project.lock().unwrap();
        p.sequences = vec![summary(a, "Main"), summary(b, "Demo")];
        p.active_sequence = 1;
    }
    let reply = h.ok(json!({"method": "duplicate_sequence"}));
    assert_eq!((reply["index"].clone(), reply["name"].clone()), (json!(2), json!("Demo copy")), "{reply}");
    let sent = std::mem::take(&mut *h.sent.lock().unwrap());
    assert!(matches!(&sent[..], [klipp_core::AppCommand::DuplicateSequence { sequence_idx: 1, new_name }, klipp_core::AppCommand::SetActiveSequence { sequence_idx: 2 }] if new_name == "Demo copy"), "{sent:?}");
    h.ok(json!({"method": "duplicate_sequence", "sequence_id": a, "name": "Vertical", "open": false}));
    let sent = std::mem::take(&mut *h.sent.lock().unwrap());
    assert!(matches!(&sent[..], [klipp_core::AppCommand::DuplicateSequence { sequence_idx: 0, new_name }] if new_name == "Vertical"), "{sent:?}");
    for (cmd, why) in [
        (json!({"method": "duplicate_sequence", "index": 4}), "no such sequence"),
        (json!({"method": "duplicate_sequence", "index": 0, "name": " "}), "needs a name"),
        (json!({"method": "batch", "commands": [{"method": "duplicate_sequence"}]}), "not part of a batch"),
    ] {
        let reply = h.call(cmd.clone());
        assert_eq!(reply["ok"], json!(false), "{cmd}");
        assert!(reply["error"].as_str().unwrap_or_default().contains(why), "{cmd}: {}", reply["error"]);
    }
    assert!(h.sent.lock().unwrap().is_empty(), "a refusal sends nothing");
}

/// #1516: a subtitle file is read over the socket as the app reads it, onto
/// a new track; a path that is relative, names no subtitle format or is not
/// there is refused, and an export's format must be its path's.
/// translate_captions says what it cannot do before any model runs
/// (#2213): no language to translate into, and a sequence with no
/// captions; into English, the captions' language comes from `from` or the
/// track's tag (#2489), so with no captions that is what is said.
#[test]
fn translate_captions_refuses_what_it_cannot_translate() {
    let mut h = harness();
    for (req, why) in [
        (json!({ "method": "translate_captions", "to": "xx" }), "no translation into xx"),
        (json!({ "method": "translate_captions", "to": "en" }), "no subtitle track with captions"),
        (json!({ "method": "translate_captions", "to": "de" }), "no subtitle track with captions"),
        (json!({ "method": "translate_captions", "to": "de", "track": 99 }), "no track 99"),
    ] {
        let r = h.call(req.clone());
        assert_eq!(r["ok"], json!(false), "{req}: {r}");
        assert!(r["error"].as_str().unwrap().contains(why), "{req}: {r}");
    }
}

/// A translation over the socket runs as the menu's does (#2213): the
/// app is sent the track's command with a job id, and the reply gives
/// that id to follow.
#[test]
fn translate_captions_hands_the_track_to_the_app_as_a_job() {
    let mut h = harness();
    let track_id = {
        let mut seq = h.sequence.write().unwrap();
        let mut track = klipp_timeline::Track::new("Subtitle 1", klipp_timeline::TrackKind::Subtitle);
        let (zero, two) = (klipp_core::Timecode::from_secs_f64(0.0, 24), klipp_core::Timecode::from_secs_f64(2.0, 24));
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello there.".into(), ..Default::default() });
        track.clips.push(klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(zero, two) });
        let id = track.id;
        seq.tracks.push(track);
        id
    };
    let r = h.ok(json!({ "method": "translate_captions", "to": "de", "download_models": true }));
    let job_id = r["job_id"].as_str().expect("a job id").to_string();
    assert_eq!(r["sentences"], json!(1));
    match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(klipp_core::AppCommand::TranslateCaptions { track_id: t, source, target, job_id: j, .. }) => {
            assert_eq!((t, source.as_str(), target.as_str(), j.as_str()), (track_id, "en", "de", job_id.as_str()));
        }
        other => panic!("expected TranslateCaptions, got {other:?}"),
    }
}

#[test]
fn subtitles_import_over_the_socket_and_export_in_the_format_asked() {
    let mut h = harness();
    let dir = std::env::temp_dir().join(format!("klipp-subs-roundtrip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let srt = dir.join("talk.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nHello\n\n").unwrap();
    h.ok(json!({ "method": "import_subtitles", "path": srt.to_str().unwrap() }));
    match h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(klipp_core::AppCommand::ImportSubtitles { path }) => assert_eq!(path, srt),
        other => panic!("expected ImportSubtitles, got {other:?}"),
    }
    for (path, why) in [
        ("talk.srt".to_string(), "absolute"),
        (dir.join("talk.txt").to_string_lossy().into_owned(), "not a subtitle file"),
        (dir.join("gone.vtt").to_string_lossy().into_owned(), "no file"),
    ] {
        let r = h.call(json!({ "method": "import_subtitles", "path": path }));
        assert_eq!(r["ok"], json!(false), "{path}: {r}");
        assert!(r["error"].as_str().unwrap().contains(why), "{path}: {r}");
    }
    let out = dir.join("out.vtt");
    h.ok(json!({ "method": "export_subtitles", "path": out.to_str().unwrap(), "format": "WebVTT" }));
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(klipp_core::AppCommand::ExportSubtitles { .. })));
    // TTML's extensions are one format, IMSC by another name.
    for (path, format) in [("out.ttml", "imsc"), ("out.dfxp", "ttml"), ("out.itt", "IMSC"), ("out.scc", "scc"), ("out.stl", "STL")] {
        h.ok(json!({ "method": "export_subtitles", "path": dir.join(path).to_str().unwrap(), "format": format }));
        assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(klipp_core::AppCommand::ExportSubtitles { .. })), "{path}");
    }
    let ttml = dir.join("talk.ttml");
    std::fs::write(&ttml, "<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p begin=\"0s\" end=\"1s\">Hello</p></div></body></tt>").unwrap();
    h.ok(json!({ "method": "import_subtitles", "path": ttml.to_str().unwrap() }));
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(klipp_core::AppCommand::ImportSubtitles { .. })));
    for (path, format, why) in [("out.ttml", Some("srt"), "ends .ttml"), ("out.srt", Some("vtt"), "ends .srt"), ("out.txt", None, "names no subtitle format"), ("out.srt", Some("sub"), "not a subtitle format"), ("out.scc", Some("ttml"), "ends .scc")] {
        let mut req = json!({ "method": "export_subtitles", "path": dir.join(path).to_str().unwrap() });
        if let Some(f) = format {
            req["format"] = json!(f);
        }
        let r = h.call(req.clone());
        assert_eq!(r["ok"], json!(false), "{req}: {r}");
        assert!(r["error"].as_str().unwrap().contains(why), "{req}: {r}");
    }
    assert!(h.cmd_rx.try_recv().is_err(), "a refused call sends nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A grade parameter keyed over the socket (#891): `at` (timeline
/// seconds) keys it at the clip's own time, in one envelope bracketed as
/// one undo step, its interpolation or ease set on that key; `remove`
/// takes the key away; a time off the clip, or a shape Klipp doesn't
/// know, is refused and sends nothing. Without `at` it sets the value, as
/// before.
#[test]
fn set_node_param_keys_a_grade_value_at_a_time() {
    use klipp_core::AppCommand as C;
    let mut h = harness();
    let (clip, node) = (h.ids.clip_a.clone(), h.ids.corrector_a.clone());
    let drain = |h: &mut Harness| -> Vec<C> { std::iter::from_fn(|| h.cmd_rx.try_recv().ok()).collect() };
    drain(&mut h);

    let reply = h.ok(json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "value": 1.5, "at": 1.5, "interpolation": "ease_in" }));
    assert_eq!(reply["key"]["clip_secs"], json!(1.5), "{reply}");
    let sent = drain(&mut h);
    assert!(matches!(sent.as_slice(), [
        C::BeginNodeParamEdit { .. },
        C::AddKeyframe { param, time_secs, value, .. },
        C::SetKeyframeEase { ease: klipp_core::KeyEase::In, .. },
        C::EndNodeParamEdit { .. },
    ] if param == "exposure" && (*time_secs - 1.5).abs() < 1e-9 && (*value - 1.5).abs() < 1e-6), "{sent:?}");
    assert_eq!(h.envelopes.lock().unwrap().last().copied(), Some((false, 4)), "one envelope, one step");

    h.ok(json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "value": 0.0, "at": 0.5, "interpolation": "hold" }));
    let sent = drain(&mut h);
    assert!(sent.iter().any(|c| matches!(c, C::SetKeyframeInterpolation { interpolation: klipp_core::Interpolation::Step, .. })), "{sent:?}");

    h.ok(json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "at": 1.5, "remove": true }));
    let sent = drain(&mut h);
    assert!(matches!(sent.as_slice(), [C::BeginNodeParamEdit { .. }, C::RemoveKeyframe { time_secs, .. }, C::EndNodeParamEdit { .. }] if (*time_secs - 1.5).abs() < 1e-9), "{sent:?}");

    for (req, says) in [
        (json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "value": 1.0, "at": 9.0 }), "off the clip"),
        (json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "value": 1.0, "at": 1.0, "interpolation": "wobbly" }), "no interpolation"),
        (json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "remove": true }), "give at"),
    ] {
        let reply = h.call(req);
        assert_eq!(reply["ok"], json!(false), "{reply}");
        assert!(reply["error"].as_str().unwrap_or_default().contains(says), "{reply}");
        assert!(drain(&mut h).is_empty(), "a refusal sent something");
    }

    // Without `at`: the value, as before.
    h.ok(json!({ "method": "set_node_param", "clip_id": clip, "node_id": node, "param": "exposure", "value": 0.25 }));
    assert!(matches!(drain(&mut h).as_slice(), [C::SetNodeParam { value, .. }] if (*value - 0.25).abs() < 1e-6));
}

/// #1912: an effect added with a label keeps it, through the stored
/// picture and undo, and is an effect the list holds: its entry is there,
/// and a later edit of it through the list keeps the label.
#[test]
fn graph_an_effect_s_label_is_kept_with_its_entry() {
    let mut h = harness();
    let root = json!({"clip":h.ids.clip_a});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let display = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.colour.scene_to_display").unwrap()["id"].clone();
    let after = json!({"root":root,"nodes":[display]});
    graph_test_edit_roundtrip(&mut h, json!([{"op":"add","after":after,"type":"klipp.fx.gaussian_blur","label":"Soft focus","params":{"radius":4.0}}]));
    let read = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let blur = read["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.fx.gaussian_blur").expect("the effect").clone();
    assert_eq!(blur["label"], "Soft focus", "{blur}");
    // An entry in the clip's list, as an unnamed add makes one: its keys,
    // its row in the inspector.
    let clip = h.sequence.read().unwrap().tracks[0].clips[0].clone();
    assert!(klipp_timeline::test_clip::effects(&clip).iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::GaussianBlur { .. })), "the effect has its entry");
    // A later edit of it, which goes through the list, keeps the name.
    let path = json!({"root":root,"nodes":[blur["id"]]});
    graph_test_edit_roundtrip(&mut h, json!([{"op":"set","path":path,"key":"radius","value":7.0}]));
    let read = h.ok(json!({"method":"graph_read","path":path}));
    assert_eq!(read["nodes"][0]["label"], "Soft focus", "{read}");
    assert_eq!(read["nodes"][0]["params"]["radius"], 7.0);
}

/// #1912: a track's grade still has no place for a correction's mix: the
/// write is refused, saying so, and changes nothing.
#[test]
fn graph_a_track_grade_mix_is_refused() {
    use klipp_graph_format::path::{Root, TrackPart};
    let mut h = harness();
    let track = {
        let mut seq = h.sequence.write().unwrap();
        let tid = seq.tracks[0].id.0;
        // As the track-root write test seeds a track's grade.
        seq.tracks[0].effects = serde_json::from_value(json!({ "video": [{ "type": "gaussian_blur", "radius": 3.0 }], "video_enabled": [true] })).unwrap();
        seq.set_root_grade(Root::Track { track: tid, part: TrackPart::Grade }, Some(klipp_nodes::NodeGraph::default())).unwrap();
        tid
    };
    let root = json!({"track":track,"part":"grade"});
    let graph = h.ok(json!({"method":"graph_read","path":{"root":root,"nodes":[]}}));
    let node = graph["nodes"].as_array().unwrap().iter().find(|n| n["type"] == "klipp.grade.corrector").unwrap()["id"].clone();
    let count = h.edits_seen.lock().unwrap().len();
    let refused = h.call(json!({"method":"graph_ops","ops":[{"op":"mix","path":{"root":root,"nodes":[node]},"mix":0.5}]}));
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(refused.to_string().contains("doesn't keep a correction's mix"), "{refused}");
    assert_eq!(h.edits_seen.lock().unwrap().len(), count, "nothing changed");
}

#[test]
fn perf_distinguishes_engine_from_effects_readbacks() {
    let mut h = harness();
    let reply = h.ok(json!({ "method": "perf" }));
    for key in ["count", "total_us"] {
        assert!(reply["readbacks"][key].is_u64(), "{reply}");
    }
    let effects = &reply["effects_readbacks"];
    for key in ["count", "bytes", "total_us", "failures"] {
        assert!(effects["totals"][key].is_u64(), "{reply}");
    }
    assert!(reply["readbacks"]["scope"].as_str().unwrap().contains("engine GPU-frame conversions only"));
    assert!(effects["scope"].as_str().unwrap().contains("title-mask"));
    assert!(effects["scope"].as_str().unwrap().contains("overlaps render time"));
}

#[test]
fn perf_exposes_picture_fallback_identity_reason_and_counts() {
    {
        let _frame = klipp_effects::residency::frame("control-test", 1.25);
        klipp_effects::residency::record("control.test-fallback", "injected diagnostic control", klipp_effects::residency::Work::Fallback);
    }
    let mut h = harness();
    let reply = h.ok(json!({"method":"perf"}));
    let fallbacks = &reply["picture_fallbacks"];
    assert!(fallbacks["by_feature"]["control.test-fallback"].as_u64().unwrap() >= 1);
    assert!(fallbacks["overflow"].is_u64());
    assert!(fallbacks.get("latest_viewer_frame").is_some());
}

#[test]
fn timeline_import_returns_the_same_report_and_reports_failures() {
    let mut h = harness();
    let path = std::env::temp_dir().join(format!("klipp-socket-report-{}.otio", uuid::Uuid::new_v4()));
    std::fs::write(&path, include_str!("../../klipp-project/tests/fixtures/import_report.otio")).unwrap();
    let parsed = klipp_project::import_report::load(&path, klipp_core::Fps::film_24(), &klipp_media::MediaPool::new()).unwrap();
    let expected = klipp_project::import_report::ImportReport::from_import(&path, &parsed);
    let result = h.ok(json!({"method":"import_timeline","path":path}));
    // Media IDs are generated per parse; compare the content and categories.
    assert_eq!(result["report"]["imported"], serde_json::to_value(expected.imported).unwrap());
    assert_eq!(result["report"]["warnings"], serde_json::to_value(expected.warnings).unwrap());
    assert_eq!(result["report"]["missing_media"][0]["name"], expected.missing_media[0].name);
    let read = h.ok(json!({"method":"import_timeline"}));
    assert_eq!(read["report"], result["report"]);
    assert_eq!(read["request_id"], result["request_id"]);
    // #2083: opened as a new project the same way, and a misnamed file by
    // what is in it; a format not read yet is refused by name.
    let misnamed = path.with_extension("xml");
    std::fs::copy(&path, &misnamed).unwrap();
    let opened = h.ok(json!({"method":"import_timeline","path":misnamed,"new_project":true}));
    assert_eq!(opened["report"]["imported"], result["report"]["imported"], "{opened}");
    std::fs::write(&misnamed, [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]).unwrap();
    let aaf = h.call(json!({"method":"import_timeline","path":misnamed,"new_project":true}));
    assert!(aaf["error"].as_str().unwrap().contains("AAF"), "{aaf}");
    std::fs::remove_file(misnamed).unwrap();
    std::fs::write(&path, "not json").unwrap();
    let failed = h.call(json!({"method":"import_timeline","path":path}));
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(!failed["error"].as_str().unwrap().is_empty());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn restore_background_whole_routes_without_a_model_or_job() {
    let mut h = harness();
    let clip_id = h.ids.clip_a.clone();
    {
        let mut seq = h.sequence.write().unwrap();
        let clip = seq.tracks.iter_mut().flat_map(|t| &mut t.clips).find(|c| c.id.to_string() == clip_id).unwrap();
        let mut graph = clip.grade().clone();
        let id = klipp_nodes::NodeId::new_v4();
        graph.nodes.push(klipp_nodes::Node {
            id, kind: klipp_nodes::NodeKind::ExternalMatte, label: "Subject".into(), enabled: true,
            params: klipp_nodes::NodeParams { matte_subject: Some(klipp_nodes::SubjectMatte::Subject), ..Default::default() },
            keyframes: Default::default(), output_link: None, input_links: Vec::new(),
        });
        graph.edges.push(klipp_nodes::Edge { from_node: id, from_port: 0, to_node: graph.output_node_id, to_port: 1 });
        clip.set_grade(graph);
    }
    let result = h.ok(json!({"method":"track_subject", "clip_id":clip_id, "into":"whole", "model":"not-a-model", "download_model":true}));
    assert!(result.get("job_id").is_none());
    assert!(matches!(h.cmd_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), klipp_core::AppCommand::RestoreBackground { clip_id: id } if id.to_string() == clip_id));
    assert!(h.cmd_rx.try_recv().is_err());
    let reply = h.call(json!({"method":"track_subject", "transition_id":"not-a-transition", "into":"whole"}));
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("not a transition"));
    let other = h.ids.clip_b.clone();
    let reply = h.call(json!({"method":"track_subject", "clip_id":other, "into":"whole"}));
    assert_eq!(reply["ok"], false);
    assert!(h.cmd_rx.try_recv().is_err());
}
