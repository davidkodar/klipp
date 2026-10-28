//! The Colour Check window: what a colourist would read off this frame,
//! in numbers, with the fault named and the knob to turn.
//!
//! Klipp measures colour the way a grading suite does — CIELAB
//! readings, skin against a band derived from photographed faces, a
//! noise floor, banding, gamut, the chain's order — and until now all
//! of it was reachable only by an assistant over the control socket. A
//! person editing in the app saw scopes, which show *what* the picture
//! is doing, and had to know the craft to say whether that was wrong.
//! This window says whether it is wrong, and what to do.
//!
//! Everything here is a reading of the frame the viewer is showing, so
//! it costs one measurement of one already-decoded frame. That is a few
//! milliseconds, and it is throttled and gated on the frame buffer's
//! generation so a paused viewer measures once and not sixty times a
//! second.

use klipp_timeline::GradeAccess;
use std::time::Instant;

use klipp_color::measure::{self, FrameMeasure, LookTarget, Verdict};
use klipp_engine::SharedFrameBuffer;

use crate::state::AppState;
use crate::theme::KlippTheme;

/// What the person asked of the colour check this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColourAction {
    None,
    /// Send a known chart through the render path and read it back.
    SelfCheck,
    /// The measured colour tools (#897), each one undo step, run by the
    /// same code an assistant's request runs.
    /// Match the selected shots (or the picture track's) to their
    /// scene's hero.
    MatchToHero,
    /// Turn the selected clip's faces onto the skin band.
    BalanceSkin,
    /// Grade the selected clip to the brief's look.
    GradeToLook,
    /// Make the timeline's best-graded shot the look the rest are held to.
    AdoptLookFromTimeline,
    /// Make the selected media pool file the look's reference.
    AdoptLookFromMedia,
    /// A starting grade for the selected clip, from the inspector's
    /// Colour tab: the `auto_colour` op an assistant sends.
    AutoColour,
}

impl ColourAction {
    /// What the tool is called while it runs and in its result.
    pub fn name(self) -> &'static str {
        match self {
            ColourAction::None => "",
            ColourAction::SelfCheck => "Check the render path",
            ColourAction::MatchToHero => "Match to Hero",
            ColourAction::BalanceSkin => "Balance Skin",
            ColourAction::GradeToLook => "Grade to Look",
            ColourAction::AdoptLookFromTimeline | ColourAction::AdoptLookFromMedia => "Adopt Look",
            ColourAction::AutoColour => "Auto Colour",
        }
    }
}

/// One reading kept for the trend: small, because there is one per
/// quarter-second of anything ever watched.
#[derive(Debug, Clone, Copy)]
pub struct TrendPoint {
    pub l: f32,
    pub cast_a: f32,
    pub cast_b: f32,
    /// The face's hue where the frame had a readable one, and how far
    /// outside the skin band it sat.
    pub skin_hue: Option<f32>,
    pub skin_off: f32,
    /// Whether anything in the frame failed against the look.
    pub fails: bool,
}

/// How the colour of a piece moves over its length.
///
/// Every scope in every grading suite shows one frame. That is the
/// right instrument for setting a shot and the wrong one for finding
/// the two places in a twenty-minute piece where the light drifted, or
/// the one cut where the join shows — for that you want the whole
/// timeline at once, which is what a spectrogram is for sound and what
/// nothing here was for picture.
///
/// It fills in as the piece is watched, from readings the window is
/// taking anyway, which is the same way the audio spectrogram fills
/// from the mix ring. Play the timeline once and the strip is complete;
/// scrub and the parts scrubbed over fill in. Nothing extra is decoded
/// and nothing is rendered twice.
#[derive(Default)]
pub struct ColourTrend {
    /// Quarter-second bucket index → the reading taken there. Bucketed
    /// so watching a passage twice does not grow the strip, and so the
    /// memory is bounded by the piece's length rather than by how long
    /// it has been open.
    points: std::collections::BTreeMap<u64, TrendPoint>,
}

/// Readings a quarter-second apart: fine enough to show a light
/// changing during a shot, coarse enough that an hour-long timeline
/// costs a few thousand points.
pub const TREND_BUCKET_SECS: f64 = 0.25;

