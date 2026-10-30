//! The timeline as the socket describes it: its text view, its clips by
//! filter, and each clip's sound, surfaces and kind (#826).

#[allow(unused_imports)]
use super::*;

/// Part of the timeline to read: one track, and what plays in a stretch
/// (#796). The default is all of it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TimelineView {
    pub(crate) track: Option<usize>,
    pub(crate) from_secs: Option<f64>,
    pub(crate) to_secs: Option<f64>,
}

impl TimelineView {
    pub(crate) fn shows(&self, a: f64, b: f64) -> bool {
        self.from_secs.is_none_or(|f| b > f) && self.to_secs.is_none_or(|t| a < t)
    }

    pub(crate) fn narrowed(&self) -> bool {
        self.track.is_some() || self.from_secs.is_some() || self.to_secs.is_some()
    }
}

/// The timeline as text: what a model reads before it plans. Tracks top
/// first; every clip with its range in seconds and frames at the
/// sequence's rate; the clip's kind and source; its grade and effects in
/// a word; the transitions on each track; the markers. Ids are there because an operation needs them,
/// at the end of the line where they cost the least attention.
pub(crate) fn timeline_text(
    seq: &klipp_timeline::Sequence,
    pool: &klipp_media::MediaPool,
    playhead_secs: Option<f64>,
    revision: u64,
    view: TimelineView,
) -> String {
    use std::fmt::Write as _;
    let fps = seq.fps.as_f64();
    let frames = |t: klipp_core::Timecode| (t.to_secs_f64() * fps).round() as i64;
    let range = |a: klipp_core::Timecode, b: klipp_core::Timecode| {
        format!("{:.2}-{:.2} s | {}-{} f", a.to_secs_f64(), b.to_secs_f64(), frames(a), frames(b))
    };
    let mut out = String::new();
    let dur = seq.total_duration();
    let _ = writeln!(
        out,
        "Sequence {}  {}x{}  {:.3} fps  duration {:.2} s ({} f)  revision {}",
        quoted(&seq.name), seq.resolution.width, seq.resolution.height, fps, dur.to_secs_f64(), frames(dur), revision,
    );
    if let Some(p) = playhead_secs {
        let _ = writeln!(out, "Playhead {:.2} s ({} f)", p, (p * fps).round() as i64);
    }
    // A grade over the whole timeline is said once here: each clip's own
    // reads "none" under it, which read as ungraded (#2492).
    let timeline_graded = seq.timeline_grade().is_some_and(|g| !g.is_identity());
    if let Some(g) = seq.timeline_grade().filter(|_| timeline_graded) {
        let _ = writeln!(out, "Timeline grade: {} nodes, over every clip after its own and its track's.", g.nodes.len());
    }
    // The aux buses (#2861), each track's sends on its own line below.
    if !seq.buses.is_empty() {
        let said: Vec<String> = seq.buses.iter().enumerate().map(|(k, b)| format!("[{k}] {}", seq.bus_text(b))).collect();
        let _ = writeln!(out, "Aux buses: {}.", said.join("; "));
    }
    let _ = writeln!(out, "Tracks, top first. Times are seconds and frames at {:.3} fps.", fps);
    if view.narrowed() {
        let mut said = Vec::new();
        if let Some(t) = view.track {
            said.push(format!("track {t} only"));
        }
        match (view.from_secs, view.to_secs) {
            (Some(a), Some(b)) => said.push(format!("what plays from {a:.2} to {b:.2} s")),
            (Some(a), None) => said.push(format!("what plays from {a:.2} s on")),
            (None, Some(b)) => said.push(format!("what plays before {b:.2} s")),
            (None, None) => {}
        }
        let _ = writeln!(out, "Showing {}.", said.join(", "));
    }
    for (i, t) in seq.tracks.iter().enumerate() {
        if view.track.is_some_and(|w| w != i) {
            continue;
        }
        let mut head = format!("\n[{i}] {:?} {}", t.kind, quoted(&t.name));
        if (t.opacity - 1.0).abs() > 1e-6 {
            let _ = write!(head, "  opacity {:.2}", t.opacity);
        }
        if t.locked {
            head.push_str("  locked");
        }
        // What the track does to every clip on it (#846), not repeated on
        // each clip below, which lists only its own.
        let fx = &t.effects;
        if !fx.is_empty() {
            let mut said: Vec<String> = fx.video_effects().iter().map(|e| {
                let d = format!("{e:?}");
                d.split(['{', '(', ' ']).next().unwrap_or("").to_string()
            }).collect();
            if fx.track_grade().is_some_and(|g| !g.is_identity()) {
                said.push("grade".into());
            }
            said.extend(fx.audio.iter().map(|e| if e.enabled { word(&e.effect_type) } else { format!("{} (bypassed)", word(&e.effect_type)) }));
            let _ = write!(head, "  track effects: {}", said.join(", "));
        }
        if !t.sends.is_empty() {
            let said: Vec<String> = t.sends.iter().map(|s| seq.send_text(s)).collect();
            let _ = write!(head, "  sends to: {}", said.join(", "));
        }
        if t.clips.is_empty() {
            head.push_str("  (empty)");
        }
        let _ = writeln!(out, "{head}");
        for c in t.clips.iter().filter(|c| view.shows(c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64())) {
            let what = match &c.kind {
                klipp_timeline::ClipKind::Media { media_id } => match pool.get(media_id) {
                    Some(m) => {
                        let mut line = format!("media {}  src {:.2}-{:.2} s", quoted(&m.filename), c.source_in.to_secs_f64(), c.source_out.to_secs_f64());
                        // What analysis knows about this part of the file.
                        let known = klipp_analysis::report::MediaAnalysis::load(&m.path)
                            .clip_summary_with(c.source_in.to_secs_f64(), c.source_out.to_secs_f64(), t.kind != klipp_timeline::TrackKind::Audio);
                        if !known.is_empty() {
                            let _ = write!(line, "  [{known}]");
                        }
                        line
                    }
                    None => "media (missing from pool)".to_string(),
                },
                klipp_timeline::ClipKind::Title(td) => {
                    let text = td.text();
                    let one_line: String = text.lines().collect::<Vec<_>>().join(" / ");
                    let shown: String = one_line.chars().take(60).collect();
                    format!("title {}", quoted(&format!("{}{}", shown, if one_line.chars().count() > 60 { "…" } else { "" })))
                }
                klipp_timeline::ClipKind::Generator(g) => format!("generator {:?}", g).chars().take(48).collect(),
                klipp_timeline::ClipKind::AdjustmentLayer => "adjustment layer".to_string(),
                klipp_timeline::ClipKind::Gap => "gap".to_string(),
                klipp_timeline::ClipKind::Sequence { .. } => "nested sequence".to_string(),
                klipp_timeline::ClipKind::Precomposition { .. } => "precomposition".to_string(),
                // Its controls (#2757): each one's value and what it drives,
                // with the offset or "unlinked at" a hand edit left.
                klipp_timeline::ClipKind::TemplatePrecomposition { template_instance: inst, .. } if !inst.links.is_empty() => {
                    let mut out = "template precomposition".to_string();
                    write_links(&mut out, inst);
                    out
                }
                klipp_timeline::ClipKind::TemplatePrecomposition { .. } => "template precomposition".to_string(),
            };
            let mut line = format!("  [{}]  {what}", range(c.timeline_in, c.timeline_out));
            // Its own name and colour on the timeline (#920).
            if let Some(name) = c.name.as_deref().filter(|n| !n.trim().is_empty()) {
                let _ = write!(line, "  named {}", quoted(name));
            }
            if c.colour_label != 0 {
                let _ = write!(line, "  colour {}", klipp_media::ColorLabel::from_u8(c.colour_label).label().to_lowercase());
            }
            // Where its grade stands (#899).
            if !c.grade_flag.is_none() {
                let _ = write!(line, "  flag {}", c.grade_flag.label().to_lowercase());
            }
            if !c.enabled {
                line.push_str("  DISABLED");
            }
            // Its recipes and their live controls (#2758).
            if let Ok(lifted) = c.picture_v2().as_ref().map(|l| &l.picture.graph.ui.frames) {
                for r in lifted.iter().filter_map(|f| serde_json::from_value::<klipp_core::RecipeInstance>(f.get("recipe")?.clone()).ok()) {
                    let _ = write!(line, "  recipe {}", quoted(&r.name));
                    write_links(&mut line, &r.instance);
                }
            }
            if (c.speed - 1.0).abs() > 1e-6 {
                let _ = write!(line, "  speed {:.2}", c.speed);
            }
            if !matches!(c.blend_mode, klipp_core::BlendMode::Normal) {
                let _ = write!(line, "  blend {:?}", c.blend_mode);
            }
            let own = c.own_grade();
            let over: Vec<&str> = [(t.effects.track_grade().is_some_and(|g| !g.is_identity()), "track's"), (timeline_graded, "timeline's")].into_iter().filter(|(on, _)| *on).map(|(_, w)| w).collect();
            let grade = match (own.is_identity(), over.is_empty()) {
                (true, true) => "none".to_string(),
                (true, false) => format!("none of its own, under the {} grade", over.join(" and ")),
                (false, _) => format!("{} nodes", own.nodes.len()),
            };
            let _ = write!(line, "  grade {grade}");
            // A push or pull on the picture, a still's especially (#2492).
            let scale = klipp_timeline::frame_glide::lane(c, "scale");
            if let (Some(a), Some(b)) = (scale.first(), scale.last()) {
                if (a.value - b.value).abs() > 1e-3 {
                    let _ = write!(line, "  zoom {:.2} to {:.2}", a.value, b.value);
                } else if (a.value - 1.0).abs() > 1e-3 {
                    let _ = write!(line, "  zoom {:.2}", a.value);
                }
            }
            // The grade group it grades inside (#237).
            if let Some(g) = c.grade_group.and_then(|g| seq.grade_group(g)) {
                let _ = write!(line, " in group {}", quoted(&g.name));
            }
            // Read from the clip's picture (M4 step 12d).
            let effects = c.picture_effects().unwrap_or_default();
            if !effects.is_empty() {
                let names: Vec<String> = effects
                    .iter()
                    .map(|e| {
                        let d = format!("{:?}", e.effect);
                        d.split(['{', '(', ' ']).next().unwrap_or("").to_string()
                    })
                    .collect();
                let _ = write!(line, "  fx {}", names.join(","));
            }
            let plugins = c.picture_ofx().map_or(0, |o| o.len());
            if plugins > 0 {
                let _ = write!(line, "  plugins {plugins}");
            }
            if c.matte_source.is_some() {
                line.push_str("  matted");
            }
            if c.consumed_as_matte {
                line.push_str("  is-a-matte");
            }
            if c.multicam_group.is_some() {
                // Counted from 1, as switch_angle takes them; the
                // primary (angle 1) carries the cuts, in timeline seconds.
                let _ = write!(line, "  multicam angle {}", c.multicam_angle as usize + 1);
                if !c.multicam_cuts.is_empty() {
                    let cuts: Vec<String> = c.multicam_cuts.iter().map(|(at, a)| format!("{:.2}s→{}", c.timeline_in.to_secs_f64() + at, *a as usize + 1)).collect();
                    let _ = write!(line, "  cuts {}", cuts.join(" "));
                }
            }
            let _ = write!(line, "  id {}", c.id.0);
            let _ = writeln!(out, "{line}");
        }
        // The transitions on the track's cuts (#711): what an agent that
        // made a dissolve reads back to see it there.
        for tr in &t.transitions {
            if transition_span(t, tr).is_some_and(|(_, from, to)| !view.shows(from, to)) {
                continue;
            }
            let spec = klipp_core::spec_for_kind(tr.kind);
            let d = tr.duration.to_secs_f64();
            let (words, _) = transition_alignment(tr.alignment);
            let mut line = format!("  transition {} ({}) {:.2} s ({} f)", spec.display_name, spec.string_key, d, (d * fps).round() as i64);
            match transition_span(t, tr) {
                Some((cut, from, to)) => {
                    let _ = write!(
                        line,
                        "  [{:.2}-{:.2} s | {}-{} f]  {words} at {:.2} s",
                        from, to, (from * fps).round() as i64, (to * fps).round() as i64, cut,
                    );
                }
                None => line.push_str("  (its first clip is gone)"),
            }
            let _ = write!(line, "  from {} to {}  id {}", tr.clip_a_id.0, tr.clip_b_id.0, tr.id.0);
            let _ = writeln!(out, "{line}");
        }
    }
    let marker_span = |m: &klipp_timeline::Marker| {
        let a = m.position.to_secs_f64();
        (a, m.duration.map_or(a, |d| (m.position + d).to_secs_f64()))
    };
    let markers: Vec<&klipp_timeline::Marker> = seq.markers.iter().filter(|m| {
        let (a, b) = marker_span(m);
        view.from_secs.is_none_or(|f| b >= f) && view.to_secs.is_none_or(|t| a < t)
    }).collect();
    if !markers.is_empty() {
        let _ = writeln!(out, "\nMarkers");
        for m in markers {
            let at = match m.duration {
                Some(d) => format!("{:.2}-{:.2} s", m.position.to_secs_f64(), (m.position + d).to_secs_f64()),
                None => format!("{:.2} s", m.position.to_secs_f64()),
            };
            let mut line = format!("  {at}  {}  {:?}", quoted(&m.label), m.color);
            if !matches!(m.kind, klipp_core::MarkerKind::None) {
                let _ = write!(line, "  {}", marker_kind_name(m.kind));
            }
            if !m.comment.is_empty() {
                let _ = write!(line, "  note {}", quoted(&m.comment));
            }
            let _ = write!(line, "  id {}", m.id.0);
            let _ = writeln!(out, "{line}");
        }
    }
    out
}

