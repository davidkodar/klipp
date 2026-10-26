//! An [`EditPlan`] lowered to the ops that build it. The plan is the
//! paper edit (see `klipp_timeline::edit_plan`); this is where it meets
//! the timeline. Nothing here places a clip: the ops go through
//! [`crate::ops::resolve`] like any other, so what the dry run says is
//! what gets built, and the build is one undo step.
//!
//! The ops come out in a fixed order so that no step moves the track
//! indices a later step names: the picture (place, zoom), then the
//! joins, then the music, then the sound effects, then the titles. A
//! text track made on top would shift every index after it, so the
//! titles go last and are named by kind rather than by index.

use crate::ops::{Anchor, ClipRef, ComposePose, Op, TrackRef};
use klipp_media::MediaPool;
use klipp_timeline::{Brief, EditPlan, PlanTitle, Sequence, TrackKind, TypeStyle};

/// A montage run uses beat-sized cuts and recognition rather than reading.
/// A suffix permits distinct runs, for example `montage: opening`.
pub fn is_montage(segment: &klipp_timeline::PlanSegment) -> bool {
    segment.section.trim().split(':').next().is_some_and(|name| name.trim().eq_ignore_ascii_case("montage"))
}

/// Explicit feature names take precedence. In a montage, a title that
/// exactly names a brief feature is also a feature label.
pub fn covers_feature(segment: &klipp_timeline::PlanSegment, feature: &str) -> bool {
    if !segment.feature.trim().is_empty() {
        return segment.feature.trim().eq_ignore_ascii_case(feature.trim());
    }
    is_montage(segment) && segment.title.as_ref().is_some_and(|t| t.text.trim().eq_ignore_ascii_case(feature.trim()))
}

/// Whether a string is `#rrggbb`.
fn is_hex_colour(s: &str) -> bool {
    let s = s.trim();
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// A join with no length given is this long.
const DEFAULT_TRANSITION_SECS: f64 = 0.5;

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
        if !item.is_still && s.source_out_secs > dur + 1e-6 {
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
            check_title(t, t.duration_secs(s.duration_secs()), brief, &format!("segment {n}"))?;
        }
        check_layers(s, pool, brief, n)?;
        if let Some(x) = &s.sfx {
            let item = pool.get(&x.media_id).ok_or_else(|| format!("segment {n}: sfx: no media item {}; media ids are in list_media, and search_sfx brings one in", x.media_id.0))?;
            if !item.has_audio {
                return Err(format!("segment {n}: sfx: {} has no sound", item.filename));
            }
            let at = x.at_secs.unwrap_or(0.0);
            if !at.is_finite() || at < 0.0 || at >= s.duration_secs() {
                return Err(format!("segment {n}: sfx: at_secs is seconds into the segment, 0 to {:.2}", s.duration_secs()));
            }
            if x.source_in_secs.is_some_and(|v| !v.is_finite() || v < 0.0 || v >= item.duration.to_secs_f64()) {
                return Err(format!("segment {n}: sfx: source_in_secs is inside {} ({:.2} s)", item.filename, item.duration.to_secs_f64()));
            }
            if x.secs.is_some_and(|v| !v.is_finite() || v <= 0.0) {
                return Err(format!("segment {n}: sfx: secs must be more than zero"));
            }
        }
        if let Some(z) = &s.zoom {
            if !(1.0..=4.0).contains(&z.from) || !(1.0..=4.0).contains(&z.to) {
                return Err(format!("segment {n}: a zoom goes from 1 to 4; 1 is as shot"));
            }
        }
        if let Some(pl) = &s.placement {
            if ![pl.x, pl.y, pl.width, pl.height, pl.rotation_deg].iter().all(|v| v.is_finite()) {
                return Err(format!("segment {n}: a placement's numbers must be real"));
            }
            if !(0.0..=4.0).contains(&pl.width) || !(0.0..=4.0).contains(&pl.height) || pl.width <= 0.0 || pl.height <= 0.0 {
                return Err(format!("segment {n}: a placement's width and height are fractions of the frame, above 0 and up to 4"));
            }
            // Off-frame is allowed, so a card can start outside and come in.
            if !(-2.0..=3.0).contains(&pl.x) || !(-2.0..=3.0).contains(&pl.y) {
                return Err(format!("segment {n}: a placement's x and y are the picture's centre, -2 to 3; 0.5,0.5 is the middle of the frame"));
            }
            if pl.opacity.is_some_and(|o| !o.is_finite() || !(0.0..=1.0).contains(&o)) {
                return Err(format!("segment {n}: a placement's opacity is 0 to 1"));
            }
        }
        if let Some(b) = &s.background {
            if b.generator.trim().is_empty() {
                return Err(format!("segment {n}: a background needs a generator by name"));
            }
            crate::generators::background_data(b, brief.map(|b| b.brand_colors.as_slice()).unwrap_or(&[])).map_err(|e| format!("segment {n}: {e}"))?;
            for c in &b.colors {
                if !is_hex_colour(c) {
                    return Err(format!("segment {n}: \"{c}\" is not a colour; use #rrggbb"));
                }
            }
        }
        if i == 0 && s.transition.is_some() {
            return Err("segment 1 has nothing before it to join to; a transition is on the segment it leads into".into());
        }
    }
    let mut seen_sections: Vec<&str> = Vec::new();
    for (i, s) in plan.segments.iter().enumerate() {
        let name = s.section.trim();
        if name.is_empty() {
            continue;
        }
        let follows_same = i > 0 && plan.segments[i - 1].section.trim() == name;
        if !follows_same && seen_sections.contains(&name) {
            return Err(format!("segment {}: the \"{name}\" section starts again after something else; a section is a run of segments, so put them together", i + 1));
        }
        if !follows_same {
            seen_sections.push(name);
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

/// Why a title `hold` seconds long can't be built, said of `place`
/// ("segment 3", "segment 3, layer 2").
fn check_title(t: &PlanTitle, hold: f64, brief: Option<&Brief>, place: &str) -> Result<(), String> {
    if t.text.trim().is_empty() {
        return Err(format!("{place}: a title needs text"));
    }
    let style = t.style.trim();
    if !(style.is_empty() || crate::title_style::look(brief, style).is_some()) {
        return Err(format!("{place}: \"{style}\" is not a title style; the styles are {}", crate::title_style::names(brief).join(", ")));
    }
    if t.secs.is_some_and(|x| !x.is_finite() || x < 0.0) {
        return Err(format!("{place}: a title's secs must be zero or more"));
    }
    if t.glow.is_some_and(|g| !(0.0..=1.0).contains(&g)) {
        return Err(format!("{place}: a title's glow is 0 to 1"));
    }
    let motion_style = crate::title_style::look(brief, if style.is_empty() { "heading" } else { style }).unwrap_or_default();
    crate::title_style::effective_motion(&motion_style, t.enter.as_ref(), t.exit.as_ref(), t.animate.as_deref(), t.animate_secs, t.stagger.as_deref(), hold)
        .map_err(|e| format!("{place}, style {:?}: {e}", motion_style.name))?;
    if [t.x, t.y].iter().flatten().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)) {
        return Err(format!("{place}: a title's x and y are fractions of the frame, 0 to 1"));
    }
    Ok(())
}

/// The most layers a scene holds.
const MAX_LAYERS: usize = 16;

