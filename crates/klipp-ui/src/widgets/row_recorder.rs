//! Test-only observations of the controls the inspector actually draws.
//!
//! Recording never drives a widget or writes a fixture. Each capture owns
//! its records, including when captures nest or a draw unwinds.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;

/// A control observed before its input is processed. Missing properties
/// mean the old widget has no such property, not a registry-derived default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub context: Vec<String>,
    pub parameter: Option<String>,
    pub label: String,
    pub value: Value,
    pub range: Option<[f64; 2]>,
    pub reset: Option<Value>,
    pub speed: Option<f64>,
    pub decimals: Option<usize>,
    pub suffix: String,
    pub keyframe_state: Option<KeyframeRecord>,
    pub widget: String,
    pub widget_id: u64,
}

/// The authored-key state supplied to a scrubby row's diamond.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyframeRecord {
    pub at_playhead: bool,
    pub any: bool,
    pub in_frame: usize,
    pub previous: Option<f64>,
    pub next: Option<f64>,
    pub interpolation: Option<String>,
}

#[derive(Default)]
struct Recording {
    context: Vec<String>,
    next_parameter: Option<String>,
    rows: Vec<Row>,
}

thread_local! {
    static RECORDINGS: RefCell<Vec<Recording>> = const { RefCell::new(Vec::new()) };
}

struct CaptureGuard;
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        RECORDINGS.with(|s| {
            s.borrow_mut().pop();
        });
    }
}

/// Run one draw with a fresh sink. Records outside a capture are discarded.
pub fn capture<T>(draw: impl FnOnce() -> T) -> (T, Vec<Row>) {
    RECORDINGS.with(|s| s.borrow_mut().push(Recording::default()));
    let guard = CaptureGuard;
    let result = draw();
    let rows = RECORDINGS.with(|s| std::mem::take(&mut s.borrow_mut().last_mut().unwrap().rows));
    drop(guard);
    (result, rows)
}

/// A lexical inspector/type/subgroup context; it does not change egui IDs.
pub struct Scope {
    depth: usize,
}

/// Distinguish repeated labels in different types and nested custom editors.
pub fn scope(name: impl Into<String>) -> Scope {
    let depth = RECORDINGS.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(recording) = s.last_mut() {
            recording.context.push(name.into());
        }
        s.len()
    });
    Scope { depth }
}

impl Drop for Scope {
    fn drop(&mut self) {
        RECORDINGS.with(|s| {
            let mut s = s.borrow_mut();
            if self.depth > 0 && s.len() == self.depth {
                s.last_mut().unwrap().context.pop();
            }
        });
    }
}

/// Record a control at its real call site, before the widget changes it.
pub fn record(mut row: Row) {
    RECORDINGS.with(|s| {
        if let Some(recording) = s.borrow_mut().last_mut() {
            row.context.clone_from(&recording.context);
            let key = recording.next_parameter.take();
            if row.parameter.is_none() {
                row.parameter = key;
            }
            recording.rows.push(row);
        }
    });
}

/// Associate the next drawn control with the key its caller edits.
pub fn next_parameter(key: &str) {
    RECORDINGS.with(|s| {
        if let Some(recording) = s.borrow_mut().last_mut() {
            recording.next_parameter = Some(key.into());
        }
    });
}

/// Observe a choice, toggle, colour or custom editor at its real draw site.
pub fn control(id: egui::Id, widget: &str, parameter: &str, label: &str, value: impl Serialize) {
    record(Row {
        context: Vec::new(),
        parameter: Some(parameter.into()),
        label: label.into(),
        value: serde_json::to_value(value).expect("serializable inspector value"),
        range: None,
        reset: None,
        speed: None,
        decimals: None,
        suffix: String::new(),
        keyframe_state: None,
        widget: widget.into(),
        widget_id: id.value(),
    });
}

/// Attach the caller's actual parameter key to the preceding numeric row.
/// This is for call sites that already associate a response with a key.
pub fn parameter(key: &str) {
    RECORDINGS.with(|s| {
        if let Some(row) = s.borrow_mut().last_mut().and_then(|r| r.rows.last_mut()) {
            row.parameter = Some(key.into());
        }
    });
}

/// Observe a numeric row without inventing properties absent from its UI.
#[allow(clippy::too_many_arguments)]
pub fn numeric(
    id: egui::Id,
    widget: &str,
    label: &str,
    value: f32,
    range: &std::ops::RangeInclusive<f32>,
    reset: Option<f32>,
    speed: Option<f64>,
    decimals: Option<usize>,
    suffix: &str,
    keyframe: Option<super::scrubby_param::KeyframeState>,
) {
    record(Row {
        context: Vec::new(),
        parameter: None,
        label: label.into(),
        value: Value::from(value),
        range: Some([f64::from(*range.start()), f64::from(*range.end())]),
        reset: reset.map(Value::from),
        speed,
        decimals,
        suffix: suffix.into(),
        keyframe_state: keyframe.map(|k| KeyframeRecord {
            at_playhead: k.has_keyframe_at_playhead,
            any: k.has_any_keyframes,
            in_frame: k.keys_in_frame,
            previous: k.prev_keyframe_time,
            next: k.next_keyframe_time,
            interpolation: k.current_interpolation.map(|i| format!("{i:?}")),
        }),
        widget: widget.into(),
        widget_id: id.value(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw() {
        numeric(
            egui::Id::new("test"),
            "slider",
            "Value",
            0.5,
            &(0.0..=1.0),
            None,
            None,
            Some(2),
            "",
            None,
        );
    }

    #[test]
    fn captures_are_isolated_nested_and_unwind_safe() {
        draw(); // No active sink.
        let (_, first) = capture(|| {
            let _scope = scope("first");
            draw();
            let (_, inner) = capture(draw);
            assert_eq!(inner.len(), 1);
            assert!(inner[0].context.is_empty());
            draw();
        });
        assert_eq!(first.len(), 2);
        assert_eq!(first[0], first[1]);
        assert_eq!(first[0].context, ["first"]);
        let _ = std::panic::catch_unwind(|| {
            capture(|| {
                let _scope = scope("panicking");
                draw();
                panic!("test unwind");
            })
        });
        let (_, next) = capture(draw);
        assert_eq!(next.len(), 1);
        assert!(next[0].context.is_empty());
        assert_eq!(next, capture(draw).1);
        assert!(capture(|| {}).1.is_empty());
    }
}