/// What `list_clips` is asked for: every field or the short form, and
/// which clips.
pub(crate) struct ClipFilter {
    pub(crate) full: bool,
    pub(crate) track: Option<serde_json::Value>,
    pub(crate) from_secs: Option<f64>,
    pub(crate) to_secs: Option<f64>,
    pub(crate) media_id: Option<String>,
}

/// The tracks and their clips as `list_clips` answers (#507). By default
/// a clip is its id, kind, range, what it is by name and its media id,
/// with speed, off, graded and effects only where they are so: on 70
/// clips the full form was 63k characters, past what a model is given
/// to read. `full` adds source ranges, frames, blend, grade group and
/// the file's path and log format.
pub(crate) fn list_clips(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, revision: u64, f: &ClipFilter) -> Result<serde_json::Value, String> {
    let fps = seq.fps.as_f64();
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    let want_track = |i: usize, t: &klipp_timeline::Track| match &f.track {
        None => true,
        Some(serde_json::Value::Number(n)) => n.as_u64() == Some(i as u64),
        Some(serde_json::Value::String(s)) => s.parse::<usize>().ok() == Some(i) || t.name.eq_ignore_ascii_case(s.trim()),
        Some(_) => false,
    };
    if f.track.is_some() && !seq.tracks.iter().enumerate().any(|(i, t)| want_track(i, t)) {
        let names: Vec<String> = seq.tracks.iter().enumerate().map(|(i, t)| format!("{i} {}", quoted(&t.name))).collect();
        return Err(format!("no track {}; the tracks are {}", f.track.as_ref().map(|v| v.to_string()).unwrap_or_default(), names.join(", ")));
    }
    let want_clip = |c: &klipp_timeline::Clip| {
        let (a, b) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
        f.from_secs.is_none_or(|from| b > from)
            && f.to_secs.is_none_or(|to| a < to)
            && f.media_id.as_ref().is_none_or(|m| c.media_id().is_some_and(|id| id.0.to_string() == m.trim()))
    };
    let narrowed = f.from_secs.is_some() || f.to_secs.is_some() || f.media_id.is_some();
    let name = |c: &klipp_timeline::Clip| -> Option<String> {
        match &c.kind {
            klipp_timeline::ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()),
            // A title or caption says what it says in `text`, whole.
            klipp_timeline::ClipKind::Title(_) => None,
            klipp_timeline::ClipKind::Generator(g) => format!("{g:?}").split(['{', '(', ' ']).next().map(str::to_string),
            _ => None,
        }
    };
    let clip_json = |c: &klipp_timeline::Clip| -> serde_json::Value {
        if f.full {
            let mut v = json!({
                "clip_id": c.id.0.to_string(),
                "kind": clip_kind_name(c),
                "enabled": c.enabled,
                "timeline_in_secs": c.timeline_in.to_secs_f64(),
                "timeline_out_secs": c.timeline_out.to_secs_f64(),
                "timeline_in_frame": (c.timeline_in.to_secs_f64() * fps).round() as i64,
                "timeline_out_frame": (c.timeline_out.to_secs_f64() * fps).round() as i64,
                "source_in_secs": c.source_in.to_secs_f64(),
                "source_out_secs": c.source_out.to_secs_f64(),
                "speed": c.speed,
                "own_name": c.name,
                "colour": (c.colour_label != 0).then(|| klipp_media::ColorLabel::from_u8(c.colour_label).label().to_lowercase()),
                "flag": (!c.grade_flag.is_none()).then(|| c.grade_flag.label().to_lowercase()),
                "blend_mode": format!("{:?}", c.blend_mode),
                "grade_is_identity": c.own_grade().is_identity(),
                "grade_nodes": c.own_grade().nodes.len(),
                "grade_group": c.grade_group.and_then(|g| seq.grade_group(g)).map(|g| g.name.clone()),
                "media": c.media_id()
                    .and_then(|id| pool.get(&id))
                    .map(|m| json!({
                        "media_id": m.id.0.to_string(),
                        "filename": m.filename,
                        "path": m.path.to_string_lossy(),
                        "log_format": log_format_json(m),
                    })),
            });
            if let Some(td) = c.title_data() {
                v["text"] = json!(td.text());
            }
            surfaces_json(c, fps, &mut v);
            water_json(seq, c, &mut v);
            sound_json(seq, c, true, &mut v);
            return v;
        }
        let mut v = json!({
            "clip_id": c.id.0.to_string(),
            "kind": clip_kind_name(c),
            "in_secs": r3(c.timeline_in.to_secs_f64()),
            "out_secs": r3(c.timeline_out.to_secs_f64()),
        });
        // Its own name when it has one (#920), else its media's or text.
        if let Some(n) = c.name.clone().filter(|n| !n.trim().is_empty()).or_else(|| name(c)) {
            v["name"] = json!(n);
        }
        // A title's or caption's words, whole and with their line breaks:
        // what is read back to correct a cue with set_text (#2430).
        if let Some(td) = c.title_data() {
            v["text"] = json!(td.text());
        }
        if c.colour_label != 0 {
            v["colour"] = json!(klipp_media::ColorLabel::from_u8(c.colour_label).label().to_lowercase());
        }
        if !c.grade_flag.is_none() {
            v["flag"] = json!(c.grade_flag.label().to_lowercase());
        }
        if let Some(id) = c.media_id() {
            v["media_id"] = json!(id.0.to_string());
        }
        if (c.speed - 1.0).abs() > 1e-6 {
            v["speed"] = json!(c.speed);
        }
        if !c.enabled {
            v["enabled"] = json!(false);
        }
        if !c.own_grade().is_identity() || c.grade_group.is_some() {
            v["graded"] = json!(true);
        }
        let fx = c.picture_effects().map_or(0, |e| e.len()) + c.picture_ofx().map_or(0, |o| o.len());
        if fx > 0 {
            v["effects"] = json!(fx);
        }
        surfaces_json(c, fps, &mut v);
        water_json(seq, c, &mut v);
        sound_json(seq, c, false, &mut v);
        v
    };
    let tracks: Vec<serde_json::Value> = seq
        .tracks
        .iter()
        .enumerate()
        .filter(|(i, t)| want_track(*i, t))
        .filter_map(|(i, t)| {
            let clips: Vec<serde_json::Value> = t.clips.iter().filter(|c| want_clip(c)).map(clip_json).collect();
            if narrowed && clips.is_empty() && f.track.is_none() {
                return None;
            }
            let mut v = json!({ "index": i, "kind": format!("{:?}", t.kind), "name": t.name });
            if f.full {
                v["opacity"] = json!(t.opacity);
                v["locked"] = json!(t.locked);
            } else if t.locked {
                v["locked"] = json!(true);
            }
            v["clips"] = json!(clips);
            // The transitions between the clips listed (#711), in both
            // forms: there are few, and nothing else shows them.
            let transitions: Vec<serde_json::Value> = t
                .transitions
                .iter()
                .filter(|tr| [tr.clip_a_id, tr.clip_b_id].iter().any(|id| t.find_clip(*id).is_some_and(want_clip)))
                .map(|tr| {
                    let spec = klipp_core::spec_for_kind(tr.kind);
                    let d = tr.duration.to_secs_f64();
                    let mut j = json!({
                        "transition_id": tr.id.0.to_string(),
                        "kind": spec.string_key,
                        "name": spec.display_name,
                        "secs": r3(d),
                        "frames": (d * fps).round() as i64,
                        "alignment": transition_alignment(tr.alignment).1,
                        "clip_a_id": tr.clip_a_id.0.to_string(),
                        "clip_b_id": tr.clip_b_id.0.to_string(),
                    });
                    if let Some((cut, from, to)) = transition_span(t, tr) {
                        j["cut_secs"] = json!(r3(cut));
                        j["from_secs"] = json!(r3(from));
                        j["to_secs"] = json!(r3(to));
                    }
                    j
                })
                .collect();
            if !transitions.is_empty() {
                v["transitions"] = json!(transitions);
            }
            Some(v)
        })
        .collect();
    let mut reply = json!({ "fps": fps, "revision": revision, "tracks": tracks });
    if !f.full {
        reply["note"] = json!("short form: pass full true for source ranges, frames, blend, grade group and file paths; get_grade for a clip's grade");
    }
    Ok(reply)
}