impl ColourTrend {
    fn bucket(secs: f64) -> u64 {
        (secs.max(0.0) / TREND_BUCKET_SECS).floor() as u64
    }

    pub fn record(&mut self, secs: f64, m: &FrameMeasure, look: &LookTarget) {
        let face = m.faces.iter().find(|f| f.readable());
        let fails = measure::judge(m, look).iter().any(|f| f.verdict == Verdict::Fail);
        self.points.insert(
            Self::bucket(secs),
            TrendPoint {
                l: m.l_p50,
                cast_a: m.cast_a,
                cast_b: m.cast_b,
                skin_hue: face.map(|f| f.hue),
                skin_off: face.map(|f| f.hue_off_band_wb).unwrap_or(0.0),
                fails,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn at(&self, secs: f64) -> Option<&TrendPoint> {
        self.points.get(&Self::bucket(secs))
    }

    /// How much of `duration` has been read at all, 0 to 1.
    pub fn coverage(&self, duration: f64) -> f32 {
        if duration <= 0.0 {
            return 0.0;
        }
        let total = (duration / TREND_BUCKET_SECS).ceil().max(1.0);
        (self.points.len() as f64 / total).min(1.0) as f32
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }
}

/// What the window keeps between frames: the last measurement, and
/// enough to know when it is stale.
pub struct ColourCheckPanel {
    measured: Option<FrameMeasure>,
    /// The frame buffer generation the measurement was taken from.
    generation: u64,
    at: Option<Instant>,
    /// The last render-path self-check, and when it was run.
    pub selfcheck: Option<(bool, String)>,
    show_all: bool,
    /// How the colour moves over the piece, filled in as it is watched.
    pub trend: ColourTrend,
    /// Whether the app can run the colour tools in this process.
    pub tools_available: bool,
    /// The colour tool running now, if one is.
    pub tool_running: Option<ColourAction>,
    /// The last colour tool's answer: whether it did what it set out to,
    /// and its readings before and after.
    pub tool_result: Option<(bool, String)>,
}

impl Default for ColourCheckPanel {
    fn default() -> Self {
        Self { measured: None, generation: u64::MAX, at: None, selfcheck: None, show_all: false, trend: ColourTrend::default(), tools_available: false, tool_running: None, tool_result: None }
    }
}

/// The look this frame is judged against: the brief's when it names
/// one, and without a brief only what every scene must get right. The same rule the
/// assistant's `look_for` uses, minus the screen-recording case, which
/// needs the beat sheet.
fn look_for(state: &AppState) -> LookTarget {
    state
        .sequence()
        .brief
        .as_ref()
        .and_then(|b| b.look.clone())
        .filter(|l| l.mid_l.is_some() || l.cast_a.is_some() || l.screencast)
        .or_else(|| LookTarget::preset("unbriefed"))
        .unwrap_or_else(|| LookTarget { name: "neutral".into(), ..Default::default() })
}

impl ColourCheckPanel {
    /// Measure the displayed frame, at most a few times a second and
    /// only when it has actually changed, and keep the reading in the
    /// trend so watching the piece draws the strip.
    fn refresh(&mut self, frame_buffer: &SharedFrameBuffer, faces: &[[f32; 4]], at_secs: f64, look: &LookTarget, prior: measure::LogPrior) {
        let due = self.at.is_none_or(|t| t.elapsed().as_millis() >= 400);
        if !due {
            return;
        }
        let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() || fb.generation == self.generation {
            return;
        }
        let mut m = measure::measure_frame(&fb.data, fb.width, fb.height, faces);
        m.log_prior = prior;
        self.trend.record(at_secs, &m, look);
        self.measured = Some(m);
        self.generation = fb.generation;
        self.at = Some(Instant::now());
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &AppState,
        frame_buffer: &SharedFrameBuffer,
        theme: &KlippTheme,
        open: &mut bool,
    ) -> ColourAction {
        let mut action = ColourAction::None;
        // The faces the analysis already found for this clip, so the
        // skin reading is the cheek band and not the whole head.
        let faces = faces_at_playhead(state);
        let look = look_for(state);
        let at_secs = state.playhead.to_secs_f64();
        self.refresh(frame_buffer, &faces, at_secs, &look, log_prior_at_playhead(state));
        egui::Window::new("Colour Check")
            .open(open)
            .default_size([420.0, 520.0])
            .min_width(300.0)
            .min_height(200.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let Some(m) = self.measured.clone() else {
                    ui.label(egui::RichText::new("Nothing on screen to read yet.").color(theme.text_secondary));
                    ui.label(egui::RichText::new("Put the playhead over a picture clip; the reading follows the viewer.").small().color(theme.text_secondary));
                    return;
                };
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.readings(ui, theme, &m);
                    ui.add_space(4.0);
                    self.faces(ui, theme, &m);
                    ui.add_space(4.0);
                    self.findings(ui, theme, &m, &look);
                    ui.add_space(4.0);
                    let acted = self.act(ui, theme, state);
                    if acted != ColourAction::None {
                        action = acted;
                    }
                    ui.add_space(4.0);
                    self.chain(ui, theme, state);
                    ui.add_space(4.0);
                    self.trend_strip(ui, theme, state);
                    ui.add_space(4.0);
                    let checked = self.render_path(ui, theme);
                    if checked != ColourAction::None {
                        action = checked;
                    }
                });
            });
        action
    }

