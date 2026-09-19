//! An [`EditPlan`] lowered to the ops that build it. The plan is the
//! paper edit (see `klipp_timeline::edit_plan`); this is where it meets
//! the timeline. Nothing here places a clip: the ops go through
//! [`crate::ops::resolve`] like any other, so what the dry run says is
//! what gets built, and the build is one undo step.
//!
//! The ops come out in three passes so that no step moves the track
//! indices a later step names: the picture (place, zoom), then the
//! joins, then the music, then the titles — a title's text track goes
//! on top and would shift every index after it, so it is made first
//! and named by kind.

use crate::ops::{Anchor, ClipRef, Op, TrackRef};
use klipp_media::MediaPool;
use klipp_timeline::{Brief, EditPlan, PlanSegment, Sequence, TrackKind};

/// A join with no length given is this long.
const DEFAULT_TRANSITION_SECS: f64 = 0.5;
/// A title with no hold given holds this long, or the segment if shorter.
const DEFAULT_TITLE_SECS: f64 = 3.0;

/// Why a plan cannot be stored, in a sentence with what would fix it.
pub fn validate(plan: &EditPlan, pool: &MediaPool, brief: Option<&Brief>) -> Result<(), String> {
    if plan.segments.is_empty() {
        return Err("an edit plan needs at least one segment".into());
    }
    for (i, s) in plan.segments.iter().enumerate() {
        let n = i + 1;
        let item = pool.get(&s.media_id).ok_or_else(|| format!("segment {n}: no media item {}; media ids are in list_media", s.media_id.0))?;
        let dur = item.duration.to_secs_f64();
        if !s.source_in_secs.is_finite() || !s.source_out_secs.is_finite() || s.source_in_secs < 0.0 || s.source_out_secs <= s.source_in_secs {
            return Err(format!("segment {n}: source {:.2}-{:.2} s is not a stretch of {}; give in and out in order", s.source_in_secs, s.source_out_secs, item.filename));
        }
        if s.source_out_secs > dur + 1e-6 {
            return Err(format!("segment {n}: source {:.2}-{:.2} s runs past the end of {} ({dur:.2} s)", s.source_in_secs, s.source_out_secs, item.filename));
        }
        if !s.feature.trim().is_empty() {
            match brief {
                Some(b) if b.feature(&s.feature).is_some() => {}
                Some(b) => {
                    let known: Vec<&str> = b.features.iter().map(|f| f.title.as_str()).collect();
                    return Err(format!("segment {n}: \"{}\" is not a feature in the brief; the brief's features are: {}; use one as it is spelt there, or add it to the brief, or leave the feature out", s.feature, if known.is_empty() { "none".to_string() } else { known.join(", ") }));
                }
                None => return Err(format!("segment {n} names the feature \"{}\" and there is no brief; set one with the features first, or leave the feature out", s.feature)),
            }
        }
        if let Some(t) = &s.title {
            if t.text.trim().is_empty() {
                return Err(format!("segment {n}: a title needs text"));
            }
            let style = t.style.trim();
            if !(style.is_empty() || matches!(style, "heading" | "callout" | "card")) {
                return Err(format!("segment {n}: \"{style}\" is not a title style; use heading, callout or card"));
            }
            if t.secs.is_some_and(|x| !x.is_finite() || x < 0.0) {
                return Err(format!("segment {n}: a title's secs must be zero or more"));
            }
        }
        if let Some(z) = &s.zoom {
            if !(1.0..=4.0).contains(&z.from) || !(1.0..=4.0).contains(&z.to) {
                return Err(format!("segment {n}: a zoom goes from 1 to 4; 1 is as shot"));
            }
        }
        if i == 0 && s.transition.is_some() {
            return Err("segment 1 has nothing before it to join to; a transition is on the segment it leads into".into());
        }
    }
    if let Some(m) = &plan.music {
        pool.get(&m.media_id).ok_or_else(|| format!("music: no media item {}; media ids are in list_media", m.media_id.0))?;
        if m.duck_db.is_some_and(|d| !(0.0..=40.0).contains(&d)) {
            return Err("music: duck_db is how far the music drops, 0 to 40".into());
        }
    }
    Ok(())
}

