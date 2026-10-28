//! First-time help (#2933): a small Welcome window over the editor, the
//! first time a project is open there, and from Help → Welcome after; and
//! an optional four-step tour, each step a callout at the real control it
//! names. The steps and their words are data (`assets/help/tour.json`).

use crate::keybindings::{KeyBindings, KeymapPreset};
use crate::state::AppState;
use crate::theme::KlippTheme;

/// The interface sizes offered in the View menu and the small-screen
/// note: Auto and the fixed ones, smaller ones for a small window (#2944).
pub const INTERFACE_SIZES: [(&str, u32); 8] = [("Auto", 0), ("80%", 80), ("90%", 90), ("100%", 100), ("125%", 125), ("150%", 150), ("175%", 175), ("200%", 200)];

/// Whether the small-screen note goes up now (#2944): once, ever, the first
/// time Auto (`ui_scale_percent` 0) makes the interface smaller (`zoom`
/// under 1) with a project open in the editor. True when `prefs` changed
/// and is to be saved.
pub fn small_screen_note_now(state: &mut AppState, prefs: &mut crate::prefs::AppearancePrefs, zoom: f32) -> bool {
    if prefs.ui_scale_percent != 0 || zoom >= 1.0 || prefs.small_screen_note_shown || state.project_path.is_none() {
        return false;
    }
    prefs.small_screen_note_shown = true;
    state.small_screen_note = Some((zoom * 100.0).round() as u32);
    true
}

/// The note that Auto made the interface smaller for a small window
/// (#2944), at the foot of the editor while `state.small_screen_note` is
/// set: the size chosen and a way to change it, there and then, or to
/// dismiss it. Never a blocking dialog.
pub fn show_small_screen_note(ctx: &egui::Context, state: &mut AppState, theme: &KlippTheme) {
    let Some(pct) = state.small_screen_note else { return };
    let screen = ctx.screen_rect();
    let open_id = egui::Id::new("klipp_small_screen_sizes");
    let mut sizes_open = ctx.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    egui::Area::new(egui::Id::new("klipp_small_screen_note"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -(crate::theme::STATUSBAR_HEIGHT + 12.0)))
        .constrain_to(screen)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Small screen \u{2014} interface set to {pct}%")).color(theme.text_primary));
                    if ui.link("Change").clicked() {
                        sizes_open = !sizes_open;
                    }
                    if ui.small_button("\u{00d7}").on_hover_text("Dismiss").clicked() {
                        state.small_screen_note = None;
                    }
                });
                if sizes_open {
                    ui.horizontal(|ui| {
                        for (label, size) in INTERFACE_SIZES {
                            if ui.selectable_label(state.ui_scale_percent == size, label).clicked() {
                                state.ui_scale_request = Some(size);
                                state.small_screen_note = None;
                            }
                        }
                    });
                    ui.label(egui::RichText::new("Also in View \u{2192} Interface Size and Preferences \u{2192} Appearance.").small().color(theme.text_dim));
                }
            });
        });
    ctx.data_mut(|d| d.insert_temp(open_id, sizes_open));
}

/// What the Welcome window asks of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WelcomeAction {
    /// Use this keyboard layout.
    Keymap(KeymapPreset),
    /// Show the Welcome window at startup, or not.
    ShowAtStartup(bool),
    /// Open the setup for an AI assistant over MCP.
    SetUpAssistant,
}

/// One step of the tour: the control it points at, and what it says.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct TourStep {
    /// The name a control registers itself under with [`anchor`].
    pub anchor: String,
    pub title: String,
    /// `{action}` stands for that action's key in the chosen layout.
    pub text: String,
}

#[derive(serde::Deserialize)]
struct TourFile {
    steps: Vec<TourStep>,
}

/// The tour's steps, as the data file has them.
pub fn tour() -> Vec<TourStep> {
    serde_json::from_str::<TourFile>(include_str!("../../../assets/help/tour.json")).map(|f| f.steps).unwrap_or_default()
}