    fn readings(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, m: &FrameMeasure) {
        section(ui, theme, "THIS FRAME");
        let cast = if m.cast() < 3.0 {
            "neutral".to_string()
        } else {
            format!("{:.1} toward {}", m.cast(), measure::cast_name(m.cast_a, m.cast_b))
        };
        row(ui, theme, "Lightness", &format!("black {:.0}  mid {:.0}  white {:.0}  L*", m.l_p1, m.l_p50, m.l_p99));
        row(ui, theme, "Contrast", &format!("{:.0} over the 5–95 range", m.contrast_range));
        row(ui, theme, "Colour", &format!("chroma {:.0}", m.chroma_mean));
        row(ui, theme, "Neutrals", &if m.neutral_solid { cast } else { format!("{cast} — nothing in the frame is truly neutral, so this is the least-coloured part of it and may be a backdrop") });
        if m.clip_hi_pct > 0.05 || m.clip_lo_pct > 0.05 {
            row(ui, theme, "Clipping", &format!("{:.1}% at white, {:.1}% at black", m.clip_hi_pct, m.clip_lo_pct));
        }
        if self.show_all || m.noise_l > 0.5 || m.banding_pct > 5.0 || m.gamut_clip_pct > 0.5 {
            row(ui, theme, "Noise floor", &format!("{:.2} L* in the flattest parts", m.noise_l));
            row(ui, theme, "Banding", &format!("{:.0}% of the flat picture steps", m.banding_pct));
            row(ui, theme, "Out of gamut", &format!("{:.1}% pinned in one channel", m.gamut_clip_pct));
        }
        if measure::looks_like_log(m) {
            ui.label(
                egui::RichText::new("This looks like camera log with no input transform. Set the clip's log format before grading: flat and colourless is what the format is for, and correcting it with contrast and saturation throws away the latitude it exists to keep.")
                    .small()
                    .color(theme.marker_warning),
            );
        }
        ui.checkbox(&mut self.show_all, "Always show noise, banding and gamut");
    }

