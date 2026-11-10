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

/// A number field. A typed value lands when the field is left or Enter is
/// pressed: "120" is one edit, one undo step and one picture, not 1, 12 and
/// 120. Dragging it still changes the value as it goes.
pub fn number_field<N: egui::emath::Numeric>(value: &mut N) -> NumberField<'_> {
    NumberField { inner: egui::DragValue::new(value).update_while_editing(false), range: None, clamps: true, suffix: String::new(), parser: None }
}

/// [`number_field`]'s field: a `DragValue` as built, which also says so
/// when a number typed into it is past its range, under the field for a
/// moment ("1000 Hz is the top"). The value was held to the range with no
/// word: a High-pass typed to 5000 read 1000, as if the 5000 was taken.
pub struct NumberField<'a> {
    inner: egui::DragValue<'a>,
    range: Option<(f64, f64)>,
    clamps: bool,
    suffix: String,
    parser: Option<Box<dyn 'a + Fn(&str) -> Option<f64>>>,
}

impl<'a> NumberField<'a> {
    pub fn speed(mut self, speed: impl Into<f64>) -> Self {
        self.inner = self.inner.speed(speed);
        self
    }
    pub fn range<Num: egui::emath::Numeric>(mut self, range: std::ops::RangeInclusive<Num>) -> Self {
        self.range = Some((range.start().to_f64(), range.end().to_f64()));
        self.inner = self.inner.range(range);
        self
    }
    pub fn clamp_range<Num: egui::emath::Numeric>(self, range: std::ops::RangeInclusive<Num>) -> Self {
        self.range(range)
    }
    pub fn clamp_existing_to_range(mut self, clamp: bool) -> Self {
        self.inner = self.inner.clamp_existing_to_range(clamp);
        self
    }
    pub fn clamp_to_range(mut self, clamp: bool) -> Self {
        self.clamps = clamp;
        #[allow(deprecated)]
        {
            self.inner = self.inner.clamp_to_range(clamp);
        }
        self
    }
    pub fn prefix(mut self, prefix: impl ToString) -> Self {
        self.inner = self.inner.prefix(prefix);
        self
    }
    pub fn suffix(mut self, suffix: impl ToString) -> Self {
        self.suffix = suffix.to_string();
        self.inner = self.inner.suffix(self.suffix.clone());
        self
    }
    pub fn min_decimals(mut self, n: usize) -> Self {
        self.inner = self.inner.min_decimals(n);
        self
    }
    pub fn max_decimals(mut self, n: usize) -> Self {
        self.inner = self.inner.max_decimals(n);
        self
    }
    pub fn max_decimals_opt(mut self, n: Option<usize>) -> Self {
        self.inner = self.inner.max_decimals_opt(n);
        self
    }
    pub fn fixed_decimals(mut self, n: usize) -> Self {
        self.inner = self.inner.fixed_decimals(n);
        self
    }
    pub fn custom_formatter(mut self, formatter: impl 'a + Fn(f64, std::ops::RangeInclusive<usize>) -> String) -> Self {
        self.inner = self.inner.custom_formatter(formatter);
        self
    }
    pub fn custom_parser(mut self, parser: impl 'a + Fn(&str) -> Option<f64>) -> Self {
        self.parser = Some(Box::new(parser));
        self
    }
    pub fn update_while_editing(mut self, update: bool) -> Self {
        self.inner = self.inner.update_while_editing(update);
        self
    }
}

/// How long the note on a number typed past its range stays, seconds.
const PAST_RANGE_SECS: f64 = 2.5;

/// What a field says when `typed` was past its range `lo..=hi`.
pub fn past_range_note(typed: f64, lo: f64, hi: f64, suffix: &str) -> Option<String> {
    let shown = |v: f64| {
        let n = if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v}") };
        format!("{n}{}", suffix.trim_end())
    };
    if typed > hi {
        Some(format!("{} is the top", shown(hi)))
    } else if typed < lo {
        Some(format!("{} is the bottom", shown(lo)))
    } else {
        None
    }
}

