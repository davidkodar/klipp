//! A short clip from a moment of a long video (#2720): the ops that build
//! one in an empty upright sequence. The moment's speech placed on clean
//! word edges, tightened (its pauses and filler taken out) when asked,
//! the picture filling the frame and following the speaker, every other
//! piece pushed in so the jumps read as cuts, captions, and a hook title
//! when asked. What `find_moments` found and `make_shorts` builds.

use crate::ops::{Op, SpokenWord};
use klipp_core::MediaId;

/// Air kept before a piece's first word and after its last, seconds.
const LEAD: f64 = 0.08;
const TAIL: f64 = 0.12;
/// A pause this long inside a moment is taken out when it is tightened.
const PAUSE: f64 = 0.5;
/// Words that carry nothing alone.
const FILLER: &[&str] = &["um", "uh", "er", "erm", "ah", "hmm", "mm"];

/// The pieces of the file to place for the moment `from..to`, in file
/// seconds: one, its ends on the words', or with `tighten` one for each
/// run of speech between the pauses, runs of only filler left out.
pub fn pieces(words: &[SpokenWord], from: f64, to: f64, tighten: bool) -> Vec<(f64, f64)> {
    let inside: Vec<&SpokenWord> = words.iter().filter(|w| w.start_secs >= from - 0.05 && w.end_secs <= to + 0.05).collect();
    let (Some(first), Some(last)) = (inside.first(), inside.last()) else { return Vec::new() };
    if !tighten {
        return vec![((first.start_secs - LEAD).max(0.0), last.end_secs + TAIL)];
    }
    let filler = |w: &SpokenWord| FILLER.contains(&w.text.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase().as_str());
    let mut runs: Vec<Vec<&SpokenWord>> = Vec::new();
    for w in inside {
        match runs.last_mut() {
            Some(run) if w.start_secs - run.last().map_or(w.start_secs, |p| p.end_secs) < PAUSE => run.push(w),
            _ => runs.push(vec![w]),
        }
    }
    let mut out: Vec<(f64, f64)> = Vec::new();
    for run in runs.iter().filter(|r| !r.iter().all(|w| filler(w))) {
        let (a, b) = ((run[0].start_secs - LEAD).max(0.0), run[run.len() - 1].end_secs + TAIL);
        match out.last_mut() {
            Some(prev) if a <= prev.1 => prev.1 = b,
            _ => out.push((a, b)),
        }
    }
    out
}