    fn faces(&self, ui: &mut egui::Ui, theme: &KlippTheme, m: &FrameMeasure) {
        section(ui, theme, "SKIN");
        if m.faces.is_empty() {
            match &m.skin_all {
                Some(s) => row(ui, theme, "No face found", &format!("skin-coloured pixels sit at {:.0}°, {:.0}% of the frame", s.hue, s.pct)),
                None => {
                    ui.label(egui::RichText::new("No face and no skin in this frame.").small().color(theme.text_secondary));
                }
            }
            return;
        }
        for (n, f) in m.faces.iter().enumerate() {
            if !f.readable() {
                row(ui, theme, &format!("Face {}", n + 1), &format!("too little skin to read, {} of {}² patches", f.patches, f.grid));
                continue;
            }
            let off = if f.hue_off_band > 0.0 {
                format!("{:.0}° outside", f.hue_off_band)
            } else {
                "on the band".to_string()
            };
            let colour = if f.hue_off_band > 6.0 {
                theme.marker_error
            } else if f.hue_off_band > 0.0 {
                theme.marker_warning
            } else {
                theme.marker_success
            };
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("Face {}", n + 1)).color(theme.text_secondary).size(crate::theme::type_scale::BODY));
                ui.label(
                    egui::RichText::new(format!("hue {:.0}° ({off})  chroma {:.0}  L* {:.0}", f.hue, f.chroma, f.median.l))
                        .color(colour)
                        .size(crate::theme::type_scale::BODY),
                );
            });
            if f.mixed_light {
                ui.label(
                    egui::RichText::new(format!("Two colours of light on one face (spread {:.0}°). A skin correction fixes one side and breaks the other; this is a lighting fault, not a grading one.", f.hue_iqr))
                        .small()
                        .color(theme.marker_warning),
                );
            }
        }
        ui.label(
            egui::RichText::new(format!(
                "Skin sits between {:.0}° and {:.0}°, aiming at {:.0}°. Those figures come from a photographed X-Rite ColorChecker and twelve portraits across skin tones, not from the idealised values — a camera renders the chart's skin patches several degrees off where the published numbers put them.",
                measure::SKIN_HUE_LOW,
                measure::SKIN_HUE_HIGH,
                measure::SKIN_HUE_CENTRE
            ))
            .small()
            .color(theme.text_dim),
        );
    }

    fn findings(&self, ui: &mut egui::Ui, theme: &KlippTheme, m: &FrameMeasure, look: &LookTarget) {
        section(ui, theme, &format!("AGAINST THE LOOK — {}", look.name.to_uppercase()));
        let found: Vec<_> = measure::judge(m, look).into_iter().filter(|f| f.verdict != Verdict::Pass).collect();
        if found.is_empty() {
            ui.label(egui::RichText::new("Nothing to fix in this frame.").color(theme.marker_success));
            return;
        }
        for f in found {
            let colour = if f.verdict == Verdict::Fail { theme.marker_error } else { theme.marker_warning };
            ui.label(egui::RichText::new(&f.what).color(colour).size(crate::theme::type_scale::BODY));
            ui.label(egui::RichText::new(f.fix).small().color(theme.text_secondary));
        }
    }

    fn chain(&self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
        let Some((_, clip)) = state.selected_clip_loc() else {
            return;
        };
        section(ui, theme, "THE GRADE'S ORDER");
        let chain = clip.grade().chain();
        let shown: Vec<String> = chain
            .iter()
            .filter(|l| !matches!(l.stage, klipp_nodes::GradeStage::Plumbing))
            .map(|l| format!("{} [{}]{}", l.label, l.stage.label(), if l.gated { ", gated" } else { "" }))
            .collect();
        ui.label(
            egui::RichText::new(if shown.is_empty() { "No grade on the selected clip.".to_string() } else { shown.join("  →  ") })
                .small()
                .color(theme.text_secondary),
        );
        for fault in clip.grade().chain_out_of_order() {
            ui.label(egui::RichText::new(fault).small().color(theme.marker_warning));
        }
    }

    /// The strip: the whole piece's colour along one axis.
    ///
    /// Three lanes, because three things go wrong and they go wrong
    /// differently. Exposure as a line, so a drift reads as a slope and
    /// a bad join as a step. The cast painted in its own colour, so a
    /// shot lit differently from its neighbours is a different colour
    /// of band rather than a number to compare. Skin as a mark per
    /// reading, green on the band and amber off it, because that is the
    /// fault a viewer names without being able to say why.
    fn trend_strip(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
        let seq = state.sequence();
        let duration = seq.total_duration().to_secs_f64();
        section(ui, theme, "OVER THE WHOLE PIECE");
        if duration <= 0.0 {
            ui.label(egui::RichText::new("Nothing on the timeline yet.").small().color(theme.text_secondary));
            return;
        }
        let covered = self.trend.coverage(duration);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(if covered >= 0.98 {
                    "Read end to end.".to_string()
                } else {
                    format!("{:.0}% read — play or scrub the rest to fill it in.", covered * 100.0)
                })
                .small()
                .color(theme.text_secondary),
            );
            if ui.add(egui::Button::new("Clear").small()).on_hover_text("Forget the readings taken so far and start the strip again. Do this after a regrade.").clicked() {
                self.trend.clear();
            }
        });
        const LANE: f32 = 22.0;
        let width = ui.available_width().max(80.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, LANE * 3.0 + 8.0), egui::Sense::hover());
        let p = ui.painter_at(rect);
        let x_of = |secs: f64| rect.left() + (secs / duration).clamp(0.0, 1.0) as f32 * rect.width();
        let lane = |i: usize| {
            egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.top() + i as f32 * (LANE + 2.0)),
                egui::vec2(rect.width(), LANE),
            )
        };
        for i in 0..3 {
            p.rect_filled(lane(i), crate::theme::radius::CONTROL, theme.bg_deep);
        }
        // The cuts, so a step in the line can be read as a join rather
        // than as a drift.
        if let Some(track) = seq.tracks.iter().find(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible) {
            for c in &track.clips {
                let x = x_of(c.timeline_in.to_secs_f64());
                p.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0_f32, theme.border));
            }
        }
        let bucket_w = (rect.width() * (TREND_BUCKET_SECS / duration) as f32).max(1.0);
        let mut last: Option<egui::Pos2> = None;
        for (bucket, pt) in self.trend.points.iter() {
            let secs = *bucket as f64 * TREND_BUCKET_SECS;
            let x = x_of(secs);
            // Exposure, 0–100 L* bottom to top of its lane.
            let l = lane(0);
            let y = l.bottom() - (pt.l / 100.0).clamp(0.0, 1.0) * l.height();
            let here = egui::pos2(x, y);
            if let Some(prev) = last {
                if (here.x - prev.x).abs() <= bucket_w * 2.5 {
                    p.line_segment([prev, here], egui::Stroke::new(1.5_f32, if pt.fails { theme.marker_warning } else { theme.accent }));
                }
            }
            p.circle_filled(here, 1.2, if pt.fails { theme.marker_warning } else { theme.accent });
            last = Some(here);
            // The cast, painted as the colour it is.
            let c = lane(1);
            let rgb = measure::lab_to_srgb8(measure::Lab { l: 60.0, a: pt.cast_a * 3.0, b: pt.cast_b * 3.0 });
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, c.top()), egui::vec2(bucket_w, c.height())),
                0.0,
                egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
            );
            // Skin: a mark where a face was read, coloured by the band.
            let s = lane(2);
            if pt.skin_hue.is_some() {
                let colour = if pt.skin_off > 6.0 {
                    theme.marker_error
                } else if pt.skin_off > 0.0 {
                    theme.marker_warning
                } else {
                    theme.marker_success
                };
                p.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, s.top() + 4.0), egui::vec2(bucket_w, s.height() - 8.0)),
                    0.0,
                    colour,
                );
            }
        }
        // Where the playhead is, so the reading above and the strip
        // below are obviously the same moment.
        let px = x_of(state.playhead.to_secs_f64());
        p.line_segment([egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())], egui::Stroke::new(1.0_f32, theme.playhead));
        ui.label(
            egui::RichText::new("Exposure, the cast in its own colour, and skin against its band. A slope is a drift inside a shot; a step at a cut is a join the viewer will see.")
                .small()
                .color(theme.text_dim),
        );
    }

    /// The tools that act on what the readings say (#897): the same
    /// measured grading an assistant runs, each one undo step, each
    /// answering with its readings before and after.
    fn act(&mut self, ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) -> ColourAction {
        let mut action = ColourAction::None;
        section(ui, theme, "ACT ON IT");
        if !self.tools_available {
            ui.label(egui::RichText::new("The colour tools are not available in this build.").small().color(theme.text_dim));
            return action;
        }
        let idle = self.tool_running.is_none();
        let clip = state.selected_clip.is_some();
        let picked = state.selected_clips.len();
        let media = state.selected_media_ids.len() == 1;
        let button = |ui: &mut egui::Ui, enabled: bool, label: &str, hover: &str| {
            ui.add_enabled(idle && enabled, egui::Button::new(label)).on_hover_text(hover).on_disabled_hover_text(hover).clicked()
        };
        ui.horizontal_wrapped(|ui| {
            let scope = if picked >= 2 { format!("the {picked} selected shots") } else { "every shot on the picture track".to_string() };
            if button(ui, true, "Match to Hero", &format!("Group {scope} into scenes by how they were lit, take the shot in each nearest the look as its hero, and move the others' primaries toward it, measuring after each round. One undo step.")) {
                action = ColourAction::MatchToHero;
            }
            if button(ui, clip, "Balance Skin", "Measure the faces in the selected clip and add a skin secondary that turns their hue onto the skin band, measured again after each round. Refused when the whole scene carries the cast: that is a white balance. One undo step.") {
                action = ColourAction::BalanceSkin;
            }
            if button(ui, clip, "Grade to Look", "Move the selected clip's primaries toward the brief's look in gentle rounds, measuring between them. A clip that looks like camera log with no input transform is left alone. One undo step.") {
                action = ColourAction::GradeToLook;
            }
        });
        ui.horizontal_wrapped(|ui| {
            if button(ui, true, "Adopt Look from Timeline", "Take the look of the timeline's best-graded shot and make it the target every colour reading and tool here is held to: it goes in the brief.") {
                action = ColourAction::AdoptLookFromTimeline;
            }
            if button(ui, media, "Adopt Look from Reference", "Take the look of the one file selected in the media pool, read across its length, and make it the target: it goes in the brief.") {
                action = ColourAction::AdoptLookFromMedia;
            }
        });
        if let Some(running) = self.tool_running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(format!("{}: measuring and grading\u{2026}", running.name())).small().color(theme.text_secondary));
            });
        } else if let Some((ok, text)) = &self.tool_result {
            ui.label(egui::RichText::new(text).small().color(if *ok { theme.text_primary } else { theme.marker_error }));
        }
        action
    }

    fn render_path(&mut self, ui: &mut egui::Ui, theme: &KlippTheme) -> ColourAction {
        let mut action = ColourAction::None;
        section(ui, theme, "THE RENDER PATH ITSELF");
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new("Check the render path"))
                .on_hover_text("Send a published 24-patch ColorChecker through the same renderer the viewer and the export use, and read it back. If the chart does not come back where it went in, every reading above is partly a measurement of Klipp rather than of your picture.")
                .clicked()
            {
                action = ColourAction::SelfCheck;
            }
        });
        if let Some((ok, text)) = &self.selfcheck {
            ui.label(
                egui::RichText::new(text)
                    .small()
                    .color(if *ok { theme.marker_success } else { theme.marker_error }),
            );
        }
        action
    }
}

