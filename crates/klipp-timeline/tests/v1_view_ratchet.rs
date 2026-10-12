//! The v1 view's uses only shrink (M4 step 12). Each of its accessors,
//! types and adapters is counted across every crate's `src` and `tests`,
//! and the count must be the one pinned in `v1_view_pins.txt`: a new use
//! fails, and so does one removed without lowering its pin in the same
//! change, so the pins are the live inventory. A symbol whose pin reaches
//! 0 moves to `GONE`, where any use fails.
//!
//! Picture revision 5 deletes the legacy payload the view is read from
//! (group G): its types, fields and type ids are counted the same way.
//!
//! Counted as text: a method as `.name(` or `fn name(`, a type or path as
//! the whole word, a field as `.name`, comment lines skipped; group G's
//! forms skip string literals, where `klipp.legacy.node` is a type id and
//! not a field. An unpinned symbol fails with its count, to pin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How a symbol is found.
#[derive(Clone, Copy)]
enum Form {
    /// A method: `.name(` or `fn name(`.
    Method,
    /// A type, a path or a constant, as the whole word.
    Word,
    /// A type as the whole word, outside string literals.
    Type,
    /// A field: `.name` outside string literals, not a call.
    Field,
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
    ("transform_scale_y_keyframes", Form::Method),
    ("edit_transform_scale_y_keyframes", Form::Method),
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
    // G: the legacy payload (picture revision 5).
    ("Legacy", Form::Type),
    ("LegacyFlags", Form::Type),
    ("ClipRecordV2", Form::Type),
    ("LegacyClipAnim", Form::Type),
    ("MissingLegacy", Form::Type),
    ("SetPayload", Form::Type),
    ("SetPayloadAt", Form::Type),
    ("legacy", Form::Field),
    ("legacy_flags", Form::Field),
    ("LEGACY_NODE", Form::Word),
    ("LEGACY_GRADE", Form::Word),
    ("LEGACY_OPACITY", Form::Word),
    ("LEGACY_TRANSFORM", Form::Word),
    ("LEGACY_SPEED_RAMP", Form::Word),
    ("LEGACY_TEMPORAL_DENOISE", Form::Word),
    ("STRUCTURAL", Form::Word),
];

/// Symbols at 0, gone: any use fails.
const GONE: &[&str] = &[];

const PINS: &str = include_str!("v1_view_pins.txt");
const FILES: &str = include_str!("v1_view_files.txt");

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

/// Whether byte `at` of `line` is inside a string literal: an odd count
/// of unescaped quotes before it.
fn in_string(line: &str, at: usize) -> bool {
    let mut quotes = 0;
    let mut escaped = false;
    for c in line[..at].chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '"' => quotes += 1,
            _ => {}
        }
    }
    quotes % 2 == 1
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
                Form::Type => !before.is_some_and(ident) && !after.is_some_and(ident) && !in_string(line, *at),
                Form::Field => before == Some('.') && !after.is_some_and(ident) && after != Some('(') && !in_string(line, *at),
            }
        })
        .count()
}

/// The files that use any of the view's symbols, relative to the crates
/// folder (`klipp-ui/src/state.rs`).
fn users() -> std::collections::BTreeSet<String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the crates folder");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(crates).expect("the crates folder").flatten() {
        sources(&krate.path().join("src"), &mut files);
        sources(&krate.path().join("tests"), &mut files);
    }
    let me = file!().rsplit('/').next().unwrap_or_default();
    files
        .iter()
        .filter(|f| !f.ends_with(me))
        .filter(|f| {
            let text = std::fs::read_to_string(f).unwrap_or_default();
            text.lines().filter(|l| !l.trim_start().starts_with("//")).any(|line| SYMBOLS.iter().any(|(name, form)| count(line, name, *form) > 0))
        })
        .filter_map(|f| f.strip_prefix(crates).ok().map(|p| p.to_string_lossy().into_owned()))
        .collect()
}

/// No file starts using the v1 view, and a file that stops is taken off
/// the list (M4 step 12f): the list only shrinks, to load, save and the
/// converter, which keep the view until picture revision 5.
#[test]
fn only_the_listed_files_use_the_v1_view() {
    let listed: std::collections::BTreeSet<String> = FILES.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect();
    let now = users();
    let mut problems = Vec::new();
    for f in now.difference(&listed) {
        problems.push(format!("{f} uses the v1 view and isn't in v1_view_files.txt: read and write the clip's picture instead"));
    }
    for f in listed.difference(&now) {
        problems.push(format!("{f} no longer uses the v1 view: take it out of v1_view_files.txt"));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
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

#[test]
fn a_field_or_type_is_counted_outside_strings() {
    assert_eq!(count("let l = n.legacy.as_ref();", "legacy", Form::Field), 1);
    assert_eq!(count("n.legacy_flags = None;", "legacy", Form::Field), 0);
    assert_eq!(count("c.legacy()", "legacy", Form::Field), 0);
    assert_eq!(count(r#"const T: &str = "klipp.legacy.node";"#, "legacy", Form::Field), 0);
    assert_eq!(count(r#"f("a\"b", n.legacy)"#, "legacy", Form::Field), 1);
    assert_eq!(count("fn node(l: Option<Legacy>) -> Node", "Legacy", Form::Type), 1);
    assert_eq!(count(r#""name": "Legacy","#, "Legacy", Form::Type), 0);
    assert_eq!(count("LegacyFlags::default()", "Legacy", Form::Type), 0);
}
