//! Read-only inspection; this binary has no write or conversion mode yet.
use klipp_convert::{Document, StoreKind};
fn main() {
    if let Err(e) = run() {
        eprintln!("klipp-convert: {e}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let kind = match args.next().as_deref() {
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
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
    let doc = Document::parse(&text, kind)?;
    println!(
        "{} picture sections read; file unchanged",
        doc.pictures.len()
    );
    Ok(())
}
