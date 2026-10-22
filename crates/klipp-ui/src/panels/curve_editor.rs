use super::title_curve::{self, TitleCurveParam};
use crate::state::{AppState, CameraKeyMenu, CameraKeyRef, DraggingCameraKey, KeyframeParamKind, SelectedKeyframe, KeyframeContextMenu};
use crate::theme::KlippTheme;
use crate::timeline::camera_lane;
use egui::{pos2, vec2, Color32, Rect, Stroke};
use klipp_core::{AppCommand, AudioParam, CameraParam, VideoParam, Keyframe};
use klipp_timeline::TrackKind;

/// Which parameter the curve editor displays.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveParam {
    Title { object_id: u32, param: TitleCurveParam },
    Audio(AudioParam),
    Video(VideoParam),
    /// Playback speed envelope.
    Speed,
    /// An effect or grade node parameter's keyframe track, by its node's
    /// path in the clip's picture and the parameter's key (M3 step 3): it
    /// follows the node through a reorder. `tag` is the effect's saved
    /// tag, or a grade node's type id (`[group, node]`, #891), which names
    /// and ranges the track from the catalog.
    Node { path: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, tag: String },
    /// Audio effect parameter keyframe track, identified by
    /// "{effect_index}:{param_name}". Reserved param names: "__bypass"
    /// and "__wetdry".
    AudioEffect(String),
    /// One of the sequence camera's moving values: the sequence's own
    /// curve, over the sequence's time, shown when no clip is selected.
    Camera(CameraParam),
    /// A mask's parameter, or a freeform mask's shape (#1947), named as
    /// the list shows it ("Face · Shape").
    Mask { track: crate::mask_tracks::MaskTrack, name: String },
}

/// The modifier stack's parameter for `param`, where it has one: what the
/// renderer evaluates its keys with (#2277).
fn modifier_param(param: &CurveParam) -> Option<klipp_core::ParamId> {
    use klipp_core::ParamId;
    // The placement and opacity lanes, whose stack the picture gives
    // (`Clip::truth_lane`); the others draw their keys as keyed.
    Some(match param {
        CurveParam::Video(VideoParam::Opacity) => ParamId::Opacity,
        CurveParam::Video(VideoParam::PositionX) => ParamId::PositionX,
        CurveParam::Video(VideoParam::PositionY) => ParamId::PositionY,
        CurveParam::Video(VideoParam::Scale) => ParamId::Scale,
        CurveParam::Video(VideoParam::ScaleY) => ParamId::ScaleY,
        CurveParam::Video(VideoParam::Rotation) => ParamId::Rotation,
        _ => return None,
    })
}

/// An effect parameter as the catalog describes it, by the effect's saved
/// tag and the parameter's key in its node (its field, or an element's
/// key): the effect's name, the parameter's label as the inspector shows
/// it, and its (min, max, default) on the curve (M3 step 3).
fn node_param(tag: &str, key: &klipp_graph_format::ParamKey) -> Option<(&'static str, &'static str, (f32, f32, f32))> {
    // A grade node's track is named by its type's id (#891).
    if let Some(t) = klipp_nodes::registry::node_types().find(|t| t.desc.id == tag) {
        let p = t.desc.params.iter().find(|p| p.key == key.as_str())?;
        let label = t.presentation(p).label.unwrap_or(p.label);
        let (lo, hi) = p.soft.or(p.range)?;
        let default = if p.default.is_finite() { p.default } else { lo };
        return Some((t.desc.name, label, (lo, hi, default)));
    }
    let t = klipp_effects::registry::effect_type(tag)?;
    let p = t.desc.params.iter().find(|p| (if p.index.is_some() { p.key } else { p.field }) == key.as_str())?;
    let label = t.presentation(p).label.unwrap_or(p.label);
    let (lo, hi) = p.soft.or(p.range)?;
    let default = if p.default.is_finite() { p.default } else { lo };
    Some((t.desc.name, label, (lo, hi, default)))
}

/// The parameters of the clip's effects and grade that can be keyed and
/// have no keys yet, by node (its name, then each parameter's label, track
/// and value now): where a track can be started.
fn startable_tracks(clip: &klipp_timeline::Clip) -> Vec<(String, Vec<(String, CurveParam, f32)>)> {
    use klipp_graph_format::ParamKey;
    let mut out = Vec::new();
    for (path, tag, keyed) in clip.effect_nodes() {
        let Some(t) = klipp_effects::registry::effect_type(&tag) else { continue };
        let params: Vec<_> = t.desc.params.iter().filter(|p| p.keyframable && !p.derived && !matches!(p.kind, klipp_effects::registry::ParamKind::Other)).filter_map(|p| {
            let key = ParamKey::from(if p.index.is_some() { p.key } else { p.field });
            if keyed.contains(&key) {
                return None;
            }
            let value = clip.node_key_state(&path, &key).and_then(|s| s.value).unwrap_or(if p.default.is_finite() { p.default } else { 0.0 });
            Some((t.presentation(p).label.unwrap_or(p.label).to_owned(), CurveParam::Node { path: path.clone(), key, tag: tag.clone() }, value))
        }).collect();
        if !params.is_empty() {
            out.push((t.desc.name.to_owned(), params));
        }
    }
    for (path, label, ty, entry, keyed) in clip.grade_nodes() {
        let Some(t) = klipp_nodes::registry::node_types().find(|t| t.desc.id == ty) else { continue };
        let params: Vec<_> = t.desc.params.iter().filter(|p| {
            p.keyframable && !p.derived && !matches!(p.kind, klipp_effects::registry::ParamKind::Other) && t.presentation(p).widget != klipp_effects::registry::presentation::Widget::Hidden && !keyed.iter().any(|k| k == p.key)
        }).map(|p| {
            let value = crate::inspector::generated::row::number(&crate::inspector::generated::grade_value(&entry, p)).unwrap_or(if p.default.is_finite() { p.default } else { 0.0 });
            (t.presentation(p).label.unwrap_or(p.label).to_owned(), CurveParam::Node { path: path.clone(), key: ParamKey::from(p.key), tag: ty.clone() }, value)
        }).collect();
        if !params.is_empty() {
            out.push((label, params));
        }
    }
    // Each mask's unkeyed parameters, its shape among them (#1947).
    for (mask, params) in crate::mask_tracks::startable(clip) {
        let params = params.into_iter().map(|(track, label, value)| {
            let name = format!("{mask} \u{00B7} {label}");
            (label, CurveParam::Mask { track, name }, value)
        }).collect();
        out.push((mask, params));
    }
    out
}

/// The header's "+ Track" menu, in the app's menu look: the selected
/// clip's nodes, each with its parameters that have no keys yet. Picking
/// one starts its track: a key at the playhead holding the value it has
/// now (one undo step), and the track shown. The key and the track.
fn start_track_menu(ui: &mut egui::Ui, state: &AppState, clip_id: klipp_core::ClipId) -> Option<(AppCommand, CurveParam)> {
    use crate::widgets::menu::{menu_body, menu_item};
    let clip = state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == clip_id)?;
    let mut picked = None;
    ui.menu_button(egui::RichText::new("+ Track").size(10.0), |ui| {
        menu_body(ui, |ui| {
            let nodes = startable_tracks(clip);
            if nodes.is_empty() {
                ui.label(egui::RichText::new("Every parameter has keys").size(10.0));
            }
            for (name, params) in nodes {
                ui.menu_button(name, |ui| {
                    menu_body(ui, |ui| {
                        for (label, param, value) in params {
                            if menu_item(ui, &label, "").clicked() {
                                picked = Some((param, value));
                                ui.close_menu();
                            }
                        }
                    })
                });
            }
        })
    })
    .response
    .on_hover_text("Start a keyframe track: a key at the playhead with the parameter's value now");
    let (param, value) = picked?;
    let t = (state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).clamp(0.0, clip.duration().to_secs_f64());
    // A mask's track: its key by the mask's own command (#1947).
    if let CurveParam::Mask { track, .. } = &param {
        let cmd = AppCommand::AddMaskKeyframe { clip_id, node_id: track.node, mask_index: track.mask, param: track.param.clone(), time_secs: t, value };
        return Some((cmd, param));
    }
    let CurveParam::Node { path, key, .. } = &param else { return None };
    let keyframe = serde_json::to_value(Keyframe { time_secs: t, value, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).ok()?;
    Some((AppCommand::AddKeyAt { addr: path.clone(), key: key.clone(), keyframe }, param))
}

impl CurveParam {
    pub fn label(&self) -> String {
        match self {
            CurveParam::Title { param, .. } => param.label(),
            CurveParam::Audio(AudioParam::Volume) => "Volume".into(),
            CurveParam::Audio(AudioParam::Pan) => "Pan".into(),
            CurveParam::Audio(AudioParam::Gain) => "Gain".into(),
            CurveParam::Video(VideoParam::Opacity) => "Opacity".into(),
            CurveParam::Video(VideoParam::PositionX) => "Position X".into(),
            CurveParam::Video(VideoParam::PositionY) => "Position Y".into(),
            CurveParam::Video(VideoParam::Scale) => "Scale".into(),
            CurveParam::Video(VideoParam::ScaleY) => "Scale Y".into(),
            CurveParam::Video(VideoParam::Rotation) => "Rotation".into(),
            CurveParam::Video(VideoParam::TiltX) => "Tilt X".into(),
            CurveParam::Video(VideoParam::TiltY) => "Tilt Y".into(),
            CurveParam::Video(VideoParam::MatteFeather) => "Matte Feather".into(),
            CurveParam::Video(VideoParam::MatteChoke) => "Matte Choke".into(),
            CurveParam::Speed => "Speed".into(),
            CurveParam::Node { key, tag, .. } => match node_param(tag, key) {
                Some((effect, label, _)) => format!("{effect}: {label}"),
                None => format!("FX: {key}"),
            },
            CurveParam::AudioEffect(key) => {
                if let Some((_idx, name)) = key.split_once(':') {
                    let pretty = match name {
                        "__bypass" => "Bypass",
                        "__wetdry" => "Wet/Dry",
                        other => other,
                    };
                    format!("Audio FX: {pretty}")
                } else {
                    format!("Audio FX: {key}")
                }
            }
            CurveParam::Camera(p) => format!("Camera {}", p.label()),
            CurveParam::Mask { name, .. } => name.clone(),
        }
    }