impl egui::Widget for NumberField<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let NumberField { inner, range, clamps, suffix, parser } = self;
        // What was typed, before it is held to the range.
        let typed = std::rc::Rc::new(std::cell::Cell::new(None::<f64>));
        let seen = typed.clone();
        let inner = inner.custom_parser(move |text| {
            let v = match &parser {
                Some(p) => p(text),
                None => text.chars().filter(|c| !c.is_whitespace()).map(|c| if c == '\u{2212}' { '-' } else { c }).collect::<String>().parse().ok(),
            };
            seen.set(v);
            v
        });
        let response = ui.add(inner);
        let note_id = response.id.with("past_range");
        let now = ui.input(|i| i.time);
        if response.lost_focus() && clamps {
            if let (Some(v), Some((lo, hi))) = (typed.get(), range) {
                if let Some(note) = past_range_note(v, lo, hi, &suffix) {
                    ui.ctx().data_mut(|d| d.insert_temp(note_id, (note, now + PAST_RANGE_SECS)));
                }
            }
        }
        if let Some((note, until)) = ui.ctx().data(|d| d.get_temp::<(String, f64)>(note_id)) {
            if now < until {
                egui::show_tooltip_at(ui.ctx(), ui.layer_id(), note_id, response.rect.left_bottom(), |ui| {
                    ui.label(note);
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(until - now));
            } else {
                ui.ctx().data_mut(|d| d.remove::<(String, f64)>(note_id));
            }
        }
        response
    }
}

#[cfg(test)]
mod past_range_tests {
    /// A number typed past a field's range is held to it, and the field
    /// says so for a moment: 5000 into a 0..1000 Hz field reads 1000, and
    /// "1000 Hz is the top" shows under it, then goes. A number inside the
    /// range says nothing. It was held with no word.
    #[test]
    fn a_number_typed_past_the_range_says_so() {
        assert_eq!(super::past_range_note(-3.0, 0.0, 1.0, ""), Some("0 is the bottom".into()));
        assert_eq!(super::past_range_note(0.5, 0.0, 1.0, " dB"), None);
        let typed = |text: &str| {
            let ctx = egui::Context::default();
            let mut value = 200.0_f32;
            let mut clock = 0.0;
            let mut rect = egui::Rect::NOTHING;
            let mut frame = |events: Vec<egui::Event>, value: &mut f32, rect: &mut egui::Rect, dt: f64| {
                clock += dt;
                let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0))), time: Some(clock), events, ..Default::default() };
                let out = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| *rect = ui.add(super::number_field(value).range(0.0..=1000.0).suffix(" Hz")).rect);
                });
                out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect::<Vec<_>>()
            };
            frame(vec![], &mut value, &mut rect, 0.05);
            let at = rect.center();
            let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            frame(vec![egui::Event::PointerMoved(at)], &mut value, &mut rect, 0.05);
            frame(vec![b(true)], &mut value, &mut rect, 0.05);
            frame(vec![b(false)], &mut value, &mut rect, 0.05);
            frame(vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }], &mut value, &mut rect, 0.05);
            frame(vec![egui::Event::Text(text.into())], &mut value, &mut rect, 0.05);
            frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }], &mut value, &mut rect, 0.05);
            let shown = frame(vec![], &mut value, &mut rect, 0.05);
            let later = frame(vec![], &mut value, &mut rect, 3.0);
            let gone = frame(vec![], &mut value, &mut rect, 0.05);
            let _ = later;
            (value, shown, gone)
        };
        let (value, shown, gone) = typed("5000");
        assert_eq!(value, 1000.0, "held to the range");
        assert!(shown.iter().any(|t| t == "1000 Hz is the top"), "said under the field: {shown:?}");
        assert!(!gone.iter().any(|t| t.contains("is the top")), "and gone after a moment: {gone:?}");
        let (value, shown, _) = typed("500");
        assert_eq!(value, 500.0);
        assert!(!shown.iter().any(|t| t.contains("is the")), "{shown:?}");
    }
}

#[cfg(test)]
mod number_field_tests {
    /// Typing leaves the value alone until Enter, and Enter changes it once.
    #[test]
    fn a_typed_number_is_one_edit() {
        let ctx = egui::Context::default();
        let mut value = 9.0_f32;
        let mut changes = 0;
        let mut frame = |events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(200.0, 100.0))), events, ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let id = egui::Id::new("field");
                    let response = ui.push_id(id, |ui| ui.add(super::number_field(&mut value))).inner;
                    ui.memory_mut(|m| m.request_focus(response.id));
                    if response.changed() {
                        changes += 1;
                    }
                });
            });
        };
        frame(Vec::new());
        frame(Vec::new());
        frame(vec![egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
        for digit in ["1", "2", "0"] {
            frame(vec![egui::Event::Text(digit.into())]);
        }
        frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        frame(Vec::new());
        assert_eq!((value, changes), (120.0, 1), "the value and how many edits made it");
    }

    /// Every number field goes through it, so no new one types live.
    #[test]
    fn every_number_field_lands_on_leaving() {
        fn scan(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    scan(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("widgets/mod.rs") && !path.ends_with("app/keymap_tests.rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    if text.contains(concat!("DragValue", "::new(")) {
                        found.push(path.display().to_string());
                    }
                }
            }
        }
        let mut found = Vec::new();
        scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
        assert!(found.is_empty(), "{found:?}");
    }
}
