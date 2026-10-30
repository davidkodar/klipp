//! The inspector for a selected gap (#3116): a gap clip on the primary
//! track, or empty space on another track. A gap has no picture, so in
//! place of the transform panel it shows what applies to it: where it
//! starts, how long it is, closing it, and filling it with a generator.

use klipp_core::{AppCommand, GeneratorData, RationalTime};

use crate::state::AppState;

/// A selected gap: its track and span.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    pub track: usize,
    pub start: RationalTime,
    pub end: RationalTime,
}

/// The selected gap, if the selection is one: the selected clip when it
/// is a gap clip, else the empty space selected on a track.
pub fn gap_of(state: &AppState) -> Option<Gap> {
    match state.selected_clip_loc() {
        Some((track, c)) if c.kind.is_gap() => Some(Gap { track, start: c.timeline_in, end: c.timeline_out }),
        Some(_) => None,
        None => state.selected_gap.map(|(track, start, end)| Gap { track, start, end }),
    }
}

/// What a gap can be filled with, by name.
pub fn fills() -> [(&'static str, GeneratorData); 4] {
    [
        ("Black", GeneratorData::SolidColor { color: [0, 0, 0, 255] }),
        ("Grey", GeneratorData::SolidColor { color: [128, 128, 128, 255] }),
        ("Placeholder", GeneratorData::Placeholder { label: "Placeholder".into(), color: [60, 60, 60, 255] }),
        ("Colour Bars", GeneratorData::ColorBars),
    ]
}

/// The gap's start and length, and its actions.
pub fn show(ui: &mut egui::Ui, state: &AppState, gap: Gap, cmds: &mut Vec<AppCommand>) {
    let tc = state.timecode_display();
    ui.add_space(6.0);
    egui::Grid::new("gap_inspector").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Start");
        ui.label(tc.position(gap.start));
        ui.end_row();
        ui.label("Length");
        ui.label(tc.duration(gap.end - gap.start));
        ui.end_row();
    });
    ui.add_space(8.0);
    if ui.button("Close Gap").on_hover_text("Take the gap out: the clips after it on its track move up to close it.").clicked() {
        cmds.push(AppCommand::CloseGap { track_index: gap.track, gap_start: gap.start, gap_end: gap.end });
    }
    ui.add_space(6.0);
    ui.label("Fill with");
    ui.horizontal_wrapped(|ui| {
        for (name, data) in fills() {
            if ui.button(name).on_hover_text(format!("Fill the gap with {} for its whole length", name.to_lowercase())).clicked() {
                cmds.push(AppCommand::FillGap { track_index: gap.track, start: gap.start, end: gap.end, data });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    /// A gap clip, or empty space, selected is a gap; a shot is not.
    #[test]
    fn a_gap_clip_or_empty_space_is_a_gap_and_a_shot_is_not() {
        let mut state = AppState::default();
        let p = state.sequence().primary_track_index().unwrap();
        let shot = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let hole = klipp_timeline::Clip::gap(t(2.0), t(5.0));
        let (shot_id, hole_id) = (shot.id, hole.id);
        state.sequences[state.active_sequence_idx].tracks[p].clips = vec![shot, hole];
        state.selected_clip = Some(hole_id);
        assert_eq!(gap_of(&state), Some(Gap { track: p, start: t(2.0), end: t(5.0) }));
        state.selected_clip = Some(shot_id);
        assert_eq!(gap_of(&state), None, "a shot shows its own panels");
        state.selected_clip = None;
        state.selected_gap = Some((0, t(1.0), t(3.0)));
        assert_eq!(gap_of(&state), Some(Gap { track: 0, start: t(1.0), end: t(3.0) }));
    }
}