    /// (min, max, default) for this parameter, for a test outside this
    /// module.
    #[cfg(test)]
    pub(crate) fn curve_range(&self) -> (f32, f32, f32) {
        self.range()
    }

    /// (min, max, default) for this parameter.
    fn range(&self) -> (f32, f32, f32) {
        match self {
            CurveParam::Title { param, .. } => param.range(),
            CurveParam::Audio(AudioParam::Volume) => (0.0, 2.0, 1.0),
            CurveParam::Audio(AudioParam::Pan) => (-1.0, 1.0, 0.0),
            CurveParam::Audio(AudioParam::Gain) => (0.0, 4.0, 1.0),
            CurveParam::Video(VideoParam::Opacity) => (0.0, 1.0, 1.0),
            CurveParam::Video(VideoParam::PositionX) => (-1920.0, 1920.0, 0.0),
            CurveParam::Video(VideoParam::PositionY) => (-1080.0, 1080.0, 0.0),
            CurveParam::Video(VideoParam::Scale | VideoParam::ScaleY) => (0.0, 4.0, 1.0),
            CurveParam::Video(VideoParam::Rotation) => (-360.0, 360.0, 0.0),
            CurveParam::Video(VideoParam::TiltX | VideoParam::TiltY) => (-80.0, 80.0, 0.0),
            CurveParam::Video(VideoParam::MatteFeather) => (0.0, 50.0, 0.0),
            CurveParam::Video(VideoParam::MatteChoke) => (0.0, 50.0, 0.0),
            CurveParam::Speed => (0.1, 4.0, 1.0),
            // The catalog's: the slider's range, or the accepted one.
            CurveParam::Node { key, tag, .. } => node_param(tag, key).map_or((0.0, 1.0, 0.5), |(_, _, range)| range),
            CurveParam::AudioEffect(key) => {
                // Reserved keys have known ranges; per-param ranges are
                // unknowable here without the AudioEffect descriptor (we
                // don't construct effects on hot UI paths). Fall back to
                // a generous default — the inspector's diamond rows
                // already enforce min/max via the descriptor.
                match key.split(':').next_back() {
                    Some("__bypass") => (0.0, 1.0, 1.0),
                    Some("__wetdry") => (0.0, 1.0, 1.0),
                    _ => (-30.0, 30.0, 0.0),
                }
            }
            CurveParam::Camera(CameraParam::X | CameraParam::Y) => (0.0, 1.0, 0.5),
            CurveParam::Camera(CameraParam::Zoom) => (0.25, 4.0, 1.0),
            CurveParam::Camera(CameraParam::Roll) => (-180.0, 180.0, 0.0),
            CurveParam::Mask { track, .. } => crate::mask_tracks::range(&track.param),
        }
    }
}

/// What the editor's time axis is: a clip's own time, or the sequence's
/// when no clip is selected and the camera's curves are what is shown.
#[derive(Debug, Clone, PartialEq)]
enum CurveScope {
    Clip { id: klipp_core::ClipId, duration: f64, name: String, kind: TrackKind },
    Sequence { duration: f64 },
}

impl CurveScope {
    fn duration(&self) -> f64 {
        match self {
            CurveScope::Clip { duration, .. } | CurveScope::Sequence { duration } => *duration,
        }
    }

    fn clip_id(&self) -> Option<klipp_core::ClipId> {
        match self {
            CurveScope::Clip { id, .. } => Some(*id),
            CurveScope::Sequence { .. } => None,
        }
    }

    fn kind(&self) -> TrackKind {
        match self {
            CurveScope::Clip { kind, .. } => *kind,
            CurveScope::Sequence { .. } => TrackKind::Video,
        }
    }
}

/// A curve in the editor's list (#2271): its parameter, and whether it
/// is drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct ShownCurve {
    pub param: CurveParam,
    pub visible: bool,
}

/// Dragging state for keyframe in the curve editor.
#[derive(Debug, Clone)]
struct CurveDrag {
    keyframe_index: usize,
    /// true = dragging a bezier handle, false = dragging the keyframe diamond
    handle: Option<bool>, // Some(true) = out handle, Some(false) = in handle
}

pub struct CurveEditorPanel {
    /// Zoom level (pixels per second on X axis)
    zoom_x: f32,
    /// Zoom level (pixels per unit on Y axis)
    zoom_y: f32,
    /// Scroll offset
    scroll: egui::Vec2,
    /// The curves shown (#2271), each drawn or hidden, with `active` the
    /// one whose keys are edited and whose parameter the selectors name.
    /// Never empty.
    curves: Vec<ShownCurve>,
    active: usize,
    selected_clip: Option<klipp_core::ClipId>,
    /// Active keyframe drag
    drag: Option<CurveDrag>,
    /// Auto-fit on first display
    needs_fit: bool,
    /// Where a drag of the selected keys began, in time and value (#1958).
    multi_from: Option<(f64, f32)>,
    /// A box being drawn on empty graph, where it began, and whether
    /// Shift adds what it holds to the selection (#1958).
    box_from: Option<(egui::Pos2, bool)>,
    /// Showing the dope sheet (#2240) in place of the curves.
    dope: bool,
    sheet: super::dope_sheet::Sheet,
    /// The last frame's graph, for a test to find a key on screen: its
    /// rect and the value range it maps.
    #[cfg(test)]
    last_view: Option<(Rect, f32, f32)>,
}

impl Default for CurveEditorPanel {
    fn default() -> Self {
        Self {
            zoom_x: 200.0,
            zoom_y: 1.0,
            scroll: egui::Vec2::ZERO,
            curves: vec![ShownCurve { param: CurveParam::Video(VideoParam::Opacity), visible: true }],
            active: 0,
            selected_clip: None,
            drag: None,
            needs_fit: true,
            multi_from: None,
            box_from: None,
            dope: false,
            sheet: Default::default(),
            #[cfg(test)]
            last_view: None,
        }
    }
}

impl CurveEditorPanel {
    /// The active curve's parameter: the one whose keys are edited.
    fn param(&self) -> &CurveParam {
        &self.curves[self.active.min(self.curves.len() - 1)].param
    }

    /// Make `param` the curve shown and edited (#2271: one for now; the
    /// list holds the ones shown beside it).
    fn set_param(&mut self, param: CurveParam) {
        self.curves = vec![ShownCurve { param, visible: true }];
        self.active = 0;
    }

