//! A node's own OFX instance (M4 step 3), on #932's fixture bundle built
//! here: its own parameters, destroyed when dropped, the frames it asks
//! for, and the frames it's handed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use klipp_plugin::{OfxHostRuntime, OfxValue};

/// The fixture bundle in a folder of its own. A missing C compiler fails
/// here, naming what to install.
fn bundle() -> PathBuf {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ofx/fixture.c");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ofx-node-{}", std::process::id()));
    let binary = dir.join("KlippFixture.ofx.bundle/Contents/Linux-x86-64/KlippFixture.ofx");
    if !binary.exists() {
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
        let status = std::process::Command::new(&cc)
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-shared", "-fPIC"])
            .arg(&src)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap_or_else(|e| panic!("C compiler '{cc}' not found ({e}): the OFX fixture is a C source; install gcc (base-devel on Arch) or set CC"));
        assert!(status.success(), "the OFX fixture didn't build");
    }
    dir
}

fn host() -> (Arc<Mutex<OfxHostRuntime>>, PathBuf) {
    let dir = bundle();
    let mut host = OfxHostRuntime::new();
    assert_eq!(host.discover_plugins_in(std::slice::from_ref(&dir)), 5, "the bundle's five plug-ins");
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
    assert_eq!(t.frames_needed(&wide, 50.0).len(), 80, "forty either side");
    // Handed the frame before only: red from it, blue 0 (none after).
    let before = (picture(7), W, H);
    let out = t.render(&HashMap::new(), &picture(100), W, H, 50.0, 1.0, vec![(49.0, before)]).expect("rendered");
    assert_eq!(&out[..3], &[7, 110, 0], "red from frame 49, green its own, no frame 51");
}