/// The face regions the analysis found for the clip under the playhead,
/// as fractions of the frame. Empty when there is no analysis, and the
/// measurement then falls back to skin-coloured pixels.
pub(crate) fn faces_at_playhead(state: &AppState) -> Vec<[f32; 4]> {
    face_regions_at_playhead(state, false)
}

/// The whole of each face the analysis found under the playhead, with
/// the neck below it (a third of the face's height): what a white
/// balance must not take for the frame's surface. The cheek band that
/// meters skin leaves the forehead, jaw and neck in, and those are skin
/// too (review of #428).
pub(crate) fn whole_faces_at_playhead(state: &AppState) -> Vec<[f32; 4]> {
    face_regions_at_playhead(state, true)
}

/// What the file of the picture clip under the playhead says about
/// being camera log (#504).
fn log_prior_at_playhead(state: &AppState) -> measure::LogPrior {
    let seq = state.sequence();
    let tc = seq.last_playhead.unwrap_or_else(|| klipp_core::Timecode::from_secs_f64(0.0, seq.fps.rate()));
    seq.tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .find_map(|t| t.clip_at_timecode(tc))
        .and_then(|(_, clip)| clip.media_id().and_then(|m| state.media_pool.get(&m)))
        .map(|item| item.log_prior())
        .unwrap_or_default()
}