/// Why segment `n`'s layers can't be built (#1926): each must start
/// and end inside the segment and hold long enough to see, its motion
/// must fit inside it, and what it shows must be there.
fn check_layers(s: &klipp_timeline::PlanSegment, pool: &MediaPool, brief: Option<&Brief>, n: usize) -> Result<(), String> {
    use klipp_timeline::{PlanLayer, LAYER_MOTIONS};
    if s.layers.len() > MAX_LAYERS {
        return Err(format!("segment {n}: {} layers; a scene holds at most {MAX_LAYERS}, so split it into two segments", s.layers.len()));
    }
    let d = s.duration_secs();
    for (k, layer) in s.layers.iter().enumerate() {
        let place = format!("segment {n}, layer {}", k + 1);
        let at = layer.at_secs();
        if !at.is_finite() || at < 0.0 || at >= d {
            return Err(format!("{place}: at_secs is seconds into the segment, 0 to below its {d:.2} s"));
        }
        let (from, to) = layer.span(d);
        if !to.is_finite() || to > d + 1e-6 {
            return Err(format!("{place}: it runs to {to:.2} s, past the segment's end at {d:.2} s; start it sooner or hold it shorter"));
        }
        let held = to - from;
        if held < 0.2 {
            return Err(format!("{place}: it holds {held:.2} s; hold a layer at least 0.2 s"));
        }
        match layer {
            PlanLayer::Media { media_id, source_in_secs, placement, enter, exit, .. } => {
                let item = pool.get(media_id).ok_or_else(|| format!("{place}: no media item {}; media ids are in list_media", media_id.0))?;
                let from_file = source_in_secs.unwrap_or(0.0);
                if !from_file.is_finite() || from_file < 0.0 {
                    return Err(format!("{place}: source_in_secs is seconds into {}, 0 or more", item.filename));
                }
                if !item.is_still && from_file + held > item.duration.to_secs_f64() + 1e-6 {
                    return Err(format!("{place}: {:.2} s from {:.2} s runs past the end of {} ({:.2} s)", held, from_file, item.filename, item.duration.to_secs_f64()));
                }
                if let Some(p) = placement {
                    if ![p.x, p.y, p.width, p.height, p.rotation_deg].iter().all(|v| v.is_finite()) {
                        return Err(format!("{place}: a placement's numbers must be real"));
                    }
                    if (p.width - p.height).abs() > 1e-4 {
                        return Err(format!("{place}: width {:.2} and height {:.2} differ; a card is scaled evenly, so give the same for both", p.width, p.height));
                    }
                    if !(0.01..=4.0).contains(&p.width) {
                        return Err(format!("{place}: a card's width is a fraction of the frame, 0.01 to 4"));
                    }
                    if !(-1.0..=2.0).contains(&p.x) || !(-1.0..=2.0).contains(&p.y) {
                        return Err(format!("{place}: a card's x and y are its centre, -1 to 2; 0.5,0.5 is the middle of the frame"));
                    }
                    if !(-360.0..=360.0).contains(&p.rotation_deg) {
                        return Err(format!("{place}: a card's turn is -360 to 360 degrees"));
                    }
                    if p.opacity.is_some_and(|o| !o.is_finite() || !(0.0..=1.0).contains(&o)) {
                        return Err(format!("{place}: a card's opacity is 0 to 1"));
                    }
                }
                let mut moving = 0.0;
                for (m, way) in [(enter, "enter"), (exit, "exit")] {
                    let Some(m) = m else { continue };
                    if !LAYER_MOTIONS.contains(&m.kind.as_str()) {
                        return Err(format!("{place}: {way} \"{}\" is not a card motion; use {}", m.kind, LAYER_MOTIONS.join(", ")));
                    }
                    let secs = m.duration_secs();
                    if !secs.is_finite() || secs <= 0.0 {
                        return Err(format!("{place}: {way} secs must be more than 0"));
                    }
                    moving += secs;
                }
                if moving > held + 1e-6 {
                    return Err(format!("{place}: its entrance and exit take {moving:.2} s and it holds {held:.2} s; shorten them or hold it longer"));
                }
            }
            PlanLayer::Title { title, .. } => check_title(title, held, brief, &place)?,
            PlanLayer::Connector { from: a, to: b, color, stroke_width, reveal_secs, .. } => {
                if a.iter().chain(b).any(|v| !v.is_finite() || !(-1.0..=2.0).contains(v)) {
                    return Err(format!("{place}: a connector's from and to are fractions of the frame, -1 to 2"));
                }
                if a == b {
                    return Err(format!("{place}: a connector from a point to itself draws nothing; give from and to apart"));
                }
                if let Some(c) = color.as_deref().filter(|c| !is_hex_colour(c)) {
                    return Err(format!("{place}: \"{c}\" is not a colour; use #rrggbb"));
                }
                if stroke_width.is_some_and(|w| !w.is_finite() || !(0.5..=100.0).contains(&w)) {
                    return Err(format!("{place}: a connector's stroke_width is 0.5 to 100, pixels of a 1080 frame"));
                }
                let reveal = reveal_secs.unwrap_or(DEFAULT_REVEAL_SECS);
                if !reveal.is_finite() || reveal < 0.0 || reveal > held + 1e-6 {
                    return Err(format!("{place}: reveal_secs is 0 to the {held:.2} s it holds"));
                }
            }
        }
    }
    Ok(())
}

/// A connector with no reveal length given is drawn on this fast.
const DEFAULT_REVEAL_SECS: f64 = 0.6;

/// The level each layer of the plan takes in its group (#1926), and
/// how many levels each group needs: cards and connectors on one set of
/// tracks, titles on another above them. A layer goes on the level
/// above every earlier layer of its group it overlaps, so layers that
/// overlap never share a track and a later one is always above.
fn layer_levels(plan: &EditPlan) -> (Vec<Vec<usize>>, usize, usize) {
    let mut levels = Vec::new();
    let (mut cards, mut titles) = (0, 0);
    for s in &plan.segments {
        let d = s.duration_secs();
        let spans: Vec<_> = s.layers.iter().map(|l| l.span(d)).collect();
        let title = |k: usize| matches!(s.layers[k], klipp_timeline::PlanLayer::Title { .. });
        let mut mine: Vec<usize> = Vec::new();
        for k in 0..s.layers.len() {
            let level = (0..k)
                .filter(|&j| title(j) == title(k) && spans[j].0.max(spans[k].0) < spans[j].1.min(spans[k].1) - 1e-6)
                .map(|j| mine[j] + 1)
                .max()
                .unwrap_or(0);
            if title(k) { titles = titles.max(level + 1) } else { cards = cards.max(level + 1) }
            mine.push(level);
        }
        levels.push(mine);
    }
    (levels, cards, titles)
}

/// A card's pose `kind` puts it in to come in or go: offset or shrunk
/// from `rest`, and unseen.
fn offstage(rest: &ComposePose, kind: &str) -> ComposePose {
    let mut p = *rest;
    p.opacity = 0.0;
    let nudge = |v: Option<f32>, by: f32| v.map(|v| (v + by).clamp(-1.0, 2.0));
    match kind {
        "rise" => p.y = nudge(p.y, 0.08),
        "drop" => p.y = nudge(p.y, -0.08),
        "slide_left" => p.x = nudge(p.x, 0.15),
        "slide_right" => p.x = nudge(p.x, -0.15),
        "scale_up" => {
            p.width = p.width.map(|w| (w * 0.6).max(0.01));
            p.height = p.width;
        }
        _ => {}
    }
    p
}

