//! Pointer steps a harness plays into the app's input, one a frame: the
//! control socket's `pointer` test hook (#3056). They reach egui as a
//! real mouse's events do, so a drag goes through the same code a
//! person's does, and never move the machine's own pointer.

use serde::{Deserialize, Serialize};

/// Where a step puts the pointer: a point on the window, in points from
/// its top left, or a time on a timeline track, in seconds, at the
/// middle of the track's height (`track` its index in the sequence).
/// A point given as `dx`/`dy` is that far from the step before.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PointerAt {
    Screen { x: f32, y: f32 },
    Timeline { secs: f64, track: usize },
    Offset { dx: f32, dy: f32 },
}

/// What a step does there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PointerAction {
    /// Move the pointer there.
    #[default]
    Move,
    /// Press `button` there.
    Press,
    /// Let `button` go there.
    Release,
    /// Press and let go, a frame apart.
    Click,
    /// Turn the wheel by `scroll` points there.
    Scroll,
    /// Nothing: the frame passes with the pointer where it is.
    Wait,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PointerButton {
    #[default]
    Primary,
    Secondary,
    Middle,
}

/// The keys held through a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PointerModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// One step: where, what, with which button and keys, held for `frames`
/// frames (1 when not given).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerStep {
    #[serde(default)]
    pub action: PointerAction,
    #[serde(default)]
    pub at: Option<PointerAt>,
    #[serde(default)]
    pub button: PointerButton,
    #[serde(default)]
    pub modifiers: PointerModifiers,
    /// The wheel's turn for `scroll`, points across and down.
    #[serde(default)]
    pub scroll: [f32; 2],
    #[serde(default)]
    pub frames: Option<u32>,
}

/// The most steps one request plays.
pub const MAX_POINTER_STEPS: usize = 512;
/// The most frames one step holds.
pub const MAX_STEP_FRAMES: u32 = 600;

/// Why `steps` can't be played: too many, a step held too long, or a
/// point that isn't a finite number. A step that says no place plays
/// where the pointer is, which lasts from one request to the next.
pub fn pointer_steps_fault(steps: &[PointerStep]) -> Option<String> {
    if steps.is_empty() || steps.len() > MAX_POINTER_STEPS {
        return Some(format!("give 1 to {MAX_POINTER_STEPS} steps, not {}", steps.len()));
    }
    for (i, s) in steps.iter().enumerate() {
        if s.frames.is_some_and(|f| f == 0 || f > MAX_STEP_FRAMES) {
            return Some(format!("step {i}: frames is 1 to {MAX_STEP_FRAMES}"));
        }
        let finite = match s.at {
            Some(PointerAt::Screen { x, y }) | Some(PointerAt::Offset { dx: x, dy: y }) => x.is_finite() && y.is_finite(),
            Some(PointerAt::Timeline { secs, .. }) => secs.is_finite(),
            None => true,
        } && s.scroll.iter().all(|v| v.is_finite());
        if !finite {
            return Some(format!("step {i}: a number that isn't finite"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_read_and_bad_ones_are_refused() {
        let steps: Vec<PointerStep> = serde_json::from_value(serde_json::json!([
            {"action": "press", "at": {"timeline": {"secs": 1.0, "track": 2}}},
            {"at": {"offset": {"dx": 40.0, "dy": 0.0}}, "modifiers": {"shift": true}, "frames": 3},
            {"action": "release"},
            {"action": "scroll", "at": {"screen": {"x": 10.0, "y": 20.0}}, "scroll": [0.0, -120.0]},
            {"action": "wait", "frames": 10}
        ]))
        .unwrap();
        assert_eq!(pointer_steps_fault(&steps), None);
        assert!(steps[1].modifiers.shift && steps[1].frames == Some(3));

        let one = |v: serde_json::Value| serde_json::from_value::<Vec<PointerStep>>(v).map(|s| pointer_steps_fault(&s));
        // Where the last request left the pointer.
        assert_eq!(one(serde_json::json!([{"action": "release"}, {"at": {"offset": {"dx": 1.0, "dy": 1.0}}}])).unwrap(), None);
        assert!(one(serde_json::json!([{"at": {"screen": {"x": 1.0, "y": 1.0}}, "frames": 0}])).unwrap().unwrap().contains("frames"));
        assert!(one(serde_json::json!([{"at": {"screen": {"x": 1.0, "y": 1.0}}, "frames": 601}])).unwrap().unwrap().contains("frames"));
        assert!(one(serde_json::json!([])).unwrap().unwrap().contains("1 to 512"));
        assert!(one(serde_json::json!([{"action": "fling"}])).is_err());
        assert!(one(serde_json::json!([{"at": {"screen": {"x": 1.0, "y": 1.0}}, "force": 3}])).is_err());
        let many = vec![serde_json::json!({"at": {"screen": {"x": 1.0, "y": 1.0}}}); 513];
        assert!(one(serde_json::Value::Array(many)).unwrap().unwrap().contains("1 to 512"));
    }
}