fn face_regions_at_playhead(state: &AppState, whole: bool) -> Vec<[f32; 4]> {
    let seq = state.sequence();
    let tc = seq.last_playhead.unwrap_or_else(|| klipp_core::Timecode::from_secs_f64(0.0, seq.fps.rate()));
    let Some((_, clip)) = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .find_map(|t| t.clip_at_timecode(tc))
    else {
        return Vec::new();
    };
    let Some(item) = clip.media_id().and_then(|m| state.media_pool.get(&m)) else {
        return Vec::new();
    };
    let source = clip.source_secs_at((tc.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0), 30.0);
    let Some(framing) = klipp_analysis::report::MediaAnalysis::load(&item.path).framing else {
        return Vec::new();
    };
    let Some(sample) = framing.people_at(source, 4.0) else {
        return Vec::new();
    };
    sample
        .faces
        .iter()
        .map(|f| {
            if whole {
                [f.x, f.y, f.w, (f.h * 4.0 / 3.0).min(1.0 - f.y)]
            } else {
                measure::skin_band(&f.landmarks).unwrap_or([f.x + f.w * 0.12, f.y + f.h * 0.15, f.w * 0.76, f.h * 0.72])
            }
        })
        .collect()
}

fn section(ui: &mut egui::Ui, theme: &KlippTheme, title: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(title).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
}