/// The ops that build a short of `pieces` of `media` (file seconds) in
/// an empty sequence, 1080×1920: placed one after another from its start,
/// fitted to the frame following the speaker, every other piece pushed
/// in by `punch` when there are several, captioned, with `hook` as a
/// title over its first three seconds when given.
pub fn short_ops(media: MediaId, pieces: &[(f64, f64)], hook: Option<&str>, punch: f32) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut at = 0.0;
    for (a, b) in pieces {
        ops.push(serde_json::json!({ "op": "place", "media_id": media, "track": "video", "at": { "secs": at }, "source_in": a, "source_out": b }));
        at += b - a;
    }
    // An empty sequence takes its first picture's size: upright again.
    ops.push(serde_json::json!({ "op": "format", "width": 1080, "height": 1920 }));
    let punch = (pieces.len() > 1).then_some(punch);
    ops.push(serde_json::json!({ "op": "refit", "markers": false, "punch": punch }));
    ops.push(serde_json::json!({ "op": "captions" }));
    if let Some(hook) = hook.map(str::trim).filter(|h| !h.is_empty()) {
        ops.push(serde_json::json!({ "op": "title", "text": hook, "at": { "secs": 0.0 }, "secs": 3.0_f64.min(at), "style": "heading", "y": 0.2 }));
    }
    ops.into_iter().map(|v| serde_json::from_value(v).expect("a short's op is well formed")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{resolve_with, Transcripts};
    use klipp_timeline::{Sequence, TrackKind};

    fn word(start: f64, text: &str) -> SpokenWord {
        SpokenWord { start_secs: start, end_secs: start + 0.3, text: text.into(), confidence: 0.9 }
    }

    /// Three runs of speech with pauses between, the middle one only "um".
    fn talk() -> Vec<SpokenWord> {
        let mut w: Vec<SpokenWord> = (0..10).map(|k| word(10.0 + k as f64 * 0.35, "one")).collect();
        w.push(word(14.5, "um"));
        w.extend((0..10).map(|k| word(16.0 + k as f64 * 0.35, "two")));
        w
    }

    /// Tightened, the pauses and the run of filler go; each piece starts
    /// just before its first word and ends just after its last.
    #[test]
    fn a_tightened_moment_is_its_runs_of_speech() {
        let p = pieces(&talk(), 10.0, 20.0, true);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!((p[0].0 - (10.0 - LEAD)).abs() < 1e-9 && (p[0].1 - (13.45 + TAIL)).abs() < 1e-9, "{p:?}");
        assert!((p[1].0 - (16.0 - LEAD)).abs() < 1e-9, "{p:?}");
        let whole = pieces(&talk(), 10.0, 20.0, false);
        assert_eq!(whole.len(), 1);
        assert!((whole[0].0 - (10.0 - LEAD)).abs() < 1e-9 && (whole[0].1 - (19.45 + TAIL)).abs() < 1e-9, "{whole:?}");
        assert!(pieces(&talk(), 30.0, 40.0, true).is_empty());
    }

    struct Said(Vec<SpokenWord>);

    impl Transcripts for Said {
        fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Some(self.0.clone())
        }
    }

    /// A short builds in an upright sequence: its pieces end to end from
    /// the start, every picture filling the frame, every other one pushed
    /// in further, captions on what is said, the hook over the opening.
    #[test]
    fn a_short_builds_upright_with_its_pieces_captions_and_hook() {
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: 1080, height: 1920 };
        seq.fps = klipp_core::Fps::new(30, 1);
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/talk.mov".into(), None);
        item.duration = klipp_core::RationalTime::from_secs_f64(60.0, 30);
        item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        item.video_codec = "hevc".into();
        item.has_audio = true;
        let media = pool.add(item);
        let p = pieces(&talk(), 10.0, 20.0, true);
        let ops = short_ops(media, &p, Some("Why do most startups fail?"), 1.15);
        let plan = resolve_with(&seq, &pool, &ops, &Said(talk())).unwrap_or_else(|r| panic!("{r:?}"));
        let mut seqs = vec![seq];
        let (mut pp, mut f) = (pool.clone(), seqs[0].fps);
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pp, fps: &mut f };
        for step in &plan.steps {
            view.apply_forward(&step.edit);
        }
        let built = &view.sequences[0];
        let pictures: Vec<&klipp_timeline::Clip> = built.tracks.iter().filter(|t| t.kind == TrackKind::Video).flat_map(|t| &t.clips).filter(|c| matches!(c.kind, klipp_timeline::ClipKind::Media { .. })).collect();
        assert_eq!(pictures.len(), 2, "two pieces");
        assert!(pictures[0].timeline_in.to_secs_f64().abs() < 1e-6 && (pictures[1].timeline_in.to_secs_f64() - pictures[0].timeline_out.to_secs_f64()).abs() < 0.04, "end to end");
        let scale = |c: &klipp_timeline::Clip| klipp_timeline::frame_glide::lane(c, "scale").iter().map(|k| k.value).fold(0.0f32, f32::max);
        // 1920x1080 fitted into 1080x1920 fills at 1920/607.5 ≈ 3.16; the
        // second pushed in further.
        assert_eq!((built.resolution.width, built.resolution.height), (1080, 1920), "upright");
        assert!(scale(pictures[0]) > 3.1 && (scale(pictures[1]) / scale(pictures[0]) - 1.15).abs() < 1e-3, "filled, the second pushed in: {} {}", scale(pictures[0]), scale(pictures[1]));
        assert!(built.tracks.iter().any(|t| t.kind == TrackKind::Subtitle && !t.clips.is_empty()), "captioned");
        assert!(built.all_clips().any(|c| c.title_data().and_then(|t| t.primary_text()).is_some_and(|t| t.text.contains("startups"))), "the hook as a title");
    }
}