/// The poses that place a card and move it in and out (#1926): resting
/// where `placement` says, coming in from its entrance's pose over the
/// entrance and leaving to its exit's over the exit, measured from its
/// end. `None` when it neither moves nor leaves the full frame.
fn card_poses(placement: Option<&klipp_timeline::PlanPlacement>, enter: Option<&klipp_timeline::LayerMotion>, exit: Option<&klipp_timeline::LayerMotion>) -> Option<Vec<ComposePose>> {
    let p = placement.cloned().unwrap_or_default();
    if p.fills_frame() && enter.is_none() && exit.is_none() {
        return None;
    }
    let rest = ComposePose { secs: 0.0, x: Some(p.x), y: Some(p.y), width: Some(p.width), height: Some(p.width), rotation_deg: Some(p.rotation_deg), opacity: p.opacity.unwrap_or(1.0), ..Default::default() };
    let ease = |m: &klipp_timeline::LayerMotion| m.ease.unwrap_or(klipp_core::keyframe::Ease::InOut);
    let mut poses = Vec::new();
    match enter {
        Some(m) => {
            poses.push(offstage(&rest, &m.kind));
            poses.push(ComposePose { secs: m.duration_secs(), ease: ease(m), ..rest });
        }
        None => poses.push(rest),
    }
    if let Some(m) = exit {
        poses.push(ComposePose { secs: 0.0, from_end_secs: Some(m.duration_secs()), ..rest });
        poses.push(ComposePose { secs: 0.0, from_end_secs: Some(0.0), ease: ease(m), ..offstage(&rest, &m.kind) });
    }
    Some(poses)
}

/// Round every segment to the music's bars: a length under a bar goes
/// to the nearest half bar (at least one), longer ones to whole bars;
/// montage sections use beats (one quarter of a bar) instead;
/// a stretch that would run past its file is shortened by whole bars.
/// The bed's in-point goes to its first beat when none was given. What
/// changed, one line per segment.
pub fn snap_to_bars(plan: &mut EditPlan, bar_secs: f64, first_beat_secs: f64, pool: &MediaPool) -> Vec<String> {
    let mut notes = Vec::new();
    if !(bar_secs > 0.05) {
        return notes;
    }
    for (i, s) in plan.segments.iter_mut().enumerate() {
        let dur = s.duration_secs();
        let unit = if is_montage(s) { bar_secs / 4.0 } else if dur < bar_secs { bar_secs / 2.0 } else { bar_secs };
        let mut want = ((dur / unit).round().max(1.0)) * unit;
        if let Some(file) = pool.get(&s.media_id).filter(|it| !it.is_still).map(|it| it.duration.to_secs_f64()) {
            while s.source_in_secs + want > file + 1e-6 && want > unit + 1e-9 {
                want -= unit;
            }
        }
        if (want - dur).abs() > 0.01 {
            notes.push(format!("segment {}: {:.2} s \u{2192} {:.2} s ({} of {:.2} s)", i + 1, dur, want, if is_montage(s) { "beats" } else if unit < bar_secs { "half bars" } else { "bars" }, unit));
            s.source_out_secs = s.source_in_secs + want;
        }
    }
    if let Some(m) = plan.music.as_mut() {
        if m.source_in_secs.is_none() && first_beat_secs > 0.0 {
            m.source_in_secs = Some(first_beat_secs);
            notes.push(format!("music: starts on its first beat at {first_beat_secs:.3} s"));
        }
    }
    notes
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
            sync_source_secs: None,
        });
        if let Some(z) = &s.zoom {
            if (z.from - z.to).abs() < 1e-4 {
                // No movement asked for: a reframe, held.
                ops.push(Op::Reframe { clip_id: clip(i), scale: Some(z.from), x: Some(z.x), y: Some(z.y) });
            } else {
                ops.push(Op::Zoom { clip_id: clip(i), from: z.from, to: z.to, x: z.x, y: z.y, ease: z.ease });
            }
        }
        // A placement that fills the frame is what a clip does anyway, so
        // only a real one is lowered: an op that says nothing would be
        // refused, and refusing a plan for saying the default is unkind.
        if let Some(p) = s.placement.as_ref().filter(|p| !p.fills_frame()) {
            ops.push(Op::Compose {
                clip_id: clip(i),
                poses: vec![ComposePose {
                    secs: 0.0,
                    x: Some(p.x),
                    y: Some(p.y),
                    width: Some(p.width),
                    height: Some(p.height),
                    rotation_deg: Some(p.rotation_deg),
                    opacity: p.opacity.unwrap_or(1.0),
                    ..Default::default()
                }], flip_x: None, flip_y: None
            });
        }
        // The delivery's size is the brief's, not the first file's: the
        // first place sets the sequence to the footage, this puts it right.
        if i == 0 && start == 0.0 && seq.tracks.iter().all(|t| t.clips.is_empty()) {
            if let Some((w, h)) = brief.and_then(|b| b.width.zip(b.height)) {
                ops.push(Op::Format { width: w, height: h, fps: None });
            }
        }
    }
    // The joins: a transition sits on the outgoing clip, the one before.
    for (i, s) in plan.segments.iter().enumerate().skip(1) {
        if let Some(kind) = &s.transition {
            ops.push(Op::Transition { clip_id: clip(i - 1), kind: kind.clone(), secs: DEFAULT_TRANSITION_SECS, remove: false, pieces: None });
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
        let from = m.source_in_secs.unwrap_or(0.0).max(0.0);
        let laid = total.min((file - from).max(0.0));
        ops.push(Op::Place {
            media_id: m.media_id,
            track: TrackRef::Index(music_track),
            at: Anchor::Secs(start),
            source_in: Some(from),
            source_out: Some(from + laid),
            insert: false,
            with_audio: true,
            sync_source_secs: None,
        });
        // The bed is what it is to the mix, and at the level asked for.
        ops.push(Op::Role { clip_id: ClipRef::At { track: music_track, secs: start + laid / 2.0 }, role: "music".into() });
        if m.volume_db.is_some() || m.fade_in_secs.is_some() || m.fade_out_secs.is_some() {
            ops.push(Op::Audio { clip_id: Some(ClipRef::At { track: music_track, secs: start + laid / 2.0 }), track: None, from: None, to: None, volume_db: m.volume_db, gain_db: None, pan: None, fade_in: m.fade_in_secs.map(|f| f as f32), fade_out: m.fade_out_secs.map(|f| f as f32) });
        }
        // Ducked under the segments' own sound; with none, there is
        // nothing to duck under and the op would refuse.
        if any_with_audio {
            ops.push(Op::Duck { clip_id: ClipRef::At { track: music_track, secs: start + laid / 2.0 }, by_db: m.duck_db.unwrap_or(9.0), fade_secs: 0.5, bridge_secs: 2.0 });
        }
    }
    // The sound effects, on a track of their own below the music: each
    // where its segment starts plus its offset, marked as an effect so
    // the mix and the check treat it as one.
    if plan.segments.iter().any(|s| s.sfx.is_some()) {
        let any_with_audio = plan.segments.iter().any(|s| s.with_audio);
        let sfx_track = seq.tracks.len() + usize::from(!has_video) + usize::from(!has_audio && any_with_audio) + usize::from(plan.music.is_some());
        ops.push(Op::AddTrack { kind: "audio".into(), name: Some("SFX".into()) });
        let mut at = start;
        for s in &plan.segments {
            if let Some(x) = &s.sfx {
                let file = pool.get(&x.media_id).map(|it| it.duration.to_secs_f64()).unwrap_or(8.0);
                let from = x.source_in_secs.unwrap_or(0.0).max(0.0);
                let secs = x.secs.unwrap_or((file - from).min(8.0)).min((file - from).max(0.05));
                let when = at + x.at_secs.unwrap_or(0.0);
                ops.push(Op::Place {
                    media_id: x.media_id,
                    track: TrackRef::Index(sfx_track),
                    at: Anchor::Secs(when),
                    source_in: Some(from),
                    source_out: Some(from + secs),
                    insert: false,
                    with_audio: true,
                    sync_source_secs: None,
                });
                ops.push(Op::Role { clip_id: ClipRef::At { track: sfx_track, secs: when + secs / 2.0 }, role: "sfx".into() });
                if let Some(v) = x.volume_db {
                    ops.push(Op::Audio { clip_id: Some(ClipRef::At { track: sfx_track, secs: when + secs / 2.0 }), track: None, from: None, to: None, volume_db: Some(v), gain_db: None, pan: None, fade_in: None, fade_out: None });
                }
            }
            at += s.duration_secs();
        }
    }
    // A scene's layers (#1926), on tracks of their own made on top: the
    // cards and connectors' first, then the titles' above them, so every
    // index below moves down by the tracks made.
    let scene_tracks = scene_layers(plan, start, brief, &mut ops);
    let video = video + scene_tracks;
    // The titles, last: a text track made on top shifts every index, so
    // it is made before any title and the picture is named one lower.
    let any_title = plan.segments.iter().any(|s| s.title.is_some());
    if any_title {
        let mut video_below = video;
        // On the first text track the sequence had, which the scene's
        // text tracks made above it would otherwise stand in for.
        let mut on = TrackRef::Kind("text".into());
        if !has_text {
            ops.push(Op::AddTrack { kind: "text".into(), name: Some("Titles".into()) });
            video_below += 1;
        } else if scene_tracks > 0 {
            let first = seq.tracks.iter().position(|t| t.kind == TrackKind::Text).unwrap_or(0);
            on = TrackRef::Index(first + usize::from(!has_video) + scene_tracks);
        }
        let color = brief.and_then(|b| b.primary_color()).map(str::to_string);
        for (i, s) in plan.segments.iter().enumerate() {
            if let Some(t) = &s.title {
                let hold = t.duration_secs(s.duration_secs());
                let mut op = title_op(t, hold, color.as_deref(), brief, ClipRef::At { track: video_below, secs: mids[i] });
                if let Op::Title { track, .. } = &mut op {
                    *track = Some(on.clone());
                }
                ops.push(op);
            }
        }
    }
    // Backgrounds go last, below every existing track. Anchor both
    // edges to the placed picture so source/frame rounding cannot leave
    // a one-frame hole. Reuse the bottom Background track across segments.
    let video_below = video + usize::from(any_title && !has_text);
    for (i, segment) in plan.segments.iter().enumerate() {
        if let Some(background) = &segment.background {
            // Build paths validate first. Keep an invalid stored plan
            // readable rather than panicking in the read-only plan view.
            if let Ok(data) = crate::generators::background_data(background, brief.map(|b| b.brand_colors.as_slice()).unwrap_or(&[])) {
                let picture = ClipRef::At { track: video_below, secs: mids[i] };
                ops.push(Op::Generator { data, at: Anchor::ClipStart(picture.clone()), secs: None, end: Some(Anchor::ClipEnd(picture)), track: None });
            }
        }
    }
    ops
}