fn row(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(egui::RichText::new(label).color(theme.text_secondary).size(crate::theme::type_scale::BODY));
        ui.label(egui::RichText::new(value).color(theme.text_primary).size(crate::theme::type_scale::BODY));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::default());
        ctx
    }

    /// A frame buffer holding a flat sRGB colour, as the viewer would
    /// have left it.
    fn buffer(rgb: [u8; 3], w: u32, h: u32) -> SharedFrameBuffer {
        let mut fb = klipp_engine::FrameBuffer::new(w, h);
        fb.data = [rgb[0], rgb[1], rgb[2], 255].repeat((w * h) as usize);
        fb.width = w;
        fb.height = h;
        fb.generation = 1;
        Arc::new(Mutex::new(fb))
    }

    /// One frame of the window, with whatever is on screen.
    pub(super) fn run(panel: &mut ColourCheckPanel, state: &AppState, fb: &SharedFrameBuffer) -> ColourAction {
        let ctx = ctx();
        let theme = KlippTheme::default();
        let mut open = true;
        let mut action = ColourAction::None;
        let _ = ctx.run(Default::default(), |ctx| {
            action = panel.show(ctx, state, fb, &theme, &mut open);
        });
        action
    }

    #[test]
    fn the_window_reads_the_frame_the_viewer_is_showing() {
        let state = AppState::default();
        let fb = buffer([128, 120, 110], 160, 90);
        let mut panel = ColourCheckPanel::default();
        run(&mut panel, &state, &fb);
        let m = panel.measured.as_ref().expect("the frame on screen was measured");
        // A warm grey: the neutrals should read as such, and the
        // reading should be of *this* frame, not of nothing.
        assert!(m.l_p50 > 40.0 && m.l_p50 < 60.0, "mid grey reads mid: {}", m.l_p50);
        assert!(m.cast_b > 0.0, "a warm grey reads warm: b* {}", m.cast_b);
    }

    #[test]
    fn a_frame_that_has_not_changed_is_not_measured_twice() {
        // A paused viewer would otherwise re-measure sixty times a
        // second for a reading that cannot have moved.
        let state = AppState::default();
        let fb = buffer([128, 128, 128], 160, 90);
        let mut panel = ColourCheckPanel::default();
        run(&mut panel, &state, &fb);
        let first = panel.at.expect("measured once");
        run(&mut panel, &state, &fb);
        assert_eq!(panel.at, Some(first), "the same frame is not measured again");
        assert_eq!(panel.generation, 1);
    }

    #[test]
    fn an_empty_viewer_says_so_rather_than_drawing_nothing() {
        let state = AppState::default();
        let fb: SharedFrameBuffer = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(0, 0)));
        let mut panel = ColourCheckPanel::default();
        run(&mut panel, &state, &fb);
        assert!(panel.measured.is_none());
    }

    #[test]
    fn a_log_looking_frame_is_recognised_in_the_window() {
        // Flat, colourless, black off the floor and nothing near white:
        // the shape of camera log with no input transform. The window
        // has to say so, because grading it as a finished picture is
        // the fault this whole panel exists to prevent.
        let state = AppState::default();
        let fb = buffer([118, 120, 122], 160, 90);
        let mut panel = ColourCheckPanel::default();
        run(&mut panel, &state, &fb);
        let m = panel.measured.as_ref().expect("measured");
        assert!(measure::looks_like_log(m), "a flat colourless frame: {m:?}");
    }

    #[test]
    fn the_look_is_the_briefs_when_there_is_one_and_neutral_otherwise() {
        let mut state = AppState::default();
        assert_eq!(look_for(&state).name, "unbriefed", "no brief: only what every scene must get right");
        let brief = klipp_timeline::Brief {
            look: LookTarget::from_measures(
                "like the reference",
                &[measure::measure_frame(&[128, 120, 110, 255].repeat(64 * 64), 64, 64, &[])],
            ),
            ..Default::default()
        };
        state.sequence_mut().brief = Some(brief);
        assert_eq!(look_for(&state).name, "like the reference", "the brief's look leads");
    }

    #[test]
    fn a_look_with_no_numbers_in_it_does_not_replace_the_neutral_target() {
        // A brief can carry a look that names nothing — a preset name
        // that never resolved. Judging against it would pass every
        // frame silently, which is worse than judging against neutral.
        let mut state = AppState::default();
        let brief = klipp_timeline::Brief {
            look: Some(LookTarget { name: "whatever".into(), ..Default::default() }),
            ..Default::default()
        };
        state.sequence_mut().brief = Some(brief);
        assert_eq!(look_for(&state).name, "unbriefed");
    }
}

