//! The animated parameter in focus (#2296): the one piece of state the
//! Inspector, the Curve Editor and the Dope Sheet share about which of a
//! clip's animated parameters the user is looking at, so a pick in one
//! shows it in the others.

use crate::layout_state::InspectorTab;
use crate::state::KeyframeParamKind;
use klipp_core::ClipId;
use klipp_timeline::Clip;

/// A parameter brought into focus, and when (the UI's time in seconds,
/// for a row's highlight to fade from).
#[derive(Debug, Clone, PartialEq)]
pub struct ParamFocus {
    pub clip_id: ClipId,
    pub kind: KeyframeParamKind,
    pub at: f64,
}

/// `clip`'s animated parameters, as the Curve Editor and the Dope Sheet
/// name them, each with the Inspector tab that holds its row: placement
/// and opacity on Video, sound and sound effects on Audio, effects on
/// Effects, the grade and its masks on Color, title layers on Title.
pub fn animated(clip: &Clip) -> Vec<(KeyframeParamKind, String, InspectorTab)> {
    let grade: Vec<_> = clip.animated_grade_keys().into_iter().map(|(path, key, _)| (path, key)).collect();
    crate::panels::dope_sheet::rows(clip)
        .into_iter()
        .map(|row| {
            let tab = match &row.kind {
                KeyframeParamKind::Video(_) => InspectorTab::Video,
                KeyframeParamKind::Audio(_) | KeyframeParamKind::AudioEffect { .. } => InspectorTab::Audio,
                KeyframeParamKind::Node { path, key } if grade.iter().any(|(p, k)| p == path && k == key) => InspectorTab::Color,
                KeyframeParamKind::Node { .. } => InspectorTab::Effects,
                KeyframeParamKind::Mask(_) => InspectorTab::Color,
                KeyframeParamKind::Title { .. } => InspectorTab::Title,
            };
            (row.kind, row.label, tab)
        })
        .collect()
}

/// How long a row brought into focus stays lit, in seconds.
pub const FLASH_SECS: f64 = 1.5;

/// What the Inspector's rows need this frame to show the focus: the
/// parameter in focus on the clip shown, in the rows' own terms, and when
/// it was set; and the clip's placement node, to read a placement row's
/// lane as the same parameter.
#[derive(Clone)]
struct View {
    focus: Option<(KeyframeParamKind, f64)>,
    placement: Option<klipp_graph_format::path::NodePath>,
    /// Each animated title-layer track's keys, by where they are held and
    /// how many, with its track: a title row shows a track by its keys.
    held: Vec<(usize, usize, KeyframeParamKind)>,
}

/// The row about to be drawn: its parameter and how many keys it has.
#[derive(Clone)]
pub(crate) struct Tag {
    kind: KeyframeParamKind,
    keys: usize,
}

impl Tag {
    /// The track `kind`, with `keys` keys.
    pub(crate) fn new(kind: KeyframeParamKind, keys: usize) -> Self {
        Tag { kind, keys }
    }

    pub(crate) fn keys(&self) -> usize {
        self.keys
    }
}

fn view_id() -> egui::Id {
    egui::Id::new("param_focus::view")
}
fn tag_id() -> egui::Id {
    egui::Id::new("param_focus::tag")
}
fn request_id() -> egui::Id {
    egui::Id::new("param_focus::request")
}
fn scrolled_id() -> egui::Id {
    egui::Id::new("param_focus::scrolled")
}
fn rows_id() -> egui::Id {
    egui::Id::new("param_focus::rows")
}
fn noting_id() -> egui::Id {
    egui::Id::new("param_focus::noting")
}

/// A placement or opacity lane as the clip's placement node holds it, so
/// a row drawn by path and the Video tab's row are one parameter.
fn same_terms(placement: Option<&klipp_graph_format::path::NodePath>, kind: &KeyframeParamKind) -> KeyframeParamKind {
    match (kind, placement) {
        (KeyframeParamKind::Video(p), Some(path)) => match crate::inspector::placement_by_path::lane_key(*p) {
            Some(key) => KeyframeParamKind::Node { path: path.clone(), key: key.into() },
            None => kind.clone(),
        },
        _ => kind.clone(),
    }
}

