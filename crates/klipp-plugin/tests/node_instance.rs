//! A node's own OFX instance (M4 step 3), on #932's fixture bundle built
//! here: its own parameters, destroyed when dropped, the frames it asks
//! for, and the frames it's handed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use klipp_plugin::{OfxHostRuntime, OfxValue, MAX_FRAMES_NEEDED};

/// The fixture bundle in a folder of its own, built once for the process.
/// The tests run in parallel, and one that looked for the binary while
/// another's compiler was still writing it loaded a half-written file and
/// found no plug-ins: it is compiled under another name and renamed into
/// place, by the first test to ask. A missing C compiler fails here,
/// naming what to install.
fn bundle() -> PathBuf {
    static BUNDLE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BUNDLE.get_or_init(|| {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ofx/fixture.c");
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ofx-node-{}", std::process::id()));
        let binary = dir.join("KlippFixture.ofx.bundle/Contents/Linux-x86-64/KlippFixture.ofx");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        let building = binary.with_extension("building");
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let status = std::process::Command::new(&cc)
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC"])
            .arg(&src)
            .arg("-o")
            .arg(&building)
            .status()
            .unwrap_or_else(|e| panic!("C compiler '{cc}' not found ({e}): the OFX fixture is a C source; install gcc (base-devel on Arch) or set CC"));
        assert!(status.success(), "the OFX fixture didn't build");
        std::fs::rename(&building, &binary).expect("the built fixture moved into place");
        dir
    })
    .clone()
}

fn host() -> (Arc<Mutex<OfxHostRuntime>>, PathBuf) {
    let dir = bundle();
    let mut host = OfxHostRuntime::new();
    assert_eq!(host.discover_plugins_in(std::slice::from_ref(&dir)), 6, "the bundle's six plug-ins");
    (Arc::new(Mutex::new(host)), dir)
}

/// The channels fixture's instances alive, from its trace: made less
/// destroyed. (Other tests here make other fixtures' instances.)
fn live(trace: &std::path::Path) -> i64 {
    let text = std::fs::read_to_string(trace).unwrap_or_default();
    text.lines().map(|l| match l { "create channels" => 1, "destroy channels" => -1, _ => 0 }).sum()
}

const W: u32 = 8;
const H: u32 = 4;

fn picture(v: u8) -> Vec<u8> {
    [v, v.wrapping_add(10), v.wrapping_add(20), 255].repeat((W * H) as usize)
}

#[test]
fn each_instance_is_its_own_and_dropping_it_destroys_it() {
    // The fixture traces its instances to this file (the only test here
    // that sets it; the other ignores the trace).
    let trace = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ofx-node-trace-{}", std::process::id()));
    let _ = std::fs::remove_file(&trace);
    std::env::set_var("KLIPP_FIXTURE_TRACE_FILE", &trace);
    let (host, _) = host();
    let before = live(&trace);
    let mut a = OfxHostRuntime::new_instance(&host, "org.klipp.fixture.channels", 25.0).expect("an instance");
    let mut b = OfxHostRuntime::new_instance(&host, "org.klipp.fixture.channels", 25.0).expect("another");
    assert_eq!(live(&trace), before + 2, "two instances");
    // Each with its own settings: `mode` 1 passes through, 0 swaps channels.
    let passthrough: HashMap<String, OfxValue> = [("mode".to_string(), OfxValue::Int(1))].into();
    let pa = a.render(&passthrough, &picture(30), W, H, 0.0, 1.0, Vec::new()).expect("rendered");
    let pb = b.render(&HashMap::new(), &picture(30), W, H, 0.0, 1.0, Vec::new()).expect("rendered");
    assert_eq!(pa, picture(30), "the passthrough instance");
    assert_eq!(&pb[..4], &[50, 225, 40, 255], "the other swaps channels: B, 255 − R, G");
    drop(a);
    assert_eq!(live(&trace), before + 1, "dropping one destroyed it");
    drop(b);
    assert_eq!(live(&trace), before);
    assert!(OfxHostRuntime::new_instance(&host, "org.klipp.fixture.nowhere", 25.0).is_err(), "no such plug-in");
}