/// Where each segment's clip will sit on the picture track, as the
/// midpoint of its stretch: what `ClipRef::At` needs, and safe against a
/// frame of snapping at either edge.
fn midpoints(plan: &EditPlan, start: f64) -> Vec<f64> {
    let mut at = start;
    plan.segments.iter().map(|s| {
        let d = s.duration_secs();
        let mid = at + d / 2.0;
        at += d;
        mid
    }).collect()
}

/// The ops that build `plan` onto `seq`: after the last clip on the
/// first picture track (from its start when empty).
pub fn to_ops(plan: &EditPlan, seq: &Sequence, pool: &MediaPool, brief: Option<&Brief>) -> Vec<Op> {
    let mut ops = Vec::new();
    let has_video = seq.tracks.iter().any(|t| t.kind == TrackKind::Video);
    let has_text = seq.tracks.iter().any(|t| t.kind == TrackKind::Text);
    let has_audio = seq.tracks.iter().any(|t| t.kind == TrackKind::Audio);
    // A picture track to build on, made on top if there is none: every
    // index below it counts from one.
    if !has_video {
        ops.push(Op::AddTrack { kind: "video".into(), name: None });
    }
    let video = if has_video { seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap_or(0) } else { 0 };
    let start = seq.tracks.get(video).filter(|_| has_video).and_then(|t| t.clips.iter().map(|c| c.timeline_out.to_secs_f64()).fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))).unwrap_or(0.0);
    let mids = midpoints(plan, start);
    let clip = |i: usize| ClipRef::At { track: video, secs: mids[i] };

    // The picture, end to end.
    for (i, s) in plan.segments.iter().enumerate() {
        ops.push(Op::Place {
            media_id: s.media_id,
            track: TrackRef::Index(video),
            at: Anchor::TrackEnd(TrackRef::Index(video)),
            source_in: Some(s.source_in_secs),
            source_out: Some(s.source_out_secs),
            insert: false,
            with_audio: s.with_audio,
        });
        if let Some(z) = &s.zoom {
            ops.push(Op::Zoom { clip_id: clip(i), from: z.from, to: z.to, x: z.x, y: z.y });
        }
    }
    // The joins: a transition sits on the outgoing clip, the one before.
    for (i, s) in plan.segments.iter().enumerate().skip(1) {
        if let Some(kind) = &s.transition {
            ops.push(Op::Transition { clip_id: clip(i - 1), kind: kind.clone(), secs: DEFAULT_TRANSITION_SECS });
        }
    }
    // The music under it all, on an audio track of its own at the
    // bottom, where making it moves no other index. A segment's own
    // sound goes on the first audio track, made at the bottom too when
    // there is none: that comes first, so the music track sits below it.
    if let Some(m) = &plan.music {
        let total = plan.duration_secs();
        let file = pool.get(&m.media_id).map(|it| it.duration.to_secs_f64()).unwrap_or(total);
        let any_with_audio = plan.segments.iter().any(|s| s.with_audio);
        let music_track = seq.tracks.len() + usize::from(!has_video) + usize::from(!has_audio && any_with_audio);
        ops.push(Op::AddTrack { kind: "audio".into(), name: Some("Music".into()) });
        let laid = total.min(file);
        ops.push(Op::Place {
            media_id: m.media_id,
            track: TrackRef::Index(music_track),
            at: Anchor::Secs(start),
            source_in: Some(0.0),
            source_out: Some(laid),
            insert: false,
            with_audio: true,
        });
        // Ducked under the segments' own sound; with none, there is
        // nothing to duck under and the op would refuse.
        if any_with_audio {
            ops.push(Op::Duck { clip_id: ClipRef::At { track: music_track, secs: start + laid / 2.0 }, by_db: m.duck_db.unwrap_or(9.0), fade_secs: 0.5 });
        }
    }
    // The titles, last: a text track made on top shifts every index, so
    // it is made before any title and the picture is named one lower.
    let any_title = plan.segments.iter().any(|s| s.title.is_some());
    if any_title {
        let mut video_below = video;
        if !has_text {
            ops.push(Op::AddTrack { kind: "text".into(), name: Some("Titles".into()) });
            video_below += 1;
        }
        let color = brief.and_then(|b| b.primary_color()).map(str::to_string);
        for (i, s) in plan.segments.iter().enumerate() {
            if let Some(t) = &s.title {
                let hold = t.secs.filter(|x| *x > 0.0).unwrap_or(DEFAULT_TITLE_SECS).min(s.duration_secs());
                ops.push(title_op(s, &t.text, hold, t.style.trim(), color.as_deref(), ClipRef::At { track: video_below, secs: mids[i] }));
            }
        }
    }
    ops
}

