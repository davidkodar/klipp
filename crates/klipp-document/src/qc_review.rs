//! The check's findings named so a decision about one can be saved
//! (#1932): every finding gets an id that is the same on every run while
//! what it judged is unchanged, and the sequence's reviews are matched to
//! the findings of a run by it.
//!
//! The id carries what the finding was judged on, so a review lapses on
//! its own when that changes. A colour finding's id holds a hash of the
//! clip's media, range, speed, picture (grade, effects, placement, grade
//! versions) and grade group; a craft or sound finding's, a hash of its
//! own words, which the check computes from the timeline.

use klipp_core::ClipId;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, FindingReview, ReviewDecision, Sequence};

/// FNV-1a over `parts`, each closed by a byte no UTF-8 text holds: the
/// same on every build and platform, which `std`'s hasher does not
/// promise.
pub fn stable_hash(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.iter().chain(std::iter::once(&0xffu8)) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn short(h: u64) -> String {
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

/// Rules whose words carry a measurement (a loudness, a contrast) that
/// moves a little from run to run with nothing changed: their numbers
/// are left out of the id, so an acceptance does not lapse on noise.
const MEASURED: &[&str] = &["loudness_drop", "title_hard_to_read"];

/// The id of a craft (`kind` "check") or sound (`kind` "sound") finding:
/// its rule, the frame it is at (or "-"), and a hash of its words — of
/// a measured finding's words with their numbers left out (#1932 review).
pub fn finding_id(kind: &str, rule: &str, at_secs: Option<f64>, fps: f64, what: &str) -> String {
    let frame = at_secs.map_or("-".to_string(), |t| ((t * fps).round() as i64).to_string());
    let words: String = if kind == "sound" || MEASURED.contains(&rule) {
        what.chars().map(|c| if c.is_ascii_digit() || c == '.' || c == '-' { '#' } else { c }).collect()
    } else {
        what.to_string()
    };
    format!("{kind}/{rule}/{frame}/{}", short(stable_hash(&[words.as_bytes()])))
}

/// What a colour finding on `clip`, over `span` of the timeline, was
/// judged on: everything the renderer reads for that picture. The file
/// and the part of it shown, at what speed; the clip's picture (grade,
/// effects, placement, keys, grade versions); its grade group whole, pre
/// and post stages included; its track's levels and effects; every
/// adjustment layer above it over the span; the timeline grade; and the
/// project's `colour` management as the host reads it (working space,
/// grade encoding, global LUT). Any of these changing is a different
/// picture to judge, and an acceptance of the old one lapses (#1932
/// review). A group's name and the clip's place on the timeline are not
/// in it: neither changes the picture.
pub fn colour_basis(seq: &Sequence, pool: &MediaPool, clip: &Clip, span: (f64, f64), colour: &str) -> u64 {
    let json = |v: serde_json::Result<String>| v.unwrap_or_default();
    let picture_of = |c: &Clip| c.picture_v2().ok().map(|p| format!("{}|{}", json(serde_json::to_string(&p.picture)), json(serde_json::to_string(&p.versions)))).unwrap_or_default();
    let media = clip.media_id().map(|id| id.0.to_string()).unwrap_or_default();
    let path = clip.media_id().and_then(|id| pool.get(&id)).map(|m| m.path.to_string_lossy().into_owned()).unwrap_or_default();
    let range = format!("{:?}|{:?}|{}|{:?}", clip.source_in, clip.source_out, clip.speed, clip.speed_keyframes);
    let picture = picture_of(clip);
    let group = clip.grade_group.and_then(|g| seq.grade_groups.iter().find(|x| x.id == g)).map(|g| {
        let mut v = serde_json::to_value(g).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("name");
        }
        v.to_string()
    }).unwrap_or_default();
    let at = seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip.id));
    let track = at.map(|i| json(serde_json::to_string(&seq.tracks[i].settings()))).unwrap_or_default();
    let fps = seq.fps.as_f64().max(1.0);
    let mut layers = String::new();
    for t in seq.tracks.iter().take(at.unwrap_or(0)).filter(|t| t.visible && t.kind == klipp_timeline::TrackKind::Video) {
        for c in t.clips.iter().filter(|c| c.enabled && matches!(c.kind, klipp_timeline::ClipKind::AdjustmentLayer)) {
            let (a, b) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
            if a < span.1 && b > span.0 {
                // Where it starts and ends against the span, in frames:
                // a layer that reaches over more of the shot is another
                // picture, one that moves elsewhere on it is not.
                let edge = |x: f64| ((x - span.0) * fps).round().clamp(-1.0, ((span.1 - span.0) * fps).round() + 1.0) as i64;
                layers.push_str(&format!("{}|{}|{}|{};", edge(a), edge(b), picture_of(c), c.blend_mode as u8));
            }
        }
    }
    let timeline = json(serde_json::to_string(&seq.timeline_graph));
    stable_hash(&[media.as_bytes(), path.as_bytes(), range.as_bytes(), picture.as_bytes(), group.as_bytes(), track.as_bytes(), layers.as_bytes(), timeline.as_bytes(), colour.as_bytes()])
}