    /// The curves shown, in order, the active one among them (#2271).
    pub fn shown_curves(&self) -> &[ShownCurve] {
        &self.curves
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let cmds = self.show_inner(ui, state, theme);
        // The effect track shown, for Previous / Next keyframe (#1401).
        state.curve_editor_track = match (self.param(), state.selected_clip) {
            (CurveParam::Node { path, key, .. }, Some(clip)) => Some((clip, path.clone(), key.clone())),
            _ => None,
        };
        state.curve_editor_mask_track = match (self.param(), state.selected_clip) {
            (CurveParam::Mask { track, .. }, Some(clip)) => Some((clip, track.clone())),
            _ => None,
        };
        // A placed kind's track (a Temporal Denoise's) is edited through
        // the clip's effect list, as its card edits it (#1419).
        match state.selected_clip.and_then(|id| state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == id)) {
            Some(clip) => crate::inspector::stack_view::route_placed(clip, cmds),
            None => cmds,
        }
    }

    fn show_inner(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        if self.selected_clip != state.selected_clip {
            self.selected_clip = state.selected_clip;
            self.drag = None;
            self.needs_fit = true;
            state.selected_keyframe = None;
            state.keyframe_context_menu = None;
        }


        let full_rect = ui.available_rect_before_wrap();
        if full_rect.width() < 40.0 || full_rect.height() < 40.0 {
            return cmds;
        }

        // Background
        ui.painter().rect_filled(full_rect, 0.0, theme.bg_panel);
        ui.painter().hline(
            full_rect.x_range(),
            full_rect.min.y,
            Stroke::new(1.0_f32, theme.border),
        );

        // ── Header (28px) ──
        let header_h = 28.0;
        let header_rect = Rect::from_min_size(full_rect.min, vec2(full_rect.width(), header_h));
        let graph_rect = Rect::from_min_max(
            pos2(full_rect.min.x + 50.0, header_rect.max.y + 1.0),
            pos2(full_rect.max.x - 8.0, full_rect.max.y - 20.0),
        );

        // Draw header background
        ui.painter().rect_filled(header_rect, 0.0, theme.bg_deep);
        ui.painter().hline(
            header_rect.x_range(),
            header_rect.max.y,
            Stroke::new(0.5_f32, theme.border),
        );

        // What is being edited: the selected clip, or the sequence camera.
        let scope = self.scope(state);

        // Header content
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(8.0);

                // Label: "Curve Editor", and its two views: one curve, or
                // every keyed track of the clip on one axis (#2240).
                ui.label(
                    egui::RichText::new("Curve Editor")
                        .size(11.0)
                        .color(theme.text_secondary),
                );
                ui.add_space(4.0);
                if ui.selectable_label(!self.dope, egui::RichText::new("Curves").size(10.0)).clicked() {
                    self.dope = false;
                    self.needs_fit = true;
                }
                if ui.selectable_label(self.dope, egui::RichText::new("Dope Sheet").size(10.0)).on_hover_text("Every animated parameter of the clip on one time axis").clicked() {
                    self.dope = true;
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Clip name, or the sequence's camera
                match &scope {
                    Some(CurveScope::Clip { name, .. }) => {
                        ui.label(
                            egui::RichText::new(name.as_str())
                                .size(11.0)
                                .color(theme.text_primary),
                        );
                    }
                    Some(CurveScope::Sequence { .. }) => {
                        ui.label(
                            egui::RichText::new("Sequence camera")
                                .size(11.0)
                                .color(theme.text_primary),
                        );
                    }
                    None => {
                        ui.label(
                            egui::RichText::new("No clip selected")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Parameter selector
                let previous_param = self.param().clone();
                self.param_selector(ui, theme, state);
                // How many keys are selected, so several never go unseen
                // (#1958).
                if state.key_selection.len() > 1 && state.key_selection.clip_id == state.selected_clip {
                    ui.label(egui::RichText::new(format!("{} keys", state.key_selection.len())).size(crate::theme::type_scale::CAPTION).color(theme.text_accent));
                }
                if *self.param() != previous_param {
                    // A curve picked is shown, from the dope sheet too.
                    self.dope = false;
                    // A selection of several keys stays across tracks.
                    if !crate::key_selection::several(state) {
                        state.selected_keyframe = None;
                    }
                    state.keyframe_context_menu = None;
                    self.drag = None;
                    self.needs_fit = true;
                }

                // A track on a parameter that has no keys yet (#891).
                if let Some(CurveScope::Clip { id, .. }) = &scope {
                    if let Some((start, param)) = start_track_menu(ui, state, *id) {
                        cmds.push(start);
                        self.set_param(param);
                        state.selected_keyframe = None;
                        state.keyframe_context_menu = None;
                        self.drag = None;
                        self.needs_fit = true;
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Zoom controls
                if ui.small_button(
                    egui::RichText::new("Fit").size(10.0).color(theme.text_secondary),
                ).clicked() {
                    self.fit_to_content(
                        &graph_rect,
                        scope.as_ref().map(CurveScope::duration).unwrap_or(1.0),
                    );
                }
            });
        });

        if graph_rect.width() < 20.0 || graph_rect.height() < 20.0 {
            return cmds;
        }

        // Get clip data
        let Some(scope) = scope else {
            // Draw empty state
            ui.painter().text(
                graph_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Select a clip to see its keyframes",
                egui::FontId::proportional(12.0),
                theme.text_dim,
            );
            return cmds;
        };

        // The dope sheet takes the whole body, the names' column with it.
        if self.dope {
            if let CurveScope::Clip { id, .. } = &scope {
                let body = Rect::from_min_max(pos2(full_rect.min.x, header_rect.max.y + 1.0), full_rect.max);
                let clip = state.sequence().all_clips().find(|c| c.id == *id).cloned();
                if let Some(clip) = clip {
                    if let Some(curve) = self.sheet.show(ui, state, theme, body, &clip, &mut cmds) {
                        // A row's name double-clicked: its curve.
                        self.set_param(curve);
                        self.dope = false;
                        self.needs_fit = true;
                    }
                }
                return cmds;
            }
        }

        let clip_duration = scope.duration();
        let track_kind = scope.kind();
        let clip_id = scope.clip_id();
        let (v_min, v_max, v_default) = self.param().range();
        let v_range = v_max - v_min;
        #[cfg(test)]
        {
            self.last_view = Some((graph_rect, v_min, v_range));
        }

        // Auto-fit on first display
        if self.needs_fit {
            self.fit_to_content(&graph_rect, clip_duration);
            self.needs_fit = false;
        }

        // Get keyframes (read-only snapshot)
        let keyframes: Vec<Keyframe> = self.get_keyframes(state, clip_id);

        // ── Interaction (scroll/zoom) ──
        let graph_response = ui.allocate_rect(graph_rect, egui::Sense::click_and_drag());
        self.handle_zoom_pan(ui, &graph_response, &graph_rect);

        // ── Draw grid ──
        self.draw_grid(ui, &graph_rect, clip_duration, v_min, v_max, theme);

        // ── Draw playhead ──
        self.draw_playhead(ui, state, &graph_rect, clip_id, clip_duration, theme);

        // ── Draw curve ──, past its keys as its modifier stack extends it
        // (#2277), the same sums the renderer runs.
        let extend = clip_id
            .and_then(|id| state.sequence().all_clips().find(|c| c.id == id))
            .and_then(|clip| modifier_param(self.param()).and_then(|p| clip.truth_lane(&p).and_then(|(_, stacks)| stacks.get(&p).map(|stack| klipp_core::modifier::extension(&stack.modifiers)))))
            .unwrap_or_default();
        self.draw_curve(
            ui, &graph_rect, &keyframes, clip_duration, v_min, v_range, v_default, extend, theme,
        );

        // ── Draw keyframe diamonds + handles ──
        let diamond_cmds = match (self.param(), clip_id) {
            (CurveParam::Camera(param), _) => self.interact_camera_keys(
                ui, state, &graph_rect, &graph_response, &keyframes, *param, clip_duration, v_min, v_range, theme,
            ),
            (_, Some(clip_id)) => self.draw_keyframes_and_interact(
                ui, state, &graph_rect, &graph_response, &keyframes,
                clip_id, clip_duration, v_min, v_range, v_default, track_kind, theme,
            ),
            (_, None) => Vec::new(),
        };
        cmds.extend(diamond_cmds);

        // ── Double-click to add keyframe ──
        if graph_response.double_clicked() {
            if let Some(pos) = graph_response.interact_pointer_pos() {
                if graph_rect.contains(pos) {
                    let t = self.x_to_time(pos.x, &graph_rect, clip_duration);
                    let v = self.y_to_value(pos.y, &graph_rect, v_min, v_range);
                    let t = t.clamp(0.0, clip_duration);
                    let v = v.clamp(v_min, v_max);
                    // The curve editor is exact: a key goes where the
                    // pointer is, not to the nearest frame.
                    if let CurveParam::Camera(param) = self.param() {
                        cmds.push(AppCommand::SetSequenceCameraKey { sequence_idx: state.active_sequence_idx, param: *param, time_secs: t, value: v });
                        state.selected_camera_key = Some(CameraKeyRef { param: *param, time_secs: t });
                    } else if let Some(clip_id) = clip_id {
                        match self.param() {
                            CurveParam::Title { object_id, param } => cmds.push(param.add(clip_id, *object_id, t, v)),
                            CurveParam::Audio(p) => {
                                cmds.push(AppCommand::AddAudioKeyframe {
                                    clip_id,
                                    param: *p,
                                    time_secs: t,
                                    value: v,
                                });
                            }
                            CurveParam::Video(p) => {
                                cmds.push(AppCommand::AddVideoKeyframe {
                                    clip_id,
                                    param: *p,
                                    time_secs: t,
                                    value: v,
                                });
                            }
                            CurveParam::Speed => {
                                // Speed keyframes: directly modify clip
                                // (no dedicated AppCommand yet — add inline)
                            }
                            CurveParam::Node { path, key, .. } => {
                                let keyframe = klipp_core::keyframe::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
                                if let Ok(keyframe) = serde_json::to_value(keyframe) {
                                    cmds.push(AppCommand::AddKeyAt { addr: path.clone(), key: key.clone(), keyframe });
                                }
                            }
                            CurveParam::AudioEffect(key) => {
                                if let Some((idx_str, param_name)) = key.split_once(':') {
                                    if let Ok(effect_index) = idx_str.parse::<usize>() {
                                        cmds.push(AppCommand::AddAudioEffectKeyframe {
                                            clip_id,
                                            effect_index,
                                            param_name: param_name.to_string(),
                                            time_secs: t,
                                            value: v,
                                        });
                                    }
                                }
                            }
                            CurveParam::Camera(_) => {}
                            // A mask's key there; the shape's is the shape as
                            // it is then (#1947).
                            CurveParam::Mask { track, .. } => cmds.push(AppCommand::AddMaskKeyframe {
                                clip_id,
                                node_id: track.node,
                                mask_index: track.mask,
                                param: track.param.clone(),
                                time_secs: t,
                                value: v,
                            }),
                        }
                    }
                }
            }
        }

        // Axis labels
        let label_rect = Rect::from_min_max(
            pos2(full_rect.min.x, graph_rect.min.y),
            pos2(graph_rect.min.x, graph_rect.max.y),
        );
        ui.painter().text(
            pos2(label_rect.center().x, label_rect.center().y),
            egui::Align2::CENTER_CENTER,
            self.param().label(),
            egui::FontId::proportional(10.0),
            theme.text_dim,
        );

        // Time axis label
        ui.painter().text(
            pos2(graph_rect.center().x, graph_rect.max.y + 8.0),
            egui::Align2::CENTER_TOP,
            "Time (s)",
            egui::FontId::proportional(10.0),
            theme.text_dim,
        );

        cmds
    }

    /// Sync the curve param from the rubber band params in state — or,
    /// with no clip selected, from the camera key selected on the lane.
    pub fn sync_param_from_state(&mut self, state: &AppState) {
        if state.selected_clip.is_none() {
            if let Some(key) = state.selected_camera_key {
                if *self.param() != CurveParam::Camera(key.param) {
                    self.set_param(CurveParam::Camera(key.param));
                    self.drag = None;
                }
            }
            return;
        }
        if let Some(clip_id) = state.selected_clip {
            let seq = &state.sequences[state.active_sequence_idx];
            if let Some((ti, _ci)) = seq.find_clip(clip_id) {
                let kind = seq.tracks[ti].kind;
                match kind {
                    TrackKind::Video => {
                        self.set_param(CurveParam::Video(state.video_rubber_band_param));
                    }
                    TrackKind::Audio => {
                        // Curve editor only mirrors clip-level rubber band
                        // params (Volume/Pan/Gain). When the rubber band is
                        // currently showing an effect param, the curve editor
                        // already supports CurveParam::AudioEffect and that
                        // path gets selected by the user via its own picker
                        // — no auto-mirror needed here.
                        if let Some(p) = state.rubber_band_param.as_clip_param() {
                            self.set_param(CurveParam::Audio(p));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// The selected clip, or the sequence when none is selected and the
    /// camera is in view: its lane shown, a key on it, or one of its
    /// curves already chosen here.
    fn scope(&self, state: &AppState) -> Option<CurveScope> {
        if let Some((duration, name, kind)) = self.find_selected_clip(state) {
            return Some(CurveScope::Clip { id: state.selected_clip?, duration, name, kind });
        }
        // Only a camera that moves, or whose lane is open: an empty one
        // is a flat line that reads as the clip's curves missing.
        let camera_in_view = state.show_camera_lane || camera_lane::has_keys(&state.sequence().camera);
        if state.selected_clip.is_none() && camera_in_view {
            return Some(CurveScope::Sequence { duration: state.sequence_duration().to_secs_f64() });
        }
        None
    }

    fn find_selected_clip(&self, state: &AppState) -> Option<(f64, String, TrackKind)> {
        let clip_id = state.selected_clip?;
        let seq = &state.sequences[state.active_sequence_idx];
        let (ti, ci) = seq.find_clip(clip_id)?;
        let clip = &seq.tracks[ti].clips[ci];
        let track_kind = seq.tracks[ti].kind;
        let duration = (clip.timeline_out - clip.timeline_in).to_secs_f64();
        let name = clip.media_id()
            .and_then(|mid| state.media_pool.get(&mid))
            .map(|m: &klipp_media::MediaItem| {
                m.path
                    .file_name()
                    .map(|f: &std::ffi::OsStr| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled".to_string())
            })
            .unwrap_or_else(|| "Untitled".to_string());
        Some((duration, name, track_kind))
    }

    fn get_keyframes(&self, state: &AppState, clip_id: Option<klipp_core::ClipId>) -> Vec<Keyframe> {
        let seq = &state.sequences[state.active_sequence_idx];
        if let CurveParam::Camera(param) = self.param() {
            return camera_lane::keys(&seq.camera, *param).to_vec();
        }
        let Some(clip_id) = clip_id else { return Vec::new() };
        for track in &seq.tracks {
            if let Some(clip) = track.find_clip(clip_id) {
                return match self.param() {
                    CurveParam::Title { object_id, param } => param.keyframes(clip, *object_id).to_vec(),
                    CurveParam::Audio(AudioParam::Volume) => clip.volume_keyframes.clone(),
                    CurveParam::Audio(AudioParam::Pan) => clip.pan_keyframes.clone(),
                    CurveParam::Audio(AudioParam::Gain) => clip.gain_keyframes.clone(),
                    // The placement and opacity lanes as the picture holds them (M4 step 12c).
                    CurveParam::Video(p @ (VideoParam::Opacity | VideoParam::PositionX | VideoParam::PositionY | VideoParam::Scale | VideoParam::ScaleY | VideoParam::Rotation | VideoParam::TiltX | VideoParam::TiltY)) => crate::inspector::placement_by_path::param_keys(clip, *p),
                    CurveParam::Video(VideoParam::MatteFeather) => clip.matte_feather_keyframes.clone(),
                    CurveParam::Video(VideoParam::MatteChoke) => clip.matte_choke_keyframes.clone(),
                    CurveParam::Speed => clip.speed_keyframes.clone(),
                    CurveParam::Node { path, key, .. } => clip.node_key_state(path, key).map(|s| s.keys).unwrap_or_default(),
                    CurveParam::AudioEffect(key) => clip.audio_effect_keyframes.get(key).cloned().unwrap_or_default(),
                    CurveParam::Camera(_) => Vec::new(),
                    CurveParam::Mask { track, .. } => crate::mask_tracks::keys(clip, track),
                };
            }
        }
        Vec::new()
    }

    fn param_selector(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
        if state.selected_clip.is_none() {
            // The sequence's own curves: the camera's four.
            let params: Vec<CurveParam> = camera_lane::PARAMS.iter().map(|p| CurveParam::Camera(*p)).collect();
            if !params.contains(self.param()) {
                self.set_param(params[0].clone());
                self.drag = None;
                self.needs_fit = true;
            }
            for p in &params {
                let active = *self.param() == *p;
                let CurveParam::Camera(camera_param) = p else { continue };
                let text = egui::RichText::new(camera_param.label()).size(10.0);
                let text = if active { text.color(camera_lane::colour(*camera_param, theme)) } else { text.color(theme.text_dim) };
                if ui.selectable_label(active, text).clicked() {
                    self.set_param(p.clone());
                }
            }
            return;
        }
        if let Some(clip) = state.selected_clip.and_then(|cid| state.sequence().all_clips().find(|c| c.id == cid)) {
            if let klipp_timeline::ClipKind::Title(title) = &clip.kind {
                let mut object_id = match self.param() {
                    CurveParam::Title { object_id, .. } if title.object(klipp_timeline::TitleObjectId(*object_id)).is_some() => Some(*object_id),
                    _ => None,
                };
                let name = object_id.and_then(|id| title.object(klipp_timeline::TitleObjectId(id))).map(|o| o.name.as_str()).unwrap_or("Whole title");
                egui::ComboBox::from_id_salt("curve_title_layer").selected_text(name).show_ui(ui, |ui| {
                    ui.selectable_value(&mut object_id, None, "Whole title");
                    for object in &title.objects {
                        ui.selectable_value(&mut object_id, Some(object.id.0), &object.name);
                    }
                });
                if let Some(object) = object_id.and_then(|id| title.object(klipp_timeline::TitleObjectId(id))) {
                    let object_id = object.id.0;
                    let params: Vec<_> = title_curve::parameters(object).into_iter().map(|param| CurveParam::Title { object_id, param }).collect();
                    if !params.contains(self.param()) {
                        self.set_param(params[0].clone());
                        self.drag = None;
                        self.needs_fit = true;
                    }
                    let previous = self.param().clone();
                    egui::ComboBox::from_id_salt("curve_title_param").selected_text(self.param().label()).show_ui(ui, |ui| {
                        for param in params { let label = param.label(); if ui.selectable_label(*self.param() == param, label).clicked() { self.set_param(param); } }
                    });
                    if *self.param() != previous { self.drag = None; self.needs_fit = true; }
                    return;
                }
            }
        }
        let track_kind = state.selected_clip.and_then(|cid| {
            state.sequences[state.active_sequence_idx].tracks.iter()
                .find(|t| t.clips.iter().any(|c| c.id == cid))
                .map(|t| t.kind)
        });
        let kind = track_kind.unwrap_or(TrackKind::Video);
        let mut params: Vec<CurveParam> = match kind {
            TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => vec![
                CurveParam::Video(VideoParam::Opacity),
                CurveParam::Video(VideoParam::PositionX),
                CurveParam::Video(VideoParam::PositionY),
                CurveParam::Video(VideoParam::Scale),
                CurveParam::Video(VideoParam::Rotation),
                CurveParam::Speed,
            ],
            TrackKind::Audio => vec![
                CurveParam::Audio(AudioParam::Volume),
                CurveParam::Audio(AudioParam::Pan),
                CurveParam::Audio(AudioParam::Gain),
            ],
        };

        // Add effect keyframe tracks from the selected clip
        if let Some(cid) = state.selected_clip {
            let seq = &state.sequences[state.active_sequence_idx];
            if let Some(clip) = seq.all_clips().find(|c| c.id == cid) {
                // By node, in the order the picture runs them.
                for (path, key, tag) in clip.animated_effect_keys() {
                    params.push(CurveParam::Node { path, key, tag });
                }
                // Then the grade's, by path through its group (#891).
                for (path, key, type_id) in clip.animated_grade_keys() {
                    params.push(CurveParam::Node { path, key, tag: type_id });
                }
                // Then its masks' keys, by mask (#1947).
                for (track, name) in crate::mask_tracks::keyed(clip) {
                    params.push(CurveParam::Mask { track, name });
                }
                let mut audio_effect_keys: Vec<String> =
                    clip.audio_effect_keyframes.keys().cloned().collect();
                audio_effect_keys.sort();
                for key in audio_effect_keys {
                    params.push(CurveParam::AudioEffect(key));
                }
            }
        }

        if !params.contains(self.param()) {
            self.set_param(params[0].clone());
            self.drag = None;
            self.needs_fit = true;
        }
        for p in &params {
            let active = *self.param() == *p;
            let text = egui::RichText::new(p.label()).size(10.0);
            let text = if active {
                text.color(theme.text_accent)
            } else {
                text.color(theme.text_dim)
            };
            if ui.selectable_label(active, text).clicked() {
                self.set_param(p.clone());
            }
        }
    }

    fn fit_to_content(&mut self, graph_rect: &Rect, clip_duration: f64) {
        if clip_duration > 0.0 && graph_rect.width() > 0.0 {
            self.zoom_x = graph_rect.width() / clip_duration as f32;
            self.scroll.x = 0.0;
        }
        self.zoom_y = 1.0;
        self.scroll.y = 0.0;
    }

    // ── Coordinate conversions ──

    /// Where the key at `t` seconds and `v` is drawn, as the last frame
    /// laid the graph out.
    #[cfg(test)]
    pub(crate) fn screen_at(&self, t: f64, v: f32) -> Option<egui::Pos2> {
        let (rect, v_min, v_range) = self.last_view?;
        Some(pos2(self.time_to_x(t, &rect, 0.0), self.value_to_y(v, &rect, v_min, v_range)))
    }

    /// Where the dope sheet drew the key at `t` on row `row` (#2240).
    #[cfg(test)]
    pub(crate) fn sheet_at(&self, row: usize, t: f64) -> Option<egui::Pos2> {
        self.sheet.screen_at(row, t)
    }

    /// Whether the dope sheet is shown.
    #[cfg(test)]
    pub(crate) fn showing_sheet(&self) -> bool {
        self.dope
    }

    /// Show `param`'s track, as picking it from the header's list does.
    #[cfg(test)]
    pub(crate) fn show_track(&mut self, param: CurveParam) {
        self.set_param(param);
        self.drag = None;
        self.needs_fit = true;
    }

    fn time_to_x(&self, t: f64, graph_rect: &Rect, _clip_duration: f64) -> f32 {
        graph_rect.left() + t as f32 * self.zoom_x - self.scroll.x
    }

    fn value_to_y(&self, v: f32, graph_rect: &Rect, v_min: f32, v_range: f32) -> f32 {
        if v_range.abs() < 1e-9 {
            return graph_rect.center().y;
        }
        let normalized = (v - v_min) / v_range;
        graph_rect.bottom() - normalized * graph_rect.height() * self.zoom_y + self.scroll.y
    }

    fn x_to_time(&self, x: f32, graph_rect: &Rect, _clip_duration: f64) -> f64 {
        ((x - graph_rect.left() + self.scroll.x) / self.zoom_x) as f64
    }

    fn y_to_value(&self, y: f32, graph_rect: &Rect, v_min: f32, v_range: f32) -> f32 {
        if v_range.abs() < 1e-9 {
            return v_min;
        }
        let normalized = (graph_rect.bottom() - y + self.scroll.y) / (graph_rect.height() * self.zoom_y);
        v_min + normalized * v_range
    }

    // ── Zoom & Pan ──

    fn handle_zoom_pan(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        graph_rect: &Rect,
    ) {
        let ctx = ui.ctx();
        let input = ctx.input(|i| {
            (
                i.smooth_scroll_delta,
                i.modifiers.ctrl,
                i.modifiers.alt,
                i.pointer.hover_pos(),
            )
        });
        let (scroll_delta, ctrl, alt, hover_pos) = input;

        // Only process if hovering the graph
        if let Some(pos) = hover_pos.filter(|p| graph_rect.contains(*p)) {
            if scroll_delta.y.abs() > 0.1 {
                if ctrl {
                    // Vertical zoom
                    let factor = if scroll_delta.y > 0.0 { 1.1 } else { 0.9 };
                    self.zoom_y = (self.zoom_y * factor).clamp(0.1, 20.0);
                } else {
                    // Horizontal zoom, centered on mouse
                    let factor = if scroll_delta.y > 0.0 { 1.1 } else { 0.9 };
                    let old_zoom = self.zoom_x;
                    self.zoom_x = (self.zoom_x * factor).clamp(10.0, 5000.0);
                    // Adjust scroll to keep time under cursor stable
                    let cursor_offset = pos.x - graph_rect.left() + self.scroll.x;
                    self.scroll.x += cursor_offset * (self.zoom_x / old_zoom - 1.0);
                }
            }
        }

        // Alt+drag or middle-click drag to pan
        if response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged() && alt)
        {
            let delta = response.drag_delta();
            self.scroll.x -= delta.x;
            self.scroll.y += delta.y;
        }

        // Clamp scroll
        self.scroll.x = self.scroll.x.max(0.0);
    }

    // ── Grid drawing ──

    fn draw_grid(
        &self,
        ui: &mut egui::Ui,
        rect: &Rect,
        clip_duration: f64,
        v_min: f32,
        v_max: f32,
        theme: &KlippTheme,
    ) {
        let painter = ui.painter();
        let grid_stroke = Stroke::new(0.5_f32, theme.border);
        let grid_stroke_major = Stroke::new(0.8_f32, theme.border_light);
        let label_font = egui::FontId::proportional(10.0);

        // Clip the painter to graph rect
        let clip_rect = *rect;

        // ── Vertical grid lines (time) ──
        let time_step = self.compute_time_step();
        let t_start = (self.scroll.x / self.zoom_x) as f64;
        let t_end = t_start + rect.width() as f64 / self.zoom_x as f64;
        let t_first = (t_start / time_step).floor() * time_step;

        let mut t = t_first;
        while t <= t_end + time_step * 0.5 {
            if t >= 0.0 && t <= clip_duration + 0.001 {
                let x = self.time_to_x(t, rect, clip_duration);
                if x >= rect.left() && x <= rect.right() {
                    let is_major = (t / (time_step * 5.0)).fract().abs() < 0.01 || t.abs() < 0.001;
                    painter.line_segment(
                        [pos2(x, rect.top()), pos2(x, rect.bottom())],
                        if is_major { grid_stroke_major } else { grid_stroke },
                    );
                    // Time label below grid
                    painter.text(
                        pos2(x, rect.bottom() + 2.0),
                        egui::Align2::CENTER_TOP,
                        format_time(t),
                        label_font.clone(),
                        theme.text_dim,
                    );
                }
            }
            t += time_step;
        }

        // ── Horizontal grid lines (value) ──
        let v_range = v_max - v_min;
        let value_step = self.compute_value_step(v_range);
        let v_first = (v_min / value_step).floor() * value_step;

        let mut v = v_first;
        while v <= v_max + value_step * 0.1 {
            let y = self.value_to_y(v, rect, v_min, v_range);
            if y >= rect.top() && y <= rect.bottom() {
                let is_major = (v / (value_step * 5.0)).fract().abs() < 0.001 * value_step
                    || v.abs() < 0.001 * value_step;
                painter.line_segment(
                    [pos2(rect.left(), y), pos2(rect.right(), y)],
                    if is_major { grid_stroke_major } else { grid_stroke },
                );
                // Value label
                painter.text(
                    pos2(rect.left() - 4.0, y),
                    egui::Align2::RIGHT_CENTER,
                    format_value(v, v_range),
                    label_font.clone(),
                    theme.text_dim,
                );
            }
            v += value_step;
        }

        // ── Border ──
        painter.rect_stroke(clip_rect, 0.0, Stroke::new(0.5_f32, theme.border));
    }

    fn compute_time_step(&self) -> f64 {
        // Choose a step so labels don't overlap (~60px minimum between ticks)
        let px_per_step = 60.0;
        let secs_per_step = px_per_step / self.zoom_x;
        // Snap to nice values: 0.1, 0.25, 0.5, 1, 2, 5, 10, ...
        nice_step(secs_per_step as f64)
    }

    fn compute_value_step(&self, v_range: f32) -> f32 {
        let px_per_step = 40.0;
        let effective_height = 200.0_f32.max(1.0); // approximate
        let units_per_step = v_range * px_per_step / (effective_height * self.zoom_y);
        nice_step(units_per_step as f64) as f32
    }

    // ── Playhead ──

    fn draw_playhead(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        graph_rect: &Rect,
        clip_id: Option<klipp_core::ClipId>,
        clip_duration: f64,
        theme: &KlippTheme,
    ) {
        let playhead_secs = state.playhead_smooth_secs;
        // A clip's time starts where the clip does; the sequence's at zero.
        let relative = match clip_id {
            Some(clip_id) => {
                let seq = &state.sequences[state.active_sequence_idx];
                let Some((ti, ci)) = seq.find_clip(clip_id) else { return };
                playhead_secs - seq.tracks[ti].clips[ci].timeline_in.to_secs_f64()
            }
            None => playhead_secs,
        };

        if relative >= 0.0 && relative <= clip_duration {
            let x = self.time_to_x(relative, graph_rect, clip_duration);
            if x >= graph_rect.left() && x <= graph_rect.right() {
                ui.painter().line_segment(
                    [pos2(x, graph_rect.top()), pos2(x, graph_rect.bottom())],
                    Stroke::new(1.5_f32, theme.accent),
                );
                // Small triangle at top
                let tri_size = 5.0;
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        pos2(x, graph_rect.top()),
                        pos2(x - tri_size, graph_rect.top() - tri_size),
                        pos2(x + tri_size, graph_rect.top() - tri_size),
                    ],
                    theme.accent,
                    Stroke::NONE,
                ));
            }
        }
    }

    // ── Curve drawing ──

    #[allow(clippy::too_many_arguments)]
    fn draw_curve(
        &self,
        ui: &mut egui::Ui,
        graph_rect: &Rect,
        keyframes: &[Keyframe],
        clip_duration: f64,
        v_min: f32,
        v_range: f32,
        v_default: f32,
        extend: (klipp_core::Extrapolation, klipp_core::Extrapolation),
        theme: &KlippTheme,
    ) {
        if clip_duration <= 0.0 {
            return;
        }

        let painter = ui.painter();
        let curve_color = theme.accent;
        let curve_stroke = Stroke::new(1.8_f32, curve_color);

        if keyframes.is_empty() {
            // Draw flat line at default value
            let y = self.value_to_y(v_default, graph_rect, v_min, v_range);
            if y >= graph_rect.top() && y <= graph_rect.bottom() {
                painter.line_segment(
                    [pos2(graph_rect.left(), y), pos2(graph_rect.right(), y)],
                    Stroke::new(1.0_f32, Color32::from_rgba_premultiplied(
                        curve_color.r(), curve_color.g(), curve_color.b(), 80,
                    )),
                );
            }
            return;
        }

        // Sample the curve densely and draw as polyline
        let num_samples = (graph_rect.width() as usize).clamp(100, 800);
        let mut points = Vec::with_capacity(num_samples + 2);

        let t_start = self.x_to_time(graph_rect.left(), graph_rect, clip_duration).max(0.0);
        let t_end = self.x_to_time(graph_rect.right(), graph_rect, clip_duration).min(clip_duration);

        if t_end <= t_start {
            return;
        }

        let dt = (t_end - t_start) / num_samples as f64;
        for i in 0..=num_samples {
            let t = t_start + dt * i as f64;
            let v = klipp_core::modifier::evaluate_extended(keyframes, extend.0, extend.1, t, v_default);
            let x = self.time_to_x(t, graph_rect, clip_duration);
            let y = self.value_to_y(v, graph_rect, v_min, v_range);
            points.push(pos2(x, y));
        }

        // Draw segments (clipped to graph); past the keys, where an
        // extension carries the curve on, fainter (#2277).
        let keyed_x = (
            self.time_to_x(keyframes[0].time_secs, graph_rect, clip_duration),
            self.time_to_x(keyframes[keyframes.len() - 1].time_secs, graph_rect, clip_duration),
        );
        let faint = Stroke::new(1.4_f32, Color32::from_rgba_premultiplied(curve_color.r() / 2, curve_color.g() / 2, curve_color.b() / 2, 140));
        for w in points.windows(2) {
            let a = w[0];
            let b = w[1];
            let outside = b.x <= keyed_x.0 + 0.5 || a.x >= keyed_x.1 - 0.5;
            let curve_stroke = if outside { faint } else { curve_stroke };
            // Basic clipping
            if (a.x >= graph_rect.left() || b.x >= graph_rect.left())
                && (a.x <= graph_rect.right() || b.x <= graph_rect.right())
            {
                painter.line_segment([a, b], curve_stroke);
            }
        }
    }

    // ── Keyframe diamonds and bezier handles ──

    #[allow(clippy::too_many_arguments)]
    fn draw_keyframes_and_interact(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        graph_rect: &Rect,
        response: &egui::Response,
        keyframes: &[Keyframe],
        clip_id: klipp_core::ClipId,
        clip_duration: f64,
        v_min: f32,
        v_range: f32,
        _v_default: f32,
        _track_kind: TrackKind,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let painter = ui.painter();
        let (p_v_min, p_v_max, _) = self.param().range();

        // Draw tangent handles for selected keyframe
        let selected_kf_idx = state.selected_keyframe.as_ref().and_then(|sk| {
            if sk.clip_id == clip_id && self.param_matches_kind(&sk.param_kind) {
                Some(sk.keyframe_index)
            } else {
                None
            }
        });

        let marked = crate::key_selection::marked_indices(state, clip_id, &self.param_to_kind(), keyframes);
        self.draw_diamonds(painter, graph_rect, keyframes, selected_kf_idx, &marked, clip_duration, v_min, v_range, theme.accent, theme);

        // ── Interaction: click to select, drag to move ──
        let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());

        // Click to select a keyframe
        if response.clicked() {
            if let Some(pos) = pointer_pos {
                let mut clicked_kf = None;
                for (i, kf) in keyframes.iter().enumerate() {
                    let x = self.time_to_x(kf.time_secs, graph_rect, clip_duration);
                    let y = self.value_to_y(kf.value, graph_rect, v_min, v_range);
                    let dist = ((pos.x - x).powi(2) + (pos.y - y).powi(2)).sqrt();
                    if dist < 10.0 {
                        clicked_kf = Some(i);
                        break;
                    }
                }
                // Shift adds the key or takes it out; a plain click selects
                // it alone, or, on empty graph, nothing (#1958).
                let shift = ui.ctx().input(|i| i.modifiers.shift);
                let clip = state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).cloned();
                match (clicked_kf, clip) {
                    (Some(idx), Some(clip)) => crate::key_selection::click(state, &clip, &self.param_to_kind(), idx, shift),
                    (Some(idx), None) => {
                        state.selected_keyframe = Some(SelectedKeyframe { clip_id, param_kind: self.param_to_kind(), keyframe_index: idx });
                    }
                    (None, _) if !shift => crate::key_selection::clear(state),
                    (None, _) => {}
                }
            }
        }

        // Right-click context menu
        if response.secondary_clicked() {
            if let Some(pos) = pointer_pos {
                for (i, kf) in keyframes.iter().enumerate() {
                    let x = self.time_to_x(kf.time_secs, graph_rect, clip_duration);
                    let y = self.value_to_y(kf.value, graph_rect, v_min, v_range);
                    let dist = ((pos.x - x).powi(2) + (pos.y - y).powi(2)).sqrt();
                    if dist < 10.0 {
                        state.keyframe_context_menu = Some(KeyframeContextMenu {
                            clip_id,
                            param_kind: self.param_to_kind(),
                            keyframe_index: i,
                            screen_pos: pos,
                        });
                        break;
                    }
                }
            }
        }

        // ── Drag keyframes ──
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = pointer_pos {
                // Check for handle hit first (if selected keyframe has bezier handles)
                let mut found_handle = false;
                if let Some(sel_idx) = selected_kf_idx {
                    if let Some(kf) = keyframes.get(sel_idx) {
                        // Only a handle drawn is taken: a linear key's
                        // unset handles sit on the key itself, which a
                        // press there means to move (#1958).
                        let bezier = kf.interpolation == klipp_core::keyframe::Interpolation::Bezier;
                        // Check in-handle
                        let hx = self.time_to_x(kf.time_secs + kf.tangent_in.0, graph_rect, clip_duration);
                        let hy = self.value_to_y(kf.value + kf.tangent_in.1, graph_rect, v_min, v_range);
                        if (bezier || kf.tangent_in != (0.0, 0.0)) && ((pos.x - hx).powi(2) + (pos.y - hy).powi(2)).sqrt() < 8.0 {
                            self.drag = Some(CurveDrag {
                                keyframe_index: sel_idx,
                                handle: Some(false), // in
                            });
                            found_handle = true;
                        }
                        if !found_handle {
                            // Check out-handle
                            let hx = self.time_to_x(kf.time_secs + kf.tangent_out.0, graph_rect, clip_duration);
                            let hy = self.value_to_y(kf.value + kf.tangent_out.1, graph_rect, v_min, v_range);
                            if (bezier || kf.tangent_out != (0.0, 0.0)) && ((pos.x - hx).powi(2) + (pos.y - hy).powi(2)).sqrt() < 8.0 {
                                self.drag = Some(CurveDrag {
                                    keyframe_index: sel_idx,
                                    handle: Some(true), // out
                                });
                                found_handle = true;
                            }
                        }
                    }
                }
                // A clip lane's Bezier handle dragged is one step too, as
                // its key is.
                if found_handle && matches!(self.param(), CurveParam::Video(_) | CurveParam::Audio(_) | CurveParam::AudioEffect(_)) {
                    cmds.push(AppCommand::BeginKeyframeDrag { key: lane_drag_key(clip_id, self.param()), clip_id });
                }
                // If no handle, check for diamond hit
                if !found_handle {
                    let alt = ui.ctx().input(|i| i.modifiers.alt);
                    if !alt {
                        for (i, kf) in keyframes.iter().enumerate() {
                            let x = self.time_to_x(kf.time_secs, graph_rect, clip_duration);
                            let y = self.value_to_y(kf.value, graph_rect, v_min, v_range);
                            let dist = ((pos.x - x).powi(2) + (pos.y - y).powi(2)).sqrt();
                            if dist < 10.0 {
                                // A key of several selected: every one
                                // moves with it, one step (#1958).
                                let clip = state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).cloned();
                                if let Some(clip) = clip.filter(|_| crate::key_selection::several(state) && state.key_selection.holds(clip_id, &self.param_to_kind(), kf.time_secs)) {
                                    cmds.extend(crate::key_selection::begin_drag(state, &clip, Some(self.param_to_kind())));
                                    self.multi_from = Some((self.x_to_time(pos.x, graph_rect, clip_duration), self.y_to_value(pos.y, graph_rect, v_min, v_range)));
                                    break;
                                }
                                self.drag = Some(CurveDrag {
                                    keyframe_index: i,
                                    handle: None,
                                });
                                // A node's key dragged: one undo step (M3 step 3).
                                if let CurveParam::Node { path, key, .. } = self.param() {
                                    cmds.push(AppCommand::BeginParamEditAt { addr: path.clone(), key: key.clone() });
                                }
                                // A mask's key too, as the grade's edits are (#1947).
                                if let CurveParam::Mask { .. } = self.param() {
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                }
                                // A clip lane's key too, as the timeline's
                                // lanes bracket theirs (#1424).
                                if matches!(self.param(), CurveParam::Video(_) | CurveParam::Audio(_) | CurveParam::AudioEffect(_)) {
                                    cmds.push(AppCommand::BeginKeyframeDrag { key: lane_drag_key(clip_id, self.param()), clip_id });
                                }
                                let found = state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).cloned();
                    if let Some(clip) = found {
                                    crate::key_selection::click(state, &clip, &self.param_to_kind(), i, false);
                                } else {
                                    state.selected_keyframe = Some(SelectedKeyframe { clip_id, param_kind: self.param_to_kind(), keyframe_index: i });
                                }
                                break;
                            }
                        }
                        // On empty graph: a box, selecting the keys it
                        // holds when let go (#1958).
                        if self.drag.is_none() && self.multi_from.is_none() {
                            self.box_from = Some((pos, ui.ctx().input(|i| i.modifiers.shift)));
                        }
                    }
                }
            }
        }

        // The selected keys dragged together (#1958).
        if let Some((t0, v0)) = self.multi_from {
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = pointer_pos {
                    let dt = self.x_to_time(pos.x, graph_rect, clip_duration) - t0;
                    let dv = self.y_to_value(pos.y, graph_rect, v_min, v_range) - v0;
                    let found = state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).cloned();
                    if let Some(clip) = found {
                        cmds.extend(crate::key_selection::drag_to(state, &clip, dt, dv));
                    }
                }
            }
            if response.drag_stopped() {
                cmds.extend(crate::key_selection::end_drag(state));
                self.multi_from = None;
            }
        }
        // The box drawn, and on release, what it holds selected.
        if let Some((from, add)) = self.box_from {
            if let Some(pos) = pointer_pos {
                let r = Rect::from_two_pos(from, pos);
                painter.rect_filled(r, 0.0, theme.accent.gamma_multiply(0.12));
                painter.rect_stroke(r, 0.0, Stroke::new(1.0_f32, theme.accent));
                if response.drag_stopped() {
                    let at = |p: egui::Pos2| (self.x_to_time(p.x, graph_rect, clip_duration), self.y_to_value(p.y, graph_rect, v_min, v_range));
                    let found = state.sequences[state.active_sequence_idx].all_clips().find(|c| c.id == clip_id).cloned();
                    if let Some(clip) = found {
                        crate::key_selection::select_box(state, &clip, &self.param_to_kind(), at(from), at(pos), add);
                    }
                }
            }
            if response.drag_stopped() || !ui.ctx().input(|i| i.pointer.primary_down()) {
                self.box_from = None;
            }
        }

        // Process drag
        let mut moved_title_index = None;
        if response.dragged_by(egui::PointerButton::Primary) {
            if let Some(ref drag) = self.drag {
                if let Some(pos) = pointer_pos {
                    let kf_idx = drag.keyframe_index;
                    if let Some(kf) = keyframes.get(kf_idx) {
                        match drag.handle {
                            None => {
                                // Moving the keyframe itself
                                let new_t = self.x_to_time(pos.x, graph_rect, clip_duration)
                                    .clamp(0.0, clip_duration);
                                let new_v = self.y_to_value(pos.y, graph_rect, v_min, v_range)
                                    .clamp(p_v_min, p_v_max);
                                match self.param() {
                                    CurveParam::Audio(p) => {
                                        cmds.push(AppCommand::MoveAudioKeyframe {
                                            clip_id,
                                            param: *p,
                                            index: kf_idx,
                                            new_time_secs: new_t,
                                            new_value: new_v,
                                        });
                                    }
                                    CurveParam::Video(p) => {
                                        cmds.push(AppCommand::MoveVideoKeyframe {
                                            clip_id,
                                            param: *p,
                                            index: kf_idx,
                                            new_time_secs: new_t,
                                            new_value: new_v,
                                        });
                                    }
                                    CurveParam::Title { object_id, param } => {
                                        cmds.push(param.move_keyframe(clip_id, *object_id, kf_idx, new_t, new_v));
                                        moved_title_index = Some(keyframes.iter().enumerate().filter(|(i, k)| *i != kf_idx && (k.time_secs < new_t || (k.time_secs == new_t && *i < kf_idx))).count());
                                    }
                                    // By the key's time now; the keys stay in time
                                    // order, so the dragged one's place follows.
                                    // Never onto another key's time: keys are named
                                    // by their time (the command refuses it too).
                                    CurveParam::Node { .. } if keyframes.iter().enumerate().any(|(i, k)| i != kf_idx && (k.time_secs - new_t).abs() < 1e-9) => {}
                                    CurveParam::Node { path, key, .. } => {
                                        cmds.push(AppCommand::MoveKeyAt { addr: path.clone(), key: key.clone(), time_secs: kf.time_secs, new_time_secs: new_t, value: new_v });
                                        moved_title_index = Some(keyframes.iter().enumerate().filter(|(i, k)| *i != kf_idx && (k.time_secs < new_t || (k.time_secs == new_t && *i < kf_idx))).count());
                                    }
                                    // A mask's key, by its time now, never onto
                                    // another's; a shape key moves in time only.
                                    CurveParam::Mask { .. } if keyframes.iter().enumerate().any(|(i, k)| i != kf_idx && (k.time_secs - new_t).abs() < 1e-9) => {}
                                    CurveParam::Mask { track, .. } => {
                                        cmds.push(AppCommand::MoveMaskKeyframe {
                                            clip_id,
                                            node_id: track.node,
                                            mask_index: track.mask,
                                            param: track.param.clone(),
                                            time_secs: kf.time_secs,
                                            new_time_secs: new_t,
                                            value: (!track.is_shape()).then_some(new_v),
                                        });
                                        moved_title_index = Some(keyframes.iter().enumerate().filter(|(i, k)| *i != kf_idx && (k.time_secs < new_t || (k.time_secs == new_t && *i < kf_idx))).count());
                                    }
                                    // A sound effect's key (#1990), its place in
                                    // time order followed as it passes another.
                                    CurveParam::AudioEffect(_) => {
                                        if let KeyframeParamKind::AudioEffect { effect_index, param_name } = self.param_to_kind() {
                                            cmds.push(AppCommand::MoveAudioEffectKeyframe { clip_id, effect_index, param_name, keyframe_index: kf_idx, new_time_secs: new_t, new_value: new_v });
                                            moved_title_index = Some(keyframes.iter().enumerate().filter(|(i, k)| *i != kf_idx && (k.time_secs < new_t || (k.time_secs == new_t && *i < kf_idx))).count());
                                        }
                                    }
                                    CurveParam::Speed
                                    | CurveParam::Camera(_) => {
                                        // Speed keyframe move not yet wired to commands
                                    }
                                }
                            }
                            Some(is_out) => {
                                // Dragging tangent handle
                                let handle_t = self.x_to_time(pos.x, graph_rect, clip_duration);
                                let handle_v = self.y_to_value(pos.y, graph_rect, v_min, v_range);
                                let dt = handle_t - kf.time_secs;
                                let dv = handle_v - kf.value;
                                let (tangent_in, tangent_out) = if is_out {
                                    (None, Some((dt, dv)))
                                } else {
                                    (Some((dt, dv)), None)
                                };
                                match self.param() {
                                    CurveParam::Audio(p) => {
                                        cmds.push(AppCommand::SetAudioKeyframeTangent {
                                            clip_id,
                                            param: *p,
                                            keyframe_index: kf_idx,
                                            tangent_in,
                                            tangent_out,
                                        });
                                    }
                                    CurveParam::Video(p) => {
                                        cmds.push(AppCommand::SetVideoKeyframeTangent {
                                            clip_id,
                                            param: *p,
                                            keyframe_index: kf_idx,
                                            tangent_in,
                                            tangent_out,
                                        });
                                    }
                                    CurveParam::Title { object_id, param } => cmds.push(param.tangent(clip_id, *object_id, kf_idx, tangent_in, tangent_out)),
                                    CurveParam::AudioEffect(_) => {
                                        if let KeyframeParamKind::AudioEffect { effect_index, param_name } = self.param_to_kind() {
                                            cmds.push(AppCommand::SetAudioEffectKeyframeTangent { clip_id, effect_index, param_name, keyframe_index: kf_idx, tangent_in, tangent_out });
                                        }
                                    }
                                    CurveParam::Speed
                                    | CurveParam::Node { .. }
                                    | CurveParam::Camera(_)
                                    | CurveParam::Mask { .. } => {
                                        // Tangent editing not yet wired for speed/effects
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(index) = moved_title_index {
            if let Some(drag) = &mut self.drag { drag.keyframe_index = index; }
            state.selected_keyframe = Some(SelectedKeyframe { clip_id, param_kind: self.param_to_kind(), keyframe_index: index });
        }
        if response.drag_stopped() {
            if let (Some(CurveDrag { handle: None, .. }), CurveParam::Node { path, key, .. }) = (&self.drag, self.param()) {
                cmds.push(AppCommand::EndParamEditAt { addr: path.clone(), key: key.clone() });
            }
            if let (Some(CurveDrag { handle: None, .. }), CurveParam::Mask { .. }) = (&self.drag, self.param()) {
                cmds.push(AppCommand::EndNodeParamEdit { clip_id });
            }
            if let (Some(_), CurveParam::Video(_) | CurveParam::Audio(_) | CurveParam::AudioEffect(_)) = (&self.drag, self.param()) {
                cmds.push(AppCommand::EndKeyframeDrag { key: lane_drag_key(clip_id, self.param()), clip_id });
            }
            self.drag = None;
        }

        cmds
    }

    /// The diamonds, and the selected one's Bezier handles, for any
    /// list of keys on the graph: a clip's in the accent, the camera's
    /// in its row's colour.
    #[allow(clippy::too_many_arguments)]
    fn draw_diamonds(
        &self,
        painter: &egui::Painter,
        graph_rect: &Rect,
        keyframes: &[Keyframe],
        selected_kf_idx: Option<usize>,
        marked: &[usize],
        clip_duration: f64,
        v_min: f32,
        v_range: f32,
        tint: Color32,
        theme: &KlippTheme,
    ) {
        let diamond_size = 5.0;
        let handle_radius = 3.5;
        for (i, kf) in keyframes.iter().enumerate() {
            let x = self.time_to_x(kf.time_secs, graph_rect, clip_duration);
            let y = self.value_to_y(kf.value, graph_rect, v_min, v_range);
            let center = pos2(x, y);

            if x < graph_rect.left() - 20.0 || x > graph_rect.right() + 20.0 {
                continue;
            }

            // Draw bezier tangent handles if this keyframe is selected
            if Some(i) == selected_kf_idx {
                let handle_color = theme.marker_info;
                let handle_stroke = Stroke::new(1.0_f32, handle_color);

                // In-tangent
                if kf.tangent_in != (0.0, 0.0) || kf.interpolation == klipp_core::keyframe::Interpolation::Bezier {
                    let hx = self.time_to_x(kf.time_secs + kf.tangent_in.0, graph_rect, clip_duration);
                    let hy = self.value_to_y(kf.value + kf.tangent_in.1, graph_rect, v_min, v_range);
                    let handle_pos = pos2(hx, hy);
                    painter.line_segment([center, handle_pos], handle_stroke);
                    painter.circle_filled(handle_pos, handle_radius, handle_color);
                }

                // Out-tangent
                if kf.tangent_out != (0.0, 0.0) || kf.interpolation == klipp_core::keyframe::Interpolation::Bezier {
                    let hx = self.time_to_x(kf.time_secs + kf.tangent_out.0, graph_rect, clip_duration);
                    let hy = self.value_to_y(kf.value + kf.tangent_out.1, graph_rect, v_min, v_range);
                    let handle_pos = pos2(hx, hy);
                    painter.line_segment([center, handle_pos], handle_stroke);
                    painter.circle_filled(handle_pos, handle_radius, handle_color);
                }
            }

            // Draw diamond
            // The primary key, or one of several selected (#1958).
            let is_selected = Some(i) == selected_kf_idx || marked.contains(&i);
            let diamond_color = if is_selected {
                theme.text_primary
            } else {
                tint
            };

            let diamond = vec![
                pos2(x, y - diamond_size),
                pos2(x + diamond_size, y),
                pos2(x, y + diamond_size),
                pos2(x - diamond_size, y),
            ];
            painter.add(egui::Shape::convex_polygon(
                diamond,
                diamond_color,
                Stroke::new(0.8_f32, theme.border),
            ));
        }
    }

    /// The sequence camera's keys on one list, in the editor: selected,
    /// dragged in time and value, and given handles, through the camera
    /// commands, named by their stored moment as on the lane. A drag is
    /// bracketed so undo takes all of it back in one step.
    #[allow(clippy::too_many_arguments)]
    fn interact_camera_keys(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        graph_rect: &Rect,
        response: &egui::Response,
        keyframes: &[Keyframe],
        param: CameraParam,
        duration: f64,
        v_min: f32,
        v_range: f32,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let sequence_idx = state.active_sequence_idx;
        let (p_v_min, p_v_max, _) = self.param().range();
        let selected = state
            .selected_camera_key
            .filter(|k| k.param == param)
            .and_then(|k| camera_lane::index_at(keyframes, k.time_secs));
        self.draw_diamonds(ui.painter(), graph_rect, keyframes, selected, &[], duration, v_min, v_range, camera_lane::colour(param, theme), theme);

        let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());
        let at = |t: f64, v: f32| pos2(self.time_to_x(t, graph_rect, duration), self.value_to_y(v, graph_rect, v_min, v_range));
        let hit = |pos: egui::Pos2| keyframes.iter().position(|kf| at(kf.time_secs, kf.value).distance(pos) < 10.0);
        let hit_handle = |pos: egui::Pos2, kf: &Keyframe| -> Option<bool> {
            if at(kf.time_secs + kf.tangent_in.0, kf.value + kf.tangent_in.1).distance(pos) < 8.0 {
                Some(false)
            } else if at(kf.time_secs + kf.tangent_out.0, kf.value + kf.tangent_out.1).distance(pos) < 8.0 {
                Some(true)
            } else {
                None
            }
        };
        let key_ref = |i: usize| CameraKeyRef { param, time_secs: keyframes[i].time_secs };

        if response.clicked() {
            if let Some(pos) = pointer_pos {
                state.selected_camera_key = hit(pos).map(key_ref);
            }
        }
        if response.secondary_clicked() {
            if let Some(pos) = pointer_pos {
                if let Some(i) = hit(pos) {
                    let key = key_ref(i);
                    state.selected_camera_key = Some(key);
                    state.camera_key_menu = Some(CameraKeyMenu { key, screen_pos: pos });
                }
            }
        }
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = pointer_pos {
                let alt = ui.ctx().input(|i| i.modifiers.alt);
                let on_handle = selected.and_then(|i| hit_handle(pos, &keyframes[i]).map(|h| (i, Some(h))));
                let start = on_handle.or_else(|| if alt { None } else { hit(pos).map(|i| (i, None)) });
                if let Some((i, handle)) = start {
                    let key = key_ref(i);
                    state.selected_camera_key = Some(key);
                    state.dragging_camera_key = Some(DraggingCameraKey { key, handle });
                    cmds.push(AppCommand::BeginSequenceCameraDrag { sequence_idx });
                }
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let (Some(drag), Some(pos)) = (state.dragging_camera_key, pointer_pos) {
                if drag.key.param == param {
                    if let Some(index) = camera_lane::index_at(keyframes, drag.key.time_secs) {
                        let kf = &keyframes[index];
                        match drag.handle {
                            None => {
                                let new_time_secs = self.x_to_time(pos.x, graph_rect, duration).clamp(0.0, duration.max(0.0));
                                let new_value = self.y_to_value(pos.y, graph_rect, v_min, v_range).clamp(p_v_min, p_v_max);
                                cmds.push(AppCommand::MoveSequenceCameraKey { sequence_idx, param, index, new_time_secs, new_value });
                                let moved = CameraKeyRef { param, time_secs: new_time_secs };
                                state.dragging_camera_key = Some(DraggingCameraKey { key: moved, handle: None });
                                state.selected_camera_key = Some(moved);
                            }
                            Some(is_out) => {
                                let dt = self.x_to_time(pos.x, graph_rect, duration) - kf.time_secs;
                                let dv = self.y_to_value(pos.y, graph_rect, v_min, v_range) - kf.value;
                                let (tangent_in, tangent_out) = if is_out { (None, Some((dt, dv))) } else { (Some((dt, dv)), None) };
                                cmds.push(AppCommand::SetSequenceCameraKeyTangent { sequence_idx, param, index, tangent_in, tangent_out });
                            }
                        }
                    }
                }
            }
        }
        if response.drag_stopped() && state.dragging_camera_key.take().is_some() {
            cmds.push(AppCommand::EndSequenceCameraDrag { sequence_idx });
        }
        cmds
    }

    fn param_to_kind(&self) -> KeyframeParamKind {
        match self.param() {
            CurveParam::Title { object_id, param } => KeyframeParamKind::Title { object_id: *object_id, param: param.clone() },
            CurveParam::Audio(p) => KeyframeParamKind::Audio(*p),
            CurveParam::Video(p) => KeyframeParamKind::Video(*p),
            CurveParam::AudioEffect(key) => {
                if let Some((idx_str, name)) = key.split_once(':') {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        return KeyframeParamKind::AudioEffect {
                            effect_index: idx,
                            param_name: name.to_string(),
                        };
                    }
                }
                KeyframeParamKind::Video(VideoParam::Opacity)
            }
            CurveParam::Node { path, key, .. } => KeyframeParamKind::Node { path: path.clone(), key: key.clone() },
            CurveParam::Mask { track, .. } => KeyframeParamKind::Mask(track.clone()),
            CurveParam::Speed | CurveParam::Camera(_) => {
                KeyframeParamKind::Video(VideoParam::Opacity) // fallback
            }
        }
    }

    fn param_matches_kind(&self, kind: &KeyframeParamKind) -> bool {
        match (self.param(), kind) {
            (CurveParam::Title { object_id: a, param: p }, KeyframeParamKind::Title { object_id: b, param: q }) => a == b && p == q,
            (CurveParam::Audio(a), KeyframeParamKind::Audio(b)) => a == b,
            (CurveParam::Video(a), KeyframeParamKind::Video(b)) => a == b,
            (CurveParam::AudioEffect(key), KeyframeParamKind::AudioEffect { effect_index, param_name }) => {
                key == &format!("{effect_index}:{param_name}")
            }
            (CurveParam::Node { path: a, key: k, .. }, KeyframeParamKind::Node { path: b, key: l }) => a == b && k == l,
            (CurveParam::Mask { track: a, .. }, KeyframeParamKind::Mask(b)) => a == b,
            _ => false,
        }
    }
}

/// The history drag a clip lane's key drag is held under.
fn lane_drag_key(clip_id: klipp_core::ClipId, param: &CurveParam) -> String {
    match param {
        // A video lane's drag names its lane, as the timeline's do, so the
        // drag is that lane's by path (M4 step 12c).
        CurveParam::Video(p) => format!("kf:video:{}:{p:?}:curve", clip_id.0),
        _ => format!("kf:curve:{}", clip_id.0),
    }
}

/// Choose a "nice" step size for grid lines.
fn nice_step(raw: f64) -> f64 {
    let magnitude = 10_f64.powf(raw.abs().log10().floor());
    let residual = raw / magnitude;
    let nice = if residual <= 1.0 {
        1.0
    } else if residual <= 2.0 {
        2.0
    } else if residual <= 5.0 {
        5.0
    } else {
        10.0
    };
    (nice * magnitude).max(0.001)
}

fn format_time(t: f64) -> String {
    if t < 1.0 {
        format!("{:.2}s", t)
    } else if t < 60.0 {
        format!("{:.1}s", t)
    } else {
        let m = (t / 60.0) as u32;
        let s = t - m as f64 * 60.0;
        format!("{}:{:04.1}", m, s)
    }
}

fn format_value(v: f32, range: f32) -> String {
    if range >= 100.0 {
        format!("{:.0}", v)
    } else if range >= 1.0 {
        format!("{:.2}", v)
    } else {
        format!("{:.3}", v)
    }
}

#[cfg(test)]
mod curve_param_tests {
    use super::*;

    #[test]
    fn title_selection_keeps_layer_and_channel_identity() {
        let mut panel = CurveEditorPanel::new();
        panel.set_param(CurveParam::Title { object_id: 7, param: TitleCurveParam::Effect("glow.1.radius".into()) });
        let selected = panel.param_to_kind();
        assert!(panel.param_matches_kind(&selected));
        assert!(!panel.param_matches_kind(&KeyframeParamKind::Title { object_id: 8, param: TitleCurveParam::Effect("glow.1.radius".into()) }));
        assert!(!panel.param_matches_kind(&KeyframeParamKind::Title { object_id: 7, param: TitleCurveParam::Effect("glow.0.radius".into()) }));
        assert!(!panel.param_matches_kind(&KeyframeParamKind::Video(VideoParam::Opacity)));
        assert_eq!(panel.param().label(), "Glow 2 · radius");
        panel.set_param(CurveParam::Title { object_id: 7, param: TitleCurveParam::Position(klipp_core::TitlePositionAxis::X) });
        assert_eq!(panel.param().range(), (-1.0, 2.0, 0.5));
    }

    /// M3 step 3: the curve editor and keyframe selection read an
    /// effect's keys by its node, never by its place in the list.
    #[test]
    fn no_positional_effect_keys_in_these_files() {
        for (name, text) in [("curve_editor.rs", include_str!("curve_editor.rs")), ("keyframe_selection.rs", include_str!("../inspector/keyframe_selection.rs"))] {
            // The file before its test module (a test-only helper above it
            // is still production code's neighbour, and scanned).
            let code = text.split("#[cfg(test)]\nmod ").next().unwrap_or(text);
            for positional in [".effect_keyframes()", "CurveParam::Effect("] {
                assert!(!code.contains(positional), "{name} reads an effect's keys by its place: {positional}");
            }
        }
    }

    #[test]
    fn audio_effect_label_humanizes_reserved_keys() {
        // The picker shows reserved kf tracks under friendly names so
        // "0:__bypass" reads as "Audio FX: Bypass" not "Audio FX: __bypass".
        assert_eq!(
            CurveParam::AudioEffect("0:__bypass".into()).label(),
            "Audio FX: Bypass",
        );
        assert_eq!(
            CurveParam::AudioEffect("2:__wetdry".into()).label(),
            "Audio FX: Wet/Dry",
        );
    }

    #[test]
    fn audio_effect_label_passes_through_real_param_names() {
        assert_eq!(
            CurveParam::AudioEffect("1:threshold".into()).label(),
            "Audio FX: threshold",
        );
        assert_eq!(
            CurveParam::AudioEffect("0:gain".into()).label(),
            "Audio FX: gain",
        );
    }

    #[test]
    fn audio_effect_range_uses_zero_one_for_reserved_keys() {
        // Bypass and wet/dry are clamped 0..1 on the chain side;
        // pin the picker matches so the curve editor doesn't draw
        // out-of-range handles.
        let (min, max, _default) = CurveParam::AudioEffect("0:__bypass".into()).range();
        assert_eq!(min, 0.0);
        assert_eq!(max, 1.0);
        let (min, max, _default) = CurveParam::AudioEffect("0:__wetdry".into()).range();
        assert_eq!(min, 0.0);
        assert_eq!(max, 1.0);
    }
}
