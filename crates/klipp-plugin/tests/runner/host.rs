//! Subprocess worker for scripts/plugin-tests.py. No oracle lives in the host.
use klipp_plugin::{AppliedEffect, OfxHostRuntime};
use std::{error::Error, path::PathBuf};

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 7 {
        return Err("expected: plugin-directory plugin-id input.rgba output.rgba width height time".into());
    }
    let width: u32 = args[4].parse()?;
    let height: u32 = args[5].parse()?;
    let time: f64 = args[6].parse()?;
    if width == 0 || height == 0 || width > 256 || height > 256 || !time.is_finite() {
        return Err("invalid diagnostic frame dimensions or time".into());
    }
    let input = std::fs::read(&args[2])?;
    if input.len() != width as usize * height as usize * 4 {
        return Err("input must be tightly packed RGBA8".into());
    }
    let mut host = OfxHostRuntime::new();
    let found = host.discover_plugins_in(&[PathBuf::from(&args[0])]);
    if found != 4 || !host.plugin_infos().iter().any(|p| p.name == args[1]) {
        return Err(format!("required fixture missing: {} (discovered {found})", args[1]).into());
    }
    let effect = AppliedEffect { plugin_name: args[1].clone(), enabled: true, ..Default::default() };
    let output = host.apply_effects_chain(&[effect], &input, width, height, time);
    std::fs::write(&args[3], output)?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