/// The id of a colour finding of `rule` on the shot of `clip` that
/// starts `shot_start_secs` on the timeline, judged on `basis`.
pub fn colour_finding_id(rule: &str, clip: &Clip, shot_start_secs: f64, fps: f64, basis: u64) -> String {
    let frame = ((shot_start_secs - clip.timeline_in.to_secs_f64()) * fps).round() as i64;
    format!("colour/{rule}/{}/{frame}/{}", clip.id.0, short(basis))
}

/// Where a finding id points: its rule, the clip (a colour finding's) and
/// the timeline seconds, as far as `seq` can still place them. `None`
/// for text that is not a finding id.
pub fn locate(seq: &Sequence, id: &str) -> Option<(String, Option<ClipId>, Option<f64>)> {
    let parts: Vec<&str> = id.split('/').collect();
    let fps = seq.fps.as_f64();
    match parts.as_slice() {
        ["colour", rule, clip, frame, hash] if is_hash(hash) => {
            let clip = ClipId(clip.parse().ok()?);
            let frame: i64 = frame.parse().ok()?;
            let at = seq.all_clips().find(|c| c.id == clip).map(|c| c.timeline_in.to_secs_f64() + frame as f64 / fps);
            Some((rule.to_string(), Some(clip), at))
        }
        [kind @ ("check" | "sound"), rule, frame, hash] if is_hash(hash) && !kind.is_empty() => {
            let at = match *frame {
                "-" => None,
                f => Some(f.parse::<i64>().ok()? as f64 / fps),
            };
            Some((rule.to_string(), None, at))
        }
        _ => None,
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// A run's findings, by their ids, against the sequence's reviews.
#[derive(Debug, Default)]
pub struct Matched<'a> {
    /// The index of each finding kept on purpose, and its review.
    pub accepted: Vec<(usize, &'a FindingReview)>,
    /// The index of each finding marked to be fixed, and its review.
    pub to_fix: Vec<(usize, &'a FindingReview)>,
    /// Reviews no finding of the run has: what they judged changed or is
    /// gone, so the decision no longer holds.
    pub lapsed: Vec<&'a FindingReview>,
}

/// Match `ids`, a run's findings in order, to `reviews`.
pub fn match_reviews<'a>(ids: &[String], reviews: &'a [FindingReview]) -> Matched<'a> {
    let mut out = Matched::default();
    for r in reviews {
        let at: Vec<usize> = ids.iter().enumerate().filter(|(_, id)| **id == r.finding_id).map(|(i, _)| i).collect();
        if at.is_empty() {
            out.lapsed.push(r);
        }
        for i in at {
            match r.decision {
                ReviewDecision::Accepted => out.accepted.push((i, r)),
                ReviewDecision::ToFix => out.to_fix.push((i, r)),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, RationalTime};
    use klipp_timeline::{Track, TrackKind};

    fn tc(s: f64) -> klipp_core::Timecode {
        RationalTime::from_secs_f64(s, 25)
    }

    fn review(id: &str, decision: ReviewDecision) -> FindingReview {
        FindingReview { finding_id: id.into(), decision, rationale: "deliberate".into(), at_secs: None, clip_id: None, revision: 3 }
    }

    /// #1932: a colour finding's id holds while its shot is unchanged —
    /// the clip moved along the timeline included — and changes when its
    /// grade, media range or group look does, so an acceptance lapses.
    #[test]
    fn a_colour_finding_id_follows_what_it_judged() {
        let mut pool = MediaPool::new();
        let water = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/klipp-1932/water.mov".into(), None));
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips.push(Clip::media(water, tc(10.0), tc(14.0), tc(0.0), tc(4.0)));
        seq.tracks = vec![v];
        let id_with = |seq: &Sequence, colour: &str| {
            let c = seq.all_clips().find(|c| c.media_id() == Some(water)).unwrap();
            colour_finding_id("black_point", c, secs(c) + 1.0, 25.0, colour_basis(seq, &pool, c, (secs(c) + 1.0, secs(c) + 3.0), colour))
        };
        let id = |seq: &Sequence| id_with(seq, "Rec709Linear|Bt1886|-");
        fn secs(c: &Clip) -> f64 {
            c.timeline_in.to_secs_f64()
        }
        let first = id(&seq);
        assert_eq!(first, id(&seq), "the same on every run");
        let (rule, clip, at) = locate(&seq, &first).expect("a finding id");
        assert_eq!((rule.as_str(), clip, at), ("black_point", Some(seq.tracks[0].clips[0].id), Some(11.0)));
        // Moved along the timeline: the same shot, the same id.
        let c = &mut seq.tracks[0].clips[0];
        (c.timeline_in, c.timeline_out) = (tc(20.0), tc(24.0));
        assert_eq!(first, id(&seq));
        // A different part of the file: a different shot.
        let mut trimmed = seq.clone();
        trimmed.tracks[0].clips[0].source_in = tc(1.0);
        assert_ne!(first, id(&trimmed));
        // Its picture changed: judged on something else.
        let mut placed = seq.clone();
        let moved = klipp_effects::BuiltinEffect::Transform { position_x: 40.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        placed.tracks[0].clips[0] = klipp_timeline::test_clip::TestClip::of(placed.tracks[0].clips[0].clone()).effect(moved).build();
        assert_ne!(first, id(&placed), "a picture change lapses the review");
        // A group look over it.
        let mut grouped = seq.clone();
        let group = klipp_timeline::GradeGroup { look: klipp_color::measure::LookTarget::preset("film_soft"), ..klipp_timeline::GradeGroup::new("warm") };
        grouped.tracks[0].clips[0].grade_group = Some(group.id);
        grouped.grade_groups.push(group);
        assert_ne!(first, id(&grouped));
        // What the renderer reads beside the clip: the timeline grade, the
        // track's levels, an adjustment layer above it over the shot, the
        // project's colour management.
        let mut timeline = seq.clone();
        timeline.timeline_graph = Some(klipp_timeline::Grade::default());
        assert_ne!(first, id(&timeline), "a timeline grade change lapses the review");
        let mut track = seq.clone();
        track.tracks[0].opacity = 0.5;
        assert_ne!(first, id(&track), "a track's levels");
        let mut layered = seq.clone();
        let mut over = Track::new("V2", TrackKind::Video);
        let mut adjust = Clip::media(klipp_core::MediaId::nil(), tc(20.0), tc(24.0), tc(0.0), tc(4.0));
        adjust.kind = klipp_timeline::ClipKind::AdjustmentLayer;
        over.clips.push(adjust);
        layered.tracks.insert(0, over);
        assert_ne!(first, id(&layered), "an adjustment layer over the shot");
        let mut below = seq.clone();
        let mut under = Track::new("V0", TrackKind::Video);
        let mut adjust = Clip::media(klipp_core::MediaId::nil(), tc(20.0), tc(24.0), tc(0.0), tc(4.0));
        adjust.kind = klipp_timeline::ClipKind::AdjustmentLayer;
        under.clips.push(adjust);
        below.tracks.push(under);
        assert_eq!(first, id(&below), "a layer under it does not touch it");
        assert_ne!(first, id_with(&seq, "Rec2020Linear|Bt1886|-"), "the project's working space");
        let mut renamed = grouped.clone();
        renamed.grade_groups[0].name = "golden".into();
        assert_eq!(id(&grouped), id(&renamed), "a group's name is not its picture");
    }

    /// Reviews sort a run's findings into accepted, to fix and the rest;
    /// a review no finding matches any more is lapsed, not dropped.
    #[test]
    fn reviews_are_matched_to_a_run_by_id() {
        let ids: Vec<String> = ["check/title_short/25/aaaaaaaaaaaa", "colour/black_point/x/0/bbbbbbbbbbbb", "sound/sound/50/cccccccccccc"].map(String::from).to_vec();
        let reviews = vec![review(&ids[1], ReviewDecision::Accepted), review(&ids[2], ReviewDecision::ToFix), review("check/flash/10/dddddddddddd", ReviewDecision::Accepted)];
        let m = match_reviews(&ids, &reviews);
        assert_eq!(m.accepted.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1]);
        assert_eq!(m.to_fix.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![2]);
        assert_eq!(m.lapsed.len(), 1);
        assert_eq!(m.lapsed[0].finding_id, "check/flash/10/dddddddddddd");
        // A craft finding's id moves with its words.
        let a = finding_id("check", "title_short", Some(1.0), 25.0, "\"Colour.\" has 1 word and 0.9 s to be read in");
        assert_eq!(a, finding_id("check", "title_short", Some(1.0), 25.0, "\"Colour.\" has 1 word and 0.9 s to be read in"));
        assert_ne!(a, finding_id("check", "title_short", Some(1.0), 25.0, "\"Colour.\" has 1 word and 1.1 s to be read in"));
        assert!(a.starts_with("check/title_short/25/"), "{a}");
        // A measured finding's numbers wobble from run to run: not in its id.
        let drop = |lufs: &str, under: &str| finding_id("check", "loudness_drop", Some(30.0), 25.0, &format!("the mix drops to {lufs} LUFS short-term from 30.0 to 36.0 s, {under} LU under the rest of the piece (-14)"));
        assert_eq!(drop("-31", "10"), drop("-32", "11"));
    }
}