/// `text` with each `{action}` written as its key in `kb`; a step that
/// names an unbound action drops the bracket it stood in.
pub fn with_keys(text: &str, kb: &KeyBindings) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else { break };
        let key = kb.display_for(&rest[open + 1..close]);
        let mut head = &rest[..open];
        let mut tail = &rest[close + 1..];
        if key.is_empty() && head.ends_with('(') && tail.starts_with(')') {
            head = head[..head.len() - 1].trim_end();
            tail = &tail[1..];
        }
        out.push_str(head);
        out.push_str(&key);
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn anchor_id(name: &str) -> egui::Id {
    egui::Id::new(("klipp_tour_anchor", name))
}

/// Registers the control drawn at `rect` under `name`, this frame, for a
/// tour step to point at.
pub fn anchor(ctx: &egui::Context, name: &str, rect: egui::Rect) {
    let frame = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(anchor_id(name), (rect, frame)));
}

/// Where the control registered as `name` was drawn this frame or the
/// last; `None` when it is not on screen.
pub fn anchor_rect(ctx: &egui::Context, name: &str) -> Option<egui::Rect> {
    let now = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<(egui::Rect, u64)>(anchor_id(name))).filter(|(_, at)| *at + 1 >= now).map(|(r, _)| r)
}

/// The Welcome window's `action` done: the layout chosen and saved, the
/// startup setting set (the one Preferences → General shows), the
/// assistant setup opened. True when `prefs` changed and is to be saved.
pub fn apply(action: WelcomeAction, state: &mut AppState, prefs: &mut crate::prefs::Preferences, kb: &mut KeyBindings) -> bool {
    match action {
        WelcomeAction::Keymap(preset) => {
            kb.set_preset(preset);
            let _ = kb.save();
            false
        }
        WelcomeAction::ShowAtStartup(on) => {
            prefs.general.show_welcome = on;
            true
        }
        WelcomeAction::SetUpAssistant => {
            state.show_assistant_setup = true;
            false
        }
    }
}

/// Whether the Welcome window opens now, once a session: the first time a
/// project is open in the editor (never over the launch screen) with
/// `prefs.show_welcome` on, which then goes off, so the next startup shows
/// none unless the window's checkbox turns it back on. True when `prefs`
/// changed and is to be saved.
pub fn open_at_startup(state: &mut AppState, prefs: &mut crate::prefs::GeneralPrefs) -> bool {
    if state.welcome_checked || state.project_path.is_none() {
        return false;
    }
    state.welcome_checked = true;
    if !prefs.show_welcome {
        return false;
    }
    state.show_welcome = true;
    prefs.show_welcome = false;
    true
}

/// The Welcome window, when `state.show_welcome`: the keyboard layout, the
/// tour or straight to editing, an AI assistant over MCP, and whether it
/// shows at startup.
pub fn show_welcome(ctx: &egui::Context, state: &mut AppState, kb: &KeyBindings, show_at_startup: bool, theme: &KlippTheme) -> Vec<WelcomeAction> {
    let mut actions = Vec::new();
    if !state.show_welcome {
        return actions;
    }
    let mut open = true;
    egui::Window::new("Welcome to Klipp")
        .id(egui::Id::new("klipp_welcome"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .default_width(380.0)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("Keyboard layout").strong().color(theme.text_primary));
            ui.label(egui::RichText::new("Start from Klipp's own keys, or the keys of an editor you know.").small().color(theme.text_dim));
            ui.add_space(4.0);
            for preset in KeymapPreset::ALL {
                if ui.radio(kb.preset == preset, preset.label()).clicked() && kb.preset != preset {
                    actions.push(WelcomeAction::Keymap(preset));
                }
            }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("You can also edit with an AI assistant, over MCP.").color(theme.text_secondary));
                if ui.link("Set up an assistant\u{2026}").clicked() {
                    actions.push(WelcomeAction::SetUpAssistant);
                }
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Take a quick tour").clicked() {
                    state.show_welcome = false;
                    state.tour_step = Some(0);
                }
                if ui.button("Start editing").clicked() {
                    state.show_welcome = false;
                }
            });
            ui.add_space(6.0);
            let mut at_startup = show_at_startup;
            if ui.checkbox(&mut at_startup, "Show at startup").changed() {
                actions.push(WelcomeAction::ShowAtStartup(at_startup));
            }
        });
    if !open {
        state.show_welcome = false;
    }
    actions
}