/// Before the Inspector draws `clip`'s rows: the focus they show, no row
/// picked yet, and whether each row's place is noted for the Keyframes
/// column (`note_rows`, only while it is shown).
pub fn begin_rows(ctx: &egui::Context, focus: Option<&ParamFocus>, clip: &Clip, note_rows: bool) {
    let placement = clip.placement_path();
    let focus = focus.filter(|f| f.clip_id == clip.id).map(|f| (same_terms(placement.as_ref(), &f.kind), f.at));
    let mut held = Vec::new();
    if let klipp_timeline::ClipKind::Title(title) = &clip.kind {
        for object in &title.objects {
            for param in crate::panels::title_curve::parameters(object) {
                let keys = param.keyframes(clip, object.id.0);
                if !keys.is_empty() {
                    held.push((keys.as_ptr() as usize, keys.len(), KeyframeParamKind::Title { object_id: object.id.0, param }));
                }
            }
        }
    }
    ctx.data_mut(|d| {
        // Shared, so a row reads it without copying the layers' keys.
        d.insert_temp(view_id(), std::sync::Arc::new(View { focus, placement, held }));
        d.remove::<Tag>(tag_id());
        d.remove::<KeyframeParamKind>(request_id());
        d.insert_temp(noting_id(), note_rows);
        d.insert_temp(rows_id(), Vec::<(KeyframeParamKind, egui::Rect)>::new());
    });
}

/// Each tagged row drawn since [`begin_rows`], with where: the rows the
/// Inspector's Keyframes column lines its strips up with (#2305).
pub fn take_rows(ctx: &egui::Context) -> Vec<(KeyframeParamKind, egui::Rect)> {
    ctx.data_mut(|d| {
        let rows = d.get_temp::<Vec<(KeyframeParamKind, egui::Rect)>>(rows_id()).unwrap_or_default();
        d.remove::<Vec<(KeyframeParamKind, egui::Rect)>>(rows_id());
        d.insert_temp(noting_id(), false);
        rows
    })
}

/// The next row drawn is `clip`'s `kind`: it lights while that is in
/// focus, a click on its name or diamond brings it into focus, and its
/// diamond says how many keys it has.
pub fn tag(ui: &egui::Ui, clip: &Clip, kind: KeyframeParamKind) {
    let keys = crate::key_selection::track_keys(clip, &kind).len();
    ui.ctx().data_mut(|d| d.insert_temp(tag_id(), Tag { kind, keys }));
}

/// The next row drawn is the title-layer track whose keys are `keys`,
/// as the clip shown holds them: found by the keys themselves, so a row
/// that reads a layer's keys needs no name for its track. Nothing for a
/// track without keys.
pub fn tag_keys(ui: &egui::Ui, keys: &[klipp_core::Keyframe]) {
    if keys.is_empty() {
        return;
    }
    let Some(view) = ui.ctx().data(|d| d.get_temp::<std::sync::Arc<View>>(view_id())) else { return };
    if let Some((_, n, kind)) = view.held.iter().find(|(at, n, _)| *at == keys.as_ptr() as usize && *n == keys.len()).cloned() {
        ui.ctx().data_mut(|d| d.insert_temp(tag_id(), Tag { kind, keys: n }));
    }
}

/// The next row drawn is `kind`, with `keys` keys: for a row whose keys
/// are not the clip's own (a sound effect's, read from its map).
pub fn tag_counted(ui: &egui::Ui, kind: KeyframeParamKind, keys: usize) {
    ui.ctx().data_mut(|d| d.insert_temp(tag_id(), Tag { kind, keys }));
}

/// The tag the next row was given, taken.
pub(crate) fn take_tag(ui: &egui::Ui) -> Option<Tag> {
    ui.ctx().data_mut(|d| {
        let tag = d.get_temp::<Tag>(tag_id());
        d.remove::<Tag>(tag_id());
        tag
    })
}

/// A tagged row drawn over `rect`: picked when `clicked`; lit, fading,
/// for [`FLASH_SECS`] after its parameter came into focus, and scrolled
/// into view once.
pub(crate) fn row_drawn(ui: &egui::Ui, tag: &Tag, rect: egui::Rect, clicked: bool) {
    // Noted for the Keyframes column while it is shown, in place.
    ui.ctx().data_mut(|d| {
        if d.get_temp::<bool>(noting_id()) == Some(true) {
            d.get_temp_mut_or_default::<Vec<(KeyframeParamKind, egui::Rect)>>(rows_id()).push((tag.kind.clone(), rect));
        }
    });
    let Some(view) = ui.ctx().data(|d| d.get_temp::<std::sync::Arc<View>>(view_id())) else { return };
    if clicked {
        ui.ctx().data_mut(|d| d.insert_temp(request_id(), tag.kind.clone()));
    }
    let Some((focus, at)) = &view.focus else { return };
    if same_terms(view.placement.as_ref(), &tag.kind) != *focus {
        return;
    }
    let age = ui.input(|i| i.time) - at;
    if !(0.0..FLASH_SECS).contains(&age) {
        return;
    }
    let scrolled = ui.ctx().data(|d| d.get_temp::<f64>(scrolled_id()));
    if scrolled != Some(*at) {
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
        ui.ctx().data_mut(|d| d.insert_temp(scrolled_id(), *at));
    }
    let accent = ui.visuals().selection.stroke.color;
    let fade = (1.0 - age / FLASH_SECS) as f32;
    ui.painter().rect_filled(rect.expand(1.0), 3.0, accent.gamma_multiply(0.25 * fade));
    ui.painter().rect_stroke(rect.expand(1.0), 3.0, egui::Stroke::new(1.0_f32, accent.gamma_multiply(fade)));
    ui.ctx().request_repaint();
}