#[cfg(test)]
mod trend_tests {
    use super::*;

    fn flat_measure(l: u8) -> FrameMeasure {
        measure::measure_frame(&[l, l, l, 255].repeat(64 * 64), 64, 64, &[])
    }

    #[test]
    fn watching_a_passage_twice_does_not_grow_the_strip() {
        // The strip is bucketed by time, not appended to, so leaving
        // the window open through three playthroughs costs what one
        // costs. An unbounded list would be a leak that only shows up
        // on a long session.
        let look = LookTarget::preset("broadcast_neutral").unwrap();
        let mut t = ColourTrend::default();
        for pass in 0..3 {
            for i in 0..40 {
                let secs = i as f64 * 0.1 + pass as f64 * 0.001;
                t.record(secs, &flat_measure(128), &look);
            }
        }
        // Four seconds at a quarter-second apart.
        assert_eq!(t.len(), 16, "one point a bucket, however often it is watched");
    }

    #[test]
    fn a_reading_is_found_again_at_the_time_it_was_taken() {
        let look = LookTarget::preset("broadcast_neutral").unwrap();
        let mut t = ColourTrend::default();
        t.record(3.1, &flat_measure(200), &look);
        assert!(t.at(3.1).is_some());
        assert!(t.at(3.2).is_some(), "the same quarter-second bucket");
        assert!(t.at(3.6).is_none(), "the next one is still unread");
        assert!(t.at(3.0).is_some());
    }

    #[test]
    fn coverage_says_how_much_of_the_piece_has_been_read() {
        let look = LookTarget::preset("broadcast_neutral").unwrap();
        let mut t = ColourTrend::default();
        assert_eq!(t.coverage(10.0), 0.0);
        for i in 0..20 {
            t.record(i as f64 * TREND_BUCKET_SECS, &flat_measure(128), &look);
        }
        // Five seconds of a ten-second piece.
        assert!((t.coverage(10.0) - 0.5).abs() < 0.01, "{}", t.coverage(10.0));
        assert_eq!(t.coverage(0.0), 0.0, "an empty timeline is not divided by");
    }

    #[test]
    fn a_failing_frame_is_marked_in_the_strip() {
        // The line changes colour where the picture is out of
        // tolerance, so a bad passage is findable without reading it.
        let look = LookTarget::preset("broadcast_neutral").unwrap();
        let mut t = ColourTrend::default();
        t.record(0.0, &flat_measure(8), &look);
        assert!(t.at(0.0).expect("recorded").fails, "a near-black frame fails a neutral look");
        t.clear();
        assert!(t.is_empty(), "a regrade starts the strip again");
    }

    #[test]
    fn the_strip_draws_on_an_empty_timeline_without_panicking() {
        let state = AppState::default();
        let fb: SharedFrameBuffer = std::sync::Arc::new(std::sync::Mutex::new(klipp_engine::FrameBuffer::new(0, 0)));
        let mut panel = ColourCheckPanel::default();
        // No clips, no duration, no readings: the commonest state a
        // window is in the moment it is first opened.
        super::tests::run(&mut panel, &state, &fb);
        assert!(panel.trend.is_empty());
    }
}
