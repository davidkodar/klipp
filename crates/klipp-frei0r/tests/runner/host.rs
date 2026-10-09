//! Subprocess worker for scripts/plugin-tests.py --format frei0r. No oracle
//! lives in the host: it opens one plug-in, sets the given parameters and
//! renders each frame, in order, through one kept `Frei0rInstanceSet`, so
//! a plug-in's history carries from frame to frame. A declared fallback is
//! printed as `KLIPP_HOST_FALLBACK`; a refusal as `KLIPP_HOST_REFUSED`,
//! exiting 3.
use klipp_frei0r::{host::scan_library, Fallback, Frei0rInstanceSet, Frei0rPlugin, Frei0rValue};
use std::{error::Error, path::Path, sync::Arc};

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 8 || !(args.len() - 5).is_multiple_of(3) {
        return Err("expected: plugin.so plugin-name width height params.json (time input.rgba output.rgba)+".into());
    }
    let width: u32 = args[2].parse()?;
    let height: u32 = args[3].parse()?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 || width as u64 * height as u64 > 1 << 22 {
        return Err("invalid diagnostic frame dimensions".into());
    }
    let params: Vec<(usize, Frei0rValue)> = serde_json::from_str(&std::fs::read_to_string(&args[4])?)?;
    let found = scan_library(Path::new(&args[0]));
    let Some(info) = found.into_iter().find(|p| p.name == args[1]) else {
        return Err(format!("required fixture missing: {}", args[1]).into());
    };
    let plugin = Arc::new(Frei0rPlugin::open(&info.path.clone(), info)?);
    let mut set = match Frei0rInstanceSet::new(plugin, width, height) {
        Ok(set) => set,
        Err(refusal) => {
            eprintln!("KLIPP_HOST_REFUSED {refusal}");
            std::process::exit(3);
        }
    };
    // By index, as the plug-in numbers them; the rest keep its defaults.
    for (index, value) in &params {
        set.set_param(*index, value);
    }
    for frame in args[5..].chunks_exact(3) {
        let time: f64 = frame[0].parse()?;
        if !time.is_finite() {
            return Err("invalid frame time".into());
        }
        let input = std::fs::read(&frame[1])?;
        if input.len() != width as usize * height as usize * 4 {
            return Err("input must be tightly packed RGBA8".into());
        }
        let output = match set.render(time, &input) {
            Ok(rendered) => {
                if let Some(Fallback::Padded { from, to }) = rendered.fallback {
                    eprintln!("KLIPP_HOST_FALLBACK padded {}x{} {}x{}", from.0, from.1, to.0, to.1);
                }
                rendered.rgba
            }
            Err(refusal) => {
                eprintln!("KLIPP_HOST_REFUSED {refusal}");
                std::process::exit(3);
            }
        };
        std::fs::write(&frame[2], output)?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
