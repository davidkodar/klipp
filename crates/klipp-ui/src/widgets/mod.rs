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

thread_local! {
    /// Below the menu bar this frame, noted as the frame begins.
    static BELOW_MENUS: std::cell::Cell<Option<egui::Rect>> = const { std::cell::Cell::new(None) };
}

/// Note where this frame's floating windows may go: below the menu bar.
/// Called as each frame begins, before any window is drawn.
pub fn note_screen(ctx: &egui::Context) {
    BELOW_MENUS.with(|r| r.set(Some(below_menu_bar(ctx))));
}

/// Every floating window of the app, kept below the menu bar so none
/// opens over the menus or is dragged under them. `egui` keeps a window
/// on the whole screen, menu bar included, unless told: the Subtitle
/// Editor and Check Spelling opened over File. Use this, never
/// `egui::Window::new` (the guard test says where it is still used).
pub fn window<'a>(title: impl Into<egui::WidgetText>) -> egui::Window<'a> {
    let w = egui::Window::new(title);
    match BELOW_MENUS.with(|r| r.get()) {
        Some(rect) => w.constrain_to(rect),
        None => w,
    }
}

#[cfg(test)]
mod below_menus_tests {
    /// No floating window opens where it pleases: each goes through
    /// [`super::window`], kept below the menu bar. A new
    /// `egui::Window::new` outside it fails here, wherever it is.
    #[test]
    fn every_window_opens_below_the_menus() {
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("widgets/mod.rs") {
                    let text = std::fs::read_to_string(&p).unwrap();
                    let opens = ["egui::", "Window::new("].concat();
                    if text.contains(&opens) {
                        out.push(p.display().to_string());
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
        assert!(found.is_empty(), "windows opened without crate::widgets::window: {found:?}");
    }

    /// Once a frame has begun, a window is kept below the menu bar.
    #[test]
    fn a_window_is_kept_below_the_menu_bar() {
        let ctx = egui::Context::default();
        let mut top = f32::MAX;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() }, |ctx| {
                super::note_screen(ctx);
                super::window("Subtitle Editor").show(ctx, |ui| ui.label("x"));
            });
            top = ctx.memory(|m| m.area_rect(egui::Id::new("Subtitle Editor"))).map_or(f32::MAX, |r| r.top());
        }
        assert!(top >= crate::panels::toolbar::MENU_BAR_HEIGHT, "opened at {top}, over the menu bar");
    }
}

