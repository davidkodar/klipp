//! The socket's `effect_command` is a test hook: absent from the default
//! build, present only with `test-hooks` (which a release build refuses).

use klipp_control::protocol::ControlCommand;
use serde_json::json;

fn add() -> serde_json::Value {
    json!({"method": "effect_command", "clip_id": "00000000-0000-0000-0000-000000000001", "action": "add", "effect": {"type": "sharpen", "amount": 0.5}})
}

#[cfg(not(feature = "test-hooks"))]
#[test]
fn the_default_build_has_no_effect_command() {
    assert!(serde_json::from_value::<ControlCommand>(add()).is_err());
}

#[cfg(feature = "test-hooks")]
#[test]
fn the_hooks_read_every_effect_action() {
    for action in ["add", "remove", "toggle", "update", "reorder", "drag_begin", "drag_step", "drag_end", "drag_abandon"] {
        let mut v = add();
        v["action"] = json!(action);
        assert!(serde_json::from_value::<ControlCommand>(v).is_ok(), "{action}");
    }
}
