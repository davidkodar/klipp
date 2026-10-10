//! The v1 view's uses only shrink (M4 step 12). Each of its accessors,
//! types and adapters is counted across every crate's `src` and `tests`,
//! and the count must be the one pinned in `v1_view_pins.txt`: a new use
//! fails, and so does one removed without lowering its pin in the same
//! change, so the pins are the live inventory. A symbol whose pin reaches
//! 0 moves to `GONE`, where any use fails.
//!
//! Counted as text: a method as `.name(` or `fn name(`, a type or path as
//! the whole word, comment lines skipped. An unpinned symbol fails with
//! its count, to pin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How a symbol is found.
#[derive(Clone, Copy)]
enum Form {
    /// A method: `.name(` or `fn name(`.
    Method,
    /// A type, a path or a constant, as the whole word.
    Word,
}

/// The v1 view, by group (m4-step12-inventory).
const SYMBOLS: &[(&str, Form)] = &[
    // A: the built-in effect list.
    ("builtin_effects", Form::Method),
    ("builtin_effects_enabled", Form::Method),
    ("set_effect_list", Form::Method),
    ("set_effect_list_with_ids", Form::Method),
    ("set_effects", Form::Method),
    ("push_effect", Form::Method),
    ("insert_effect", Form::Method),
    ("insert_effect_with_id", Form::Method),
    ("remove_effect", Form::Method),
    ("move_effect", Form::Method),
    ("set_effect_enabled", Form::Method),
    ("replace_effect", Form::Method),
    ("remove_builtin_effect", Form::Method),
    ("move_builtin_effect", Form::Method),
    ("effect_id_list", Form::Method),
    ("effect_id_at", Form::Method),
    ("effect_entries", Form::Method),
    ("refresh_effect_ids", Form::Method),
    ("effects_state", Form::Method),
    ("restore_effects_state", Form::Method),
    ("own_effects", Form::Method),
    ("set_own_effects", Form::Method),
    // B: effect keys by "{index}:{param}".
    ("effect_keyframes", Form::Method),
    ("edit_effect_keyframes", Form::Method),
    ("effect_param_keyframes", Form::Method),
    ("edit_effect_keyframes_of", Form::Method),
    ("edit_existing_effect_keyframes", Form::Method),
    ("remove_effect_keyframes", Form::Method),
    ("restore_effect_keyframes", Form::Method),
    ("edit_effect_modifiers", Form::Method),
    ("edit_existing_effect_modifiers", Form::Method),
    ("remove_effect_modifiers", Form::Method),
    ("ParamId::Effect", Form::Word),
    // C: the clip's lanes.
    ("opacity_keyframes", Form::Method),
    ("edit_opacity_keyframes", Form::Method),
    ("transform_pos_x_keyframes", Form::Method),
    ("edit_transform_pos_x_keyframes", Form::Method),
    ("transform_pos_y_keyframes", Form::Method),
    ("edit_transform_pos_y_keyframes", Form::Method),
    ("transform_scale_keyframes", Form::Method),
    ("edit_transform_scale_keyframes", Form::Method),
    ("transform_rotation_keyframes", Form::Method),
    ("edit_transform_rotation_keyframes", Form::Method),
    ("transform_tilt_x_keyframes", Form::Method),
    ("edit_transform_tilt_x_keyframes", Form::Method),
    ("transform_tilt_y_keyframes", Form::Method),
    ("edit_transform_tilt_y_keyframes", Form::Method),
    ("video_param_keyframes", Form::Method),
    ("video_param_value", Form::Method),
    ("edit_video_param_keyframes", Form::Method),
    ("param_modifiers", Form::Method),
    ("edit_param_modifiers", Form::Method),
    // D: OFX and track effect lists.
    ("ofx_effects", Form::Method),
    ("edit_ofx_effect", Form::Method),
    ("edit_ofx_effects", Form::Method),
    ("set_ofx_effects", Form::Method),
    ("video_effects", Form::Method),
    ("video_effects_enabled", Form::Method),
    ("edit_video_effects", Form::Method),
    ("set_video_effect_list", Form::Method),
    // E: the view and its types.
    ("ClipPictureFields", Form::Word),
    ("ClipPictureParts", Form::Word),
    ("PictureView", Form::Word),
    // F: graph-lift's v1 side.
    ("delift", Form::Word),
    ("to_v1", Form::Word),
    ("unlift", Form::Word),
    ("LEGACY_EFFECT", Form::Word),
    ("LEGACY_OFX", Form::Word),
];