/// The tour's callout for `state.tour_step`, at its control, with Back,
/// Next and Skip on every step; Done on the last ends it.
pub fn show_tour(ctx: &egui::Context, state: &mut AppState, kb: &KeyBindings, theme: &KlippTheme) {
    let steps = tour();
    let Some(at) = state.tour_step.filter(|s| *s < steps.len()) else {
        state.tour_step = None;
        return;
    };
    let step = &steps[at];
    let screen = ctx.screen_rect();
    let target = anchor_rect(ctx, &step.anchor);
    if let Some(r) = target {
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("klipp_tour_ring")))
            .rect_stroke(r.expand(3.0), 4.0_f32, egui::Stroke::new(2.0_f32, theme.accent));
    }
    const WIDTH: f32 = 300.0;
    const HEIGHT: f32 = 150.0;
    // Below the control when there is room, else above it; kept on screen.
    let pos = match target {
        Some(r) if r.bottom() + 10.0 + HEIGHT <= screen.bottom() => egui::pos2(r.left(), r.bottom() + 10.0),
        Some(r) => egui::pos2(r.left(), r.top() - 10.0 - HEIGHT),
        None => screen.center() - egui::vec2(WIDTH, HEIGHT) / 2.0,
    };
    let pos = egui::pos2(pos.x.clamp(screen.left() + 8.0, (screen.right() - WIDTH - 8.0).max(screen.left())), pos.y.clamp(screen.top() + 8.0, (screen.bottom() - HEIGHT).max(screen.top())));
    let mut next = Some(at);
    egui::Area::new(egui::Id::new("klipp_tour_callout"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(WIDTH);
                ui.label(egui::RichText::new(format!("Step {} of {}", at + 1, steps.len())).small().color(theme.text_dim));
                ui.label(egui::RichText::new(&step.title).strong().color(theme.text_primary));
                ui.label(with_keys(&step.text, kb));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if at > 0 && ui.button("Back").clicked() {
                        next = Some(at - 1);
                    }
                    let last = at + 1 == steps.len();
                    if ui.button(if last { "Done" } else { "Next" }).clicked() {
                        next = (!last).then_some(at + 1);
                    }
                    if ui.button("Skip tour").clicked() {
                        next = None;
                    }
                });
            });
        });
    state.tour_step = next;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window over a fresh 1280×800 frame, `events` sent; its texts
    /// and where they are drawn.
    struct Screen {
        ctx: egui::Context,
        time: f64,
        texts: Vec<(String, egui::Rect)>,
    }

    impl Screen {
        fn new() -> Self {
            Screen { ctx: egui::Context::default(), time: 0.0, texts: Vec::new() }
        }

        fn frame(&mut self, events: Vec<egui::Event>, draw: &mut dyn FnMut(&egui::Context)) {
            self.time += 0.1;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), time: Some(self.time), events, ..Default::default() };
            let out = self.ctx.run(input, |ctx| draw(ctx));
            self.texts = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
                    _ => None,
                })
                .collect();
        }

        fn has(&self, text: &str) -> bool {
            self.texts.iter().any(|(t, _)| t == text)
        }

        fn click(&mut self, text: &str, draw: &mut dyn FnMut(&egui::Context)) {
            let at = self.texts.iter().find(|(t, _)| t == text).unwrap_or_else(|| panic!("no {text:?} among {:?}", self.texts.iter().map(|t| &t.0).collect::<Vec<_>>())).1.center();
            self.frame(vec![egui::Event::PointerMoved(at)], draw);
            self.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }], draw);
            self.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }], draw);
        }
    }

    /// The tour's data: four steps, each at a control the app registers,
    /// and every `{action}` an action a layout binds.
    #[test]
    fn the_tour_is_four_steps_at_registered_controls() {
        let steps = tour();
        assert_eq!(steps.iter().map(|s| s.anchor.as_str()).collect::<Vec<_>>(), ["media_pool", "timeline", "blade_tool", "export"]);
        let sources = [
            include_str!("browser/media_tab.rs"),
            include_str!("panels/timeline.rs"),
            include_str!("timeline/toolbar.rs"),
            include_str!("panels/toolbar.rs"),
        ];
        let kb = KeyBindings::defaults();
        for s in &steps {
            assert!(sources.iter().any(|src| src.contains(&format!("crate::welcome::anchor(ui.ctx(), \"{}\"", s.anchor))), "{} is registered", s.anchor);
            let text = with_keys(&s.text, &kb);
            assert!(!text.contains('{') && !text.contains('}'), "{text}");
            for name in s.text.split('{').skip(1).map(|p| p.split('}').next().unwrap_or_default()) {
                assert!(kb.bindings.contains_key(name), "{name} is an action");
            }
        }
        // A key the layout leaves unbound drops its bracket.
        let mut none = KeyBindings::defaults();
        none.bindings.remove("export");
        assert_eq!(with_keys("export it here ({export}).", &none), "export it here.");
    }

    /// A fresh profile shows the Welcome window; choosing a layout there
    /// changes the bindings; "Start editing" closes it.
    #[test]
    fn the_welcome_window_picks_a_layout() {
        assert!(crate::prefs::Preferences::default().general.show_welcome, "a fresh profile shows it");
        let mut state = AppState { show_welcome: true, ..Default::default() };
        let mut kb = KeyBindings::defaults();
        let theme = KlippTheme::default();
        let mut screen = Screen::new();
        let mut got = Vec::new();
        let mut draw = |ctx: &egui::Context| got.extend(show_welcome(ctx, &mut state, &kb, true, &theme));
        screen.frame(Vec::new(), &mut draw);
        screen.frame(Vec::new(), &mut draw);
        assert!(screen.has("Welcome to Klipp") && screen.has("Take a quick tour") && screen.has("Show at startup") && screen.has("Set up an assistant\u{2026}"));
        let other = KeymapPreset::ALL[1];
        screen.click(other.label(), &mut draw);
        drop(draw);
        assert_eq!(got, [WelcomeAction::Keymap(other)]);
        let before = kb.bindings.clone();
        kb.set_preset(other);
        assert_ne!(kb.bindings, before, "the bindings are the layout's");
        let mut draw = |ctx: &egui::Context| got.extend(show_welcome(ctx, &mut state, &kb, true, &theme));
        screen.click("Show at startup", &mut draw);
        screen.click("Start editing", &mut draw);
        drop(draw);
        assert!(got.contains(&WelcomeAction::ShowAtStartup(false)));
        assert!(!state.show_welcome && state.tour_step.is_none());
    }

    /// First launch is judged by the settings: a fresh profile is welcomed
    /// once a project is open (not on the launch screen); the next launch,
    /// its "show at startup" off, is not; nor is an existing profile, whose
    /// settings file is from before the window.
    #[test]
    fn only_a_fresh_profile_is_welcomed() {
        let mut prefs = crate::prefs::Preferences::default().general;
        let mut state = AppState::default();
        assert!(!open_at_startup(&mut state, &mut prefs) && !state.show_welcome, "the launch screen: not yet");
        state.project_path = Some(crate::project_paths::UNTITLED.into());
        assert!(open_at_startup(&mut state, &mut prefs) && state.show_welcome, "in the editor: welcomed");
        assert!(!prefs.show_welcome, "and off for the next startup");
        state.show_welcome = false;
        assert!(!open_at_startup(&mut state, &mut prefs) && !state.show_welcome, "once a session");
        // The next launch, with the setting saved off.
        let saved = serde_json::to_string(&crate::prefs::Preferences { general: prefs.clone(), ..Default::default() }).unwrap();
        let mut next = crate::prefs::Preferences::from_json_str(&saved).general;
        let mut state = AppState { project_path: Some(crate::project_paths::UNTITLED.into()), ..Default::default() };
        assert!(!open_at_startup(&mut state, &mut next) && !state.show_welcome, "show at startup off: no welcome");
        // An existing profile: its settings file has no such field.
        let mut old: serde_json::Value = serde_json::from_str(&saved).unwrap();
        old["general"].as_object_mut().unwrap().remove("show_welcome");
        let mut existing = crate::prefs::Preferences::from_json_str(&old.to_string()).general;
        let mut state = AppState { project_path: Some(crate::project_paths::UNTITLED.into()), ..Default::default() };
        assert!(!open_at_startup(&mut state, &mut existing) && !state.show_welcome, "an existing profile: no welcome");
        // Help → Welcome opens it any time.
        assert!(include_str!("panels/toolbar.rs").contains("state.show_welcome = true"));
    }

    /// Tooltips name the key of the layout in effect, and drop it where
    /// the layout leaves the action unbound.
    #[test]
    fn a_tooltip_names_the_chosen_layouts_key() {
        let ctx = egui::Context::default();
        let mut kb = KeyBindings::defaults();
        let mut tip = String::new();
        let run = |kb: &KeyBindings, tip: &mut String| {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                crate::keybindings::publish(ctx, kb);
                *tip = crate::keybindings::tip(ctx, "Loop playback", "toggle_loop");
            });
        };
        run(&kb, &mut tip);
        assert_eq!(tip, format!("Loop playback ({})", kb.display_for("toggle_loop")));
        kb.bindings.insert("toggle_loop".into(), vec![crate::keybindings::KeyCombo::parse("Ctrl+Shift+L").unwrap()]);
        run(&kb, &mut tip);
        assert_eq!(tip, "Loop playback (Ctrl+Shift+L)", "the user's key");
        kb.bindings.remove("toggle_loop");
        run(&kb, &mut tip);
        assert_eq!(tip, "Loop playback", "no key, none named");
        // The tooltips that named fixed keys name the layout's now.
        for (src, fixed) in [
            (include_str!("panels/transport.rs"), "Loop playback (Ctrl+L)"),
            (include_str!("browser/mod.rs"), "Collapse browser  (Ctrl+Shift+B)"),
            (include_str!("widgets/inspector_group.rs"), "Collapse inspector  (Ctrl+Shift+I)"),
            (include_str!("panels/viewer.rs"), "Extract frame to bin (Ctrl+Alt+B)"),
            (include_str!("panels/timeline.rs"), "Fit timeline to window (Shift+Z)"),
            (include_str!("inspector/color_and_luts.rs"), "Switch to the Colour workspace (C)"),
        ] {
            assert!(!src.contains(fixed), "{fixed}");
        }
    }

    /// An empty timeline says how to add its first clip, in the layout's
    /// key; the hint goes once a clip is on it.
    #[test]
    fn an_empty_timeline_says_how_to_add_a_clip() {
        let ctx = egui::Context::default();
        let kb = KeyBindings::defaults();
        let theme = KlippTheme::default();
        let hint = crate::panels::timeline::empty_timeline_hint(&kb);
        assert!(hint.contains(&kb.display_for("insert_edit")), "{hint}");
        let mut state = AppState::default();
        let mut panel = crate::timeline::NewTimelinePanel::default();
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let mut drawn = |state: &mut AppState| -> bool {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 600.0))), ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    let _ = panel.show(ui, state, &theme, &mut old, &kb, false);
                });
            });
            out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == hint))
        };
        assert!(state.sequence().tracks.iter().all(|t| t.clips.is_empty()));
        assert!(drawn(&mut state), "the hint on an empty timeline");
        let clip = klipp_timeline::Clip::gap(klipp_core::Timecode::from_frames(0, 24), klipp_core::Timecode::from_frames(48, 24));
        let clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..clip };
        state.sequence_mut().tracks[0].clips.push(clip);
        assert!(!drawn(&mut state), "gone once a clip is placed");
    }

    /// The tour, from the window: each of the four callouts at its control,
    /// Next to the last; and from each step, Skip ends it; Back goes back.
    #[test]
    fn the_tour_steps_through_its_controls_and_skips_from_each() {
        let theme = KlippTheme::default();
        let kb = KeyBindings::defaults();
        let anchors = [
            ("media_pool", egui::Rect::from_min_size(egui::pos2(10.0, 60.0), egui::vec2(300.0, 200.0))),
            ("timeline", egui::Rect::from_min_size(egui::pos2(10.0, 450.0), egui::vec2(1200.0, 300.0))),
            ("blade_tool", egui::Rect::from_min_size(egui::pos2(400.0, 420.0), egui::vec2(24.0, 24.0))),
            ("export", egui::Rect::from_min_size(egui::pos2(1240.0, 4.0), egui::vec2(28.0, 28.0))),
        ];
        let steps = tour();
        let run = |state: &mut AppState, screen: &mut Screen, clicks: &[&str]| -> Vec<Option<usize>> {
            let mut seen = Vec::new();
            let mut draw = |ctx: &egui::Context| {
                for (name, rect) in anchors {
                    anchor(ctx, name, rect);
                }
                show_tour(ctx, state, &kb, &theme);
            };
            // An area measures itself on its first frame, unseen.
            screen.frame(Vec::new(), &mut draw);
            screen.frame(Vec::new(), &mut draw);
            for click in clicks {
                screen.click(click, &mut draw);
                seen.push(None);
            }
            seen
        };
        // Through every step with Next: each callout is drawn by its control.
        let mut state = AppState { tour_step: Some(0), ..Default::default() };
        let mut screen = Screen::new();
        for (i, (name, rect)) in anchors.iter().enumerate() {
            run(&mut state, &mut screen, &[]);
            assert_eq!(state.tour_step, Some(i));
            let (_, title_at) = screen.texts.iter().find(|(t, _)| *t == steps[i].title).cloned().unwrap_or_else(|| panic!("step {i}'s title"));
            let near = title_at.top() >= rect.bottom() || title_at.bottom() <= rect.top();
            assert!(near && (title_at.left() - rect.left()).abs() < 340.0, "{name}: the callout at {title_at:?} is by its control at {rect:?}");
            for b in ["Next", "Skip tour"].iter().chain((i > 0).then_some(&"Back")) {
                assert!(screen.has(b) || (i == 3 && *b == "Next" && screen.has("Done")), "step {i}: {b}");
            }
            let last = i == anchors.len() - 1;
            run(&mut state, &mut screen, &[if last { "Done" } else { "Next" }]);
        }
        assert_eq!(state.tour_step, None, "Done ends it");
        // Skip from each step; Back from the second.
        for i in 0..anchors.len() {
            let mut state = AppState { tour_step: Some(i), ..Default::default() };
            let mut screen = Screen::new();
            run(&mut state, &mut screen, &["Skip tour"]);
            assert_eq!(state.tour_step, None, "skipped at step {i}");
        }
        let mut state = AppState { tour_step: Some(1), ..Default::default() };
        let mut screen = Screen::new();
        run(&mut state, &mut screen, &["Back"]);
        assert_eq!(state.tour_step, Some(0));
    }
}
