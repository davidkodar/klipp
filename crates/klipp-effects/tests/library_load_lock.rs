//! Plug-in loading and GPU start-up never overlap (#1032). A frei0r
//! plug-in that links NVIDIA's OpenGL runs driver code from its load-time
//! initialisers (and its unload-time destructors) with the dynamic linker's
//! lock held, and creating a Vulkan instance on another thread at the same
//! moment deadlocked inside the driver. One process-wide lock
//! (`klipp-load-lock`) keeps them apart.
//!
//! Acquisitions are counted per thread, so each check sees only what its
//! own thread did, whatever the tests running beside it take. A missing
//! prerequisite (no GPU, no frei0r plug-ins) is reported as unverified;
//! with `KLIPP_TEST_REQUIRE_GPU=1` it fails instead.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use klipp_effects::gpu_context::GpuContext;

fn required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// Report a missing prerequisite as unverified, or fail when required.
fn unverified(what: &str) {
    assert!(!required(), "required but missing: {what}");
    eprintln!("UNVERIFIED: {what}");
}

/// A shared library this process has loaded that isn't a frei0r plug-in:
/// it opens, and is then rejected for its missing entry points.
fn not_a_plug_in() -> Option<PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines().filter_map(|l| l.split_whitespace().nth(5)).find(|p| p.contains("/libm.so")).map(PathBuf::from)
}

/// The lock this thread takes while running `f`.
fn taken_by<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = klipp_load_lock::times_taken_here();
    let out = f();
    (out, klipp_load_lock::times_taken_here() - before)
}

/// Each way a frei0r library is opened or closed takes the lock: a scan
/// (of a file that fails to open, and of a library rejected after it
/// opened, which closes again), first use's open and its close, and a
/// first use rejected after it opened (#1164 review), as well as GPU start-up.
#[test]
fn every_library_open_and_close_and_gpu_start_up_take_the_load_lock() {
    let (_, n) = taken_by(|| klipp_frei0r::host::scan_library(Path::new("/nonexistent/not-a-plug-in.so")));
    assert!(n >= 1, "a scan takes the lock to open");
    let (_, n) = taken_by(GpuContext::new);
    assert!(n >= 1, "GPU start-up takes the lock");

    let Some(rejected) = not_a_plug_in() else {
        return unverified("no non-plug-in library found to reject");
    };
    let (found, n) = taken_by(|| klipp_frei0r::host::scan_library(&rejected));
    assert!(found.is_empty());
    assert!(n >= 2, "a scan that rejects an opened library closes it under the lock too ({n})");

    let Some(info) = klipp_frei0r::discover_frei0r_plugins().into_iter().next() else {
        return unverified("no frei0r plug-ins installed for first use");
    };
    let (refused, n) = taken_by(|| klipp_frei0r::Frei0rPlugin::open(&rejected, info.clone()).err());
    assert!(refused.is_some(), "a library without frei0r's entry points is refused");
    assert!(n >= 2, "a first use refused after the library opened closes it under the lock ({n})");

    let (plugin, n) = taken_by(|| klipp_frei0r::Frei0rPlugin::open(&info.path.clone(), info).expect("an installed plug-in opens"));
    assert!(n >= 1, "first use takes the lock to open");
    let ((), n) = taken_by(|| drop(plugin));
    assert!(n >= 1, "closing a plug-in takes the lock");
}

/// The ordering: while the lock is held, GPU start-up and a library scan
/// wait for it, and each finishes only after it is released. Each worker
/// says it has started before the hold is measured from, so a worker that
/// merely started late can't pass for one that waited.
#[test]
fn gpu_start_up_and_library_loading_wait_for_the_load_lock() {
    let held = klipp_load_lock::library_loading();
    let (started, ready) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let (s1, d1) = (started.clone(), done.clone());
    std::thread::spawn(move || {
        let _ = s1.send(());
        let _ = GpuContext::new();
        let _ = d1.send(("GPU start-up", Instant::now()));
    });
    std::thread::spawn(move || {
        let _ = started.send(());
        let _ = klipp_frei0r::host::scan_library(Path::new("/nonexistent/not-a-plug-in.so"));
        let _ = done.send(("library scan", Instant::now()));
    });
    for _ in 0..2 {
        ready.recv_timeout(Duration::from_secs(30)).expect("both workers start");
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(finished.try_recv().is_err(), "nothing finished while the lock was held");
    let released = Instant::now();
    drop(held);
    for _ in 0..2 {
        let (what, at) = finished.recv_timeout(Duration::from_secs(120)).expect("each finishes once the lock is free");
        assert!(at >= released, "{what} finished while the lock was held");
    }
}

const RACE_CHILD: &str = "KLIPP_LOAD_LOCK_RACE_CHILD";

/// Cold frei0r discovery (every plug-in opened and closed) and GPU
/// start-up on two threads at once, three rounds, in a child process the
/// test watches: a native deadlock can't be timed out from inside the
/// process that has it, so the parent kills a child that hangs and fails.
/// A race with no plug-ins or no GPU proves nothing: unverified, or a
/// failure when required.
#[test]
fn plug_in_discovery_and_gpu_start_up_race_without_hanging() {
    if std::env::var_os(RACE_CHILD).is_some() {
        race();
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "plug_in_discovery_and_gpu_start_up_race_without_hanging", "--nocapture", "--test-threads=1"])
        .env(RACE_CHILD, "1")
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "the race failed in the child: {status}");
            return;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("plug-in discovery and GPU start-up deadlocked: the child was still running after 300 s and was killed");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The race the child runs.
fn race() {
    for round in 0..3 {
        let discovery = std::thread::spawn(klipp_frei0r::discover_frei0r_plugins);
        let gpu = std::thread::spawn(|| GpuContext::new().is_some());
        let found = discovery.join().unwrap().len();
        let started = gpu.join().unwrap();
        eprintln!("round {round}: {found} frei0r plug-ins scanned, GPU started: {started}");
        if found == 0 || !started {
            return unverified(&format!("the race needs frei0r plug-ins and a GPU ({found} plug-ins, GPU started: {started})"));
        }
    }
}
