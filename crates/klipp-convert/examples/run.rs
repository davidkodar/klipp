//! The one-time conversion (#1087): every project on this machine and the
//! config folder's colour presets, to revision 2, each backed up.
//!
//!     cargo run --release -p klipp-convert --example run -- --confirm-real-run [--exclude <dir>]...
//!
//! Before each project's swap, its written copy must render as the
//! original does today (`tests/render_inputs.rs`), or the project stops.
//!
//! Run once, at phase 2a's switch, with the user's go-ahead and no Klipp
//! running. It is an example rather than the binary because the dry run it
//! repeats compares the frozen reader with today's reader, which only the
//! tests and examples may link.

#[allow(dead_code, unused_imports)]
#[path = "../tests/parity.rs"]
mod parity;

/// The render check before each swap: the written copy renders, clip by
/// clip, as the original's render copy does today (#1068's property).
#[allow(dead_code, unused_imports)]
#[path = "../tests/render_inputs.rs"]
mod render_inputs;

use std::path::PathBuf;

use klipp_convert::run::{no_klipp_running, parse_args, real_preconditions, real_settings, run, Hooks};
use klipp_convert::StoreKind;
use serde_json::Value;

fn parity(raw: &Value, kind: StoreKind) -> Result<(), String> {
    let raw = raw.clone();
    std::panic::catch_unwind(move || {
        parity::compare(raw, kind);
    })
    .map_err(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_else(|| "differs".into()))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("klipp-convert run: {e}");
            std::process::exit(2);
        }
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("klipp-convert run: refused: HOME isn't set");
        std::process::exit(1);
    };
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("klipp");
    let settings = match real_settings(&home, &config, args.excluded) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("klipp-convert run: refused: {e}");
            std::process::exit(1);
        }
    };
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let hooks = Hooks { render_check: &render_inputs::render_check, swap_allowed: &no_klipp_running, ..Hooks::NONE };
    match run(&settings, &|| real_preconditions(&repo), &parity, &hooks) {
        Ok(report) => {
            let path = settings.backup_root.join(format!("run-report-{}.json", settings.date));
            let written = serde_json::to_string_pretty(&report).map_err(|e| e.to_string()).and_then(|text| std::fs::write(&path, text).map_err(|e| e.to_string()));
            println!(
                "converted {}, skipped {}, stopped {}, excluded {}, recovered {}, damaged journal entries {}; presets converted {}, stopped {}",
                report.converted.len(),
                report.skipped.len(),
                report.stopped.len(),
                report.excluded.len(),
                report.recovered.len(),
                report.damaged_journals.len(),
                report.presets_converted,
                report.presets_stopped.len(),
            );
            match written {
                Ok(()) => println!("report in {}", path.display()),
                Err(e) => {
                    // The run is done; only its report is missing, so it
                    // goes to the terminal instead.
                    eprintln!("klipp-convert run: writing the report to {}: {e}", path.display());
                    println!("{report:#?}");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("klipp-convert run: {e}");
            std::process::exit(1);
        }
    }
}
