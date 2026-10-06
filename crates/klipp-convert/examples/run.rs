//! The one-time conversion (#1087): every project on this machine and the
//! config folder's colour presets, to revision 2, each backed up.
//!
//!     cargo run --release -p klipp-convert --example run -- --confirm-real-run [--exclude <dir>]...
//!
//! Run once, at phase 2a's switch, with the user's go-ahead and no Klipp
//! running. It is an example rather than the binary because the dry run it
//! repeats compares the frozen reader with today's reader, which only the
//! tests and examples may link.

#[allow(dead_code, unused_imports)]
#[path = "../tests/parity.rs"]
mod parity;

use std::path::PathBuf;

use klipp_convert::run::{parse_args, real_preconditions, real_settings, run, Hooks};
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
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("klipp");
    let settings = match real_settings(&home, &config, args.excluded) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("klipp-convert run: refused: {e}");
            std::process::exit(1);
        }
    };
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    match run(&settings, &|| real_preconditions(&repo), &parity, &Hooks::NONE) {
        Ok(report) => {
            let path = settings.backup_root.join(format!("run-report-{}.json", settings.date));
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap_or_default());
            println!(
                "converted {}, skipped {}, stopped {}, excluded {}, recovered {}; presets converted {}, stopped {}; report in {}",
                report.converted.len(),
                report.skipped.len(),
                report.stopped.len(),
                report.excluded.len(),
                report.recovered.len(),
                report.presets_converted,
                report.presets_stopped.len(),
                path.display()
            );
        }
        Err(e) => {
            eprintln!("klipp-convert run: {e}");
            std::process::exit(1);
        }
    }
}
