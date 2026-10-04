//! Which clips an effect from the browser, the palette or a drop lands
//! on (#842). With several clips selected it is all of them that can
//! take it, as one undo step: one clip at a time, the other nine of a
//! ten-clip selection were left untouched without a word.

use klipp_core::{AppCommand, ClipId};
use klipp_timeline::{Sequence, TrackKind};

/// What an effect works on, which decides the clips it can go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// Pictures: clips on video and text tracks.
    Picture,
    /// Sound: clips on audio tracks.
    Sound,
}

/// The clips an effect goes on, and how many selected clips could not
/// take it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    pub clips: Vec<ClipId>,
    pub skipped: usize,
}

/// The clips an effect of `kind` goes on: the selection (`selected` and
/// the `primary` clip), or, for a drop, the clip dropped on when it is
/// not part of the selection. A single clip takes it as it always did;
/// of several, those on a locked track or a track of the other kind are
/// skipped and counted.
pub fn targets(seq: &Sequence, primary: Option<ClipId>, selected: &[ClipId], dropped_on: Option<ClipId>, kind: EffectKind) -> Targets {
    let mut base: Vec<ClipId> = Vec::new();
    for id in selected.iter().copied().chain(primary) {
        if !base.contains(&id) {
            base.push(id);
        }
    }
    if let Some(c) = dropped_on {
        if !base.contains(&c) {
            base = vec![c];
        }
    }
    if base.len() <= 1 {
        return Targets { clips: base, skipped: 0 };
    }
    let takes = |id: &ClipId| {
        seq.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == *id)).is_some_and(|t| {
            !t.locked
                && match kind {
                    EffectKind::Picture => matches!(t.kind, TrackKind::Video | TrackKind::Text),
                    EffectKind::Sound => t.kind == TrackKind::Audio,
                }
        })
    };
    let clips: Vec<ClipId> = base.iter().copied().filter(takes).collect();
    Targets { skipped: base.len() - clips.len(), clips }
}

/// Whether a drop on `dropped_on` selects that clip: only when it is
/// outside the selection. Dropped on a selected clip, the selection
/// stays, even when that clip could not take the effect (a sound clip
/// under a picture effect) and the rest of the selection did.
pub fn drop_reselects(primary: Option<ClipId>, selected: &[ClipId], dropped_on: ClipId) -> bool {
    primary != Some(dropped_on) && !selected.contains(&dropped_on)
}

/// The commands for `targets`, `make` giving each clip's, as one undo
/// step when there is more than one clip.
pub fn commands(targets: &Targets, mut make: impl FnMut(ClipId) -> Vec<AppCommand>) -> Vec<AppCommand> {
    let many = targets.clips.len() > 1;
    let mut out = Vec::new();
    if many {
        out.push(AppCommand::BeginHistoryBatch);
    }
    for id in &targets.clips {
        out.extend(make(*id));
    }
    if many {
        out.push(AppCommand::EndHistoryBatch);
    }
    out
}

/// What the toast says: where `name` went, and what it skipped.
pub fn said(name: &str, targets: &Targets) -> String {
    let n = targets.clips.len();
    let on = if n == 1 { "the clip".to_string() } else { format!("{n} clips") };
    match targets.skipped {
        0 => format!("\"{name}\" applied to {on}"),
        s => format!("\"{name}\" applied to {on}; {s} selected clip{} could not take it (another kind of track, or locked)", if s == 1 { "" } else { "s" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track};

    fn tc(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn clip(from: f64) -> Clip {
        Clip::media(MediaId::new(), tc(from), tc(from + 1.0), tc(0.0), tc(1.0))
    }

    /// V1 with three pictures, V2 locked with one, A1 with two sounds.
    fn timeline() -> Sequence {
        let mut seq = Sequence::new_default();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![clip(0.0), clip(1.0), clip(2.0)];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![clip(0.0)];
        v2.locked = true;
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips = vec![clip(0.0), clip(1.0)];
        seq.tracks = vec![v1, v2, a1];
        seq
    }

    #[test]
    fn a_selection_takes_the_effect_where_it_can() {
        let seq = timeline();
        let v = |i: usize| seq.tracks[0].clips[i].id;
        let (locked, a) = (seq.tracks[1].clips[0].id, |i: usize| seq.tracks[2].clips[i].id);
        let all = [v(0), v(1), v(2), locked, a(0), a(1)];
        let t = targets(&seq, Some(v(0)), &all, None, EffectKind::Picture);
        assert_eq!(t.clips, vec![v(0), v(1), v(2)], "the three unlocked pictures");
        assert_eq!(t.skipped, 3, "the locked picture and the two sounds");
        let t = targets(&seq, Some(v(0)), &all, None, EffectKind::Sound);
        assert_eq!((t.clips, t.skipped), (vec![a(0), a(1)], 4));
    }

    #[test]
    fn one_clip_takes_it_as_before_and_a_drop_outside_the_selection_takes_only_its_clip() {
        let seq = timeline();
        let v = |i: usize| seq.tracks[0].clips[i].id;
        let a0 = seq.tracks[2].clips[0].id;
        // One clip: as it always was, whatever its track.
        assert_eq!(targets(&seq, Some(a0), &[], None, EffectKind::Picture).clips, vec![a0]);
        // Dropped on a clip of the selection: the whole selection.
        assert_eq!(targets(&seq, Some(v(0)), &[v(0), v(1)], Some(v(1)), EffectKind::Picture).clips, vec![v(0), v(1)]);
        // Dropped on a clip outside it: that clip alone.
        assert_eq!(targets(&seq, Some(v(0)), &[v(0), v(1)], Some(v(2)), EffectKind::Picture).clips, vec![v(2)]);
    }

    /// #850 review: a drop on a selected clip that cannot take the effect
    /// keeps the selection; a drop outside it selects the clip dropped on.
    #[test]
    fn a_drop_keeps_the_selection_it_landed_in() {
        let (p, s, out) = (ClipId::new(), ClipId::new(), ClipId::new());
        assert!(!drop_reselects(Some(p), &[p, s], s), "dropped on a selected clip, though it is skipped");
        assert!(drop_reselects(Some(p), &[p, s], out), "dropped outside: that clip");
        assert!(!drop_reselects(Some(p), &[], p), "already the one selected");
        assert!(drop_reselects(Some(p), &[], out));
    }

    #[test]
    fn several_clips_are_one_undo_step() {
        let seq = timeline();
        let v = |i: usize| seq.tracks[0].clips[i].id;
        let t = targets(&seq, Some(v(0)), &[v(0), v(1)], None, EffectKind::Picture);
        let cmds = commands(&t, |clip_id| vec![AppCommand::SelectClip(Some(clip_id))]);
        assert!(matches!(cmds.first(), Some(AppCommand::BeginHistoryBatch)) && matches!(cmds.last(), Some(AppCommand::EndHistoryBatch)));
        assert_eq!(cmds.len(), 4);
        let one = targets(&seq, Some(v(0)), &[], None, EffectKind::Picture);
        assert_eq!(commands(&one, |c| vec![AppCommand::SelectClip(Some(c))]).len(), 1, "one clip needs no batch");
        assert!(said("Vignette", &t).contains("2 clips"));
        assert!(said("Vignette", &Targets { clips: vec![v(0)], skipped: 2 }).contains("2 selected clips could not take it"));
    }
}