/// The ops that lay a plan's scene layers onto tracks made for them
/// (#1926), the plan starting at `start` seconds; how many tracks they
/// made, all on top. "Scene cards" tracks hold cards and connectors,
/// "Scene titles" tracks above them hold titles; each layer goes on its
/// level's track, from its segment's start plus its `at_secs`.
fn scene_layers(plan: &EditPlan, start: f64, brief: Option<&Brief>, ops: &mut Vec<Op>) -> usize {
    use klipp_timeline::PlanLayer;
    let (levels, cards, titles) = layer_levels(plan);
    let name = |what: &str, k: usize, of: usize| if of == 1 { what.to_string() } else { format!("{what} {}", k + 1) };
    // Each made on top: the last made is the highest level, and the
    // titles' go above every card track.
    for k in 0..cards {
        ops.push(Op::AddTrack { kind: "video".into(), name: Some(name("Scene cards", k, cards)) });
    }
    for k in 0..titles {
        ops.push(Op::AddTrack { kind: "text".into(), name: Some(name("Scene titles", k, titles)) });
    }
    let card_track = |level: usize| titles + cards - 1 - level;
    let title_track = |level: usize| titles - 1 - level;
    let color = brief.and_then(|b| b.primary_color()).map(str::to_string);
    let mut at = start;
    for (s, levels) in plan.segments.iter().zip(&levels) {
        let d = s.duration_secs();
        for (layer, level) in s.layers.iter().zip(levels) {
            let (from, to) = layer.span(d);
            let (begin, held) = (at + from, to - from);
            match layer {
                PlanLayer::Media { media_id, source_in_secs, placement, enter, exit, .. } => {
                    let track = card_track(*level);
                    let from_file = source_in_secs.unwrap_or(0.0);
                    ops.push(Op::Place { media_id: *media_id, track: TrackRef::Index(track), at: Anchor::Secs(begin), source_in: Some(from_file), source_out: Some(from_file + held), insert: false, with_audio: false, sync_source_secs: None });
                    if let Some(poses) = card_poses(placement.as_ref(), enter.as_ref(), exit.as_ref()) {
                        ops.push(Op::Compose { clip_id: ClipRef::At { track, secs: begin + held / 2.0 }, poses, flip_x: None, flip_y: None });
                    }
                }
                PlanLayer::Connector { from: a, to: b, color, stroke_width, reveal_secs, .. } => {
                    ops.push(Op::Connector {
                        at: Anchor::Secs(begin),
                        secs: held,
                        from: *a,
                        to: *b,
                        color: color.clone(),
                        stroke_width: *stroke_width,
                        reveal_secs: Some(reveal_secs.unwrap_or(DEFAULT_REVEAL_SECS)),
                        track: Some(TrackRef::Index(card_track(*level))),
                    });
                }
                PlanLayer::Title { title, .. } => {
                    let mut op = title_op(title, held, color.as_deref(), brief, ClipRef::At { track: 0, secs: begin });
                    if let Op::Title { at: when, track, .. } = &mut op {
                        *when = Anchor::Secs(begin);
                        *track = Some(TrackRef::Index(title_track(*level)));
                    }
                    ops.push(op);
                }
            }
        }
        at += d;
    }
    cards + titles
}