/// Why `path` (media `id`) is music, when it is: a track from a library,
/// its credit file beside it, or media whose every clip on the timeline
/// plays in the music role. `None` for anything that may carry a voice.
pub(crate) fn music_reason(ctx: &ControlContext, path: &std::path::Path, id: klipp_core::MediaId) -> Option<&'static str> {
    if klipp_analysis::is_licensed_audio(path) {
        return Some("a library track");
    }
    let seq = ctx.sequence.read().ok()?;
    let roles: Vec<klipp_core::ClipRole> = seq.tracks.iter().flat_map(|t| t.clips.iter())
        .filter(|c| c.media_id() == Some(id))
        .map(|c| sound_role(&seq, c))
        .collect();
    (!roles.is_empty() && roles.iter().all(|r| *r == klipp_core::ClipRole::Music)).then_some("its clips play as music")
}

/// What `c`'s sound is, by its role: its own when one was set, else
/// that of the clip holding its sound settings, as the document's ops
/// read it (#777). The Inspector sets a role on the clip selected, which
/// may be the sound half of a linked pair; read from the picture only,
/// that role was lost to the music bed and the sound report (#862).
pub(crate) fn sound_role(seq: &klipp_timeline::Sequence, c: &klipp_timeline::Clip) -> klipp_core::ClipRole {
    if c.role != klipp_core::ClipRole::default() { c.role } else { sound_owner(seq, c).role }
}

