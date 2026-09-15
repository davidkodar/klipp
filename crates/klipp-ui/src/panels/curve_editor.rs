use crate::state::{AppState, KeyframeParamKind, SelectedKeyframe, KeyframeContextMenu};
use crate::theme::KlippTheme;
use egui::{pos2, vec2, Color32, Rect, Stroke};
use klipp_core::{AppCommand, AudioParam, VideoParam, Keyframe};
use klipp_timeline::TrackKind;

/// Which parameter the curve editor displays.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveParam {
    Audio(AudioParam),
    Video(VideoParam),
    /// Playback speed envelope.
    Speed,
    /// Video effect parameter keyframe track, identified by
    /// "{effect_index}:{param_name}".
    Effect(String),
    /// Audio effect parameter keyframe track, identified by
    /// "{effect_index}:{param_name}". Reserved param names: "__bypass"
    /// and "__wetdry".
    AudioEffect(String),
}

impl CurveParam {
    pub fn label(&self) -> String {
        match self {
            CurveParam::Audio(AudioParam::Volume) => "Volume".into(),
            CurveParam::Audio(AudioParam::Pan) => "Pan".into(),
            CurveParam::Audio(AudioParam::Gain) => "Gain".into(),
            CurveParam::Video(VideoParam::Opacity) => "Opacity".into(),
            CurveParam::Video(VideoParam::PositionX) => "Position X".into(),
            CurveParam::Video(VideoParam::PositionY) => "Position Y".into(),
            CurveParam::Video(VideoParam::Scale) => "Scale".into(),
            CurveParam::Video(VideoParam::Rotation) => "Rotation".into(),
            CurveParam::Video(VideoParam::MatteFeather) => "Matte Feather".into(),
            CurveParam::Video(VideoParam::MatteChoke) => "Matte Choke".into(),
            CurveParam::Speed => "Speed".into(),
            CurveParam::Effect(key) => {
                if let Some((_idx, name)) = key.split_once(':') {
                    format!("FX: {name}")
                } else {
                    format!("FX: {key}")
                }
            }
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
        }
    }

    /// (min, max, default) for this parameter.
    fn range(&self) -> (f32, f32, f32) {
        match self {
            CurveParam::Audio(AudioParam::Volume) => (0.0, 2.0, 1.0),
            CurveParam::Audio(AudioParam::Pan) => (-1.0, 1.0, 0.0),
            CurveParam::Audio(AudioParam::Gain) => (0.0, 4.0, 1.0),
            CurveParam::Video(VideoParam::Opacity) => (0.0, 1.0, 1.0),
            CurveParam::Video(VideoParam::PositionX) => (-1920.0, 1920.0, 0.0),
            CurveParam::Video(VideoParam::PositionY) => (-1080.0, 1080.0, 0.0),
            CurveParam::Video(VideoParam::Scale) => (0.0, 4.0, 1.0),
            CurveParam::Video(VideoParam::Rotation) => (-360.0, 360.0, 0.0),
            CurveParam::Video(VideoParam::MatteFeather) => (0.0, 50.0, 0.0),
            CurveParam::Video(VideoParam::MatteChoke) => (0.0, 50.0, 0.0),
            CurveParam::Speed => (0.1, 4.0, 1.0),
            CurveParam::Effect(_) => (0.0, 1.0, 0.5),
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
        }
    }
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
    /// Currently active parameter
    param: CurveParam,
    /// Active keyframe drag
    drag: Option<CurveDrag>,
    /// Auto-fit on first display
    needs_fit: bool,
}

impl Default for CurveEditorPanel {
    fn default() -> Self {
        Self {
            zoom_x: 200.0,
            zoom_y: 1.0,
            scroll: egui::Vec2::ZERO,
            param: CurveParam::Video(VideoParam::Opacity),
            drag: None,
            needs_fit: true,
        }
    }
}

