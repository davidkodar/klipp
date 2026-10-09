//! The list of frei0r plug-ins that keep frame history
//! (`klipp_frei0r::temporal`) against the installed plug-ins (#1472).
//!
//! Each installed filter renders one frame fresh, and the same frame after
//! 40 others, at its default parameters and again with every number at 0.5
//! (Delay0r keeps nothing at its default delay of 0). If either differs, it
//! keeps history. Each plug-in runs in a child process of this test, so a
//! plug-in that crashes ends only its own case.
//!
//! Only a mismatch between the list and the plug-ins fails. The plug-ins
//! are third-party binaries, so a crash, or one that can't be probed
//! (its output is random), is reported, not failed: a distribution update
//! must not fail the build over code this project doesn't own. Runs on
//! every test run; with no frei0r filter installed (`frei0r-plugins`) it
//! says so and passes.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use klipp_frei0r::{host::scan_library, temporal, Frei0rEffect, Frei0rPlugin, Frei0rValue, ParamKind, PluginType};

const CHILD: &str = "KLIPP_FREI0R_PROBE";
const W: u32 = 128;
const H: u32 = 96;

fn frame(seed: u32) -> Vec<u8> {
    // A small LCG: deterministic, no dependency.
    let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..W * H * 4)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// The child: probe one plug-in in one mode, print the verdict.
#[test]
fn probe_one_plug_in() {
    let Ok(spec) = std::env::var(CHILD) else { return };
    let (mode, path) = spec.split_once(':').expect("mode:path");
    let info = scan_library(Path::new(path)).into_iter().next().expect("a frei0r plug-in");
    let plugin = std::sync::Arc::new(Frei0rPlugin::open(&info.path.clone(), info).expect("opens"));
    let make = || {
        let effect = Frei0rEffect::new(plugin.clone(), W, H).expect("an instance");
        if mode == "numbers" {
            for (index, param) in plugin.info.params.iter().enumerate() {
                if param.kind == ParamKind::Double {
                    effect.set_param(index, &Frei0rValue::Double(0.5));
                }
            }
        }
        effect
    };
    let (a, b) = (frame(1), frame(2));
    let at = 1.6;
    let fresh = make().run(at, &a);
    if make().run(at, &a) != fresh {
        println!("PROBE nondeterministic");
        return;
    }
    let effect = make();
    for k in 0..40 {
        effect.run(f64::from(k) * 0.04, &b);
    }
    println!("PROBE {}", if effect.run(at, &a) != fresh { "stateful" } else { "stateless" });
}

/// One child's verdict: `stateful`, `stateless`, `nondeterministic`, or
/// `crashed`.
fn verdict(path: &Path, mode: &str) -> String {
    let mut child = Command::new(std::env::current_exe().expect("this test"))
        .args(["--exact", "probe_one_plug_in", "--nocapture", "--test-threads=1"])
        .env(CHILD, format!("{mode}:{}", path.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("child starts");
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("child state") {
            let mut out = String::new();
            std::io::Read::read_to_string(child.stdout.as_mut().expect("stdout"), &mut out).ok();
            // libtest prints the verdict on its own "test …" line.
            let said = out.split("PROBE ").nth(1).and_then(|rest| rest.split_whitespace().next());
            return match said {
                Some(v) if status.success() => v.to_string(),
                _ => "crashed".to_string(),
            };
        }
        if started.elapsed() > Duration::from_secs(30) {
            child.kill().ok();
            child.wait().ok();
            return "crashed".to_string();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_temporal_list_is_the_plug_ins_that_keep_history() {
    if std::env::var(CHILD).is_ok() {
        return;
    }
    let dirs = klipp_frei0r::search_paths();
    let mut filters: Vec<(String, PathBuf)> = Vec::new();
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "so")) {
            // Read the plug-in's description only; nothing renders here.
            if let Some(info) = scan_library(&path).into_iter().find(|i| i.plugin_type == PluginType::Filter) {
                filters.push((temporal::key(&info), path));
            }
        }
    }
    if filters.is_empty() {
        eprintln!("skipped: no frei0r filters installed in {dirs:?} (install frei0r-plugins to probe them)");
        return;
    }
    filters.sort();
    filters.dedup_by(|a, b| a.0 == b.0);

    let mut failures = Vec::new();
    let mut notes = Vec::new();
    let mut stateful = Vec::new();
    for (key, path) in &filters {
        let mut keeps = false;
        let mut decided = false;
        for mode in ["defaults", "numbers"] {
            match verdict(path, mode).as_str() {
                "stateful" => (keeps, decided) = (true, true),
                "stateless" => decided = true,
                other => notes.push(format!("{key} ({mode}): {other}")),
            }
        }
        let listed = temporal::TEMPORAL_PLUGINS.iter().any(|(k, _)| k == key);
        if keeps {
            stateful.push(key.clone());
            if !listed {
                failures.push(format!("{key} keeps frame history and isn't in TEMPORAL_PLUGINS"));
            }
        } else if listed && decided {
            failures.push(format!("{key} is in TEMPORAL_PLUGINS but keeps no history"));
        } else if listed {
            notes.push(format!("{key} is in TEMPORAL_PLUGINS and couldn't be probed"));
        }
    }
    for (key, _) in temporal::TEMPORAL_PLUGINS {
        if !filters.iter().any(|(k, _)| k == key) {
            notes.push(format!("{key} is in TEMPORAL_PLUGINS but not installed here"));
        }
    }
    eprintln!("{} filters probed; keep history: {stateful:?}", filters.len());
    for note in &notes {
        eprintln!("note: {note}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