/// The clip a sound's settings live on: the picture an audio clip is
/// linked to, as the `audio` and `clean_audio` ops keep them, else the
/// clip itself.
pub(crate) fn sound_owner<'a>(seq: &'a klipp_timeline::Sequence, c: &'a klipp_timeline::Clip) -> &'a klipp_timeline::Clip {
    c.linked_clip_id
        .and_then(|id| seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).flat_map(|t| t.clips.iter()).find(|p| p.id == id))
        .unwrap_or(c)
}

/// A clip's sound as set (#740): its level and gain in decibels, pan,
/// fades, how many volume keys it has, its role and its audio effects —
/// what `audio`, `duck`, `role` and `clean_audio` wrote, read back. In
/// the full form for every clip of media; in the brief only what differs
/// from as it came in. A sound linked to a picture keeps its settings on
/// the picture clip, and is read from there on either half.
pub(crate) fn sound_json(seq: &klipp_timeline::Sequence, c: &klipp_timeline::Clip, full: bool, v: &mut serde_json::Value) {
    if !matches!(c.kind, klipp_timeline::ClipKind::Media { .. }) {
        return;
    }
    let role = sound_role(seq, c);
    let c = sound_owner(seq, c);
    let db = |linear: f32| if linear > 0.0 { ((20.0 * linear.log10()) * 10.0).round() / 10.0 } else { -144.0 };
    let mut sound = serde_json::Map::new();
    let mut put = |key: &str, value: serde_json::Value, set: bool| {
        if full || set {
            sound.insert(key.into(), value);
        }
    };
    put("volume_db", json!(db(c.volume)), (c.volume - 1.0).abs() > 1e-4);
    put("gain_db", json!(db(c.gain)), (c.gain - 1.0).abs() > 1e-4);
    put("pan", json!((c.pan * 100.0).round() / 100.0), c.pan.abs() > 1e-4);
    put("fade_in_secs", json!((c.fade_in_secs * 1000.0).round() / 1000.0), c.fade_in_secs > 0.0);
    put("fade_out_secs", json!((c.fade_out_secs * 1000.0).round() / 1000.0), c.fade_out_secs > 0.0);
    put("volume_keys", json!(c.volume_keyframes.len()), !c.volume_keyframes.is_empty());
    put("role", json!(format!("{role:?}").to_lowercase()), role != klipp_core::ClipRole::default());
    let effects: Vec<&str> = c.audio_effects.iter().filter(|e| e.enabled).map(|e| e.effect_type.as_str()).collect();
    put("effects", json!(effects), !effects.is_empty());
    if !sound.is_empty() {
        v["sound"] = serde_json::Value::Object(sound);
    }
}