impl CurveEditorPanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

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

        // Find selected clip and its track kind
        let clip_info = self.find_selected_clip(state);

        // Header content
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(8.0);

                // Label: "Curve Editor"
                ui.label(
                    egui::RichText::new("Curve Editor")
                        .size(11.0)
                        .color(theme.text_secondary),
                );

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Clip name
                if let Some((_, ref name, _)) = clip_info {
                    ui.label(
                        egui::RichText::new(name.as_str())
                            .size(11.0)
                            .color(theme.text_primary),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("No clip selected")
                            .size(11.0)
                            .color(theme.text_dim),
                    );
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Parameter selector
                self.param_selector(ui, theme, state);

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Zoom controls
                if ui.small_button(
                    egui::RichText::new("Fit").size(10.0).color(theme.text_secondary),
                ).clicked() {
                    self.fit_to_content(
                        &graph_rect,
                        clip_info.as_ref().map(|(dur, _, _)| *dur).unwrap_or(1.0),
                    );
                }
            });
        });

        if graph_rect.width() < 20.0 || graph_rect.height() < 20.0 {
            return cmds;
        }

        // Get clip data
        let Some((clip_duration, _clip_name, track_kind)) = clip_info else {
            // Draw empty state
            ui.painter().text(
                graph_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Select a clip to view its keyframe curves",
                egui::FontId::proportional(12.0),
                theme.text_dim,
            );
            return cmds;
        };

        let clip_id = state.selected_clip.unwrap();
        let (v_min, v_max, v_default) = self.param.range();
        let v_range = v_max - v_min;

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

        // ── Draw curve ──
        self.draw_curve(
            ui, &graph_rect, &keyframes, clip_duration, v_min, v_range, v_default, theme,
        );

        // ── Draw keyframe diamonds + handles ──
        let diamond_cmds = self.draw_keyframes_and_interact(
            ui, state, &graph_rect, &graph_response, &keyframes,
            clip_id, clip_duration, v_min, v_range, v_default, track_kind, theme,
        );
        cmds.extend(diamond_cmds);

        // ── Double-click to add keyframe ──
        if graph_response.double_clicked() {
            if let Some(pos) = graph_response.interact_pointer_pos() {
                if graph_rect.contains(pos) {
                    let t = self.x_to_time(pos.x, &graph_rect, clip_duration);
                    let v = self.y_to_value(pos.y, &graph_rect, v_min, v_range);
                    let t = t.clamp(0.0, clip_duration);
                    let v = v.clamp(v_min, v_max);
                    match &self.param {
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
                        CurveParam::Effect(key) => {
                            if let Some((idx_str, param_name)) = key.split_once(':') {
                                if let Ok(effect_index) = idx_str.parse::<usize>() {
                                    cmds.push(AppCommand::AddEffectKeyframe {
                                        clip_id,
                                        effect_index,
                                        param_name: param_name.to_string(),
                                        time_secs: t,
                                        value: v,
                                    });
                                }
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
            self.param.label(),
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

    /// Sync the curve param from the rubber band params in state.
    pub fn sync_param_from_state(&mut self, state: &AppState) {
        if let Some(clip_id) = state.selected_clip {
            let seq = &state.sequences[state.active_sequence_idx];
            if let Some((ti, _ci)) = seq.find_clip(clip_id) {
                let kind = seq.tracks[ti].kind;
                match kind {
                    TrackKind::Video => {
                        self.param = CurveParam::Video(state.video_rubber_band_param);
                    }
                    TrackKind::Audio => {
                        // Curve editor only mirrors clip-level rubber band
                        // params (Volume/Pan/Gain). When the rubber band is
                        // currently showing an effect param, the curve editor
                        // already supports CurveParam::AudioEffect and that
                        // path gets selected by the user via its own picker
                        // — no auto-mirror needed here.
                        if let Some(p) = state.rubber_band_param.as_clip_param() {
                            self.param = CurveParam::Audio(p);
                        }
                    }
                    _ => {}
                }
            }
        }
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

    fn get_keyframes(&self, state: &AppState, clip_id: klipp_core::ClipId) -> Vec<Keyframe> {
        let seq = &state.sequences[state.active_sequence_idx];
        for track in &seq.tracks {
            if let Some(clip) = track.find_clip(clip_id) {
                return match &self.param {
                    CurveParam::Audio(AudioParam::Volume) => clip.volume_keyframes.clone(),
                    CurveParam::Audio(AudioParam::Pan) => clip.pan_keyframes.clone(),
                    CurveParam::Audio(AudioParam::Gain) => clip.gain_keyframes.clone(),
                    CurveParam::Video(VideoParam::Opacity) => clip.opacity_keyframes.clone(),
                    CurveParam::Video(VideoParam::PositionX) => clip.transform_pos_x_keyframes.clone(),
                    CurveParam::Video(VideoParam::PositionY) => clip.transform_pos_y_keyframes.clone(),
                    CurveParam::Video(VideoParam::Scale) => clip.transform_scale_keyframes.clone(),
                    CurveParam::Video(VideoParam::Rotation) => clip.transform_rotation_keyframes.clone(),
                    CurveParam::Video(VideoParam::MatteFeather) => clip.matte_feather_keyframes.clone(),
                    CurveParam::Video(VideoParam::MatteChoke) => clip.matte_choke_keyframes.clone(),
                    CurveParam::Speed => clip.speed_keyframes.clone(),
                    CurveParam::Effect(key) => clip.effect_keyframes.get(key).cloned().unwrap_or_default(),
                    CurveParam::AudioEffect(key) => clip.audio_effect_keyframes.get(key).cloned().unwrap_or_default(),
                };
            }
        }
        Vec::new()
    }

    fn param_selector(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
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
                let mut effect_keys: Vec<String> = clip.effect_keyframes.keys().cloned().collect();
                effect_keys.sort();
                for key in effect_keys {
                    params.push(CurveParam::Effect(key));
                }
                let mut audio_effect_keys: Vec<String> =
                    clip.audio_effect_keyframes.keys().cloned().collect();
                audio_effect_keys.sort();
                for key in audio_effect_keys {
                    params.push(CurveParam::AudioEffect(key));
                }
            }
        }

        for p in &params {
            let active = self.param == *p;
            let text = egui::RichText::new(p.label()).size(10.0);
            let text = if active {
                text.color(theme.text_accent)
            } else {
                text.color(theme.text_dim)
            };
            if ui.selectable_label(active, text).clicked() {
                self.param = p.clone();
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
        clip_id: klipp_core::ClipId,
        clip_duration: f64,
        theme: &KlippTheme,
    ) {
        let seq = &state.sequences[state.active_sequence_idx];
        let Some((ti, ci)) = seq.find_clip(clip_id) else { return };
        let clip = &seq.tracks[ti].clips[ci];
        let clip_start_secs = clip.timeline_in.to_secs_f64();
        let playhead_secs = state.playhead_smooth_secs;
        let clip_relative = playhead_secs - clip_start_secs;

        if clip_relative >= 0.0 && clip_relative <= clip_duration {
            let x = self.time_to_x(clip_relative, graph_rect, clip_duration);
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
            let v = klipp_core::keyframe::evaluate(keyframes, t, v_default);
            let x = self.time_to_x(t, graph_rect, clip_duration);
            let y = self.value_to_y(v, graph_rect, v_min, v_range);
            points.push(pos2(x, y));
        }

        // Draw segments (clipped to graph)
        for w in points.windows(2) {
            let a = w[0];
            let b = w[1];
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
        let diamond_size = 5.0;
        let handle_radius = 3.5;
        let (p_v_min, p_v_max, _) = self.param.range();

        // Draw tangent handles for selected keyframe
        let selected_kf_idx = state.selected_keyframe.as_ref().and_then(|sk| {
            if sk.clip_id == clip_id && self.param_matches_kind(&sk.param_kind) {
                Some(sk.keyframe_index)
            } else {
                None
            }
        });

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
            let is_selected = Some(i) == selected_kf_idx;
            let diamond_color = if is_selected {
                theme.text_primary
            } else {
                theme.accent
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
                if let Some(idx) = clicked_kf {
                    state.selected_keyframe = Some(SelectedKeyframe {
                        clip_id,
                        param_kind: self.param_to_kind(),
                        keyframe_index: idx,
                    });
                } else {
                    state.selected_keyframe = None;
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
                        // Check in-handle
                        let hx = self.time_to_x(kf.time_secs + kf.tangent_in.0, graph_rect, clip_duration);
                        let hy = self.value_to_y(kf.value + kf.tangent_in.1, graph_rect, v_min, v_range);
                        if ((pos.x - hx).powi(2) + (pos.y - hy).powi(2)).sqrt() < 8.0 {
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
                            if ((pos.x - hx).powi(2) + (pos.y - hy).powi(2)).sqrt() < 8.0 {
                                self.drag = Some(CurveDrag {
                                    keyframe_index: sel_idx,
                                    handle: Some(true), // out
                                });
                                found_handle = true;
                            }
                        }
                    }
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
                                self.drag = Some(CurveDrag {
                                    keyframe_index: i,
                                    handle: None,
                                });
                                state.selected_keyframe = Some(SelectedKeyframe {
                                    clip_id,
                                    param_kind: self.param_to_kind(),
                                    keyframe_index: i,
                                });
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Process drag
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
                                match &self.param {
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
                                    CurveParam::Speed
                                    | CurveParam::Effect(_)
                                    | CurveParam::AudioEffect(_) => {
                                        // Speed/effect keyframe move not yet wired to commands
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
                                match &self.param {
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
                                    CurveParam::Speed
                                    | CurveParam::Effect(_)
                                    | CurveParam::AudioEffect(_) => {
                                        // Tangent editing not yet wired for speed/effects
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if response.drag_stopped() {
            self.drag = None;
        }

        cmds
    }

    fn param_to_kind(&self) -> KeyframeParamKind {
        match &self.param {
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
            CurveParam::Speed | CurveParam::Effect(_) => {
                KeyframeParamKind::Video(VideoParam::Opacity) // fallback
            }
        }
    }

    fn param_matches_kind(&self, kind: &KeyframeParamKind) -> bool {
        match (&self.param, kind) {
            (CurveParam::Audio(a), KeyframeParamKind::Audio(b)) => a == b,
            (CurveParam::Video(a), KeyframeParamKind::Video(b)) => a == b,
            (CurveParam::AudioEffect(key), KeyframeParamKind::AudioEffect { effect_index, param_name }) => {
                key == &format!("{effect_index}:{param_name}")
            }
            _ => false,
        }
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
