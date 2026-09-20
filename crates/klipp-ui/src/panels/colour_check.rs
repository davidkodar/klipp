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
}

impl Default for ColourCheckPanel {
    fn default() -> Self {
        Self { measured: None, generation: u64::MAX, at: None, selfcheck: None, show_all: false }
    }
}

/// The look this frame is judged against: the brief's when it names
/// one, a neutral broadcast target when it does not. The same rule the
/// assistant's `look_for` uses, minus the screen-recording case, which
/// needs the beat sheet.
fn look_for(state: &AppState) -> LookTarget {
    state
        .sequence()
        .brief
        .as_ref()
        .and_then(|b| b.look.clone())
        .filter(|l| l.mid_l.is_some() || l.cast_a.is_some() || l.screencast)
        .or_else(|| LookTarget::preset("broadcast_neutral"))
        .unwrap_or_else(|| LookTarget { name: "neutral".into(), ..Default::default() })
}

impl ColourCheckPanel {
    /// Measure the displayed frame, at most a few times a second and
    /// only when it has actually changed.
    fn refresh(&mut self, frame_buffer: &SharedFrameBuffer, faces: &[[f32; 4]]) {
        let due = self.at.is_none_or(|t| t.elapsed().as_millis() >= 400);
        if !due {
            return;
        }
        let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() || fb.generation == self.generation {
            return;
        }
        self.measured = Some(measure::measure_frame(&fb.data, fb.width, fb.height, faces));
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
        self.refresh(frame_buffer, &faces);
        let look = look_for(state);
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
                    self.chain(ui, theme, state);
                    ui.add_space(4.0);
                    action = self.render_path(ui, theme);
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
        let chain = clip.node_graph.chain();
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
        for fault in clip.node_graph.chain_out_of_order() {
            ui.label(egui::RichText::new(fault).small().color(theme.marker_warning));
        }
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
fn faces_at_playhead(state: &AppState) -> Vec<[f32; 4]> {
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
    let source = clip.source_in.to_secs_f64() + (tc.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0) * clip.speed.max(0.001);
    let Some(framing) = klipp_analysis::report::MediaAnalysis::load(&item.path).framing else {
        return Vec::new();
    };
    let Some(sample) = framing.people_at(source, 4.0) else {
        return Vec::new();
    };
    sample
        .faces
        .iter()
        .map(|f| measure::skin_band(&f.landmarks).unwrap_or([f.x + f.w * 0.12, f.y + f.h * 0.15, f.w * 0.76, f.h * 0.72]))
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
    fn run(panel: &mut ColourCheckPanel, state: &AppState, fb: &SharedFrameBuffer) -> ColourAction {
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
        assert_eq!(look_for(&state).name, "broadcast_neutral", "no brief, no look: a neutral target");
        let mut brief = klipp_timeline::Brief::default();
        brief.look = LookTarget::from_measures(
            "like the reference",
            &[measure::measure_frame(&[128, 120, 110, 255].repeat(64 * 64), 64, 64, &[])],
        );
        state.sequence_mut().brief = Some(brief);
        assert_eq!(look_for(&state).name, "like the reference", "the brief's look leads");
    }

    #[test]
    fn a_look_with_no_numbers_in_it_does_not_replace_the_neutral_target() {
        // A brief can carry a look that names nothing — a preset name
        // that never resolved. Judging against it would pass every
        // frame silently, which is worse than judging against neutral.
        let mut state = AppState::default();
        let mut brief = klipp_timeline::Brief::default();
        brief.look = Some(LookTarget { name: "whatever".into(), ..Default::default() });
        state.sequence_mut().brief = Some(brief);
        assert_eq!(look_for(&state).name, "broadcast_neutral");
    }
}