/// A clip's tracked surfaces and what it is pinned to (#684), on its
/// readout: each surface's id and name, the timeline stretch the track
/// covers, which is where a pin follows it, and the stretches followed
/// uncertainly (`low`) or not at all (`lost`, the last good place held)
/// with the mean confidence and the times of corrections set by hand
/// A clip's reflection's water (#2795), on its readout: the colour
/// measured under it, whether the tint and darkness follow it, and whether
/// the measurement is for footage, a range or a waterline that has since
/// changed (then measure_water measures it again).
pub(crate) fn water_json(seq: &klipp_timeline::Sequence, c: &klipp_timeline::Clip, v: &mut serde_json::Value) {
    let Some((_, klipp_effects::BuiltinEffect::Reflection { water, water_key, tint_auto, darken_auto, .. })) = klipp_render::water::reflection_of(c) else { return };
    let now = klipp_render::water::footprint_key(seq, c.id);
    v["water"] = json!({
        "measured": water_key != 0,
        "srgb": (water_key != 0).then_some(water),
        "outdated": water_key != 0 && now != Some(water_key),
        "tint_from_water": tint_auto,
        "darken_from_water": darken_auto,
    });
}

/// (#2754).
pub(crate) fn surfaces_json(c: &klipp_timeline::Clip, fps: f64, v: &mut serde_json::Value) {
    let on_timeline = |src: f64| ((c.timeline_in.to_secs_f64() + c.elapsed_for_source_secs(src, fps)) * 1000.0).round() / 1000.0;
    if !c.surfaces.is_empty() {
        v["surfaces"] = json!(c.surfaces.iter().map(|s| {
            let mut j = json!({ "surface_id": s.id.to_string(), "name": s.name, "frames": s.keys.len() });
            if let Some((a, b)) = s.tracked_secs() {
                let (a, b) = (on_timeline(a), on_timeline(b));
                j["tracked_secs"] = json!([a.min(b), a.max(b)]);
            }
            let spans = s.weak_spans();
            for (key, state) in [("low", klipp_timeline::TrackState::Low), ("lost", klipp_timeline::TrackState::Lost)] {
                let these: Vec<[f64; 2]> = spans.iter().filter(|sp| sp.2 == state).map(|sp| {
                    let (a, b) = (on_timeline(sp.0), on_timeline(sp.1));
                    [a.min(b), a.max(b)]
                }).collect();
                if !these.is_empty() {
                    j[key] = json!(these);
                }
            }
            let corrections: Vec<f64> = s.keys.iter().filter(|k| k.state == klipp_timeline::TrackState::Manual).map(|k| on_timeline(k.source_secs)).collect();
            if !corrections.is_empty() {
                j["corrections"] = json!(corrections);
            }
            let rated: Vec<f32> = s.keys.iter().filter_map(|k| k.confidence).collect();
            if !rated.is_empty() {
                j["confidence"] = json!((rated.iter().sum::<f32>() / rated.len() as f32 * 100.0).round() / 100.0);
            }
            j
        }).collect::<Vec<_>>());
    }
    if let Some(pin) = c.surface_pin {
        v["pinned_to"] = json!({ "clip_id": pin.host.0.to_string(), "surface_id": pin.surface.to_string() });
    }
}

