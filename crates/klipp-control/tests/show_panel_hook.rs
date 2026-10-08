//! The socket's `show_panel` is a test hook, as `effect_command` is: absent
//! from the default build, present only with `test-hooks` (which a release
//! build refuses).

use klipp_control::protocol::ControlCommand;
use serde_json::json;

#[cfg(not(feature = "test-hooks"))]
#[test]
fn the_default_build_has_no_show_panel() {
    assert!(serde_json::from_value::<ControlCommand>(json!({"method": "show_panel", "inspector_tab": "effects"})).is_err());
}

#[cfg(feature = "test-hooks")]
#[test]
fn the_hooks_read_show_panel() {
    for v in [json!({"method": "show_panel", "inspector_tab": "effects"}), json!({"method": "show_panel", "workspace": "colour"}), json!({"method": "show_panel"})] {
        assert!(serde_json::from_value::<ControlCommand>(v.clone()).is_ok(), "{v}");
    }
}
