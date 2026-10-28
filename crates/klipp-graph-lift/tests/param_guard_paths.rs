//! Every path a number takes into an effect passes the one guard
//! (`klipp_effects::registry::guard_number` and what wraps it, #2995):
//! each writer below is read for its call, and a new reader of an effect
//! from outside data, outside these, fails until it is guarded and listed.

const FILES: &[(&str, &str)] = &[
    ("klipp-effects/src/animate.rs", include_str!("../../klipp-effects/src/animate.rs")),
    ("klipp-graph-lift/src/effect_params.rs", include_str!("../src/effect_params.rs")),
    ("klipp-graph-lift/src/ops.rs", include_str!("../src/ops.rs")),
    ("klipp-timeline/src/picture_ops.rs", include_str!("../../klipp-timeline/src/picture_ops.rs")),
    ("klipp-project/src/load.rs", include_str!("../../klipp-project/src/load.rs")),
    ("klipp-compose/src/resolve.rs", include_str!("../../klipp-compose/src/resolve.rs")),
    ("klipp-compose/src/render.rs", include_str!("../../klipp-compose/src/render.rs")),
    ("klipp-document/src/ops.rs", include_str!("../../klipp-document/src/ops.rs")),
    ("klipp-nodes/src/params.rs", include_str!("../../klipp-nodes/src/params.rs")),
    ("klipp-control/src/graph_model.rs", include_str!("../../klipp-control/src/graph_model.rs")),
];

/// (file, function, the guard it calls, how many times).
const GUARDED: &[(&str, &str, &str, usize)] = &[
    // Every keyframed value, as the render animates it.
    ("klipp-effects/src/animate.rs", "pub fn animate(", "guard_number(", 1),
    // The render's read of an effect node, and the edits' guard.
    ("klipp-graph-lift/src/effect_params.rs", "pub fn effect(", "guard_entry(", 1),
    ("klipp-graph-lift/src/effect_params.rs", "pub fn guard_params(", "guard_entry(", 1),
    // The graph ops: a value, a payload, a new node, keys.
    ("klipp-graph-lift/src/ops.rs", "fn set_param(", "guard_params(", 1),
    ("klipp-graph-lift/src/ops.rs", "fn set_payload(", "guard_params(", 1),
    ("klipp-graph-lift/src/ops.rs", "fn held_node(", "guard_params(", 1),
    ("klipp-graph-lift/src/ops.rs", "fn apply_anim(", "guard_key(", 2),
    ("klipp-graph-lift/src/ops.rs", "fn guard_key(", "guard_number(", 1),
    // The same ops on a clip's effect list.
    ("klipp-timeline/src/picture_ops.rs", "fn write_effect_op(", "guard_params(", 2),
    // A project file's effect lists.
    ("klipp-project/src/load.rs", "fn read_builtin_effects(", "guard_entry(", 1),
    // A placement's stored Transform, and the list the render falls back to.
    ("klipp-compose/src/resolve.rs", "fn decode(", "guard_entry(", 1),
    ("klipp-compose/src/render.rs", "pub fn list_effects(", "guard_effect(", 1),
    // The document ops' settings.
    ("klipp-document/src/ops.rs", "pub fn with_params(", "guard_number(", 1),
    // A grade value.
    ("klipp-nodes/src/params.rs", "pub fn apply_override(", "if !value.is_finite() && ", 1),
];

/// (file, readers of an effect from JSON outside tests): guarded above, or
/// reading a plug-in's own values, which the registry doesn't describe.
const READERS: &[(&str, usize)] = &[
    // The validation the ops run after the guard.
    ("klipp-graph-lift/src/ops.rs", 1),
    ("klipp-project/src/load.rs", 1),
    ("klipp-compose/src/resolve.rs", 1),
    // A frei0r plug-in's parameters.
    ("klipp-control/src/graph_model.rs", 1),
];

/// The body of the function whose signature starts `sig`: to the brace
/// that closes it at the signature's indent.
fn body<'a>(text: &'a str, file: &str, sig: &str) -> &'a str {
    let at = text.find(sig).unwrap_or_else(|| panic!("{file}: no {sig}"));
    let line = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let indent = &text[line..at];
    let close = format!("\n{indent}}}\n");
    let end = text[at..].find(&close).unwrap_or_else(|| panic!("{file}: {sig} doesn't close"));
    &text[at..at + end]
}

/// The source outside its `#[cfg(test)]` module.
fn non_test(text: &str) -> &str {
    text.find("#[cfg(test)]\nmod tests").map_or(text, |at| &text[..at])
}

#[test]
fn every_effect_write_path_calls_the_guard() {
    for (file, sig, call, n) in GUARDED {
        let text = FILES.iter().find(|(f, _)| f == file).unwrap_or_else(|| panic!("{file} isn't read")).1;
        let got = body(text, file, sig).matches(call).count();
        assert_eq!(got, *n, "{file}: {sig} calls {call} {got} times, not {n}");
    }
}

#[test]
fn no_unguarded_reader_of_an_effect_from_json() {
    for (file, text) in FILES {
        let text = non_test(text);
        let n = text.matches("from_value::<klipp_effects::BuiltinEffect>").count()
            + text.matches("from_value::<BuiltinEffect>").count()
            + text.matches("BuiltinEffect = serde_json::from_value").count();
        let known = READERS.iter().find(|(f, _)| f == file).map_or(0, |r| r.1);
        assert_eq!(n, known, "{file}: {n} readers of an effect from JSON, {known} known guarded: guard a new one and list it");
    }
}
