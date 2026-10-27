//! What a tracked subject matte (#239) must cover: the source times a
//! render of the sequence asks a clip for, and how many of them a matte
//! sequence has a frame for. The export checks this before its first
//! frame, and the External Matte card says it.

use std::path::{Path, PathBuf};

use klipp_nodes::matte_sequence;
use klipp_nodes::NodeKind;
use klipp_timeline::{Clip, GradeAccess, Sequence};

/// The sequence frames a render draws `clip` at: its own span, and the
/// spans of the transitions it is in (whose handles it gives).
pub fn span(seq: &Sequence, clip: &Clip) -> (klipp_core::Timecode, klipp_core::Timecode) {
    let (mut start, mut end) = (clip.timeline_in, clip.timeline_out);
    for track in &seq.tracks {
        if track.find_clip(clip.id).is_none() {
            continue;
        }
        for tr in track.transitions.iter().filter(|t| t.clip_a_id == clip.id || t.clip_b_id == clip.id) {
            let Some((s, e)) = transition_span(track, tr) else { continue };
            start = start.min(s);
            end = end.max(e);
        }
    }
    (start, end)
}

/// The sequence frames transition `tr` on `track` runs over.
fn transition_span(track: &klipp_timeline::Track, tr: &klipp_timeline::Transition) -> Option<(klipp_core::Timecode, klipp_core::Timecode)> {
    let cut = track.find_clip(tr.clip_b_id)?.timeline_in;
    Some(match tr.alignment {
        klipp_core::TransitionAlignment::Center => {
            let half = klipp_core::RationalTime { value: tr.duration.value / 2, rate: tr.duration.rate };
            (cut - half, cut + half)
        }
        klipp_core::TransitionAlignment::StartAtCut => (cut, cut + tr.duration),
        klipp_core::TransitionAlignment::EndAtCut => (cut - tr.duration, cut),
    })
}

/// The turn a clip's frames are decoded upright by: the clip's own when
/// it has one, else its media's, as the frame sources decode them. A
/// subject matte is made and read by the same turn (#985 phase 3).
pub fn decode_rotation(clip: &Clip, media_rotation: i32) -> i32 {
    if clip.source_rotation != 0 { clip.source_rotation } else { media_rotation }
}

/// The folder of the subject matte `model` (0 the subject, 1 the
/// foreground) makes of the media file at `path`, upright by `rotation`
/// (#985 phase 3): what a transition cutting round the subject reads.
pub fn media_dir(path: &Path, rotation: i32, model: u32) -> Option<PathBuf> {
    let model = if model == 1 { klipp_analysis::subject_matte::MatteModel::Foreground } else { klipp_analysis::subject_matte::MatteModel::Subject };
    klipp_analysis::subject_matte::dir_for(path, rotation, model)
}

/// A piece transition that cuts round the subject (#985 phase 3): which
/// clip's subject (the outgoing one's when the pieces leave, the
/// incoming's when they assemble), by which model, and the source times
/// of that clip it reads.
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionSubject {
    pub transition: klipp_core::TransitionId,
    pub clip: klipp_core::ClipId,
    /// 0 the subject, 1 the foreground.
    pub model: u32,
    pub times: Vec<f64>,
}

/// The subject a piece transition with `kind` and `params` cuts out, if
/// it cuts one: its model, and whether it is the incoming clip's.
pub fn transition_subject_of(kind: klipp_core::TransitionKind, params: &klipp_core::TransitionUserParams) -> Option<(u32, bool)> {
    let cut = params.subject.filter(|_| klipp_core::piece_transition::is_piece_kind(kind))?;
    let assemble = klipp_core::piece_transition::PieceParams::read(kind, params).assemble;
    Some((if cut.model == klipp_core::piece_transition::SubjectModel::Foreground { 1 } else { 0 }, assemble))
}

/// The time of a media's last frame, `end` seconds long at `fps`: where
/// its picture, and its subject matte, are read past its end.
pub fn last_frame_secs(end: f64, fps: f64) -> f64 {
    (end - 1.0 / fps.max(1.0)).max(0.0)
}