/// Symbols at 0, gone: any use fails.
const GONE: &[&str] = &[];

const PINS: &str = include_str!("v1_view_pins.txt");

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// How often `name` occurs in `line` as `form`.
fn count(line: &str, name: &str, form: Form) -> usize {
    line.match_indices(name)
        .filter(|(at, _)| {
            let before = line[..*at].chars().next_back();
            let after = line[at + name.len()..].chars().next();
            match form {
                Form::Method => after == Some('(') && (before == Some('.') || line[..*at].ends_with("fn ")),
                Form::Word => !before.is_some_and(ident) && !after.is_some_and(ident),
            }
        })
        .count()
}

/// Every symbol's count across the workspace's sources.
fn counts() -> BTreeMap<&'static str, usize> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the crates folder");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(crates).expect("the crates folder").flatten() {
        sources(&krate.path().join("src"), &mut files);
        sources(&krate.path().join("tests"), &mut files);
    }
    assert!(files.len() > 200, "the scan found {} files: the crates folder moved?", files.len());
    let mut out: BTreeMap<&str, usize> = SYMBOLS.iter().map(|(n, _)| (*n, 0)).collect();
    // This file names them all.
    let me = file!().rsplit('/').next().unwrap_or_default();
    for file in files.iter().filter(|f| !f.ends_with(me)) {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
            for (name, form) in SYMBOLS {
                *out.get_mut(name).unwrap() += count(line, name, *form);
            }
        }
    }
    out
}

fn pins() -> BTreeMap<String, usize> {
    PINS.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (name, n) = l.split_once(char::is_whitespace)?;
            Some((name.to_owned(), n.trim().parse().ok()?))
        })
        .collect()
}

#[test]
fn the_v1_views_uses_only_shrink() {
    let (now, pinned) = (counts(), pins());
    let mut problems = Vec::new();
    for (name, n) in &now {
        match pinned.get(*name) {
            None => problems.push(format!("{name} isn't pinned: add `{name} {n}` to v1_view_pins.txt")),
            Some(p) if n > p => problems.push(format!("{name}: {n} uses, pinned at {p}: a new use of the v1 view")),
            Some(p) if n < p => problems.push(format!("{name}: {n} uses, pinned at {p}: lower its pin to {n} in this change{}", if *n == 0 { ", and move it to GONE" } else { "" })),
            Some(_) => {}
        }
    }
    for name in pinned.keys().filter(|k| !now.contains_key(k.as_str())) {
        problems.push(format!("{name} is pinned but not scanned: take it out of v1_view_pins.txt"));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn a_gone_symbol_stays_gone() {
    let now = counts();
    let back: Vec<_> = GONE.iter().filter(|n| now.get(*n).is_some_and(|c| *c > 0)).collect();
    assert!(back.is_empty(), "gone, and used again: {back:?}");
}

#[test]
fn a_method_is_counted_as_a_call_or_a_definition() {
    assert_eq!(count("c.builtin_effects().len()", "builtin_effects", Form::Method), 1);
    assert_eq!(count("pub fn builtin_effects(&self)", "builtin_effects", Form::Method), 1);
    assert_eq!(count("c.builtin_effects_enabled()", "builtin_effects", Form::Method), 0);
    assert_eq!(count("let builtin_effects = 1;", "builtin_effects", Form::Method), 0);
    assert_eq!(count("use klipp_graph_lift::delift;", "delift", Form::Word), 1);
    assert_eq!(count("fn undelifted()", "delift", Form::Word), 0);
}
