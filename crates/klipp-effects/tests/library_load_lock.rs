//! Plug-in loading and GPU start-up never overlap (#1032). A frei0r
//! plug-in that links NVIDIA's OpenGL runs driver code from its load-time
//! initialisers (and its unload-time destructors) with the dynamic linker's
//! lock held, and creating a Vulkan instance on another thread at the same
//! moment deadlocked inside the driver: the process hung without a word.
//! One process-wide lock (`klipp-load-lock`) keeps them apart.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use klipp_effects::gpu_context::GpuContext;

/// Each way a frei0r library is opened or closed, and a GPU start, takes
/// the lock: cold discovery's scan (even of a file that fails to open),
/// first use's open, the close when a plug-in drops, and `GpuContext::new`.
#[test]
fn library_loads_unloads_and_gpu_start_up_take_the_load_lock() {
    let before = klipp_load_lock::times_taken();
    let _ = klipp_frei0r::host::scan_library(Path::new("/nonexistent/not-a-plug-in.so"));
    assert!(klipp_load_lock::times_taken() > before, "cold discovery's scan takes the lock");

    let before = klipp_load_lock::times_taken();
    let _ = GpuContext::new();
    assert!(klipp_load_lock::times_taken() > before, "GPU start-up takes the lock");

    let Some(info) = klipp_frei0r::discover_frei0r_plugins().into_iter().next() else {
        eprintln!("first use and close not checked: no frei0r plug-ins installed");
        return;
    };
    let before = klipp_load_lock::times_taken();
    let plugin = klipp_frei0r::Frei0rPlugin::open(&info.path.clone(), info).expect("an installed plug-in opens");
    assert!(klipp_load_lock::times_taken() > before, "first use's open takes the lock");
    let before = klipp_load_lock::times_taken();
    drop(plugin);
    assert!(klipp_load_lock::times_taken() > before, "closing a plug-in takes the lock");
}

/// The ordering itself: while the lock is held, GPU start-up and a library
/// scan wait for it, and each finishes only after it is released.
#[test]
fn gpu_start_up_and_library_loading_wait_for_the_load_lock() {
    let held = klipp_load_lock::library_loading();
    let (done, finished) = mpsc::channel();
    let gpu = done.clone();
    std::thread::spawn(move || {
        let _ = GpuContext::new();
        let _ = gpu.send(("GPU start-up", Instant::now()));
    });
    std::thread::spawn(move || {
        let _ = klipp_frei0r::host::scan_library(Path::new("/nonexistent/not-a-plug-in.so"));
        let _ = done.send(("library scan", Instant::now()));
    });
    std::thread::sleep(Duration::from_millis(300));
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
#[test]
fn plug_in_discovery_and_gpu_start_up_race_without_hanging() {
    if std::env::var_os(RACE_CHILD).is_some() {
        race();
        return;
    }
    if klipp_frei0r::host::search_paths().iter().all(|d| !d.is_dir()) {
        eprintln!("not verified: no frei0r plug-in folder to race against");
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
    }
}