/// Every piece transition of `seq` cutting round a subject, with the
/// source times it reads (its media's length unknown: none held at its
/// end).
pub fn transition_subjects(seq: &Sequence) -> Vec<TransitionSubject> {
    transition_subjects_with(seq, |_| None)
}

/// [`transition_subjects`], each time held to the media's frames: before
/// its start at its first, past its end at its last (`last_of`, the
/// clip's media's last frame in seconds, when known), where the decoder
/// shows the picture and the matte is read.
pub fn transition_subjects_with(seq: &Sequence, last_of: impl Fn(&Clip) -> Option<f64>) -> Vec<TransitionSubject> {
    let fps = seq.fps.as_f64();
    let step = klipp_core::RationalTime::from_frames(1, seq.fps.rate());
    let mut out = Vec::new();
    for track in &seq.tracks {
        for tr in &track.transitions {
            let Some((model, incoming)) = transition_subject_of(tr.kind, &tr.user_params) else { continue };
            let Some(side) = track.find_clip(if incoming { tr.clip_b_id } else { tr.clip_a_id }) else { continue };
            if !side.enabled {
                continue;
            }
            let Some((start, end)) = transition_span(track, tr) else { continue };
            let last = last_of(side).unwrap_or(f64::MAX);
            let mut times = Vec::new();
            let mut t = start;
            while t < end {
                // Before the media's start its first frame shows, past its
                // end its last, and its matte is read there: counted where
                // it is read.
                times.push(crate::render::source_secs(side, t, fps).max(0.0).min(last));
                t += step;
            }
            out.push(TransitionSubject { transition: tr.id, clip: side.id, model, times });
        }
    }
    out
}

/// The source times (seconds) a render of `seq` asks `clip` for: one for
/// every sequence frame it is drawn at, through its speed, ramp and
/// direction, and the frames either side a blend reads, never before 0.
pub fn source_times(seq: &Sequence, clip: &Clip) -> Vec<f64> {
    source_times_held(seq, clip, None)
}

/// [`source_times`], each held to the media's frames: before its start
/// at its first, past its end at its last (`last`, its last frame in
/// seconds, when known), where the decoder shows the picture and a
/// matte is read.
pub fn source_times_held(seq: &Sequence, clip: &Clip, last: Option<f64>) -> Vec<f64> {
    let fps = seq.fps.as_f64();
    let (start, end) = span(seq, clip);
    let step = klipp_core::RationalTime::from_frames(1, seq.fps.rate());
    let mut out = Vec::new();
    let mut t = start;
    while t < end {
        out.push(crate::render::source_secs(clip, t, fps));
        t += step;
    }
    if let (Some(first), Some(last)) = (out.first().copied(), out.last().copied()) {
        let frame = 1.0 / fps.max(1.0);
        let (lo, hi) = (first.min(last), first.max(last));
        out.push((lo - frame).max(0.0));
        out.push(hi + frame);
    }
    match last {
        Some(last) => {
            for s in &mut out {
                *s = s.max(0.0).min(last);
            }
        }
        None => out.retain(|s| *s >= 0.0),
    }
    out
}

/// The settings of `clip`'s enabled External Mattes: its grade's, and
/// those beside its picture's chain (#2758), which a recipe reads a
/// tracked subject by.
pub fn external_mattes(clip: &Clip) -> Vec<klipp_nodes::NodeParams> {
    let mut out: Vec<klipp_nodes::NodeParams> = clip.grade().nodes.iter().filter(|n| n.kind == NodeKind::ExternalMatte && n.enabled).map(|n| n.params.clone()).collect();
    if let Ok(lifted) = clip.picture_v2() {
        for n in lifted.picture.graph.nodes.iter().filter(|n| n.enabled) {
            let Some(payload) = n.legacy.as_ref().map(|l| &l.payload) else { continue };
            if payload.get("kind").and_then(serde_json::Value::as_str) != Some("ExternalMatte") {
                continue;
            }
            if let Some(params) = payload.get("params").and_then(|p| serde_json::from_value::<klipp_nodes::NodeParams>(p.clone()).ok()) {
                out.push(params);
            }
        }
    }
    out
}

