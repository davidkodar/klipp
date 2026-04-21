//! Klipp perf harness — runs scenarios from `scenarios::ALL` against
//! the current codebase and prints a one-line human summary per scenario
//! plus a JSON result dump.
//!
//! Invocation:
//!   cargo run --release -p klipp-perf             # run all scenarios
//!   cargo run --release -p klipp-perf -- --list   # list scenario names
//!   cargo run --release -p klipp-perf -- --scenario s01_colorbalance_1node_1080p
//!
//! Results land in `target/perf-results/<timestamp>_<scenario>.json`.
//! No regression tooling — numbers are for eyeballing. Diffs between
//! runs are manual and intentional for this phase.

mod fixtures;
mod scenarios;
mod stats;

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> std::io::Result<()> {
    // Force wgpu to select the Vulkan backend. On this machine (CachyOS
    // + NVIDIA + Wayland) wgpu's default `Backends::all` negotiation
    // sometimes picks GLES first, then drops the device mid-run. For a
    // reproducible harness we want one backend, deterministically. The
    // user can override by exporting WGPU_BACKEND before invocation.
    if std::env::var("WGPU_BACKEND").is_err() {
        // SAFETY: single-threaded at this point.
        unsafe { std::env::set_var("WGPU_BACKEND", "vulkan") };
    }
    // Same VK_DRIVER_FILES shim the main app uses. Idempotent — the
    // klipp-effects layer also sets it, but doing it here first means
    // adapter enumeration sees the right ICD from the very first probe.
    if std::env::var("VK_DRIVER_FILES").is_err() {
        let icd_path = "/usr/share/vulkan/icd.d/nvidia_icd.json";
        if std::path::Path::new(icd_path).exists() {
            unsafe { std::env::set_var("VK_DRIVER_FILES", icd_path) };
        }
    }

    // Minimal logging — scenarios print their own summary. RUST_LOG
    // override still works for debug introspection of the executor.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut selected: Option<String> = None;
    let mut list_only = false;
    let mut cpu_only = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--list" => list_only = true,
            "--cpu-only" => cpu_only = true,
            "--scenario" => {
                i += 1;
                selected = args.get(i).cloned();
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            other => {
                eprintln!("unknown arg: {other}");
                print_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    if list_only {
        for s in scenarios::ALL {
            println!("  {:<40} {}", s.name, s.desc);
        }
        return Ok(());
    }

    let results_dir = PathBuf::from("target/perf-results");
    fs::create_dir_all(&results_dir)?;
    let run_stamp = epoch_stamp();

    println!(
        "{:<40} {:<5}  p50         p99         max         mean        budget      res    n",
        "scenario", "ok"
    );
    println!("{}", "─".repeat(120));

    let mut any_failed = false;
    for s in scenarios::ALL {
        if let Some(ref filter) = selected {
            if s.name != filter {
                continue;
            }
        }
        let result = (s.run)(cpu_only);
        stats::print_human(&result);
        if !result.passed {
            any_failed = true;
        }

        let json = serde_json::to_string_pretty(&result)
            .expect("scenario result must serialize");
        let path = results_dir.join(format!("{run_stamp}_{}.json", result.scenario));
        fs::write(&path, json)?;
    }

    println!();
    println!("JSON results: {}", results_dir.display());
    if any_failed {
        println!("Some scenarios missed their p99 budget (see FAIL above).");
    } else {
        println!("All run scenarios met their p99 budget.");
    }
    Ok(())
}

fn print_help() {
    eprintln!("Klipp perf harness");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  cargo run --release -p klipp-perf                              # all scenarios (GPU path)");
    eprintln!("  cargo run --release -p klipp-perf -- --cpu-only                # force CPU fallback path");
    eprintln!("  cargo run --release -p klipp-perf -- --list                    # list scenarios");
    eprintln!("  cargo run --release -p klipp-perf -- --scenario <name>         # run one");
    eprintln!();
    eprintln!("Results: target/perf-results/<stamp>_<scenario>.json");
}

fn epoch_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}