/// A title in one of the plan's three styles: a heading in the upper
/// third that rises in, a callout low and backed that fades in, or a
/// full-frame card in the brand's colour.
fn title_op(_s: &PlanSegment, text: &str, secs: f64, style: &str, color: Option<&str>, over: ClipRef) -> Op {
    let (x, y, size, bold, text_color, backing, full_frame, animate, animate_secs) = match style {
        "callout" => (0.5, 0.85, 40.0, false, None, Some(0.6), None, "fade", 0.3),
        "card" => (0.5, 0.5, 84.0, true, Some("#ffffff".to_string()), None, Some(color.unwrap_or("#101418").to_string()), "fade", 0.4),
        _ => (0.5, 0.22, 72.0, true, color.map(str::to_string), None, None, "rise", 0.4),
    };
    Op::Title {
        text: text.to_string(),
        at: Anchor::ClipStart(over),
        secs,
        track: Some(TrackRef::Kind("text".into())),
        x: Some(x), y: Some(y), size: Some(size), color: text_color, font: None, bold: Some(bold), align: None,
        backing, backing_color: None, full_frame, outline: None, outline_color: None,
        glow: None, glow_color: None, gradient: None,
        animate: Some(animate.into()), animate_secs: Some(animate_secs), stagger: None,
        loop_effect: None, glitch: None, chromatic: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::resolve;
    use klipp_core::{Fps, MediaId, Timecode};
    use klipp_timeline::{Feature, PlanMusic, PlanTitle, PlanZoom};

    fn tc(secs: f64) -> Timecode {
        Timecode { value: (secs * 25.0).round() as i64, rate: 25 }
    }

    fn fixture() -> (Sequence, MediaPool, MediaId, MediaId) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/screen.mov".into(), None);
        item.duration = tc(60.0);
        item.video_codec = "h264".into();
        item.has_audio = true;
        let screen = pool.add(item);
        let mut music = klipp_media::MediaItem::synthetic_for_test("/tmp/bed.wav".into(), None);
        music.duration = tc(120.0);
        music.has_audio = true;

        let bed = pool.add(music);
        (seq, pool, screen, bed)
    }

    fn seg(media: MediaId, a: f64, b: f64) -> PlanSegment {
        PlanSegment { feature: String::new(), media_id: media, source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, zoom: None }
    }

    #[test]
    fn a_plan_builds_end_to_end_with_its_titles_music_and_joins() {
        let (seq, pool, screen, bed) = fixture();
        let brief = Brief { features: vec![Feature { title: "Search".into(), description: String::new(), rank: 1 }], brand_colors: vec!["#ff6600".into()], ..Default::default() };
        let plan = EditPlan {
            segments: vec![
                PlanSegment { title: Some(PlanTitle { text: "Klipp".into(), secs: Some(2.0), style: "card".into() }), ..seg(screen, 0.0, 3.0) },
                PlanSegment { feature: "Search".into(), with_audio: true, title: Some(PlanTitle { text: "Search".into(), secs: None, style: String::new() }), zoom: Some(PlanZoom { from: 1.0, to: 1.15, x: 0.0, y: 0.0 }), ..seg(screen, 10.0, 16.0) },
                PlanSegment { transition: Some("cross_dissolve".into()), title: Some(PlanTitle { text: "Try it".into(), secs: Some(9.0), style: "callout".into() }), ..seg(screen, 30.0, 34.0) },
            ],
            music: Some(PlanMusic { media_id: bed, duck_db: None }),
            approved: true,
            notes: String::new(),
        };
        validate(&plan, &pool, Some(&brief)).unwrap();
        let ops = to_ops(&plan, &seq, &pool, Some(&brief));
        // Picture first, then the join, the music, the text track, the titles.
        let kinds: Vec<&str> = ops.iter().map(|o| match o { Op::Place { .. } => "place", Op::Zoom { .. } => "zoom", Op::Transition { .. } => "transition", Op::Duck { .. } => "duck", Op::AddTrack { kind, .. } => kind.as_str(), Op::Title { .. } => "title", _ => "?" }).collect();
        assert_eq!(kinds, vec!["place", "place", "zoom", "place", "transition", "audio", "place", "duck", "text", "title", "title", "title"]);
        let built = resolve(&seq, &pool, &ops).unwrap();
        assert!((built.duration_secs - 13.0).abs() < 1e-6, "{}", built.text());
        // The titles land where their segments start, on the new text track.
        let titles: Vec<(f64, f64)> = built.steps.iter().filter(|s| matches!(s.op, Op::Title { .. })).map(|s| (s.after[0].timeline_in_secs, s.after[0].timeline_out_secs)).collect();
        assert_eq!(titles, vec![(0.0, 2.0), (3.0, 6.0), (9.0, 13.0)], "{}", built.text());
        assert!(matches!(&ops[9], Op::Title { full_frame: Some(c), .. } if c == "#ff6600"), "the card takes the brand colour");
        // The bed is on its own track under the segment's sound, and ducked there.
        let music = built.steps.iter().find(|s| matches!(&s.op, Op::Place { media_id, .. } if *media_id == bed)).unwrap();
        assert_eq!(music.after[0].track, 2, "{}", built.text());
        assert!(built.steps.iter().any(|s| matches!(s.op, Op::Duck { .. })));

        // With no sound of its own in the cut, the music is laid but not ducked.
        let mut silent = plan.clone();
        silent.segments.iter_mut().for_each(|s| s.with_audio = false);
        let ops = to_ops(&silent, &seq, &pool, Some(&brief));
        assert!(!ops.iter().any(|o| matches!(o, Op::Duck { .. })));
        resolve(&seq, &pool, &ops).unwrap();
    }

    #[test]
    fn a_plan_that_cannot_be_built_says_which_segment_and_why() {
        let (_, pool, screen, _) = fixture();
        let brief = Brief { features: vec![Feature { title: "Search".into(), ..Default::default() }], ..Default::default() };
        let bad = |p: EditPlan| validate(&p, &pool, Some(&brief)).unwrap_err();
        assert!(bad(EditPlan::default()).contains("at least one"));
        assert!(bad(EditPlan { segments: vec![seg(screen, 50.0, 70.0)], ..Default::default() }).contains("past the end"));
        assert!(bad(EditPlan { segments: vec![seg(screen, 5.0, 5.0)], ..Default::default() }).contains("in order"));
        assert!(bad(EditPlan { segments: vec![PlanSegment { feature: "Filters".into(), ..seg(screen, 1.0, 2.0) }], ..Default::default() }).contains("Search"));
        assert!(bad(EditPlan { segments: vec![PlanSegment { transition: Some("cross_dissolve".into()), ..seg(screen, 1.0, 2.0) }], ..Default::default() }).contains("nothing before it"));
        assert!(bad(EditPlan { segments: vec![PlanSegment { title: Some(PlanTitle { text: "x".into(), secs: None, style: "banner".into() }), ..seg(screen, 1.0, 2.0) }], ..Default::default() }).contains("heading, callout or card"));
        let unknown = EditPlan { segments: vec![seg(MediaId::new(), 1.0, 2.0)], ..Default::default() };
        assert!(bad(unknown).contains("no media item"));
    }

    #[test]
    fn a_plan_builds_after_what_is_already_down() {
        let (mut seq, pool, screen, _) = fixture();
        seq.tracks[0].clips.push(klipp_timeline::Clip::media(screen, tc(0.0), tc(4.0), tc(20.0), tc(24.0)));
        let plan = EditPlan { segments: vec![PlanSegment { title: Some(PlanTitle { text: "Next".into(), secs: Some(1.0), style: String::new() }), ..seg(screen, 0.0, 2.0) }], ..Default::default() };
        let built = resolve(&seq, &pool, &to_ops(&plan, &seq, &pool, None)).unwrap();
        let place = built.steps.iter().find(|s| matches!(s.op, Op::Place { .. })).unwrap();
        assert_eq!((place.after[0].timeline_in_secs, place.after[0].timeline_out_secs), (4.0, 6.0));
        let title = built.steps.iter().find(|s| matches!(s.op, Op::Title { .. })).unwrap();
        assert_eq!((title.after[0].timeline_in_secs, title.after[0].timeline_out_secs), (4.0, 5.0));
    }
}
