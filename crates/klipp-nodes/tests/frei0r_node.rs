//! A frei0r node's instances across frames (M4 step 5): history carried
//! through play, started again on a seek, a step back, a new size, another
//! source, a node switched back on; a repaint runs nothing. On the native
//! fixtures (`klipp-frei0r/tests/fixtures/frei0r/fixture.c`), built here.

use std::path::PathBuf;
use std::sync::Arc;

use klipp_core::{ClipId, MediaId};
use klipp_frei0r::{host::scan_library, Frei0rPlugin, Frei0rValue};
use klipp_nodes::frame_context::{PluginInstances, SlotStamp};
use klipp_nodes::frei0r_node::{render, switched_off};
use klipp_nodes::NodeId;

const W: u32 = 16;
const H: u32 = 8;

/// The fixture plug-in `kind` (fixture.c's KLIPP_KIND), built once into
/// the target directory. A missing C compiler fails here, by name.
fn fixture(kind: u32, name: &str) -> Arc<Frei0rPlugin> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../klipp-frei0r/tests/fixtures/frei0r/fixture.c");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("frei0r-fixtures");
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let so = dir.join(format!("klipp-fixture-{name}.so"));
    // Tests build in parallel, in one process: each under a name of its own,
    // then renamed into place (the same bytes, so the last rename wins).
    static BUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = BUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let partial = dir.join(format!("klipp-fixture-{name}.{}-{n}.so", std::process::id()));
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let status = std::process::Command::new(&cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC", &format!("-DKLIPP_KIND={kind}")])
        .arg(&src)
        .arg("-o")
        .arg(&partial)
        .status()
        .unwrap_or_else(|e| panic!("C compiler '{cc}' not found ({e}): the frei0r fixtures are C sources; install gcc (base-devel on Arch) or set CC"));
    assert!(status.success(), "the frei0r fixture didn't build");
    std::fs::rename(&partial, &so).expect("fixture in place");
    let info = scan_library(&so).into_iter().next().expect("the fixture is a frei0r plug-in");
    Arc::new(Frei0rPlugin::open(&so, info).expect("the fixture opens"))
}

/// A picture whose every byte names frame `n`.
fn picture(n: u8) -> Vec<u8> {
    vec![n; (W * H * 4) as usize]
}

fn stamp(frame: i64) -> SlotStamp {
    SlotStamp { frame, clip_time_secs: frame as f64 / 25.0, fps: 25.0, width: W, height: H, source: (MediaId::nil(), 0.0), enabled: true }
}

/// The history fixture outputs the previous frame's input, its first
/// frame its own: which frame's picture came out.
fn shown(out: &[u8]) -> u8 {
    assert!(out.iter().all(|&b| b == out[0]), "one picture, whole");
    out[0]
}

struct Run {
    instances: PluginInstances,
    plugin: Arc<Frei0rPlugin>,
    clip: ClipId,
    node: NodeId,
}

impl Run {
    fn new() -> Self {
        Self { instances: PluginInstances::default(), plugin: fixture(3, "history"), clip: ClipId::new(), node: NodeId::new_v4() }
    }

    fn at(&mut self, now: SlotStamp, input: u8) -> u8 {
        let out = render(&mut self.instances, self.clip, self.node, now, &self.plugin, &[], &picture(input)).expect("rendered");
        self.instances.end_frame(now.frame);
        shown(&out.rgba)
    }
}

#[test]
fn play_carries_history_and_a_seek_starts_it_again() {
    let mut r = Run::new();
    assert_eq!(r.at(stamp(10), 10), 10, "the first frame is its own");
    assert_eq!(r.at(stamp(11), 11), 10, "playing on: the frame before");
    assert_eq!(r.at(stamp(12), 12), 11);
    // Up to four frames on is still play (a dropped frame or three).
    assert_eq!(r.at(stamp(16), 16), 12, "four on: carried");
    // Further is a seek.
    assert_eq!(r.at(stamp(30), 30), 30, "a seek forward starts again");
    // Any step back is a seek too.
    assert_eq!(r.at(stamp(31), 31), 30);
    assert_eq!(r.at(stamp(29), 29), 29, "a step back starts again");
}

#[test]
fn a_repaint_runs_nothing_and_an_edit_at_rest_starts_again() {
    let mut r = Run::new();
    r.at(stamp(5), 5);
    assert_eq!(r.at(stamp(6), 6), 5);
    // The same frame, the same picture: the last output, not a second
    // run (which would have shown frame 6's own picture).
    for _ in 0..3 {
        assert_eq!(r.at(stamp(6), 6), 5, "a repaint is the last output");
    }
    // The same frame with another picture (an edit upstream): started
    // again, so its first-frame output, the same every time.
    assert_eq!(r.at(stamp(6), 60), 60, "an edit at rest starts again");
    assert_eq!(r.at(stamp(6), 61), 61);
    // Params changed at rest likewise: the fixture has none, so the
    // colour fixture's run below holds it.
}