/// What a tagged row's diamond says on hover: how many keys the track
/// has and what the arrows and the diamond do; `None` with no keys.
pub(crate) fn diamond_hover(keys: usize, at_playhead: bool) -> Option<String> {
    if keys == 0 {
        return None;
    }
    let count = if keys == 1 { "1 key".to_string() } else { format!("{keys} keys") };
    let here = if at_playhead { "\u{25C6} removes the one at the playhead" } else { "\u{25C6} adds one at the playhead" };
    Some(format!("{count}. \u{25C0} \u{25B6} jump between them; {here}. A click on the name shows its curve."))
}

/// The row picked this frame, if one was: the parameter to bring into focus.
pub fn take_request(ctx: &egui::Context) -> Option<KeyframeParamKind> {
    ctx.data_mut(|d| {
        let picked = d.get_temp::<KeyframeParamKind>(request_id());
        d.remove::<KeyframeParamKind>(request_id());
        picked
    })
}

/// The Inspector tab that holds `kind` on `clip`, if it is animated there.
pub fn tab_of(clip: &Clip, kind: &KeyframeParamKind) -> Option<InspectorTab> {
    let placement = clip.placement_path();
    let want = same_terms(placement.as_ref(), kind);
    animated(clip).into_iter().find(|(k, ..)| same_terms(placement.as_ref(), k) == want).map(|(.., tab)| tab)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::test_clip::TestClip;

    /// A clip's keyed opacity and position are on the Video tab, named as
    /// the Curve Editor names them; a clip with no keys has none.
    #[test]
    fn a_clips_animated_parameters_and_their_tabs() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let plain = Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        assert!(animated(&plain).is_empty());
        let keyed = TestClip::of(plain).lane("opacity", 1.0, 0.5).lane("pos_x", 1.0, 10.0).lane("pos_x", 2.0, 20.0).build();
        let found = animated(&keyed);
        let names: Vec<(&str, InspectorTab)> = found.iter().map(|(_, l, tab)| (l.as_str(), *tab)).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(found.iter().all(|(_, _, tab)| *tab == InspectorTab::Video), "{names:?}");
        assert!(found.iter().any(|(k, ..)| *k == KeyframeParamKind::Video(klipp_core::VideoParam::PositionX)));
    }

    /// The Inspector shown once, `events` given, with `tab` open: what it
    /// drew (each text and where) and the tab it leaves open.
    struct Shown {
        state: crate::state::AppState,
        panel: crate::inspector::NewInspectorPanel,
        ctx: egui::Context,
        tab: InspectorTab,
        clock: f64,
    }

    impl Shown {
        fn new(clip: Clip, tab: InspectorTab) -> Self {
            let mut state = crate::state::AppState::default();
            let v = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
            state.selected_clip = Some(clip.id);
            state.sequences[0].tracks[v].clips.push(clip);
            let ctx = egui::Context::default();
            crate::icons::install(&ctx);
            let mut s = Shown { state, panel: Default::default(), ctx, tab, clock: 1.0 };
            s.frame(Vec::new());
            s
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.clock += 0.05;
            let prefs = crate::prefs::Preferences::default();
            let (state, panel, tab) = (&self.state, &mut self.panel, &mut self.tab);
            let out = self.ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 1600.0))), time: Some(self.clock), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = panel.show(ui, state, &prefs, &crate::theme::KlippTheme::default(), tab);
                });
            });
            // As the app applies a pick.
            if let Some(kind) = self.panel.focus_request.take() {
                self.state.param_focus = Some(ParamFocus { clip_id: self.state.selected_clip.unwrap(), kind, at: self.clock });
            }
            out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
        }

        fn click(&mut self, label: &str) {
            let drawn = self.frame(Vec::new());
            let at = drawn.iter().find(|(t, _)| t == label).unwrap_or_else(|| panic!("{label:?} drawn: {:?}", drawn.iter().map(|d| &d.0).collect::<Vec<_>>())).1;
            self.frame(vec![egui::Event::PointerMoved(at)]);
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
            }
        }
    }

    fn animated_clip() -> Clip {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0))).lane("opacity", 1.0, 0.5).lane("pos_x", 1.0, 10.0).lane("pos_x", 2.0, 20.0).build()
    }

    /// On another tab, the Animated strip names the clip's keyed
    /// parameters and the Video tab is marked; a chip opens the Video tab
    /// and brings its parameter into focus.
    #[test]
    fn a_chip_opens_its_tab_and_brings_its_parameter_into_focus() {
        let mut s = Shown::new(animated_clip(), InspectorTab::Audio);
        let drawn: Vec<String> = s.frame(Vec::new()).into_iter().map(|(t, _)| t).collect();
        assert!(drawn.iter().any(|t| t == "Animated" || t.ends_with(" animated")) && drawn.iter().any(|t| t.ends_with("\u{25C6}") && t.starts_with("VID")), "{drawn:?}");
        let chip = drawn.iter().find(|t| t.starts_with("\u{25C6} ") && t.contains("Position X")).expect("a Position X chip").clone();
        s.click(&chip);
        assert_eq!(s.tab, InspectorTab::Video);
        let focus = s.state.param_focus.clone().expect("in focus");
        assert_eq!(tab_of(&animated_clip(), &focus.kind), Some(InspectorTab::Video));
        assert!(matches!(focus.kind, KeyframeParamKind::Video(klipp_core::VideoParam::PositionX)));
    }

    /// A parameter brought into focus elsewhere (the Curve Editor) opens
    /// the tab that holds it once; the user's own tab click is kept after.
    #[test]
    fn the_tab_follows_a_parameter_newly_in_focus() {
        let clip = animated_clip();
        let mut s = Shown::new(clip.clone(), InspectorTab::Audio);
        s.state.param_focus = Some(ParamFocus { clip_id: clip.id, kind: KeyframeParamKind::Video(klipp_core::VideoParam::Opacity), at: s.clock });
        s.frame(Vec::new());
        assert_eq!(s.tab, InspectorTab::Video);
        s.tab = InspectorTab::Audio;
        s.frame(Vec::new());
        assert_eq!(s.tab, InspectorTab::Audio, "the same focus moves it once");
    }

    /// A click on an animated row's name brings it into focus.
    #[test]
    fn a_rows_name_clicked_brings_it_into_focus() {
        let mut s = Shown::new(animated_clip(), InspectorTab::Video);
        s.click("Opacity");
        let focus = s.state.param_focus.clone().expect("in focus");
        assert_eq!(tab_of(&animated_clip(), &focus.kind), Some(InspectorTab::Video), "{:?}", focus.kind);
    }

    /// A title layer's keyed position: listed on the Title tab, and its
    /// row, found by its keys, takes a click into focus.
    #[test]
    fn a_title_layers_row_is_found_by_its_keys() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello".into(), ..Default::default() });
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        td.objects[0].transform.center_x_keyframes = vec![key(0.5, 0.2), key(1.5, 0.8)];
        let clip = Clip { kind: klipp_timeline::ClipKind::Title(td), ..Clip::gap(tc(0.0), tc(4.0)) };
        let listed = animated(&clip);
        let (kind, _, tab) = listed.iter().find(|(k, ..)| matches!(k, KeyframeParamKind::Title { .. })).cloned().unwrap_or_else(|| panic!("a title track: {listed:?}"));
        assert_eq!(tab, InspectorTab::Title);
        let (clip_id, layer) = (clip.id, klipp_timeline::TitleObjectId(0));
        let mut s = Shown::new(clip, InspectorTab::Title);
        // The layer selected, as its position rows edit it.
        s.state.selected_title_object = Some((clip_id, layer));
        s.click("X");
        assert_eq!(s.state.param_focus.as_ref().map(|f| &f.kind), Some(&kind));
    }

    /// More than four animated parameters fold behind their count, which
    /// opens on a click.
    #[test]
    fn many_animated_parameters_fold_behind_a_count() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0)))
            .lane("opacity", 1.0, 0.5).lane("pos_x", 1.0, 10.0).lane("pos_y", 1.0, 10.0).lane("scale", 1.0, 1.5).lane("rotation", 1.0, 30.0).build();
        let mut s = Shown::new(clip, InspectorTab::Video);
        let chips = |s: &mut Shown| s.frame(Vec::new()).into_iter().filter(|(t, _)| t.starts_with("\u{25C6} ") && !t.ends_with(" animated") && t != "\u{25C6} Keyframes").count();
        assert_eq!(chips(&mut s), 0, "folded");
        s.click("\u{25C6} 5 animated");
        assert_eq!(chips(&mut s), 5);
    }
}
