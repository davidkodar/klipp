//! The socket's `pointer` is a test hook, as `show_panel` is: absent from
//! the default build, present only with `test-hooks` (which a release
//! build refuses). Its steps are read whole or refused.

use klipp_control::protocol::ControlCommand;
use serde_json::json;

fn drag() -> serde_json::Value {
    json!({"method": "pointer", "steps": [
        {"action": "press", "at": {"timeline": {"secs": 1.0, "track": 1}}},
        {"at": {"offset": {"dx": 120.0, "dy": 0.0}}, "frames": 4},
        {"action": "release", "modifiers": {"shift": true}},
        {"action": "scroll", "at": {"screen": {"x": 300.0, "y": 200.0}}, "scroll": [0.0, -60.0]}
    ]})
}

#[cfg(not(feature = "test-hooks"))]
#[test]
fn the_default_build_has_no_pointer() {
    assert!(serde_json::from_value::<ControlCommand>(drag()).is_err());
}

#[cfg(feature = "test-hooks")]
#[test]
fn the_hooks_read_pointer_steps() {
    let ControlCommand::Pointer { steps } = serde_json::from_value::<ControlCommand>(drag()).unwrap() else { panic!("a pointer request") };
    assert_eq!(steps.len(), 4);
    assert_eq!(klipp_core::pointer::pointer_steps_fault(&steps), None);
    assert!(serde_json::from_value::<ControlCommand>(json!({"method": "pointer", "steps": [{"action": "drag"}]})).is_err());
}