/// A clip's kind as a client reads it.
pub(crate) fn clip_kind_name(c: &klipp_timeline::Clip) -> &'static str {
    match &c.kind {
        klipp_timeline::ClipKind::Media { .. } => "media",
        klipp_timeline::ClipKind::Title(_) => "title",
        klipp_timeline::ClipKind::Generator(_) => "generator",
        klipp_timeline::ClipKind::AdjustmentLayer => "adjustment",
        klipp_timeline::ClipKind::Gap => "gap",
        klipp_timeline::ClipKind::Sequence { .. } => "sequence",
        klipp_timeline::ClipKind::Precomposition { .. } => "precomposition",
        klipp_timeline::ClipKind::TemplatePrecomposition { .. } => "template_precomposition",
    }
}

pub(crate) fn log_format_json(m: &klipp_media::MediaItem) -> serde_json::Value {
    json!({
        // What the file's metadata says, and what Klipp decodes it as —
        // the user's choice when they made one, the detection otherwise.
        "detected": format!("{:?}", m.detected_log_format),
        "effective": format!("{:?}", m.effective_log_format()),
    })
}

/// A placed template's or recipe's controls (#2757, #2758): each with its
/// value, and each target it drives, its range and curve, and whether set
/// by hand or unlinked.
fn write_links(out: &mut String, inst: &klipp_core::TemplateInstance) {
    use std::fmt::Write as _;
    let mut seen: Vec<&str> = Vec::new();
    for l in &inst.links {
        if !seen.contains(&l.parameter.as_str()) {
            seen.push(&l.parameter);
            let v = match inst.overrides.get(&l.parameter) { Some(klipp_core::TemplateValue::Number(n)) => *n, _ => l.from[0] };
            let _ = write!(out, "  control {} = {v}:", quoted(&l.parameter));
        }
        let state = if !l.linked { format!(" unlinked at {}", l.frozen) } else if l.offset != 0.0 { format!(" offset {:+}", l.offset) } else { String::new() };
        let _ = write!(out, " {} {}..{} {}{state};", word(&l.property), l.to[0], l.to[1], l.curve.name());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2492: a timeline grade is said, and a clip with none of its own
    /// reads as under it, not as ungraded; a still's zoom is said.
    #[test]
    fn a_timeline_grade_and_a_zoom_are_said() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let still = klipp_timeline::test_clip::TestClip::of(klipp_timeline::Clip::gap(tc(0.0), tc(4.0))).lane("scale", 0.0, 1.0).lane("scale", 4.0, 1.2).build();
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks[0].clips = vec![still];
        let pool = klipp_media::MediaPool::new();
        let plain = timeline_text(&seq, &pool, None, 1, TimelineView::default());
        assert!(plain.contains("grade none") && !plain.contains("Timeline grade") && plain.contains("zoom 1.00 to 1.20"), "{plain}");
        use klipp_nodes::{graph::{Edge, Node, NodeGraph, NodeId}, kinds::NodeKind, params::NodeParams};
        let node = |kind: NodeKind, params: NodeParams| Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
        let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, NodeParams { exposure: Some(0.5), ..Default::default() }), node(NodeKind::Output, NodeParams::default())];
        let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
        let graph = NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
        assert!(!graph.is_identity());
        seq.timeline_grade_slot().set(Some(graph));
        let text = timeline_text(&seq, &pool, None, 1, TimelineView::default());
        assert!(text.contains("Timeline grade: 3 nodes") && text.contains("grade none of its own, under the timeline's grade"), "{text}");
    }

    /// #2861: the buses are listed with where they go, and a track's sends
    /// on its line; a sequence without them says nothing of them.
    #[test]
    fn the_buses_and_a_tracks_sends_are_said() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let pool = klipp_media::MediaPool::new();
        assert!(!timeline_text(&seq, &pool, None, 1, TimelineView::default()).contains("Aux buses"));
        let (mut verb, delay) = (klipp_timeline::AuxBus::new("Verb"), klipp_timeline::AuxBus::new("Delay"));
        verb.output = Some(delay.id);
        verb.volume = 0.5;
        let a = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
        seq.tracks[a].sends = vec![klipp_timeline::AuxSend { pre_fader: true, ..klipp_timeline::AuxSend::new(verb.id) }];
        seq.buses = vec![verb, delay];
        let text = timeline_text(&seq, &pool, None, 1, TimelineView::default());
        assert!(text.contains("Aux buses: [0] \"Verb\" -6.0 dB, to \"Delay\"; [1] \"Delay\" +0.0 dB, to master."), "{text}");
        assert!(text.contains("sends to: \"Verb\" +0.0 dB pre-fader"), "{text}");
    }

    /// #2430: a caption's or title's words come back whole, with their
    /// line breaks, in the short form and in full.
    #[test]
    fn a_caption_lists_its_whole_text() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let words = "A first line that runs well past forty characters\nand a second line";
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: words.into(), ..Default::default() });
        let caption = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), source_in: tc(0.0), source_out: tc(2.0), ..klipp_timeline::Clip::gap(tc(1.0), tc(3.0)) };
        let mut subs = klipp_timeline::Track::new("Subtitle 1", klipp_timeline::TrackKind::Subtitle);
        subs.clips.push(caption);
        let mut seq = klipp_timeline::Sequence::new_default();
        seq.tracks = vec![subs];
        let pool = klipp_media::MediaPool::new();
        for full in [false, true] {
            let v = list_clips(&seq, &pool, 0, &ClipFilter { full, track: None, from_secs: None, to_secs: None, media_id: None }).unwrap();
            let clip = &v["tracks"][0]["clips"][0];
            assert_eq!(clip["text"].as_str(), Some(words), "full {full}: {clip}");
            assert!(clip.get("name").is_none(), "no cut-short copy beside it: {clip}");
        }
    }
}