/// A title in one of the plan's three styles: a heading in the upper
/// third that rises in, a callout low and backed that fades in, or a
/// full-frame card in the brand's colour.
fn title_op(t: &PlanTitle, secs: f64, brand: Option<&str>, brief: Option<&Brief>, over: ClipRef) -> Op {
    let style = if t.style.trim().is_empty() { "heading" } else { t.style.trim() };
    let look = crate::title_style::look(brief, style).unwrap_or_else(|| TypeStyle::builtin("heading").unwrap());
    let card = t.full_frame.or(look.full_frame).unwrap_or(false);
    // A look the brief wrote is its own; a built-in one defers to the brand.
    let from_brief = brief.is_some_and(|b| b.type_styles.iter().any(|s| s.name.trim().eq_ignore_ascii_case(style)));
    // The words' colour: the title's, the look's, else the brand's (white on a card).
    let color = t.color.clone().or(look.color.clone()).or_else(|| if card { Some("#ffffff".into()) } else { brand.map(str::to_string) });
    let fill = t.fill.clone().or_else(|| if from_brief { look.fill.clone().or_else(|| brand.map(str::to_string)) } else { brand.map(str::to_string).or(look.fill.clone()) });
    let (full_frame, backing_color, backing) = if card {
        (Some(fill.or_else(|| brand.map(str::to_string)).unwrap_or_else(|| "#101418".into())), None, None)
    } else if t.fill.is_some() {
        (None, t.fill.clone(), None)
    } else {
        (None, None, look.backing)
    };
    let motion = crate::title_style::inherit_motion(&look, t.enter.as_ref(), t.exit.as_ref(), t.animate.as_deref(), t.animate_secs.map(|v| v as f32), t.stagger.as_deref());
    Op::Title {
        text: t.text.clone(),
        at: Anchor::ClipStart(over),
        style: None,
        secs,
        track: Some(TrackRef::Kind("text".into())),
        x: Some(t.x.or(look.x).unwrap_or(0.5)), y: Some(t.y.or(look.y).unwrap_or(0.10)), size: look.size, color, font: look.font.clone(), bold: look.bold, align: look.align.clone(),
        backing, backing_color, full_frame, outline: look.outline, outline_color: look.outline_color.clone(),
        glow: t.glow.or(look.glow), glow_color: t.glow_color.clone().or(look.glow_color.clone()), gradient: t.gradient.clone().or(look.gradient.clone()).map(crate::ops::GradientSpec::Pair),
        animate: motion.animate, enter: motion.enter, exit: motion.exit,
        animate_secs: motion.animate_secs, stagger: motion.stagger,
        loop_effect: look.loop_effect.clone(), glitch: None, chromatic: None, roll_shape: None, edge_fade: None,
        over_faces: false,
        shadow: look.shadow.clone(),
        typeset: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::resolve;
    use klipp_core::{Fps, MediaId, Timecode};
    use klipp_timeline::{Feature, PlanBackground, PlanMusic, PlanPlacement, PlanSegment, PlanSfx, PlanTitle, PlanZoom};

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
        PlanSegment { feature: String::new(), media_id: media, source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, sfx: None, zoom: None, placement: None, background: None, section: String::new(), layers: Vec::new() }
    }

    #[test]
    fn montage_snaps_to_beats_and_can_name_features_with_titles() {
        let (_, pool, screen, _) = fixture();
        let mut plan = EditPlan { segments: (0..8).map(|i| PlanSegment { section: "montage: features".into(), title: Some(PlanTitle { text: "Search".into(), ..Default::default() }), ..seg(screen, i as f64 * 2.0, i as f64 * 2.0 + 0.85) }).collect(), ..Default::default() };
        snap_to_bars(&mut plan, 3.4, 0.0, &pool);
        assert!((plan.duration_secs() - 6.8).abs() < 1e-6);
        assert!(plan.segments.iter().all(|s| (s.duration_secs() - 0.85).abs() < 1e-6));
        assert!(covers_feature(&plan.segments[0], "search"));
        plan.segments[0].feature = "Other".into();
        assert!(!covers_feature(&plan.segments[0], "Search"));
        plan.segments[0].section.clear();
        snap_to_bars(&mut plan, 3.4, 0.0, &pool);
        assert!((plan.segments[0].duration_secs() - 1.7).abs() < 1e-6);
    }

    #[test]
    fn an_opening_card_can_overlay_the_shot_at_an_authored_position() {
        let (seq, pool, screen, _) = fixture();
        let mut plan = EditPlan { segments: vec![PlanSegment { title: Some(PlanTitle { text: "Opening".into(), style: "card".into(), full_frame: Some(false), x: Some(0.25), y: Some(0.75), glow: Some(0.5), gradient: Some(["#ff0000".into(), "#0000ff".into()]), animate: Some("slide_left".into()), animate_secs: Some(0.5), ..Default::default() }), ..seg(screen, 0.0, 3.0) }], ..Default::default() };
        validate(&plan, &pool, None).unwrap();
        let ops = to_ops(&plan, &seq, &pool, None);
        assert!(ops.iter().any(|op| matches!(op, Op::Title { x: Some(x), y: Some(y), full_frame: None, glow: Some(g), gradient: Some(_), animate: Some(a), .. } if *x == 0.25 && *y == 0.75 && *g == 0.5 && a == "slide_left")));
        let built = resolve(&seq, &pool, &ops).unwrap();
        let title = built.steps.iter().find_map(|step| if let crate::history::EditCommand::AddTitleClip { clip, .. } = &step.edit { Some(clip) } else { None }).unwrap();
        let klipp_timeline::ClipKind::Title(data) = &title.kind else { panic!("title clip") };
        assert_eq!(data.objects.len(), 1, "an overlay must not have a full-frame solid object");
        assert!(plan.text(None).contains("overlay"));
        plan.segments[0].title.as_mut().unwrap().full_frame = None;
        assert!(plan.text(None).contains("covers the shot"));
        assert!(to_ops(&plan, &seq, &pool, None).iter().any(|op| matches!(op, Op::Title { full_frame: Some(_), .. })));
    }

    #[test]
    fn a_still_plan_can_hold_and_snap_beyond_the_import_default() {
        let (seq, mut pool, screen, _) = fixture();
        pool.get_mut(&screen).unwrap().duration = tc(5.0);
        pool.get_mut(&screen).unwrap().is_still = true;
        let mut plan = EditPlan { segments: vec![seg(screen, 0.0, 21.0)], ..Default::default() };
        validate(&plan, &pool, None).unwrap();
        snap_to_bars(&mut plan, 4.0, 0.0, &pool);
        assert_eq!(plan.duration_secs(), 20.0);
        let built = resolve(&seq, &pool, &to_ops(&plan, &seq, &pool, None)).unwrap();
        assert_eq!(built.steps[0].after[0].timeline_out_secs, 20.0);
        pool.get_mut(&screen).unwrap().is_still = false;
        assert!(validate(&plan, &pool, None).is_err());
    }

    #[test]
    fn backgrounds_follow_picture_edges_and_stay_below_titles_and_picture() {
        let (seq, pool, screen, _) = fixture();
        let plan = EditPlan { segments: (0..2).map(|i| PlanSegment {
            background: Some(PlanBackground { generator: "gradient".into(), colors: vec!["#101820".into(), "#304050".into()] }),
            placement: Some(PlanPlacement { width: 0.7, height: 0.7, ..Default::default() }),
            title: Some(PlanTitle { text: "Overlay".into(), ..Default::default() }),
            ..seg(screen, i as f64 * 2.0 + 0.03, i as f64 * 2.0 + 0.88)
        }).collect(), ..Default::default() };
        validate(&plan, &pool, None).unwrap();
        let built = resolve(&seq, &pool, &to_ops(&plan, &seq, &pool, None)).unwrap();
        let pictures: Vec<_> = built.steps.iter().filter(|s| matches!(s.op, Op::Place { .. })).map(|s| &s.after[0]).collect();
        let backgrounds: Vec<_> = built.steps.iter().filter(|s| matches!(s.op, Op::Generator { .. })).map(|s| &s.after[0]).collect();
        assert_eq!(backgrounds.len(), 2);
        assert_eq!(backgrounds[0].track, backgrounds[1].track);
        for (picture, background) in pictures.iter().zip(&backgrounds) {
            assert!(background.track > picture.track);
            assert_eq!(background.timeline_in_secs, picture.timeline_in_secs);
            assert_eq!(background.timeline_out_secs, picture.timeline_out_secs);
        }
    }

    #[test]
    fn a_placement_a_background_and_a_section_are_checked_before_they_are_stored() {
        let (_seq, pool, screen, _bed) = fixture();
        let placed = |p: PlanPlacement| EditPlan {
            segments: vec![PlanSegment { placement: Some(p), ..seg(screen, 0.0, 2.0) }],
            ..Default::default()
        };

        // Half the frame, in the corner, turned and half seen: the point of the field.
        let ok = placed(PlanPlacement { x: 0.25, y: 0.3, width: 0.5, height: 0.5, rotation_deg: -6.0, opacity: Some(0.5) });
        assert!(validate(&ok, &pool, None).is_ok(), "{:?}", validate(&ok, &pool, None));

        // A size of zero is a picture nobody can see.
        let flat = placed(PlanPlacement { width: 0.0, ..Default::default() });
        assert!(validate(&flat, &pool, None).unwrap_err().contains("width and height"));

        // Off-frame is allowed so a card can fly in; far off is a mistake.
        let adrift = placed(PlanPlacement { x: 9.0, ..Default::default() });
        assert!(validate(&adrift, &pool, None).unwrap_err().contains("picture's centre"));

        let murky = placed(PlanPlacement { opacity: Some(1.4), ..Default::default() });
        assert!(validate(&murky, &pool, None).unwrap_err().contains("opacity"));

        // A background names a generator, and its colours are colours.
        let nameless = EditPlan {
            segments: vec![PlanSegment { background: Some(PlanBackground { generator: String::new(), colors: vec![] }), ..seg(screen, 0.0, 2.0) }],
            ..Default::default()
        };
        assert!(validate(&nameless, &pool, None).unwrap_err().contains("generator"));
        let off_colour = EditPlan {
            segments: vec![PlanSegment { background: Some(PlanBackground { generator: "gradient".into(), colors: vec!["blue".into()] }), ..seg(screen, 0.0, 2.0) }],
            ..Default::default()
        };
        assert!(validate(&off_colour, &pool, None).unwrap_err().contains("#rrggbb"));

        // A section is a run: the same name may not come back after something else.
        let in_a_run = |names: [&str; 3]| EditPlan {
            segments: names.iter().map(|n| PlanSegment { section: (*n).into(), ..seg(screen, 0.0, 2.0) }).collect(),
            ..Default::default()
        };
        assert!(validate(&in_a_run(["montage", "montage", ""]), &pool, None).is_ok());
        assert!(validate(&in_a_run(["montage", "", "montage"]), &pool, None).unwrap_err().contains("run of segments"));

        // And a title sits inside the frame.
        let astray = EditPlan {
            segments: vec![PlanSegment { title: Some(PlanTitle { text: "Strata".into(), x: Some(1.8), ..Default::default() }), ..seg(screen, 0.0, 2.0) }],
            ..Default::default()
        };
        assert!(validate(&astray, &pool, None).unwrap_err().contains("fractions of the frame"));
    }

    #[test]
    fn a_placement_becomes_a_compose_op_and_the_frame_as_shot_becomes_nothing() {
        let (seq, pool, screen, _bed) = fixture();
        let built = |placement| {
            let plan = EditPlan { segments: vec![PlanSegment { placement, ..seg(screen, 0.0, 2.0) }], ..Default::default() };
            to_ops(&plan, &seq, &pool, None)
        };

        // Half the frame, left of centre and high, turned and half seen.
        let ops = built(Some(PlanPlacement { x: 0.25, y: 0.3, width: 0.5, height: 0.5, rotation_deg: -6.0, opacity: Some(0.5) }));
        let Some(Op::Compose { poses, .. }) = ops.iter().find(|o| matches!(o, Op::Compose { .. })) else {
            panic!("a placement lowers to a compose op")
        };
        assert_eq!(poses.len(), 1);
        assert!((poses[0].x.unwrap() - 0.25).abs() < 1e-6 && (poses[0].width.unwrap() - 0.5).abs() < 1e-6);
        assert!((poses[0].rotation_deg.unwrap() + 6.0).abs() < 1e-6 && (poses[0].opacity - 0.5).abs() < 1e-6);
        // And it builds: the op is one the timeline accepts.
        assert!(resolve(&seq, &pool, &ops).is_ok(), "{:?}", resolve(&seq, &pool, &ops).err());

        // The frame as shot is what a clip does anyway, so nothing is said.
        assert!(!built(Some(PlanPlacement::default())).iter().any(|o| matches!(o, Op::Compose { .. })));
        assert!(!built(None).iter().any(|o| matches!(o, Op::Compose { .. })));
    }

    #[test]
    fn a_plan_builds_end_to_end_with_its_titles_music_and_joins() {
        let (seq, pool, screen, bed) = fixture();
        let brief = Brief { features: vec![Feature { title: "Search".into(), description: String::new(), rank: 1 }], brand_colors: vec!["#ff6600".into()], width: Some(1280), height: Some(720), ..Default::default() };
        let plan = EditPlan {
            segments: vec![
                PlanSegment { title: Some(PlanTitle { text: "Klipp".into(), secs: Some(2.0), style: "card".into(), color: None, fill: None, ..Default::default() }), ..seg(screen, 0.0, 3.0) },
                PlanSegment { feature: "Search".into(), with_audio: true, title: Some(PlanTitle { text: "Search".into(), secs: None, style: String::new(), color: None, fill: None, ..Default::default() }), zoom: Some(PlanZoom { from: 1.0, to: 1.15, x: 0.0, y: 0.0, ease: Default::default() }), ..seg(screen, 10.0, 16.0) },
                PlanSegment { transition: Some("cross_dissolve".into()), title: Some(PlanTitle { text: "Try it".into(), secs: Some(9.0), style: "callout".into(), color: None, fill: None, ..Default::default() }), ..seg(screen, 30.0, 34.0) },
            ],
            music: Some(PlanMusic { media_id: bed, duck_db: None, source_in_secs: Some(0.5), volume_db: Some(-6.0), fade_in_secs: None, fade_out_secs: Some(1.5) }),
            approved: true,
            snap_to_bars: false,
            notes: String::new(),
        };
        validate(&plan, &pool, Some(&brief)).unwrap();
        let ops = to_ops(&plan, &seq, &pool, Some(&brief));
        // Picture first, then the join, the music, the text track, the titles.
        let kinds: Vec<&str> = ops.iter().map(|o| match o { Op::Place { .. } => "place", Op::Zoom { .. } => "zoom", Op::Reframe { .. } => "reframe", Op::Format { .. } => "format", Op::Role { .. } => "role", Op::Audio { .. } => "audio", Op::Transition { .. } => "transition", Op::Duck { .. } => "duck", Op::AddTrack { kind, .. } => kind.as_str(), Op::Title { .. } => "title", _ => "?" }).collect();
        assert_eq!(kinds, vec!["place", "format", "place", "zoom", "place", "transition", "audio", "place", "role", "audio", "duck", "text", "title", "title", "title"]);
        let built = resolve(&seq, &pool, &ops).unwrap();
        assert!((built.duration_secs - 13.0).abs() < 1e-6, "{}", built.text());
        // The titles land where their segments start, on the new text track.
        let titles: Vec<(f64, f64)> = built.steps.iter().filter(|s| matches!(s.op, Op::Title { .. })).map(|s| (s.after[0].timeline_in_secs, s.after[0].timeline_out_secs)).collect();
        assert_eq!(titles, vec![(0.0, 2.0), (3.0, 6.0), (9.0, 13.0)], "{}", built.text());
        assert!(matches!(&ops[12], Op::Title { full_frame: Some(c), .. } if c == "#ff6600"), "the card takes the brand colour");
        assert!(built.text().contains("1280x720"), "the delivery's size is the brief's: {}", built.text());
        let music = built.steps.iter().find(|s| matches!(&s.op, Op::Place { media_id, .. } if *media_id == bed)).unwrap();
        assert!((music.after[0].source_in_secs - 0.5).abs() < 1e-6, "the bed starts at its in-point");
        // The bed is on its own track under the segment's sound, and ducked there.
        let music = built.steps.iter().find(|s| matches!(&s.op, Op::Place { media_id, .. } if *media_id == bed)).unwrap();
        assert_eq!(music.after[0].track, 2, "{}", built.text());
        assert!(built.steps.iter().any(|s| matches!(s.op, Op::Duck { .. })));

        // With no sound of its own in the cut, the music is laid but not ducked.
        let mut silent = plan.clone();
        silent.segments.iter_mut().for_each(|s| s.with_audio = false);
        let ops = to_ops(&silent, &seq, &pool, Some(&brief));
        assert!(!ops.iter().any(|o| matches!(o, Op::Duck { .. })));
    }

    /// A segment's sound effect lands on an "SFX" track of its own,
    /// below the music, at the segment's start plus its offset, as an
    /// effect to the mix; a title's glow, gradient and staggered
    /// entrance reach the title op.
    #[test]
    fn sound_effects_and_title_extras_reach_the_ops() {
        let (seq, pool, screen, bed) = fixture();
        let plan = EditPlan {
            segments: vec![
                PlanSegment {
                    title: Some(PlanTitle { text: "Klipp".into(), glow: Some(0.6), glow_color: Some("#ff2a78".into()), animate: Some("rise".into()), stagger: Some("char".into()), ..Default::default() }),
                    sfx: Some(PlanSfx { media_id: bed, at_secs: Some(0.5), source_in_secs: Some(1.0), secs: Some(1.5), volume_db: Some(-6.0) }),
                    ..seg(screen, 0.0, 3.0)
                },
                seg(screen, 3.0, 6.0),
            ],
            music: Some(PlanMusic { media_id: bed, duck_db: None, source_in_secs: None, volume_db: None, fade_in_secs: None, fade_out_secs: None }),
            approved: true,
            ..Default::default()
        };
        validate(&plan, &pool, None).unwrap();
        let ops = to_ops(&plan, &seq, &pool, None);
        let sfx_track = ops.iter().position(|o| matches!(o, Op::AddTrack { name: Some(n), .. } if n == "SFX")).expect("an SFX track");
        let music_track = ops.iter().position(|o| matches!(o, Op::AddTrack { name: Some(n), .. } if n == "Music")).unwrap();
        assert!(sfx_track > music_track, "the SFX track is made after the music's, below it");
        let place = ops.iter().find(|o| matches!(o, Op::Place { at: Anchor::Secs(t), source_in: Some(f), source_out: Some(g), .. } if (*t - 0.5).abs() < 1e-9 && (*f - 1.0).abs() < 1e-9 && (*g - 2.5).abs() < 1e-9));
        assert!(place.is_some(), "the effect is placed half a second in, 1.0-2.5 of its file: {ops:?}");
        assert!(ops.iter().any(|o| matches!(o, Op::Role { role, .. } if role == "sfx")));
        assert!(ops.iter().any(|o| matches!(o, Op::Audio { volume_db: Some(v), .. } if (*v + 6.0).abs() < 1e-6)));
        let title = ops.iter().find(|o| matches!(o, Op::Title { .. })).unwrap();
        let Op::Title { glow, glow_color, animate, stagger, y, .. } = title else { unreachable!() };
        assert_eq!((*glow, glow_color.as_deref(), animate.as_deref(), stagger.as_deref()), (Some(0.6), Some("#ff2a78"), Some("rise"), Some("char")));
        assert_eq!(*y, Some(0.10), "a heading sits in the top tenth");
        // An effect past the segment's end is refused.
        let mut late = plan.clone();
        late.segments[0].sfx.as_mut().unwrap().at_secs = Some(3.5);
        assert!(validate(&late, &pool, None).unwrap_err().contains("at_secs"));
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
        assert!(bad(EditPlan { segments: vec![PlanSegment { title: Some(PlanTitle { text: "x".into(), secs: None, style: "banner".into(), color: None, fill: None, ..Default::default() }), ..seg(screen, 1.0, 2.0) }], ..Default::default() }).contains("heading, callout, card"));
        let unknown = EditPlan { segments: vec![seg(MediaId::new(), 1.0, 2.0)], ..Default::default() };
        assert!(bad(unknown).contains("no media item"));
    }

    #[test]
    fn a_plan_builds_after_what_is_already_down() {
        let (mut seq, pool, screen, _) = fixture();
        seq.tracks[0].clips.push(klipp_timeline::Clip::media(screen, tc(0.0), tc(4.0), tc(20.0), tc(24.0)));
        let plan = EditPlan { segments: vec![PlanSegment { title: Some(PlanTitle { text: "Next".into(), secs: Some(1.0), style: String::new(), color: None, fill: None, ..Default::default() }), ..seg(screen, 0.0, 2.0) }], ..Default::default() };
        let built = resolve(&seq, &pool, &to_ops(&plan, &seq, &pool, None)).unwrap();
        let place = built.steps.iter().find(|s| matches!(s.op, Op::Place { .. })).unwrap();
        assert_eq!((place.after[0].timeline_in_secs, place.after[0].timeline_out_secs), (4.0, 6.0));
        let title = built.steps.iter().find(|s| matches!(s.op, Op::Title { .. })).unwrap();
        assert_eq!((title.after[0].timeline_in_secs, title.after[0].timeline_out_secs), (4.0, 5.0));
    }

    /// The issue's scene (#1926): two cards that come in staggered and
    /// overlap, a connector drawn on between them and a title, over one
    /// segment, and a plain segment with a title after it.
    fn scene_plan(screen: MediaId) -> EditPlan {
        serde_json::from_value(serde_json::json!({
            "segments": [
                {"media_id": screen, "source_in_secs": 0.0, "source_out_secs": 6.0, "layers": [
                    {"kind": "media", "media_id": screen, "source_in_secs": 20.0, "at_secs": 0.4, "secs": 5.2,
                     "placement": {"x": 0.3, "y": 0.5, "width": 0.3, "height": 0.3},
                     "enter": {"kind": "rise", "secs": 0.5}, "exit": {"kind": "fade", "secs": 0.4}},
                    {"kind": "media", "media_id": screen, "source_in_secs": 30.0, "at_secs": 0.8, "secs": 4.8,
                     "placement": {"x": 0.7, "y": 0.5, "width": 0.3, "height": 0.3}, "enter": {"kind": "rise", "secs": 0.5}},
                    {"kind": "connector", "from": [0.45, 0.5], "to": [0.55, 0.5], "at_secs": 1.4, "secs": 4.2, "reveal_secs": 0.6},
                    {"kind": "title", "at_secs": 2.0, "title": {"text": "Plan to ship", "secs": 3.0}}
                ]},
                {"media_id": screen, "source_in_secs": 10.0, "source_out_secs": 12.0, "title": {"text": "Next"}}
            ]
        }))
        .unwrap()
    }

    /// `seq` with every step of `ops` applied, as the build applies them.
    fn built_on(seq: &Sequence, pool: &MediaPool, ops: &[Op]) -> Sequence {
        let plan = resolve(seq, pool, ops).unwrap_or_else(|e| panic!("{e:?}"));
        let (mut seqs, mut p, mut fps) = (vec![seq.clone()], pool.clone(), seq.fps);
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        for step in &plan.steps {
            view.apply_forward(&step.edit);
        }
        seqs.remove(0)
    }

    /// The clips on the track named `name`: where each starts and ends.
    fn spans_on(seq: &Sequence, name: &str) -> (usize, Vec<(f64, f64)>) {
        let at = seq.tracks.iter().position(|t| t.name == name).unwrap_or_else(|| panic!("no track {name}: {:?}", seq.tracks.iter().map(|t| &t.name).collect::<Vec<_>>()));
        (at, seq.tracks[at].clips.iter().map(|c| (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())).collect())
    }

    /// #1926: the plan as reviewed and the timeline as built agree on
    /// every layer's timing and on where layers overlap. Each layer sits
    /// on a track named for what it holds, a layer later in the list above
    /// those it overlaps, the titles' tracks above the cards', and the
    /// plain segment's title on the plan's own Titles track on top.
    #[test]
    fn a_scene_s_layers_build_where_the_plan_says() {
        let (seq, pool, screen, _) = fixture();
        let plan = scene_plan(screen);
        validate(&plan, &pool, None).unwrap();
        let text = plan.text(None);
        for line in ["layer 1 0:00.40-0:05.60", "layer 2 0:00.80-0:05.60", "layer 3 0:01.40-0:05.60", "layer 4 0:02.00-0:05.00",
                     "overlap: layers 1-2 0:00.80-0:05.60, 1-3 0:01.40-0:05.60, 1-4 0:02.00-0:05.00, 2-3 0:01.40-0:05.60, 2-4 0:02.00-0:05.00, 3-4 0:02.00-0:05.00"] {
            assert!(text.contains(line), "{line}: {text}");
        }
        let built = built_on(&seq, &pool, &to_ops(&plan, &seq, &pool, None));
        let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() <= 0.04 + 1e-9 && (a.1 - b.1).abs() <= 0.04 + 1e-9;
        let (a, card_a) = spans_on(&built, "Scene cards 1");
        let (b, card_b) = spans_on(&built, "Scene cards 2");
        let (c, connector) = spans_on(&built, "Scene cards 3");
        let (t, title) = spans_on(&built, "Scene titles");
        // What the plan says, absolute: the segment starts at 0.
        let d = plan.segments[0].duration_secs();
        let want: Vec<(f64, f64)> = plan.segments[0].layers.iter().map(|l| l.span(d)).collect();
        for (name, got, want) in [("card 1", &card_a, want[0]), ("card 2", &card_b, want[1]), ("connector", &connector, want[2]), ("title", &title, want[3])] {
            assert!(got.len() == 1 && near(got[0], want), "{name}: built {got:?}, planned {want:?}");
        }
        // The overlaps the timeline holds are the ones the text read out.
        let laid = [card_a[0], card_b[0], connector[0], title[0]];
        let mut overlaps = Vec::new();
        for i in 0..4 {
            for j in i + 1..4 {
                let (from, to) = (laid[i].0.max(laid[j].0), laid[i].1.min(laid[j].1));
                if to > from {
                    overlaps.push((i + 1, j + 1));
                }
            }
        }
        assert_eq!(overlaps, plan.segments[0].layer_overlaps().iter().map(|o| (o.0, o.1)).collect::<Vec<_>>());
        assert!(t < c && c < b && b < a, "stacked as listed, titles above cards: titles {t}, cards {c} {b} {a}");
        let (titles, next) = spans_on(&built, "Titles");
        assert!(titles < t && next.len() == 1 && near(next[0], (6.0, 8.0)), "the plain segment's title on the plan's Titles track on top: {titles} {next:?}");
        let picture = built.tracks.iter().position(|tr| tr.kind == TrackKind::Video && !tr.name.starts_with("Scene")).unwrap();
        assert!(a < picture, "the cards are over the picture");
        // The connector is a title with no words: one path, drawn on.
        let clip = &built.tracks[c].clips[0];
        let data = clip.title_data().expect("a title clip");
        let [object] = data.objects.as_slice() else { panic!("one object: {:?}", data.objects.len()) };
        let klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Path(path), .. }) = &object.content else { panic!("a path") };
        let keys: Vec<f64> = path.trim.as_ref().map(|t| t.keyframes.iter().map(|k| k.time_secs).collect()).unwrap_or_default();
        assert_eq!(keys, vec![0.0, 0.6], "drawn on over its reveal");
        // A card's entrance and exit are its poses.
        let ops = to_ops(&plan, &seq, &pool, None);
        let poses: Vec<_> = ops.iter().filter_map(|op| match op { Op::Compose { poses, .. } => Some(poses.len()), _ => None }).collect();
        assert_eq!(poses, vec![4, 2], "card 1 comes in and goes; card 2 comes in");
    }

    /// #1926: with a text track already there, a plain segment's title
    /// still goes on it and not on a scene's track made above it.
    #[test]
    fn a_scene_leaves_the_plain_titles_on_the_text_track_there_was() {
        let (mut seq, pool, screen, _) = fixture();
        seq.tracks.insert(0, klipp_timeline::Track::new("Words", TrackKind::Text));
        let plan = scene_plan(screen);
        let built = built_on(&seq, &pool, &to_ops(&plan, &seq, &pool, None));
        let (_, words) = spans_on(&built, "Words");
        assert_eq!(words.len(), 1, "the plain segment's title");
        let (_, scene) = spans_on(&built, "Scene titles");
        assert_eq!(scene.len(), 1, "the scene's title");
    }

    /// #1926: a layer that can't be built as planned is refused, saying
    /// which segment and layer and what would fix it.
    #[test]
    fn a_layer_that_cannot_be_built_is_refused_by_name() {
        let (_, pool, screen, _) = fixture();
        let refused = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = serde_json::to_value(scene_plan(screen)).unwrap();
            edit(&mut v["segments"][0]["layers"]);
            let plan: EditPlan = serde_json::from_value(v).unwrap();
            validate(&plan, &pool, None).unwrap_err()
        };
        let why = refused(&|l| l[0]["secs"] = serde_json::json!(6.0));
        assert!(why.contains("segment 1, layer 1") && why.contains("past the segment's end"), "{why}");
        let why = refused(&|l| l[0]["exit"]["secs"] = serde_json::json!(5.0));
        assert!(why.contains("entrance and exit"), "{why}");
        let why = refused(&|l| l[1]["placement"]["height"] = serde_json::json!(0.5));
        assert!(why.contains("layer 2") && why.contains("scaled evenly"), "{why}");
        let why = refused(&|l| l[2]["to"] = serde_json::json!([0.45, 0.5]));
        assert!(why.contains("layer 3") && why.contains("itself"), "{why}");
        let why = refused(&|l| l[2]["reveal_secs"] = serde_json::json!(9.0));
        assert!(why.contains("reveal_secs"), "{why}");
        let why = refused(&|l| l[0]["enter"]["kind"] = serde_json::json!("spin"));
        assert!(why.contains("not a card motion"), "{why}");
        let why = refused(&|l| l[3]["title"]["text"] = serde_json::json!(""));
        assert!(why.contains("segment 1, layer 4: a title needs text"), "{why}");
    }
}
