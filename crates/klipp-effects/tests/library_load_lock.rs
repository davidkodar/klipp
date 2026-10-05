//! Loading the frei0r plug-ins and starting a GPU on another thread at
//! the same moment both finish (#1032). Without the shared lock, a
//! plug-in that links NVIDIA's OpenGL and a Vulkan instance being made
//! deadlocked inside the driver, and the process hung without a word.

use std::sync::mpsc;
use std::time::Duration;

#[test]
fn plug_in_loading_and_gpu_start_up_do_not_deadlock() {
    for round in 0..3 {
        let (done, finished) = mpsc::channel();
        let scan = done.clone();
        std::thread::spawn(move || {
            let found = klipp_frei0r::discover_frei0r_plugins().len();
            let _ = scan.send(format!("frei0r scan: {found} plug-ins"));
        });
        std::thread::spawn(move || {
            let gpu = klipp_effects::gpu_context::GpuContext::new().is_some();
            let _ = done.send(format!("GPU start-up: {gpu}"));
        });
        for _ in 0..2 {
            match finished.recv_timeout(Duration::from_secs(60)) {
                Ok(what) => eprintln!("round {round}: {what}"),
                Err(_) => panic!("round {round}: plug-in loading and GPU start-up deadlocked"),
            }
        }
    }
}