#[test]
fn a_temporal_plug_in_names_its_frames_and_reads_what_it_was_handed() {
    let (host, _) = host();
    let d = host.lock().unwrap().describe("org.klipp.fixture.temporal").expect("described");
    assert!(d.filter && d.temporal && !d.sequential_render && !d.unsafe_threads, "{d:?}");
    assert!(host.lock().unwrap().describe("org.klipp.fixture.nowhere").is_none());
    let mut t = OfxHostRuntime::new_instance(&host, "org.klipp.fixture.temporal", 25.0).expect("an instance");
    assert_eq!(t.frames_needed(&HashMap::new(), 50.0), [49.0, 51.0], "the frames either side, in frames");
    let wide: HashMap<String, OfxValue> = [("mode".to_string(), OfxValue::Int(5))].into();
    // Forty either side is more than it names: the nearest, half each side.
    let near: Vec<f64> = (18..=82).filter(|f| *f != 50).map(f64::from).collect();
    assert_eq!(t.frames_needed(&wide, 50.0), near, "the {MAX_FRAMES_NEEDED} nearest");
    // A million either side isn't walked: the same nearest (the whole walk
    // would name two million).
    let vast: HashMap<String, OfxValue> = [("mode".to_string(), OfxValue::Int(6))].into();
    assert_eq!(t.frames_needed(&vast, 50.0), near);
    // Handed the frame before only: red from it, blue 0 (none after).
    let before = (picture(7), W, H);
    let out = t.render(&HashMap::new(), &picture(100), W, H, 50.0, 1.0, vec![(49.0, before)]).expect("rendered");
    assert_eq!(&out[..3], &[7, 110, 0], "red from frame 49, green its own, no frame 51");
}

/// Two hosts rendering at once, as the viewer and an export do, each under
/// its own lock: each plug-in reads the neighbour its own render handed,
/// never the other's (the render context is one for the process).
#[test]
fn renders_on_two_hosts_at_once_read_their_own_frames() {
    let (a, _) = host();
    let (b, _) = host();
    let run = |host: Arc<Mutex<OfxHostRuntime>>, red: u8| {
        std::thread::spawn(move || {
            let mut t = OfxHostRuntime::new_instance(&host, "org.klipp.fixture.temporal", 25.0).expect("an instance");
            for i in 0..300 {
                let out = t.render(&HashMap::new(), &picture(100), W, H, 50.0, 1.0, vec![(49.0, (picture(red), W, H))]).expect("rendered");
                assert_eq!(out[0], red, "render {i} read another render's frame 49");
            }
        })
    };
    let (ta, tb) = (run(a, 7), run(b, 60));
    ta.join().expect("host a's renders");
    tb.join().expect("host b's renders");
}

/// A host dropped while another's instances still render, as an export's
/// host drops when it ends and the viewer's renders on: the plug-in kept
/// the host pointer the last setHost gave it, the dropped host's own, and
/// the next render called through freed memory (SIGSEGV). Nor may the
/// dropped host unload a plug-in the other still holds.
#[test]
fn a_dropped_host_leaves_the_other_hosts_plug_ins_working() {
    let (kept, _) = host();
    let mut t = OfxHostRuntime::new_instance(&kept, "org.klipp.fixture.temporal", 25.0).expect("an instance");
    // A second host loads the same plug-ins (its setHost the last), and
    // goes.
    let (gone, _) = host();
    drop(gone);
    let out = t.render(&HashMap::new(), &picture(100), W, H, 50.0, 1.0, vec![(49.0, (picture(7), W, H))]).expect("rendered");
    assert_eq!(&out[..3], &[7, 110, 0], "the kept host's instance still renders");
    // (The identity plug-in: the channels one's instances are counted by
    // another test, from a trace every test here writes to.)
    let mut fresh = OfxHostRuntime::new_instance(&kept, "org.klipp.fixture.identity", 25.0).expect("and makes new instances");
    assert_eq!(fresh.render(&HashMap::new(), &picture(30), W, H, 0.0, 1.0, Vec::new()).expect("rendered"), picture(30));
}

/// The race as the app has it: one host's instance renders on its thread
/// while hosts come and go on another (exports starting and ending beside
/// the viewer). Every render reads its own frames, and nothing crashes.
#[test]
fn hosts_dropped_while_another_renders_on_its_thread() {
    let (kept, _) = host();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let renders = {
        let (kept, done) = (Arc::clone(&kept), Arc::clone(&done));
        std::thread::spawn(move || {
            let mut t = OfxHostRuntime::new_instance(&kept, "org.klipp.fixture.temporal", 25.0).expect("an instance");
            let mut n = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) || n < 50 {
                let out = t.render(&HashMap::new(), &picture(100), W, H, 50.0, 1.0, vec![(49.0, (picture(7), W, H))]).expect("rendered");
                assert_eq!(&out[..3], &[7, 110, 0], "render {n}");
                n += 1;
            }
            n
        })
    };
    for _ in 0..20 {
        let (other, _) = host();
        let mut i = OfxHostRuntime::new_instance(&other, "org.klipp.fixture.identity", 25.0).expect("an instance");
        i.render(&HashMap::new(), &picture(30), W, H, 0.0, 1.0, Vec::new()).expect("rendered");
        drop(i);
        drop(other);
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let n = renders.join().expect("the kept host's renders, uncrashed");
    assert!(n >= 50, "{n} renders");
}