#[test]
fn a_new_size_another_source_or_a_node_switched_back_on_starts_again() {
    let mut r = Run::new();
    r.at(stamp(1), 1);
    assert_eq!(r.at(stamp(2), 2), 1);
    // Another size: the instance is another size's.
    let bigger = |f| SlotStamp { width: W * 2, ..stamp(f) };
    let out = render(&mut r.instances, r.clip, r.node, bigger(3), &r.plugin, &[], &vec![3; (W * 2 * H * 4) as usize]).unwrap();
    assert_eq!(shown(&out.rgba), 3, "a new size starts again");
    r.instances.end_frame(3);
    // Back to the first size: started again too.
    assert_eq!(r.at(stamp(4), 4), 4);
    // Another source (a relink, a slip): its history isn't this one's.
    let other = SlotStamp { source: (MediaId::new(), 0.0), ..stamp(5) };
    let out = render(&mut r.instances, r.clip, r.node, other, &r.plugin, &[], &picture(5)).unwrap();
    assert_eq!(shown(&out.rgba), 5, "another source starts again");
    r.instances.end_frame(5);
    // Switched off for a frame, then on: started again.
    let other = |f| SlotStamp { source: (MediaId::nil(), 0.0), ..stamp(f) };
    assert_eq!(r.at(other(6), 6), 6);
    assert_eq!(r.at(other(7), 7), 6, "carried");
    switched_off(&mut r.instances, r.clip, r.node, other(8));
    r.instances.end_frame(8);
    assert_eq!(r.at(other(9), 9), 9, "switched back on starts again");
}

#[test]
fn two_nodes_and_two_clips_keep_their_own_history() {
    let plugin = fixture(3, "history");
    let mut instances = PluginInstances::default();
    let (c1, c2, n1, n2) = (ClipId::new(), ClipId::new(), NodeId::new_v4(), NodeId::new_v4());
    let mut at = |clip, node, f: i64, input: u8| {
        let out = render(&mut instances, clip, node, stamp(f), &plugin, &[], &picture(input)).unwrap();
        shown(&out.rgba)
    };
    // Frame 0: each its own first frame, each with its own picture.
    assert_eq!(at(c1, n1, 0, 10), 10);
    assert_eq!(at(c1, n2, 0, 20), 20);
    assert_eq!(at(c2, n1, 0, 30), 30);
    // Frame 1: each the frame before *its own*.
    assert_eq!(at(c1, n1, 1, 11), 10, "clip 1 node 1");
    assert_eq!(at(c1, n2, 1, 21), 20, "clip 1 node 2");
    assert_eq!(at(c2, n1, 1, 31), 30, "clip 2 node 1");
}

#[test]
fn a_parameter_changed_at_rest_starts_again_and_one_changed_in_play_carries() {
    // The params fixture: `gain` (index 1) scales green; no history, so
    // what's held is that the repaint memo sees the parameters.
    let plugin = fixture(2, "params");
    let mut instances = PluginInstances::default();
    let (clip, node) = (ClipId::new(), NodeId::new_v4());
    let green = |instances: &mut PluginInstances, f, gain: f64| {
        let out = render(instances, clip, node, stamp(f), &plugin, &[Frei0rValue::Bool(false), Frei0rValue::Double(gain)], &picture(100)).unwrap();
        out.rgba[1]
    };
    assert_eq!(green(&mut instances, 0, 0.5), 100, "gain 0.5 is identity");
    assert_eq!(green(&mut instances, 0, 0.5), 100, "a repaint");
    assert_eq!(green(&mut instances, 0, 0.25), 50, "the new gain at rest, not the memo");
    assert_eq!(green(&mut instances, 1, 1.0), 200, "and in play");
}

#[test]
fn an_unaligned_size_is_padded_and_said_so() {
    let plugin = fixture(0, "colour");
    let mut instances = PluginInstances::default();
    let now = SlotStamp { width: 37, height: 23, ..stamp(0) };
    let out = render(&mut instances, ClipId::new(), NodeId::new_v4(), now, &plugin, &[], &vec![7; 37 * 23 * 4]).expect("padded, not refused");
    assert_eq!(out.rgba.len(), 37 * 23 * 4);
    assert!(klipp_nodes::frei0r_node::fell_back(&out), "the fallback is declared");
}

#[test]
fn the_slot_counts_what_it_keeps() {
    use klipp_nodes::frame_context::SlotBytes;
    assert_eq!(klipp_nodes::frei0r_node::Frei0rSlot::default().bytes(), 0, "nothing made yet");
    let mut r = Run::new();
    r.at(stamp(0), 0);
    let held = r.instances.bytes();
    // The instance's frame, plus the repaint memo's input and output.
    assert!(held >= (W * H * 4) as usize * 3, "{held} bytes counted");
}
