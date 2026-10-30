//! Klipp perf harness — runs scenarios from `scenarios::ALL` against
//! the current codebase and prints a one-line human summary per scenario
//! plus a JSON result dump.
//!
//! Invocation:
//!   cargo run --release -p klipp-perf             # run all scenarios
//!   cargo run --release -p klipp-perf -- --list   # list scenario names
//!   cargo run --release -p klipp-perf -- --scenario s01_colorbalance_1node_1080p
//!   cargo run --release -p klipp-perf -- --match s2   # every scenario whose name has "s2"
//!
//! Results land in `target/perf-results/<timestamp>_<scenario>.json`.
//! No regression tooling — numbers are for eyeballing. Diffs between
//! runs are manual and intentional for this phase.

mod fixtures;
mod composite;
mod nesting;
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
    let mut matching: Option<String> = None;
    let mut list_only = false;
    let mut cpu_only = false;
    let mut in_process = false;
    let mut no_header = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--list" => list_only = true,
            "--cpu-only" => cpu_only = true,
            "--in-process" => in_process = true,
            "--no-header" => no_header = true,
            "--scenario" => {
                i += 1;
                selected = args.get(i).cloned();
            }
            "--match" => {
                i += 1;
                matching = args.get(i).cloned();
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

    // ── Process isolation ───────────────────────────────────────
    //
    // Scenarios are NOT independent when run back-to-back in one
    // process. Each builds its own `GraphExecutor`, and each executor
    // allocates 1080p ping-pong chain buffers (~66 MB) plus pipelines on
    // the shared device. wgpu frees those lazily, so a scenario running
    // fourth measures a GPU carrying three predecessors' allocations.
    //
    // Measured: `s04` reads 4.09 / 4.13 ms run alone and 7.7-7.8 ms as
    // the fourth scenario in a process — with priming succeeding on
    // every frame, so it is not a silent fallback. `s01` is immune
    // because its cost is dominated by an upload that is
    // bandwidth-bound; `s04`'s is GPU-side work, which is not. A
    // harness where the answer depends on position cannot be used to
    // compare two paths, which is the one thing it is for.
    //
    // So each scenario gets a fresh process. `--in-process` opts out
    // (and is what the children are invoked with).
    if selected.is_none() && !in_process {
        let exe = std::env::current_exe()?;
        print_table_header();
        let mut any_failed = false;
        let mut any_unsettled = false;
        for s in scenarios::ALL.iter().filter(|s| matching.as_deref().is_none_or(|m| s.name.contains(m))) {
            let mut cmd = std::process::Command::new(&exe);
            cmd.arg("--scenario").arg(s.name).arg("--in-process").arg("--no-header");
            if cpu_only {
                cmd.arg("--cpu-only");
            }
            match cmd.status()?.code() {
                Some(0) => {}
                Some(2) => any_unsettled = true,
                _ => any_failed = true,
            }
        }
        print_run_summary(&PathBuf::from("target/perf-results"), any_failed, any_unsettled);
        return Ok(());
    }

    let results_dir = PathBuf::from("target/perf-results");
    fs::create_dir_all(&results_dir)?;
    let run_stamp = epoch_stamp();

    if !no_header {
        print_table_header();
    }

    let mut any_failed = false;
    let mut any_unsettled = false;
    for s in scenarios::ALL {
        if let Some(ref filter) = selected {
            if s.name != filter {
                continue;
            }
        }
        let result = (s.run)(cpu_only);
        stats::print_human(&result);
        // An unsettled run is not a failure against the budget — it is
        // the absence of a measurement. Conflating the two would report
        // a regression that nobody can act on.
        if !result.settled {
            any_unsettled = true;
        } else if !result.passed {
            any_failed = true;
        }

        let json = serde_json::to_string_pretty(&result)
            .expect("scenario result must serialize");
        let path = results_dir.join(format!("{run_stamp}_{}.json", result.scenario));
        fs::write(&path, json)?;
    }

    if !no_header {
        print_run_summary(&results_dir, any_failed, any_unsettled);
    }
    // Exit code carries the verdict so the isolating parent can aggregate
    // without parsing stdout: 0 = pass, 1 = missed budget, 2 = unsettled.
    if any_unsettled {
        std::process::exit(2);
    }
    if any_failed {
        std::process::exit(1);
    }
    Ok(())
}

fn print_table_header() {
    println!(
        "{:<40} {:<5}  p50         p99         sd       drift    budget      res    n",
        "scenario", "ok"
    );
    println!("{}", "─".repeat(120));
}

fn print_run_summary(results_dir: &std::path::Path, any_failed: bool, any_unsettled: bool) {
    println!();
    println!("JSON results: {}", results_dir.display());
    if any_unsettled {
        println!(
            "Some scenarios never settled (UNSET above) — their samples were still \
             trending, so they carry no verdict. Re-run with the GPU already warm."
        );
    }
    if any_failed {
        println!("Some scenarios missed their p99 budget (see FAIL above).");
    } else if !any_unsettled {
        println!("All run scenarios met their p99 budget.");
    }
}

fn print_help() {
    eprintln!("Klipp perf harness");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  cargo run --release -p klipp-perf                              # all scenarios (GPU path)");
    eprintln!("  cargo run --release -p klipp-perf -- --cpu-only                # force CPU fallback path");
    eprintln!("  cargo run --release -p klipp-perf -- --list                    # list scenarios");
    eprintln!("  cargo run --release -p klipp-perf -- --scenario <name>         # run one");
    eprintln!("  cargo run --release -p klipp-perf -- --match <text>           # every scenario whose name has <text>");
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
