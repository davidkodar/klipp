//! `klipp-convert`: the conversion's command line. It changes nothing.
//!
//! `klipp-convert scan --out <report>` lists every Klipp project on this
//! machine and what would stop it converting, writing `<report>.txt` and
//! `<report>.json` (#1054). `klipp-convert <store kind> <file>` reads one
//! file with the frozen v1 reader.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use klipp_convert::{Document, StoreKind};

fn usage() -> ExitCode {
    eprintln!("usage: klipp-convert scan --out <report path> [--home <dir>] [--config <dir>]");
    eprintln!("       klipp-convert <project|look|stills|grade-preset|tool-preset|effect-preset> <file>");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("scan") => scan(&args[1..]),
        Some("run") => {
            eprintln!("klipp-convert: the conversion runs as `cargo run --release -p klipp-convert --example run -- --confirm-real-run`, which links today's reader for the dry run's comparison (#1087)");
            ExitCode::from(2)
        }
        Some(_) => match inspect(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("klipp-convert: {e}");
                if e.starts_with("usage") { ExitCode::from(2) } else { ExitCode::FAILURE }
            }
        },
        None => usage(),
    }
}

/// One file read with the frozen v1 reader.
fn inspect(args: &[String]) -> Result<(), String> {
    let mut args = args.iter();
    let kind = match args.next().map(String::as_str) {
        Some("project") => StoreKind::Project,
        Some("look") => StoreKind::Look,
        Some("stills") => StoreKind::Stills,
        Some("grade-preset") => StoreKind::GradePreset,
        Some("tool-preset") => StoreKind::ToolPreset,
        Some("effect-preset") => StoreKind::EffectPreset,
        _ => return Err("usage: klipp-convert <project|look|stills|grade-preset|tool-preset|effect-preset> <file>".into()),
    };
    let file = args.next().ok_or("missing file")?;
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
    let doc = Document::parse(&text, kind)?;
    println!("{} picture sections read; file unchanged", doc.pictures.len());
    Ok(())
}

/// The dry run over this machine.
fn scan(args: &[String]) -> ExitCode {
    let mut out: Option<PathBuf> = None;
    let mut home = std::env::var_os("HOME").map(PathBuf::from);
    let mut config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).map(PathBuf::from);
        match args[i].as_str() {
            "--out" => out = value,
            "--home" => home = value,
            "--config" => config = value,
            _ => return usage(),
        }
        i += 2;
    }
    let (Some(out), Some(home), Some(config)) = (out, home, config) else { return usage() };
    let report = klipp_convert::scan::scan(&home, &config);
    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("klipp-convert: {e}");
            return ExitCode::FAILURE;
        }
    };
    let write = |ext: &str, body: &str| std::fs::write(out.with_extension(ext), body);
    if let Err(e) = write("json", &json).and_then(|_| write("txt", &report.text())) {
        eprintln!("klipp-convert: could not write the report: {e}");
        return ExitCode::FAILURE;
    }
    print!("{}", report.text());
    ExitCode::SUCCESS
}