/// The models `clip`'s grade binds tracked subjects by (#1881): its
/// enabled External Mattes bound to a subject, each model once, by its
/// index (0 the subject, 1 the foreground); beside its chain too (#2758).
pub fn subject_models_of(clip: &Clip) -> Vec<u32> {
    let mut out: Vec<u32> = external_mattes(clip)
        .iter()
        .filter_map(|p| p.matte_subject.map(|m| m.index() as u32))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The matte sequence folders `clip`'s grade reads by path: its External
/// Mattes whose file is a matte sequence; beside its chain too (#2758).
pub fn sequences_of(clip: &Clip) -> Vec<PathBuf> {
    external_mattes(clip)
        .into_iter()
        .filter_map(|p| p.matte_image_path)
        .filter(|p| matte_sequence::is_sequence(p))
        .collect()
}

/// How many of `times` the sequence in `dir` has a frame for (within half
/// a frame), of how many: `None` when `dir` isn't a sequence this build
/// reads.
pub fn coverage(dir: &Path, times: &[f64]) -> Option<(usize, usize)> {
    let manifest = matte_sequence::read_manifest(dir)?;
    let keys = matte_sequence::frames(dir);
    let half = manifest.half_frame_us();
    let covered = times
        .iter()
        .filter(|t| {
            let want = matte_sequence::key_of(**t);
            let at = keys.partition_point(|k| *k < want);
            [at.checked_sub(1), (at < keys.len()).then_some(at)].into_iter().flatten().any(|i| (keys[i] - want).abs() <= half)
        })
        .count();
    Some((covered, times.len()))
}

/// A clip whose tracked matte doesn't cover what the render will ask for.
#[derive(Debug, Clone, PartialEq)]
pub struct Uncovered {
    pub clip: klipp_core::ClipId,
    /// The piece transition that reads it, and its model (#985 phase 3);
    /// `None` for a matte on the clip's grade.
    pub transition: Option<(klipp_core::TransitionId, u32)>,
    /// The model of a subject the clip's grade is bound to (#1881); `None`
    /// for a folder bound by path, or a transition's.
    pub subject: Option<u32>,
    pub folder: PathBuf,
    pub covered: usize,
    pub needed: usize,
    /// The source range the render reads, seconds.
    pub from_secs: f64,
    pub to_secs: f64,
}

/// [`uncovered`], and every piece transition cutting round a subject
/// whose matte doesn't cover the frames it reads (#985 phase 3), its
/// folder found by `dir_of` (the clip, the model): none found is none
/// covered. `last_of` is the clip's media's last frame, in seconds
/// ([`transition_subjects_with`]).
pub fn uncovered_with(seq: &Sequence, dir_of: impl Fn(&Clip, u32) -> Option<PathBuf>, last_of: impl Fn(&Clip) -> Option<f64>) -> Vec<Uncovered> {
    let mut out = uncovered_held(seq, &last_of);
    // Grades bound to a tracked subject (#1881): its folder by the media
    // as linked now; none found is none covered.
    for clip in seq.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| c.enabled) {
        let models = subject_models_of(clip);
        if models.is_empty() {
            continue;
        }
        let times = source_times_held(seq, clip, last_of(clip));
        let (from_secs, to_secs) = times.iter().fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
        for model in models {
            let folder = dir_of(clip, model).unwrap_or_default();
            let (covered, needed) = coverage(&folder, &times).unwrap_or((0, times.len()));
            if covered < needed {
                out.push(Uncovered { clip: clip.id, transition: None, subject: Some(model), folder, covered, needed, from_secs, to_secs });
            }
        }
    }
    for ts in transition_subjects_with(seq, &last_of) {
        let Some(clip) = seq.tracks.iter().find_map(|t| t.find_clip(ts.clip)) else { continue };
        let folder = dir_of(clip, ts.model).unwrap_or_default();
        let (covered, needed) = coverage(&folder, &ts.times).unwrap_or((0, ts.times.len()));
        if covered < needed {
            let (from_secs, to_secs) = ts.times.iter().fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
            out.push(Uncovered { clip: ts.clip, transition: Some((ts.transition, ts.model)), subject: None, folder, covered, needed, from_secs, to_secs });
        }
    }
    out
}

