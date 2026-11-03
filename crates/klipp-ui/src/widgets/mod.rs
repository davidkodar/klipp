pub mod color_slider;
pub mod color_wheel;
pub mod curves_editor;
pub mod easing_curve;
pub mod grade_drag;
pub mod inspector_group;
pub mod jog_wheel;
pub mod keyframe_editor;
pub mod menu;
pub mod panel_rail;
pub mod resizer;
pub mod scopes;
pub mod scope_window;
pub mod role_pill;
pub mod scrubby_param;
#[cfg(any(test, feature = "row-recorder"))]
pub mod row_recorder;
pub mod segmented;
pub use scrubby_param::EasePreset;
pub mod slider;
pub mod toast;
pub mod toggle;
pub mod timecode_edit;
pub mod tooltip;
pub mod vertical_slider;
pub mod waveform;
pub mod wordmark;

/// Where a floating panel may open and be dragged: below the menu bar, so it
/// never sits over the menus (#3428). `egui` would otherwise open it at the
/// window's top left.
pub fn below_menu_bar(ctx: &egui::Context) -> egui::Rect {
    let screen = ctx.screen_rect();
    screen.with_min_y(screen.min.y + crate::panels::toolbar::MENU_BAR_HEIGHT)
}
