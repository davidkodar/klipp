//! The analysis-driven effects' adapters: Deflicker and Video
//! Stabilization measure the clip's media first. The value the
//! measurement is smoothed by is applied where the analysis is read
//! (M4 step 9), so an edit of it needs nothing more.

use super::{Adapter, AdapterCtx};
use crate::inspector::effects_tab::{deflicker_status, stabilization_status, DEFLICKER_WINDOW_HINT};

pub(super) struct Deflicker;

impl Adapter for Deflicker {
    fn before_rows(&self, ui: &mut egui::Ui, ctx: &mut AdapterCtx) {
        deflicker_status(ui, ctx.theme, ctx.clip, ctx.state, ctx.cmds);
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
}

#[cfg(test)]
mod trajectory_tests {
    use crate::state::AppState;
    use crate::theme::KlippTheme;
    use klipp_effects::rolling_shutter::Homography;
    use klipp_effects::stabilization::{accumulate_trajectory, decompose_homography, smooth_trajectory, StabilizationData};

    /// A clip of a measured media file with a stabilisation effect at
    /// `smoothing`, and the state holding the file's analysis (M4 step 9):
    /// what the analysis leaves, and what the render reads.
    fn measured(smoothing: f32) -> (klipp_timeline::Clip, AppState, StabilizationData) {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        // The analysis key reads the file: a stand-in that exists.
        let mut item = klipp_media::MediaItem::synthetic_for_test(std::env::current_exe().expect("the test binary"), None);
        item.fps = klipp_core::Fps::new(24, 1);
        item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        let clip = klipp_timeline::test_clip::TestClip::of(klipp_timeline::Clip::media(item.id, tc(0.0), tc(2.0), tc(0.0), tc(2.0))).effect(klipp_effects::BuiltinEffect::VideoStabilization { smoothing, crop_ratio: 0.0, zoom: false }).build();
        // A shake in the analysis' own units, frame heights (#2771): a few
        // pixels of a 1080-line frame each way, drifting, well inside the
        // hand's shake the smoothing may take out (#2823). In pixels, as it
        // was written, every smoothing was held to the same limit and drew
        // the same line.
        let step = |i: usize| Homography { h: [[1.0, 0.0, if i.is_multiple_of(2) { 0.004 } else { -0.003 }], [0.0, 1.0, 0.001], [0.0, 0.0, 1.0]] };
        let frame_transforms: Vec<Homography> = (0..47).map(step).collect();
        let accumulated = accumulate_trajectory(&frame_transforms);
        let trajectory = accumulated.iter().map(decompose_homography).collect();
        let data = StabilizationData { frame_transforms, accumulated, trajectory, fps: 24.0, frame_count: 48 };
        let mut state = AppState::default();
        let key = klipp_analysis::assets::asset_key(klipp_analysis::assets::AssetKind::Stabilisation, &item).expect("a key");
        state.analyses.insert(key, std::sync::Arc::new(serde_json::to_value(&data).expect("serialises")));
        state.media_pool.add(item);
        (clip, state, data)
    }

    /// The plot's two lines as drawn (the raw thin one, the smoothed
    /// thicker one), each its points.
    fn plotted(clip: &klipp_timeline::Clip, state: &AppState) -> (Vec<egui::Pos2>, Vec<egui::Pos2>) {
        let ctx = egui::Context::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 600.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| crate::inspector::effects_tab::stabilization_status(ui, &KlippTheme::default(), clip, state, &mut Vec::new()));
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
    /// raw `tx` (the analysis' trajectory) and the smoothed `tx` (smoothed
    /// as the render derives it at the effect's smoothing, whose
    /// corrections it applies), evenly spaced in time; and another
    /// smoothing redraws it from the new derivation.
    #[test]
    fn the_trajectory_plot_draws_from_the_same_data() {
        for smoothing in [1.0f32, 0.3] {
            let (clip, state, data) = measured(smoothing);
            let raw: Vec<f32> = data.trajectory.iter().map(|p| p.tx as f32).collect();
            // As the render derives it: the window a share of the media's length.
            let smooth: Vec<f32> = smooth_trajectory(&data, (smoothing as f64 * 2.0).max(0.05)).smoothed.iter().map(|p| p.tx as f32).collect();
            let (raw_line, smooth_line) = plotted(&clip, &state);
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
        let line = |s: f32| {
            let (clip, state, _) = measured(s);
            plotted(&clip, &state).1
        };
        assert_ne!(line(1.0), line(0.3), "another smoothing draws another smoothed line");
    }
}