/// Every clip of `seq` graded by a tracked matte that doesn't cover the
/// source times a render asks it for: what an export checks before its
/// first frame.
pub fn uncovered(seq: &Sequence) -> Vec<Uncovered> {
    uncovered_held(seq, |_| None)
}

/// [`uncovered`], each clip's times held to its media's frames (`last_of`,
/// its media's last frame: [`source_times_held`]).
fn uncovered_held(seq: &Sequence, last_of: impl Fn(&Clip) -> Option<f64>) -> Vec<Uncovered> {
    let mut out = Vec::new();
    for track in &seq.tracks {
        for clip in track.clips.iter().filter(|c| c.enabled) {
            let folders = sequences_of(clip);
            if folders.is_empty() {
                continue;
            }
            let times = source_times_held(seq, clip, last_of(clip));
            let (from_secs, to_secs) = times.iter().fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(*t), b.max(*t)));
            for folder in folders {
                if let Some((covered, needed)) = coverage(&folder, &times) {
                    if covered < needed {
                        out.push(Uncovered { clip: clip.id, transition: None, subject: None, folder, covered, needed, from_secs, to_secs });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{ClipKind, Track, TrackKind};

    fn tc(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, 25)
    }

    fn clip(from: f64, to: f64, source_in: f64) -> Clip {
        let mut c = Clip::gap(tc(from), tc(to));
        c.kind = ClipKind::Media { media_id: klipp_core::MediaId::new() };
        c.source_in = tc(source_in);
        c.source_out = tc(source_in + to - from);
        c
    }

    /// The times asked are the clip's frames through its source, plus the
    /// handles a transition reads, plus a frame either side; a sequence
    /// covering some of them says how many.
    #[test]
    fn the_times_a_render_asks_for_and_how_many_are_covered() {
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let (a, b) = (clip(0.0, 2.0, 10.0), clip(2.0, 4.0, 0.0));
        let mut v = Track::new("V1", TrackKind::Video);
        v.transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: tc(0.4),
            clip_a_id: a.id,
            clip_b_id: b.id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        v.clips = vec![a.clone(), b.clone()];
        seq.tracks = vec![v];
        let times = source_times(&seq, &a);
        // 2 s of clip plus 0.2 s of handle after its out, at 25 fps, and
        // a frame either side.
        assert_eq!(times.len(), 55 + 2, "{times:?}");
        let (lo, hi) = times.iter().fold((f64::MAX, f64::MIN), |(l, h), t| (l.min(*t), h.max(*t)));
        assert!((lo - 9.96).abs() < 1e-6 && (hi - 12.2).abs() < 1e-6, "{lo} {hi}");

        let dir = std::env::temp_dir().join(format!("klipp-matte-coverage-{}", klipp_core::MediaId::new().0));
        let m = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: 2, height: 2, rotation: 0, model: "t".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
        matte_sequence::create(&dir, &m).unwrap();
        for f in 249..=280 {
            matte_sequence::put(&dir, &m, f * 40_000, &[255; 4]).unwrap();
        }
        assert_eq!(coverage(&dir, &times), Some((32, 57)), "10.0 s to 11.2 s: frames 250..280, and 9.96");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// #985 phase 3: a piece transition cutting round the outgoing
    /// subject reads that clip's frames across the transition, handle
    /// included; its matte short of them is counted, by the transition,
    /// and with none found, none of them is covered.
    #[test]
    fn a_transition_cutting_round_the_subject_counts_its_frames() {
        use klipp_core::piece_transition::{PieceParams, SubjectCut, SubjectModel, SubjectMotion};
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let (a, b) = (clip(0.0, 2.0, 10.0), clip(2.0, 4.0, 0.0));
        let kind = klipp_core::TransitionKind::Shatter;
        let tid = klipp_core::TransitionId::new();
        let mut v = Track::new("V1", TrackKind::Video);
        let plain = klipp_core::spec_for_kind(kind).default_params;
        v.transitions.push(klipp_timeline::Transition { id: tid, kind, duration: tc(0.4), clip_a_id: a.id, clip_b_id: b.id, user_params: plain, alignment: klipp_core::TransitionAlignment::Center });
        v.clips = vec![a.clone(), b.clone()];
        seq.tracks = vec![v];
        assert!(transition_subjects(&seq).is_empty(), "no subject by default");
        let p = PieceParams { subject: Some(SubjectCut { motion: SubjectMotion::Stays, model: SubjectModel::Subject }), ..PieceParams::read(kind, &plain) };
        seq.tracks[0].transitions[0].user_params = p.to_user(kind);
        let found = transition_subjects(&seq);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].transition, found[0].clip, found[0].model), (tid, a.id, 0));
        // 1.8 s to 2.2 s of the sequence: a's 11.8 s to 12.2 s.
        assert_eq!(found[0].times.len(), 10, "{:?}", found[0].times);
        assert!((found[0].times[0] - 11.8).abs() < 1e-6);

        let dir = std::env::temp_dir().join(format!("klipp-matte-transition-{}", klipp_core::MediaId::new().0));
        let m = matte_sequence::Manifest { format: matte_sequence::FORMAT, fps: 25.0, width: 2, height: 2, rotation: 0, model: "t".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: true };
        matte_sequence::create(&dir, &m).unwrap();
        for f in 295..=299 {
            matte_sequence::put(&dir, &m, f * 40_000, &[255; 4]).unwrap();
        }
        let short = uncovered_with(&seq, |c, model| (c.id == a.id && model == 0).then(|| dir.clone()), |_| None);
        assert_eq!(short.len(), 1, "{short:?}");
        assert_eq!((short[0].transition, short[0].covered, short[0].needed), (Some((tid, 0)), 5, 10));
        let none = uncovered_with(&seq, |_, _| None, |_| None);
        assert_eq!((none[0].covered, none[0].needed), (0, 10));
        // The incoming clip's when the pieces arrive, by the foreground.
        let p = PieceParams { assemble: true, subject: Some(SubjectCut { motion: SubjectMotion::Stays, model: SubjectModel::Foreground }), ..p };
        seq.tracks[0].transitions[0].user_params = p.to_user(kind);
        let found = transition_subjects(&seq);
        assert_eq!((found[0].clip, found[0].model), (b.id, 1));
        // b's handle before its media's start: read, and counted, at its
        // first frame, as the decoder shows it, never dropped.
        assert_eq!(found[0].times.len(), 10, "{:?}", found[0].times);
        assert_eq!(found[0].times.iter().filter(|t| **t == 0.0).count(), 6, "{:?}", found[0].times);
        // The outgoing clip's handle 0.2 s past its media's end (12.0 s
        // long at 25 fps: its last frame 11.96 s): read, and counted, at
        // that last frame, so a matte made to the end covers it.
        let p = PieceParams { assemble: false, ..p };
        seq.tracks[0].transitions[0].user_params = p.to_user(kind);
        let last = last_frame_secs(12.0, 25.0);
        let held = transition_subjects_with(&seq, |c| (c.id == a.id).then_some(last));
        assert_eq!(held[0].times.len(), 10);
        assert!(held[0].times.iter().all(|t| *t <= last + 1e-9), "{:?}", held[0].times);
        assert_eq!(held[0].times.iter().filter(|t| (**t - last).abs() < 1e-9).count(), 6, "{:?}", held[0].times);
        let dir_end = std::env::temp_dir().join(format!("klipp-matte-end-{}", klipp_core::MediaId::new().0));
        matte_sequence::create(&dir_end, &m).unwrap();
        for f in 290..=299 {
            matte_sequence::put(&dir_end, &m, f * 40_000, &[255; 4]).unwrap();
        }
        assert!(uncovered_with(&seq, |_, _| Some(dir_end.clone()), |c| (c.id == a.id).then_some(last)).is_empty(), "made to the end, the handle past it is covered");
        assert!(!uncovered_with(&seq, |_, _| Some(dir_end.clone()), |_| None).is_empty(), "unheld, it never could be");
        let _ = std::fs::remove_dir_all(&dir_end);
        let _ = std::fs::remove_dir_all(dir);
    }
}
