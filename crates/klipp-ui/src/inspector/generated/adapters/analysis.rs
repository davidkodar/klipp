//! The analysis-driven effects' adapters: Deflicker and Video
//! Stabilization measure the clip first, and an edit of the value the
//! measurement is smoothed by redoes only the smoothing.

use super::{Adapter, AdapterCtx};
use crate::inspector::effects_tab::{deflicker_status, stabilization_status, DEFLICKER_WINDOW_HINT};
use crate::inspector::generated::RowOutcome;
use klipp_core::AppCommand;

fn edited(outcome: RowOutcome) -> bool {
    matches!(outcome, RowOutcome::Edited | RowOutcome::Keyed | RowOutcome::Reset)
}

pub(super) struct Deflicker;

impl Adapter for Deflicker {
    fn before_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        deflicker_status(ui, ctx.theme, ctx.clip, ctx.state, ctx.cmds);
    }

    fn after_row(&self, ctx: &mut AdapterCtx, key: &str, outcome: RowOutcome) {
        // The measurement stands; only the smoothing over it is redone.
        if key == "window_secs" && edited(outcome) {
            ctx.cmds.push(AppCommand::RegainDeflicker { clip_id: ctx.clip.id });
        }
    }

    fn after_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        ui.label(egui::RichText::new(DEFLICKER_WINDOW_HINT).size(10.0).color(ctx.theme.text_dim));
    }
}

pub(super) struct Stabilization;

impl Adapter for Stabilization {
    fn before_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        stabilization_status(ui, ctx.theme, ctx.clip, ctx.state, ctx.cmds);
        ui.add_space(4.0);
    }

    fn after_row(&self, ctx: &mut AdapterCtx, key: &str, outcome: RowOutcome) {
        // Re-smoothing is cheap (no decode), so it follows the slider; it
        // needs a measurement to smooth.
        if key == "smoothing" && edited(outcome) && ctx.clip.stabilization_data.is_some() {
            ctx.cmds.push(AppCommand::ResmoothStabilization { clip_id: ctx.clip.id });
        }
    }
}

#[cfg(test)]
mod trajectory_tests {
    use crate::state::AppState;
    use crate::theme::KlippTheme;
    use klipp_effects::rolling_shutter::Homography;
    use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, smooth_trajectory, StabilizationData};

    /// A clip measured for stabilisation, its trajectory smoothed at
    /// `smoothing`: what the analysis leaves, and what the render reads.
    fn measured(smoothing: f64) -> klipp_timeline::Clip {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let step = |i: usize| Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 2.0 } else { -1.5 }], [0.0, 1.0, 0.5], [0.0, 0.0, 1.0]] };
        let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
        let accumulated = accumulate_trajectory(&frame_transforms);
        let trajectory = accumulated.iter().map(decompose_homography).collect();
        let data = StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 };
        clip.smoothed_trajectory = Some(smooth_trajectory(&data, smoothing));
        clip.stabilization_data = Some(data);
        clip
    }

    /// The plot's two lines as drawn (the raw thin one, the smoothed
    /// thicker one), each its points.
    fn plotted(clip: &klipp_timeline::Clip) -> (Vec<egui::Pos2>, Vec<egui::Pos2>) {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 600.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::inspector::effects_tab::stabilization_status(ui, &KlippTheme::default(), clip, &state, &mut Vec::new()));
        });
        let mut lines: Vec<(f32, Vec<egui::Pos2>)> = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Path(p) if p.points.len() > 2 => Some((p.stroke.width, p.points.clone())),
            _ => None,
        }).collect();
        lines.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(lines.len(), 2, "the plot draws a raw and a smoothed line");
        (lines.remove(0).1, lines.remove(0).1)
    }

    /// The plot draws the measurement and the smoothing the render reads,
    /// point for point: both its lines are one scale and offset of the
    /// raw `tx` (`stabilization_data.trajectory`) and the smoothed `tx`
    /// (`smoothed_trajectory.smoothed`, whose corrections the render
    /// applies), evenly spaced in time; and smoothing the clip again
    /// redraws it from the new data.
    #[test]
    fn the_trajectory_plot_draws_from_the_same_data() {
        for smoothing in [1.0, 0.3] {
            let clip = measured(smoothing);
            let raw: Vec<f32> = clip.stabilization_data.as_ref().unwrap().trajectory.iter().map(|p| p.tx as f32).collect();
            let smooth: Vec<f32> = clip.smoothed_trajectory.as_ref().unwrap().smoothed.iter().map(|p| p.tx as f32).collect();
            let (raw_line, smooth_line) = plotted(&clip);
            assert_eq!((raw_line.len(), smooth_line.len()), (raw.len(), smooth.len()), "a point per frame");
            // One scale for both lines: a value higher up the plot is a
            // larger tx, by the same pixels per unit in both.
            let (lo, hi) = raw.iter().chain(&smooth).fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            let at = |line: &[egui::Pos2], values: &[f32]| line.iter().zip(values).map(|(p, &v)| (p.y, v)).collect::<Vec<_>>();
            let points: Vec<(f32, f32)> = at(&raw_line, &raw).into_iter().chain(at(&smooth_line, &smooth)).collect();
            let (y_lo, y_hi) = (points.iter().find(|(_, v)| *v == lo).unwrap().0, points.iter().find(|(_, v)| *v == hi).unwrap().0);
            let per_unit = (y_lo - y_hi) / (hi - lo);
            assert!(per_unit > 0.0, "a larger tx is drawn higher");
            for (y, v) in &points {
                let want = y_lo - (v - lo) * per_unit;
                assert!((y - want).abs() < 1e-3, "smoothing {smoothing}: tx {v} drawn at y {y}, the data's place is {want}");
            }
            for line in [&raw_line, &smooth_line] {
                let dx = line[1].x - line[0].x;
                assert!(line.windows(2).all(|w| (w[1].x - w[0].x - dx).abs() < 1e-3), "evenly spaced in time");
            }
        }
        assert_ne!(plotted(&measured(1.0)).1, plotted(&measured(0.3)).1, "another smoothing draws another smoothed line");
    }
}
