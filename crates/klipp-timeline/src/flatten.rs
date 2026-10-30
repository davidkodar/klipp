//! Compound clip flattening — shared by viewer (`sync_sequence_to_engine`)
//! and exporter (`snapshot.rs`).
//!
//! [`flatten_sequence`] produces a flat copy of a `Sequence` with every
//! `ClipKind::Sequence { sequence_id }` clip replaced by the inner clips
//! it references, with timeline coordinates remapped onto the parent.
//! The viewer engine has no native compound-clip awareness — it consumes
//! a flat sequence; the exporter does its own per-clip mapping on top of
//! the same flat output.
//!
//! A `ClipKind::Precomposition` is left exactly where it is: it is
//! composed on its own surface by the renderer, and flattening it would
//! be the wrong picture that kind exists to rule out. What flattening
//! does for it instead is resolve its scene into [`Clip::precomp`] — an
//! instance of the inner sequence, flattened itself, with every clip
//! re-identified for that instance — and put the scene's sound where
//! the mixer will find it (see [`resolve_precomposition`]).
//!
//! # Known limitations
//!
//! **Linked clip pairs:** If a compound clip's inner sequence contains
//! clips linked via `linked_clip_id` (audio/video pairs), the flattened
//! output preserves the original `ClipId` values. If the same inner clip
//! appears in two separate compound clip instances, both flattened copies
//! share the same `ClipId`. The engine's linked-clip resolution
//! (`iter().find(|c| c.id == lid)`) returns the first match, which may
//! not be the correct instance.
//!
//! **Workaround:** Compound clips that contain linked audio/video pairs
//! will have their audio/video sync preserved if the compound clip is the
//! only user of that inner sequence. When the same inner sequence is used
//! in multiple compound clip instances simultaneously, audio/video sync
//! within those instances may be incorrect. Re-IDing flattened clips is
//! deferred to a follow-up PRD.

use crate::{Clip, ClipKind, Sequence, Track, TrackKind};
use klipp_core::{ClipId, RationalTime, SequenceId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// What a template binding names (#225, #2757): a title layer's property,
/// `object:<i>.<property>` (the i-th layer of the composition's titles, in
/// track order), or a media slot, `clip:<n>.media` (its n-th clip).
enum Target<'a> {
    Object(usize, &'a str),
    Slot(usize),
}

/// A graph node's parameter, `node:<op>.<param>`: a recipe's (#2758),
/// set when the recipe is built into a clip's picture. A composition has
/// no graph ops to name.
const NODE_TARGETS_LATER: &str = "is a recipe's node parameter (a recipe sets it when it is placed); a composition template cannot bind one";

fn binding_parts(property: &str) -> Result<Target<'_>, String> {
    if property.starts_with("node:") {
        return Err(format!("template binding '{property}' {NODE_TARGETS_LATER}"));
    }
    if let Some(rest) = property.strip_prefix("clip:") {
        let Some((n, "media")) = rest.split_once('.') else {
            return Err(format!("template binding '{property}' must use clip:<index>.media"));
        };
        let n = n.parse::<usize>().map_err(|_| format!("template binding '{property}' has an invalid clip index"))?;
        return Ok(Target::Slot(n));
    }
    let Some((object, path)) = property.strip_prefix("object:").and_then(|p| p.split_once('.')) else {
        return Err(format!("template binding '{property}' must use object:<index>.<property> or clip:<index>.media"));
    };
    let index = object.parse::<usize>().map_err(|_| format!("template binding '{property}' has an invalid object index"))?;
    Ok(Target::Object(index, path))
}

/// The number a title layer's `path` holds (#2757): what a control's link
/// drives. `None` for a path that is not a number of that layer.
fn layer_number(o: &crate::TitleObject, path: &str) -> Option<f64> {
    use crate::TitleObjectContent as C;
    let tb = match &o.content { C::TextBox(tb) => Some(tb), _ => None };
    Some(match path {
        "size" => tb?.font_size as f64,
        "tracking" => tb?.tracking as f64,
        "line_spacing" => tb?.line_spacing as f64,
        "opacity" => o.opacity as f64,
        "x" => o.transform.center.x as f64,
        "y" => o.transform.center.y as f64,
        "rotation" => o.transform.rotation as f64,
        "motion.enter.duration_secs" => o.fx().motion.enter.as_ref()?.duration_secs as f64,
        "motion.exit.duration_secs" => o.fx().motion.exit.as_ref()?.duration_secs as f64,
        _ => return None,
    })
}

/// Sets that number (#2757), kept in its range: a size above 1, an
/// opacity 0 to 1, a duration above a frame's worth.
fn set_layer_number(o: &mut crate::TitleObject, path: &str, v: f64) {
    use crate::TitleObjectContent as C;
    let v = v as f32;
    match (path, &mut o.content) {
        ("size", C::TextBox(tb)) => tb.font_size = v.max(1.0),
        ("tracking", C::TextBox(tb)) => tb.tracking = v,
        ("line_spacing", C::TextBox(tb)) => tb.line_spacing = v.max(0.1),
        ("opacity", _) => o.opacity = v.clamp(0.0, 1.0),
        ("x", _) => o.transform.center.x = v,
        ("y", _) => o.transform.center.y = v,
        ("rotation", _) => o.transform.rotation = v,
        ("motion.enter.duration_secs", _) => if let Some(m) = o.fx_mut().motion.enter.as_mut() { m.duration_secs = v.max(0.01) },
        ("motion.exit.duration_secs", _) => if let Some(m) = o.fx_mut().motion.exit.as_mut() { m.duration_secs = v.max(0.01) },
        _ => {}
    }
}

/// What a link to `property` in `sequence` reads now: the number it drives,
/// for an unlink to keep (#2757).
pub fn linked_value(sequence: &Sequence, property: &str) -> Option<f64> {
    let Ok(Target::Object(index, path)) = binding_parts(property) else { return None };
    let o = sequence.tracks.iter().flat_map(|t| &t.clips).filter_map(|c| c.title_data()).flat_map(|t| t.objects.iter()).nth(index)?;
    layer_number(o, path)
}

/// A slot no media was given for: a dark neutral picture, so the
/// template shows where its media goes (#2757).
pub const SLOT_PLACEHOLDER: [u8; 4] = [52, 54, 60, 255];

/// Validate the binding grammar once, at insertion, so flattening cannot
/// silently reinterpret a saved template instance.
pub fn validate_template_instance(instance: &klipp_core::TemplateInstance, sequence: &Sequence) -> Result<(), String> {
    for (parameter, property) in &instance.bindings {
        let (index, path) = match binding_parts(property).map_err(|e| format!("parameter '{parameter}': {e}"))? {
            Target::Slot(n) => {
                sequence.tracks.iter().flat_map(|track| &track.clips).nth(n).ok_or_else(|| format!("slot '{parameter}' targets missing clip {n}"))?;
                continue;
            }
            Target::Object(index, path) => (index, path),
        };
        let target = sequence.tracks.iter().flat_map(|track| &track.clips)
            .filter_map(|clip| match &clip.kind { ClipKind::Title(title) => Some(title), _ => None })
            .flat_map(|title| title.objects.iter()).nth(index)
            .ok_or_else(|| format!("parameter '{parameter}' targets missing object {index}"))?;
        if let Some(value) = instance.overrides.get(parameter) {
            use crate::TitleObjectContent as C;
            match (path, value) {
                ("text", klipp_core::TemplateValue::Text(_)) if matches!(target.content, C::TextBox(_)) => {}
                ("fill_and_glow", klipp_core::TemplateValue::Color(_)) if matches!(target.content, C::Shape(_)) => {}
                ("color", klipp_core::TemplateValue::Color(_)) if matches!(target.content, C::TextBox(_) | C::Shape(_)) => {}
                ("font", klipp_core::TemplateValue::Text(_)) if matches!(target.content, C::TextBox(_)) => {}
                ("picture", klipp_core::TemplateValue::Text(_)) if matches!(target.content, C::Image(_)) => {}
                ("motion.enter.duration_secs", klipp_core::TemplateValue::Number(_)) if target.fx().motion.enter.is_some() => {}
                ("motion.enter.direction", klipp_core::TemplateValue::Enum(direction)) if ["left", "right", "up", "down"].contains(&direction.as_str()) && target.fx().motion.enter.is_some() => {}
                ("float.keyframes", klipp_core::TemplateValue::Number(_)) if !target.transform.center_y_keyframes.is_empty() => {}
                _ => return Err(format!("value for parameter '{parameter}' does not fit '{property}'")),
            }
        }
    }
    // A control's links (#2757): each to a number of a layer there is.
    for link in &instance.links {
        let Target::Object(index, path) = binding_parts(&link.property).map_err(|e| format!("control '{}': {e}", link.parameter))? else {
            return Err(format!("control '{}' drives '{}', which is not a number", link.parameter, link.property));
        };
        let target = sequence.tracks.iter().flat_map(|track| &track.clips).filter_map(|c| c.title_data()).flat_map(|t| t.objects.iter()).nth(index)
            .ok_or_else(|| format!("control '{}' targets missing object {index}", link.parameter))?;
        if layer_number(target, path).is_none() {
            return Err(format!("control '{}' drives '{}', which is not a number of that layer", link.parameter, link.property));
        }
    }
    Ok(())
}

fn apply_template_instance(sequence: &mut Sequence, instance: &klipp_core::TemplateInstance) -> Result<(), String> {
    validate_template_instance(instance, sequence)?;
    // Media slots (#2757): the clip given its media, or the placeholder.
    for (slot, property) in &instance.bindings {
        let Ok(Target::Slot(n)) = binding_parts(property) else { continue };
        let Some(clip) = sequence.tracks.iter_mut().flat_map(|track| track.clips.iter_mut()).nth(n) else { continue };
        clip.kind = match instance.media_slots.get(slot).and_then(|id| id.parse::<uuid::Uuid>().ok()) {
            Some(id) => ClipKind::Media { media_id: klipp_core::MediaId(id) },
            None => ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: SLOT_PLACEHOLDER }),
        };
    }
    // Each title layer's index counts across the composition's titles,
    // in track order, as `validate_template_instance` counts them.
    let mut first = 0;
    for track in &mut sequence.tracks {
        for clip in &mut track.clips {
            let ClipKind::Title(title) = &mut clip.kind else { continue };
            let base = first;
            first += title.objects.len();
            for (parameter, property) in &instance.bindings {
                let Some(value) = instance.overrides.get(parameter) else { continue };
                let Ok(Target::Object(index, path)) = binding_parts(property) else { continue };
                let Some(index) = index.checked_sub(base) else { continue };
                let Some(target) = title.objects.get_mut(index) else { continue };
                match (path, value) {
                    ("color", klipp_core::TemplateValue::Color(color)) => {
                        let rgba = color.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                        match &mut target.content {
                            crate::TitleObjectContent::TextBox(tb) => tb.color = rgba,
                            crate::TitleObjectContent::Shape(shape) => shape.fill = Some(rgba),
                            _ => {}
                        }
                    }
                    ("font", klipp_core::TemplateValue::Text(family)) => {
                        if let crate::TitleObjectContent::TextBox(tb) = &mut target.content { tb.font_name = family.clone(); }
                    }
                    ("picture", klipp_core::TemplateValue::Text(path)) => {
                        if let crate::TitleObjectContent::Image(img) = &mut target.content { img.path = path.clone(); }
                    }
                    ("text", klipp_core::TemplateValue::Text(text)) => {
                        if let crate::TitleObjectContent::TextBox(box_) = &mut target.content { box_.text = text.clone(); }
                    }
                    ("fill_and_glow", klipp_core::TemplateValue::Color(color)) => {
                        let rgba = [
                            (color[0].clamp(0.0, 1.0) * 255.0) as u8,
                            (color[1].clamp(0.0, 1.0) * 255.0) as u8,
                            (color[2].clamp(0.0, 1.0) * 255.0) as u8,
                            (color[3].clamp(0.0, 1.0) * 255.0) as u8,
                        ];
                        if let crate::TitleObjectContent::Shape(shape) = &mut target.content { shape.fill = Some(rgba); }
                        if let Some(glow) = target.fx_mut().glows.first_mut() { glow.color = rgba; }
                    }
                    ("motion.enter.duration_secs", klipp_core::TemplateValue::Number(seconds)) => {
                        if let Some(motion) = target.fx_mut().motion.enter.as_mut() { motion.duration_secs = *seconds as f32; }
                    }
                    ("motion.enter.direction", klipp_core::TemplateValue::Enum(direction)) => {
                        if let Some(motion) = target.fx_mut().motion.enter.as_mut() {
                            motion.kind = match direction.as_str() {
                                "left" => crate::MotionKind::SlideRight,
                                "right" => crate::MotionKind::SlideLeft,
                                "up" => crate::MotionKind::Rise,
                                "down" => crate::MotionKind::Drop,
                                _ => motion.kind,
                            };
                        }
                    }
                    ("float.keyframes", klipp_core::TemplateValue::Number(amount)) => {
                        for key in &mut target.transform.center_y_keyframes { key.value += *amount as f32 * 0.01; }
                    }
                    _ => return Err(format!("unsupported value for template binding '{parameter}' at '{property}'")),
                }
            }
        }
    }
    // Each control's links, from where the control stands (#2757).
    let mut objects: Vec<&mut crate::TitleObject> = sequence.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).filter_map(|c| match &mut c.kind { ClipKind::Title(t) => Some(t), _ => None }).flat_map(|t| t.objects.iter_mut()).collect();
    for link in &instance.links {
        let Ok(Target::Object(index, path)) = binding_parts(&link.property) else { continue };
        let control = match instance.overrides.get(&link.parameter) {
            Some(klipp_core::TemplateValue::Number(n)) => *n,
            _ => link.from[0],
        };
        if let Some(o) = objects.get_mut(index) {
            set_layer_number(o, path, link.effective(control));
        }
    }
    Ok(())
}

/// Maximum nesting depth for compound clips. Beyond this depth the
/// recursion stops and no further clips are produced.
pub const MAX_COMPOUND_DEPTH: u8 = 8;

/// Produce a flat copy of `seq` with all `ClipKind::Sequence` clips
/// recursively replaced by the inner clips they reference.
/// `all_sequences` must contain every sequence in the project.
/// The returned `Sequence` shares `seq`'s id, name, fps, resolution,
/// markers, in_point, out_point, and last_playhead — only `tracks` differ.
pub fn flatten_sequence(seq: &Sequence, all_sequences: &[Sequence]) -> Sequence {
    flatten_top(seq, all_sequences, None)
}

/// Each precomposition's scene as last flattened, by what it was made
/// from (#3117): the clip (its id names the scene's clips, its kind holds
/// a template's overrides), the scenes above it, and every sequence the
/// top one reaches. An edit flattens again only the scenes it changed:
/// with 200 template instances on the timeline every edit flattened all
/// 200, about 33 ms. A scene not reached by a pass is dropped after it.
#[derive(Default)]
pub struct FlattenCache {
    scenes: HashMap<u64, Arc<Sequence>>,
    used: HashSet<u64>,
    /// Scenes taken from the cache and made again, since it was made.
    pub hits: usize,
    pub misses: usize,
}

/// [`flatten_sequence`], each precomposition's scene taken from `cache`
/// when nothing it was made from has changed.
pub fn flatten_sequence_cached(seq: &Sequence, all_sequences: &[Sequence], cache: &mut FlattenCache) -> Sequence {
    let flat = flatten_top(seq, all_sequences, Some(cache));
    let FlattenCache { scenes, used, .. } = cache;
    scenes.retain(|k, _| used.contains(k));
    used.clear();
    flat
}

/// The scenes a pass resolves, and the cache it takes them from.
struct Scenes<'c> {
    cache: Option<&'c mut FlattenCache>,
    /// The fingerprint of every sequence the top one reaches.
    sources: u64,
}

fn flatten_top(seq: &Sequence, all_sequences: &[Sequence], cache: Option<&mut FlattenCache>) -> Sequence {
    // Grade groups wrap each member's grade before anything is expanded,
    // each sequence with its own groups, so a scene inside a compound
    // grades as it does on its own (#237); then each track's effects go
    // onto its clips the same way (#846).
    let mut flat = if seq.bakes_for_render() || all_sequences.iter().any(Sequence::bakes_for_render) {
        let bake = |s: &Sequence| s.baked_for_render().into_owned();
        let all: Vec<Sequence> = all_sequences.iter().map(bake).collect();
        let top = bake(seq);
        let sources = if cache.is_some() { reached_fingerprint(&top, &all) } else { 0 };
        flatten_within(&top, &all, &mut Vec::new(), None, &mut Scenes { cache, sources })
    } else {
        let sources = if cache.is_some() { reached_fingerprint(seq, all_sequences) } else { 0 };
        flatten_within(seq, all_sequences, &mut Vec::new(), None, &mut Scenes { cache, sources })
    };
    // Each Frame Glide over its clip's length as it is now (#611): the
    // viewer and the export both render from here, so a trim moves End
    // to the clip's last frame in both.
    crate::frame_glide::refresh(&mut flat);
    flat
}

/// [`flatten_sequence`] within an instance: `namespace` is the clip
/// this sequence is the scene of, `None` at the top. The chain of
/// sequences being resolved is kept so a scene that contains itself, at
/// any remove, is refused rather than followed forever.
///
/// This instance's clips are renamed under `namespace` *before* the
/// scenes inside it are resolved, so a scene inside a scene is named
/// under the instance-scoped id of the clip that holds it: the same
/// leaf reached through two instances of a middle scene is two leaves.
fn flatten_within(seq: &Sequence, all_sequences: &[Sequence], ancestors: &mut Vec<SequenceId>, namespace: Option<ClipId>, scenes: &mut Scenes) -> Sequence {
    ancestors.push(seq.id);
    let mut flat = seq.clone();
    let mut graded = HashSet::new();
    flat.tracks = seq
        .tracks
        .iter()
        .map(|track| flatten_track(track, all_sequences, 0, &mut graded))
        .collect();
    if let Some(ns) = namespace {
        let ids: HashMap<ClipId, ClipId> = flat
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| (c.id, instance_id(ns, c.id)))
            .collect();
        reidentify(&mut flat, &ids);
        graded = graded.iter().map(|id| ids.get(id).copied().unwrap_or(*id)).collect();
    }
    let mut sound = Vec::new();
    for track in &mut flat.tracks {
        let carrier = Controls::of(track);
        for clip in &mut track.clips {
            if let ClipKind::Precomposition { sequence_id, .. } | ClipKind::TemplatePrecomposition { sequence_id, .. } = clip.kind {
                // A clip that is off is off: no scene, no sound.
                clip.precomp = if clip.enabled { resolve_scene(clip, sequence_id, all_sequences, ancestors, scenes) } else { None };
                // A graded compound's sound stays with its own audio
                // clip, which is flattened as before: taken from the
                // scene too, it would play twice.
                if let Some(scene) = clip.precomp.as_ref().filter(|_| !graded.contains(&clip.id)) {
                    sound.extend(scene_sound(clip, scene, &carrier));
                }
            }
        }
    }
    flat.tracks.extend(sound);
    ancestors.pop();
    flat
}

/// One precomposition clip's scene, as an instance of its own.
///
/// The inner sequence is flattened like any other (its compounds
/// expanded, its own precompositions resolved), and every clip in it is
/// given an id derived from this clip's id and its own — the same scene
/// laid in twice is two sets of clips, and a link, a matte or a
/// transition inside the scene points at the copy in its own instance.
/// The persisted ids are untouched; this is a render-time copy.
///
/// `None`, and so nothing rendered, when the scene is missing, when it
/// contains itself at any remove, or past [`MAX_COMPOUND_DEPTH`].
pub fn resolve_precomposition(
    clip: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    ancestors: &mut Vec<SequenceId>,
) -> Option<Arc<Sequence>> {
    resolve_scene(clip, sequence_id, all_sequences, ancestors, &mut Scenes { cache: None, sources: 0 })
}

fn resolve_scene(clip: &Clip, sequence_id: SequenceId, all_sequences: &[Sequence], ancestors: &mut Vec<SequenceId>, scenes: &mut Scenes) -> Option<Arc<Sequence>> {
    if ancestors.contains(&sequence_id) || ancestors.len() >= MAX_COMPOUND_DEPTH as usize {
        return None;
    }
    let key = scenes.cache.is_some().then(|| scene_key(scenes.sources, clip, ancestors));
    if let (Some(k), Some(cache)) = (key, scenes.cache.as_deref_mut()) {
        if let Some(scene) = cache.scenes.get(&k).cloned() {
            cache.used.insert(k);
            cache.hits += 1;
            return Some(scene);
        }
    }
    let mut inner = all_sequences.iter().find(|s| s.id == sequence_id)?.clone();
    if let ClipKind::TemplatePrecomposition { template_instance: instance, .. } = &clip.kind {
        if apply_template_instance(&mut inner, instance).is_err() {
            return None;
        }
    }
    let scene = Arc::new(flatten_within(&inner, all_sequences, ancestors, Some(clip.id), scenes));
    if let (Some(k), Some(cache)) = (key, scenes.cache.as_deref_mut()) {
        cache.scenes.insert(k, Arc::clone(&scene));
        cache.used.insert(k);
        cache.misses += 1;
    }
    Some(scene)
}

/// What a scene is made from, as one number: the sequences the top one
/// reaches, the clip that places it, and the scenes above it.
fn scene_key(sources: u64, clip: &Clip, ancestors: &[SequenceId]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    sources.hash(&mut h);
    clip.id.hash(&mut h);
    serde_json::to_vec(&clip.kind).unwrap_or_default().hash(&mut h);
    ancestors.hash(&mut h);
    h.finish()
}

/// A fingerprint of every sequence `top` reaches through its compounds
/// and precompositions, at any remove: what its scenes are made of.
fn reached_fingerprint(top: &Sequence, all: &[Sequence]) -> u64 {
    use std::hash::{Hash, Hasher};
    let refs = |s: &Sequence| -> Vec<SequenceId> {
        s.tracks.iter().flat_map(|t| t.clips.iter()).filter_map(|c| match c.kind {
            ClipKind::Precomposition { sequence_id, .. } | ClipKind::TemplatePrecomposition { sequence_id, .. } | ClipKind::Sequence { sequence_id } => Some(sequence_id),
            _ => None,
        }).collect()
    };
    let mut reached: Vec<SequenceId> = Vec::new();
    let mut todo = refs(top);
    while let Some(id) = todo.pop() {
        if id == top.id || reached.contains(&id) {
            continue;
        }
        reached.push(id);
        if let Some(s) = all.iter().find(|s| s.id == id) {
            todo.extend(refs(s));
        }
    }
    reached.sort_by_key(|id| id.0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for id in reached {
        id.hash(&mut h);
        if let Some(s) = all.iter().find(|s| s.id == id) {
            serde_json::to_vec(s).unwrap_or_default().hash(&mut h);
        }
    }
    h.finish()
}

/// A track's say over the sound on it.
struct Controls {
    muted: bool,
    solo: bool,
    volume: f32,
    pan: f32,
}

impl Controls {
    fn of(track: &Track) -> Self {
        Self { muted: track.muted, solo: track.solo, volume: track.volume, pan: track.pan }
    }
}

/// A scene's sound, on tracks the mixer reads.
///
/// A surface cannot carry sound, so each scene track with audio-bearing
/// clips becomes a runtime audio track of the containing sequence: its
/// clips sliced onto the precomposition's window (at the instance's
/// ids), and its controls composed with the containing track's — muted
/// if either is, the volumes multiplied, the pans added. Solo is settled
/// inside the scene, where it was set: a soloed scene track silences
/// its siblings and nothing outside the scene. The runtime track takes
/// the containing track's solo, so it stands or falls with it. A scene
/// inside the scene has already had this done at its own level, and its
/// runtime tracks come through here like any other, composed once more.
///
/// A video clip whose audio half sits on an audio track of the scene
/// sounds through that half, as it does in the scene — and with the
/// picture's sound settings, as it does in the scene: a linked pair's
/// volume, pan, fades and automation live on the picture, and the
/// picture stays inside the instance where the mixer cannot see it. So
/// the settings are written onto the sound clip here, while the picture
/// is still in hand, and the link is dropped: the runtime clip carries
/// what it needs and points at nothing.
fn scene_sound(clip: &Clip, scene: &Sequence, carrier: &Controls) -> Vec<Track> {
    let rate = clip.timeline_in.rate;
    let (t_in, s_in, s_out) = (clip.timeline_in.to_secs_f64(), clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
    let any_solo = scene.tracks.iter().any(|t| t.solo);
    let find = |id: ClipId| scene.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id);
    let mut out = Vec::new();
    for track in &scene.tracks {
        let clips: Vec<Clip> = track
            .clips
            .iter()
            .filter(|c| c.enabled && c.media_id().is_some())
            .filter(|c| {
                !(track.kind == TrackKind::Video
                    && c.linked_clip_id.is_some_and(|lid| {
                        scene.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|p| p.id == lid))
                    }))
            })
            .filter_map(|c| {
                let mut sliced = slice_inner_clip_to_compound(c, t_in, s_in, s_out, clip.speed, rate)?;
                if let Some(picture) = c.linked_clip_id.and_then(find).filter(|p| p.id != c.id) {
                    take_sound_settings(&mut sliced, picture);
                }
                sliced.linked_clip_id = None;
                // What the slice trimmed off the head, in the clip's own
                // seconds: automation is written from the clip's start,
                // and the start moved.
                let head = sliced.source_in.to_secs_f64() - c.source_in.to_secs_f64();
                shift_sound_automation(&mut sliced, head, clip.speed);
                Some(sliced)
            })
            .collect();
        if clips.is_empty() {
            continue;
        }
        let mut t = Track::new(&format!("{} · {}", scene.name, track.name), TrackKind::Audio);
        t.clips = clips;
        t.muted = track.muted || carrier.muted || (any_solo && !track.solo);
        t.solo = carrier.solo;
        t.volume = track.volume * carrier.volume;
        t.pan = (track.pan + carrier.pan).clamp(-1.0, 1.0);
        t.precomposition_sound = true;
        out.push(t);
    }
    out
}

/// The built-in effect at list place `i` of `clip`, as its picture's node
/// holds it.
fn entry_at(clip: &Clip, i: usize) -> Option<klipp_effects::BuiltinEffect> {
    let id = clip.effect_node_at(false, i)?;
    let lifted = clip.picture_v2().ok()?;
    let mut n = lifted.picture.graph.nodes.iter().find(|n| n.id == id)?.clone();
    klipp_graph_lift::effect_params::into_memory(&mut n).ok()?;
    klipp_graph_lift::effect_params::effect_of(&n).ok()
}

/// `clip` plays with `picture`'s sound settings — the ones the mixer
/// reads from a linked pair's picture: the levels, the fades, the
/// channel layout, and the automation and modifiers on them.
fn take_sound_settings(clip: &mut Clip, picture: &Clip) {
    clip.volume = picture.volume;
    clip.gain = picture.gain;
    clip.pan = picture.pan;
    clip.fade_in_secs = picture.fade_in_secs;
    clip.fade_out_secs = picture.fade_out_secs;
    clip.fade_in_curve = picture.fade_in_curve;
    clip.fade_out_curve = picture.fade_out_curve;
    clip.channel_config = picture.channel_config;
    clip.volume_keyframes = picture.volume_keyframes.clone();
    clip.gain_keyframes = picture.gain_keyframes.clone();
    clip.pan_keyframes = picture.pan_keyframes.clone();
    // The picture's clip-level modifiers are its sound settings too; its
    // effect-targeted ones animate the picture's effects, which a sound
    // clip doesn't carry (#1074).
    let effect = |id: &klipp_core::ParamId| matches!(id, klipp_core::ParamId::Effect { .. });
    clip.edit_param_modifiers(|v| v.retain(|id, _| effect(id)));
    clip.edit_param_modifiers(|v| v.extend(picture.sound_modifiers().iter().map(|(id, m)| (id.clone(), m.clone()))));
}

/// Sound automation is written in seconds from the clip's start; a slice
/// that trimmed `head` seconds off the start, played at `speed`, moves
/// every key earlier by that head and closer together by that speed, so
/// the level at any moment of the scene is the level at that moment of
/// the parent. A key now before the start still anchors the value there.
fn shift_sound_automation(clip: &mut Clip, head: f64, speed: f64) {
    if head.abs() < 1e-9 && (speed - 1.0).abs() < 1e-9 {
        return;
    }
    let speed = if speed.abs() < 1e-9 { 1.0 } else { speed };
    for keys in [&mut clip.volume_keyframes, &mut clip.gain_keyframes, &mut clip.pan_keyframes] {
        for k in keys.iter_mut() {
            k.time_secs = (k.time_secs - head) / speed;
        }
    }
}

/// The id a scene's clip has inside one instance of the scene: a name
/// derived from the instance and the clip, the same every time they are
/// resolved together and different for every other instance.
pub fn instance_id(instance: ClipId, clip: ClipId) -> ClipId {
    ClipId(uuid::Uuid::new_v5(&instance.0, clip.0.as_bytes()))
}

/// Every clip in `seq` renamed by `ids`, and every reference between
/// them renamed to match; a reference to a clip outside `seq` is left
/// as it is.
pub fn reidentify(seq: &mut Sequence, ids: &HashMap<ClipId, ClipId>) {
    let renamed = |id: ClipId| ids.get(&id).copied().unwrap_or(id);
    for track in &mut seq.tracks {
        for c in &mut track.clips {
            c.id = renamed(c.id);
            c.linked_clip_id = c.linked_clip_id.map(renamed);
            if let Some(link) = &mut c.transform_link {
                match &mut link.target {
                    crate::TransformLinkTarget::Clip(id) => *id = renamed(*id),
                    crate::TransformLinkTarget::Tracker { clip_id, .. } => *clip_id = renamed(*clip_id),
                }
            }
            if let Some(m) = &mut c.matte_source {
                m.source_clip_id = renamed(m.source_clip_id);
            }
            if let Some(pin) = &mut c.surface_pin {
                pin.host = renamed(pin.host);
            }
            // A Transform's focal point names a clip: it follows the rename,
            // an entry edit through the clip's picture (M4 step 12f).
            let edits: Vec<(usize, klipp_effects::BuiltinEffect)> = (0..)
                .map_while(|i| c.effect_node_at(false, i).map(|_| i))
                .filter_map(|i| {
                    let mut e = entry_at(c, i)?;
                    let klipp_effects::BuiltinEffect::Transform { focal_ref: Some(f), .. } = &mut e else { return None };
                    match f {
                        klipp_effects::FocalRef::Glyph { title_clip_id, .. } => *title_clip_id = renamed(ClipId(*title_clip_id)).0,
                        klipp_effects::FocalRef::Tracker { clip_id, .. } => *clip_id = renamed(ClipId(*clip_id)).0,
                    }
                    Some((i, e))
                })
                .collect();
            let updates: Vec<crate::EffectEdit> = edits.iter().map(|(i, e)| crate::EffectEdit::Update(*i, e)).collect();
            if let Some(Err(why)) = c.apply_effect_edits(&updates) {
                tracing::warn!("clip {:?}: a focal point's clip renamed: {why}", c.id);
            }
        }
        for tr in &mut track.transitions {
            tr.clip_a_id = renamed(tr.clip_a_id);
            tr.clip_b_id = renamed(tr.clip_b_id);
        }
        if let Some(a) = &mut track.storyline_anchor {
            a.anchor_clip_id = renamed(a.anchor_clip_id);
        }
    }
}

/// Returns true if a compound clip on a `parent_kind` track should pull in
/// inner clips from a track of `inner_kind`. Mirrors the per-track-type
/// split in the export path: video/text/subtitle compose together; audio
/// is its own lane.
fn track_kinds_compatible(parent_kind: TrackKind, inner_kind: TrackKind) -> bool {
    match parent_kind {
        TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => matches!(
            inner_kind,
            TrackKind::Video | TrackKind::Text | TrackKind::Subtitle
        ),
        TrackKind::Audio => inner_kind == TrackKind::Audio,
    }
}

/// Whether a compound's inner sequence has a timeline grade of its own.
fn has_timeline_grade(sequence_id: SequenceId, all_sequences: &[Sequence]) -> bool {
    all_sequences.iter().find(|s| s.id == sequence_id).and_then(|s| s.timeline_grade()).is_some_and(|g| !g.is_identity())
}

/// `graded` collects the compounds on picture tracks whose inner sequence
/// has a timeline grade: they are left whole and drawn as the scene they
/// are, so the grade has the scene's own composite to go over (#370).
/// Inlined, the inner clips joined the parent's layers and the grade had
/// nothing of its own to grade, so it was dropped without a word.
fn flatten_track(track: &Track, all_sequences: &[Sequence], depth: u8, graded: &mut HashSet<ClipId>) -> Track {
    let mut flat = track.clone();
    let track_kind = track.kind;
    flat.clips = track
        .clips
        .iter()
        .flat_map(|clip| {
            if let ClipKind::Sequence { sequence_id, .. } = &clip.kind {
                if track_kind == TrackKind::Video && has_timeline_grade(*sequence_id, all_sequences) {
                    let mut scene = clip.clone();
                    scene.kind = ClipKind::Precomposition { sequence_id: *sequence_id };
                    graded.insert(scene.id);
                    return vec![scene];
                }
                if depth < MAX_COMPOUND_DEPTH {
                    return flatten_clip_for_track(
                        clip,
                        *sequence_id,
                        all_sequences,
                        track_kind,
                        depth,
                    );
                }
            }
            vec![clip.clone()]
        })
        .collect();
    flat
}

/// Expand one compound clip into the inner clips it contains, with time
/// coordinates remapped onto the parent timeline. Inner clips are pulled
/// from every inner track regardless of kind. Returns an empty `Vec` if
/// `sequence_id` does not exist in `all_sequences` (defensive — matches
/// the export path's behavior when the project graph is inconsistent).
///
/// Most callers want [`flatten_clip_for_track`] instead, which respects
/// the parent track's kind (video/audio split).
pub fn flatten_clip(
    compound: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    depth: u8,
) -> Vec<Clip> {
    flatten_clip_inner(compound, sequence_id, all_sequences, None, depth)
}

/// Expand one compound clip, restricting inner clips to tracks whose kind
/// is compatible with `parent_kind` (see `track_kinds_compatible`).
pub fn flatten_clip_for_track(
    compound: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    parent_kind: TrackKind,
    depth: u8,
) -> Vec<Clip> {
    flatten_clip_inner(compound, sequence_id, all_sequences, Some(parent_kind), depth)
}

fn flatten_clip_inner(
    compound: &Clip,
    sequence_id: SequenceId,
    all_sequences: &[Sequence],
    parent_kind: Option<TrackKind>,
    depth: u8,
) -> Vec<Clip> {
    let inner_seq = match all_sequences.iter().find(|s| s.id == sequence_id) {
        Some(s) => s,
        None => return Vec::new(),
    };

    let compound_timeline_in = compound.timeline_in.to_secs_f64();
    let compound_source_in = compound.source_in.to_secs_f64();
    let compound_source_out = compound.source_out.to_secs_f64();
    let compound_speed = compound.speed;

    let rate = compound.timeline_in.rate;
    let mut out = Vec::new();

    for inner_track in &inner_seq.tracks {
        if let Some(pk) = parent_kind {
            if !track_kinds_compatible(pk, inner_track.kind) {
                continue;
            }
        }
        for inner_clip in &inner_track.clips {
            if !inner_clip.enabled {
                continue;
            }
            if matches!(inner_clip.kind, ClipKind::Gap) {
                continue;
            }

            let sliced = match slice_inner_clip_to_compound(
                inner_clip,
                compound_timeline_in,
                compound_source_in,
                compound_source_out,
                compound_speed,
                rate,
            ) {
                Some(c) => c,
                None => continue,
            };

            // Recurse into nested compound clips — feed the sliced synthetic
            // back through flatten_clip_inner so the inner-of-inner clips get
            // remapped against the new compound bounds.
            if let ClipKind::Sequence {
                sequence_id: nested_id,
                ..
            } = &sliced.kind
            {
                if depth + 1 < MAX_COMPOUND_DEPTH {
                    out.extend(flatten_clip_inner(
                        &sliced,
                        *nested_id,
                        all_sequences,
                        parent_kind,
                        depth + 1,
                    ));
                }
                continue;
            }

            out.push(sliced);
        }
    }
    out
}

/// The integration step a ramped inner clip's source window is found
/// with when sliced.
const SLICE_STEP_FPS: f64 = 60.0;

/// Slice one inner clip to a compound clip's source window and remap its
/// time coordinates onto the parent timeline.
///
/// - Returns `None` if the inner clip lies entirely outside the compound's
///   source window (caller should drop it).
/// - Honours `compound_speed`: parent-timeline duration equals the
///   inner-timeline visible span divided by the compound speed.
/// - Multiplies the inner clip's own `speed` by `compound_speed` so the
///   restored leaf plays at the combined rate.
///
/// All `*_secs` parameters are seconds; `rate` is the `RationalTime` rate
/// the output time fields will be expressed in.
pub fn slice_inner_clip_to_compound(
    inner: &Clip,
    compound_timeline_in_secs: f64,
    compound_source_in_secs: f64,
    compound_source_out_secs: f64,
    compound_speed: f64,
    rate: u32,
) -> Option<Clip> {
    let ic_in = inner.timeline_in.to_secs_f64();
    let ic_out = inner.timeline_out.to_secs_f64();

    if ic_out <= compound_source_in_secs || ic_in >= compound_source_out_secs {
        return None;
    }

    let visible_in = ic_in.max(compound_source_in_secs);
    let visible_out = ic_out.min(compound_source_out_secs);

    // Defensive: avoid divide-by-zero if speed is somehow 0. A compound
    // plays its scene forward: reverse is a media clip's (#617), and a
    // negative speed here put the slice before the compound's start.
    let speed = if compound_speed.abs() < 1e-9 { 1.0 } else { compound_speed.abs() };

    let parent_in =
        compound_timeline_in_secs + (visible_in - compound_source_in_secs) / speed;
    let parent_out =
        compound_timeline_in_secs + (visible_out - compound_source_in_secs) / speed;

    // What the window trims off the inner clip's head and tail, moved into
    // its source through its own speed, and from the far end of its range
    // when it plays reversed (#617). Taken one to one, a retimed clip inside
    // a compound showed the wrong part of its file.
    let (head, kept) = (visible_in - ic_in, visible_out - ic_in);
    let (src_in, src_out) = (inner.source_in.to_secs_f64(), inner.source_out.to_secs_f64());
    let moved = |t: f64| if t <= 0.0 { 0.0 } else { inner.speed_integral(t, SLICE_STEP_FPS) };
    let whole = (ic_out - ic_in) <= kept + 1e-9 && head <= 1e-9;
    let (inner_source_in_secs, inner_source_out_secs) = if whole {
        (src_in, src_out)
    } else if inner.is_reversed() {
        (src_out - moved(kept), src_out - moved(head))
    } else {
        (src_in + moved(head), src_in + moved(kept))
    };

    let mut flat = inner.clone();
    flat.timeline_in = RationalTime::from_secs_f64(parent_in, rate);
    flat.timeline_out = RationalTime::from_secs_f64(parent_out, rate);
    flat.source_in = RationalTime::from_secs_f64(inner_source_in_secs, rate);
    flat.source_out = RationalTime::from_secs_f64(inner_source_out_secs, rate);
    flat.speed = inner.speed * speed;
    Some(flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GradeGroup, Sequence, Track, TrackKind};
    use crate::test_clip::{self, TestClip};
    use klipp_core::{Fps, MediaId, RationalTime, Resolution, SequenceId};
    use std::collections::BTreeMap;

    fn make_seq(name: &str) -> Sequence {
        Sequence {
            title_nits: crate::DEFAULT_TITLE_NITS,
            node_labels: Vec::new(),
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: name.to_string(),
            fps: Fps::new(24, 1),
            resolution: Resolution { width: 1920, height: 1080 },
            first_clip_autoset: Default::default(),
            tracks: Vec::new(),
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
            reviews: Vec::new(),
            awaiting_refit: false,
            version_of: None,
        }
    }

    /// #846: the render copy puts each track's effects on its clips after
    /// their own: picture effects after the clip's, the track's grade after
    /// the clip's and its group's, the audio chain after the clip's. A clip
    /// added later takes them, gaps and adjustment layers take nothing,
    /// baking again changes nothing, and the document is untouched.
    #[test]
    fn a_tracks_effects_go_on_every_clip_of_it_in_the_render_copy() {
        use klipp_effects::BuiltinEffect as E;
        let exposed = |ev: f32| {
            let mut g = klipp_nodes::NodeGraph::default();
            g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.params.exposure = Some(ev));
            g
        };
        let corrector = |g: &klipp_nodes::NodeGraph| g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        let fx = |kind: &str| klipp_core::AudioEffectInstance { effect_type: kind.into(), enabled: true, wet_dry: 1.0, params: vec![] };
        let mut seq = make_seq("scene");
        let mut group = GradeGroup::new("Night");
        group.post = Some(exposed(-1.0).into());
        let group_node = corrector(group.post.as_ref().map(|g| &g.0).unwrap());
        let track_grade = exposed(0.3);
        let track_node = corrector(&track_grade);

        let mut v = Track::new("V1", TrackKind::Video);
        v.effects.set_video_effect_list(vec![E::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.7 }], vec![]);
        v.effects.grade = Some(track_grade.into());
        let mut a = TestClip::of(Clip::media(MediaId::new(), t(0), t(24), t(0), t(24))).effect(E::Opacity { value: 0.5 }).off().build();
        a.edit_effect_keyframes(|v| v.insert("0:value".into(), vec![klipp_core::Keyframe { time_secs: 0.0, value: 0.2, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }]));
        a.edit_param_modifiers(|v| v.insert(klipp_core::ParamId::Effect { effect_index: 0, param_name: "value".into() }, klipp_core::ModifierStack::default()));
        a.grade_group = Some(group.id);
        let own_node = corrector(&a.picture.view().node_graph.0);
        let mut adjustment = Clip::media(MediaId::new(), t(24), t(48), t(0), t(24));
        adjustment.kind = ClipKind::AdjustmentLayer;
        v.clips = vec![a, adjustment];
        let mut sound = Track::new("A1", TrackKind::Audio);
        sound.effects.audio = vec![fx("compressor")];
        let mut line = Clip::media(MediaId::new(), t(0), t(24), t(0), t(24));
        line.audio_effects = vec![fx("eq")];
        sound.clips = vec![line];
        seq.tracks = vec![v, sound];
        seq.grade_groups = vec![group];
        let before = seq.clone();

        let flat = flatten_sequence(&seq, std::slice::from_ref(&seq));
        let a = &flat.tracks[0].clips[0];
        assert!(matches!(test_clip::effects(a).as_slice(), [E::Opacity { .. }, E::Vignette { .. }]), "{:?}", test_clip::effects(a));
        assert_eq!(test_clip::enabled(a), vec![false, true], "the clip's own switch kept, the track's on");
        let own = &before.tracks[0].clips[0];
        // Effect keyframes by key with their values, and modifier keys.
        let animation = |c: &Clip| {
            let mut keys: Vec<(String, Vec<f32>)> = c.effect_keyframes().iter().map(|(k, v)| (k.clone(), v.iter().map(|f| f.value).collect())).collect();
            keys.sort_by(|x, y| x.0.cmp(&y.0));
            let mut mods: Vec<String> = c.param_modifiers().keys().map(|k| format!("{k:?}")).collect();
            mods.sort();
            (keys, mods)
        };
        assert_eq!(animation(a), animation(own), "the clip's own animation stays on its own effect");
        let order: Vec<_> = a.picture.view().node_graph.0.topo_sort().unwrap().into_iter().filter(|id| [own_node, group_node, track_node].contains(id)).collect();
        assert_eq!(order, vec![own_node, group_node, track_node], "own grade, then the group's, then the track's");
        assert!(flat.tracks[0].clips[1].builtin_effects().is_empty() && flat.tracks[0].clips[1].baked_own.is_none(), "an adjustment layer takes nothing");
        let chain: Vec<&str> = flat.tracks[1].clips[0].audio_effects.iter().map(|e| e.effect_type.as_str()).collect();
        assert_eq!(chain, vec!["eq"], "the clip's own chain alone");
        assert_eq!(flat.tracks[1].effects.audio.len(), 1, "the track's on the track, for its summed clips (#2560)");

        // Baking the render copy again changes nothing.
        let mut again = flat.clone();
        again.bake_grade_groups();
        again.bake_track_effects();
        let effects = |s: &Sequence| s.all_clips().map(|c| (c.builtin_effects().to_vec(), c.audio_effects.clone(), c.picture.view().node_graph.clone())).collect::<Vec<_>>();
        assert!(effects(&again) == effects(&flat), "baked once, however often it is baked");

        // The document keeps its own, and the render copy gives them back.
        assert!(effects(&seq) == effects(&before), "the document is untouched");
        assert!(effects(&flat.with_own_grades()) == effects(&seq), "the render copy gives back each clip's own");
        let back = flat.with_own_grades();
        let back = &back.tracks[0].clips[0];
        assert_eq!((test_clip::enabled(back), animation(back)), (test_clip::enabled(own), animation(own)), "and its flags and animation");
        assert!(!test_clip::effects(own).is_empty() && back.effect_id_list() == own.effect_id_list(), "and each effect's node id (#1376)");

        // A clip added to the sound track later takes nothing of the
        // track's: the track runs its effects on its summed clips (#2560).
        seq.tracks[1].clips.push(Clip::media(MediaId::new(), t(24), t(48), t(0), t(24)));
        let flat = flatten_sequence(&seq, std::slice::from_ref(&seq));
        assert!(flat.tracks[1].clips[1].audio_effects.is_empty());
    }

    /// #1060: a clip's grade for a one-picture preview (the scopes) is
    /// the one the render copy runs: inside its group's, then its track's,
    /// for each kind of clip and track, with and without either.
    #[test]
    fn a_clips_render_grade_is_the_render_copys() {
        let exposed = |ev: f32| {
            let mut g = klipp_nodes::NodeGraph::default();
            g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.params.exposure = Some(ev));
            g
        };
        let mut seq = make_seq("scene");
        let mut group = GradeGroup::new("Night");
        group.pre = Some(exposed(0.5).into());
        group.post = Some(exposed(-1.0).into());
        let mut tracks = Vec::new();
        for (kind, track_grade) in [(TrackKind::Video, true), (TrackKind::Video, false), (TrackKind::Text, true), (TrackKind::Audio, true)] {
            let mut track = Track::new("T", kind);
            if track_grade {
                track.effects.grade = Some(exposed(0.3).into());
            }
            for (in_group, clip_kind) in [(true, None), (false, None), (true, Some(ClipKind::AdjustmentLayer)), (false, Some(ClipKind::Gap))] {
                let start = track.clips.len() as i64 * 24;
                let mut c = Clip::media(MediaId::new(), t(start), t(start + 24), t(0), t(24));
                if let Some(k) = clip_kind {
                    c.kind = k;
                }
                c.view_mut().node_graph = exposed(0.1).into();
                c.grade_group = in_group.then_some(group.id);
                track.clips.push(c);
            }
            tracks.push(track);
        }
        seq.tracks = tracks;
        seq.grade_groups = vec![group];

        let baked = seq.baked_for_render();
        let mut checked = 0;
        for (track, baked_track) in seq.tracks.iter().zip(&baked.tracks) {
            for (clip, baked_clip) in track.clips.iter().zip(&baked_track.clips) {
                let grade = seq.render_grade(clip.id).expect("the clip is on a track");
                assert!(*grade == baked_clip.picture.view().node_graph.0, "{:?} {:?} in group {}", track.kind, clip.kind, clip.grade_group.is_some());
                checked += 1;
            }
        }
        assert_eq!(checked, 16);
        assert!(seq.render_grade(ClipId::new()).is_none(), "no clip, no grade");
    }

    /// #237: the render copy grades a group's members pre-clip, own,
    /// post-clip; a clip outside the group, and the document itself, are
    /// left as they were; a member inside a compound is wrapped by its own
    /// sequence's group.
    #[test]
    fn a_grade_group_wraps_its_members_in_the_render_copy() {
        let exposed = |ev: f32| {
            let mut g = klipp_nodes::NodeGraph::default();
            g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.params.exposure = Some(ev));
            g
        };
        let mut seq = make_seq("scene");
        let mut group = GradeGroup::new("Day exterior");
        group.pre = Some(exposed(0.5).into());
        group.post = Some(exposed(-1.0).into());
        let pre_node = group.pre.as_ref().map(|g| &g.0).unwrap().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        let post_node = group.post.as_ref().map(|g| &g.0).unwrap().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        let mut v = Track::new("V1", TrackKind::Video);
        let mut member = Clip::media(MediaId::new(), t(0), t(24), t(0), t(24));
        member.grade_group = Some(group.id);
        let own_node = member.picture.view().node_graph.0.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        let outsider = Clip::media(MediaId::new(), t(24), t(48), t(0), t(24));
        v.clips = vec![member, outsider];
        seq.tracks = vec![v];
        seq.grade_groups = vec![group];
        let before = seq.clone();

        let flat = flatten_sequence(&seq, std::slice::from_ref(&seq));
        let ids = |c: &Clip| c.picture.view().node_graph.0.topo_sort().unwrap().into_iter().filter(|id| [pre_node, own_node, post_node].contains(id)).collect::<Vec<_>>();
        assert_eq!(ids(&flat.tracks[0].clips[0]), vec![pre_node, own_node, post_node], "pre, own, post in that order");
        assert!(flat.tracks[0].clips[1].picture.view().node_graph.0.is_identity(), "a clip outside the group is not graded by it");
        let graphs = |s: &Sequence| s.all_clips().map(|c| c.picture.view().node_graph.clone()).collect::<Vec<_>>();
        assert!(graphs(&seq) == graphs(&before) && seq.grade_groups == before.grade_groups, "the document is untouched");
        // What reads the render copy to change a clip's grade reads the
        // clip's own: the group's nodes never travel into it.
        assert!(graphs(&flat.with_own_grades()) == graphs(&seq), "the render copy gives back each clip's own grade");
        assert_eq!(flat.tracks[0].clips[0].own_grade(), &seq.tracks[0].clips[0].picture.view().node_graph.0);

        // The same scene as a graded compound's content: its member still
        // runs inside its group.
        let mut reel = make_seq("reel");
        let mut rv = Track::new("V1", TrackKind::Video);
        rv.clips.push(Clip::sequence(seq.id, t(0), t(48)));
        reel.tracks = vec![rv];
        let flat = flatten_sequence(&reel, &[reel.clone(), seq.clone()]);
        let member = flat.all_clips().find(|c| c.grade_group.is_some()).expect("the member is in the flattened reel");
        assert_eq!(ids(member), vec![pre_node, own_node, post_node]);
    }

    /// #617: a compound's window trims an inner clip's source through
    /// its speed, and from the far end of its range when it plays
    /// reversed: a reversed clip cut to its second half on the timeline
    /// keeps the first half of its source.
    #[test]
    fn a_compound_window_trims_an_inner_clip_through_its_speed_and_direction() {
        let rate = 24;
        let inner = |speed: f64| {
            let mut c = Clip::media(MediaId::new(), t(0), t(48), t(0), t(48));
            c.speed = speed;
            c
        };
        let secs = |rt: RationalTime| (rt.to_secs_f64() * 24.0).round() as i64;
        // The compound shows inner seconds 1..2 (frames 24..48).
        let fwd = slice_inner_clip_to_compound(&inner(1.0), 0.0, 1.0, 2.0, 1.0, rate).unwrap();
        assert_eq!((secs(fwd.source_in), secs(fwd.source_out)), (24, 48));
        let rev = slice_inner_clip_to_compound(&inner(-1.0), 0.0, 1.0, 2.0, 1.0, rate).unwrap();
        assert_eq!((secs(rev.source_in), secs(rev.source_out)), (0, 24), "reversed: the later half of the timeline is the source's start");
        assert!(rev.is_reversed());
        // A clip at 2x covering the whole window keeps its whole range.
        let mut fast = Clip::media(MediaId::new(), t(0), t(24), t(0), t(48));
        fast.speed = 2.0;
        let whole = slice_inner_clip_to_compound(&fast, 0.0, 0.0, 1.0, 1.0, rate).unwrap();
        assert_eq!((secs(whole.source_in), secs(whole.source_out)), (0, 48));
        let half = slice_inner_clip_to_compound(&fast, 0.0, 0.5, 1.0, 1.0, rate).unwrap();
        assert_eq!((secs(half.source_in), secs(half.source_out)), (24, 48), "half the timeline at 2x is the second 24 source frames");
    }

    /// #370: a compound whose sequence has a timeline grade is left whole
    /// on its picture track, drawn as the scene it is so the grade has a
    /// composite to go over; its sound is still flattened from its own
    /// audio clip, and taken once, not a second time from the scene. An
    /// ungraded compound is inlined as it always was.
    #[test]
    fn a_graded_compound_is_drawn_whole_and_heard_once() {
        let make_inner = |graded: bool| {
            let mut inner = make_seq("scene");
            let media = MediaId::new();
            let mut v = Track::new("V1", TrackKind::Video);
            let mut a = Track::new("A1", TrackKind::Audio);
            let mut pic = Clip::media(media, t(0), t(48), t(0), t(48));
            let mut snd = Clip::media(media, t(0), t(48), t(0), t(48));
            pic.linked_clip_id = Some(snd.id);
            snd.linked_clip_id = Some(pic.id);
            v.clips.push(pic);
            a.clips.push(snd);
            inner.tracks = vec![v, a];
            if graded {
                let mut g = klipp_nodes::NodeGraph::default();
                g.nodes.iter_mut().for_each(|n| {
                    if n.kind == klipp_nodes::NodeKind::Corrector {
                        n.params.exposure = Some(-1.0);
                    }
                });
                inner.timeline_graph = Some(g.into());
            }
            inner
        };
        for graded in [true, false] {
            let inner = make_inner(graded);
            if graded {
                assert!(has_timeline_grade(inner.id, std::slice::from_ref(&inner)), "the fixture's grade is a grade");
            }
            let mut outer = make_seq("reel");
            let mut v = Track::new("V1", TrackKind::Video);
            let mut a = Track::new("A1", TrackKind::Audio);
            v.clips.push(Clip::sequence(inner.id, t(0), t(48)));
            a.clips.push(Clip::sequence(inner.id, t(0), t(48)));
            outer.tracks = vec![v, a];
            let flat = flatten_sequence(&outer, &[outer.clone(), inner.clone()]);
            assert_eq!(flat.tracks.len(), 2, "graded={graded}: no second copy of the sound");
            let pic = &flat.tracks[0].clips;
            assert_eq!(pic.len(), 1);
            if graded {
                assert!(matches!(pic[0].kind, ClipKind::Precomposition { sequence_id } if sequence_id == inner.id), "drawn whole: {:?}", pic[0].kind);
                assert!(pic[0].precomp.as_ref().is_some_and(|s| s.timeline_graph.is_some()), "with its scene, grade and all");
            } else {
                assert!(matches!(pic[0].kind, ClipKind::Media { .. }), "inlined: {:?}", pic[0].kind);
            }
            assert!(flat.tracks[1].clips.iter().all(|c| matches!(c.kind, ClipKind::Media { .. })) && flat.tracks[1].clips.len() == 1, "graded={graded}: the sound from the compound's own audio clip");
        }
    }

    #[test]
    fn precomposition_applies_persisted_template_override_before_render() {
        let mut inner = make_seq("template");
        let mut title_track = Track::new("V1", TrackKind::Video);
        let mut clip = Clip::gap(t(0), t(24));
        clip.kind = ClipKind::Title(crate::TitleData::single_text(crate::TextBox { text: "default".into(), ..Default::default() }));
        title_track.clips.push(clip);
        inner.tracks.push(title_track);
        let mut outer = make_seq("outer");
        let mut pre = Clip::precomposition(inner.id, t(0), t(24));
        pre.kind = ClipKind::TemplatePrecomposition {
            sequence_id: inner.id,
            template_instance: klipp_core::TemplateInstance {
                template_id: uuid::Uuid::new_v4(),
                bindings: BTreeMap::from([("command".into(), "object:0.text".into())]),
                overrides: BTreeMap::from([("command".into(), klipp_core::TemplateValue::Text("resolved".into()))]),
                media_slots: BTreeMap::new(),
                links: Vec::new(),
            },
        };
        let pre_id = pre.id;
        outer.tracks.push(Track::new("V1", TrackKind::Video));
        outer.tracks[0].clips.push(pre);
        let flat = flatten_sequence(&outer, &[outer.clone(), inner.clone()]);
        let resolved = flat.tracks[0].clips.iter().find(|c| c.id == pre_id).and_then(|c| c.precomp.as_ref()).unwrap();
        let title_clip = resolved.tracks.iter().flat_map(|track| &track.clips).find(|clip| matches!(clip.kind, ClipKind::Title(_))).unwrap();
        let ClipKind::Title(data) = &title_clip.kind else { panic!("expected title") };
        let crate::TitleObjectContent::TextBox(text) = &data.objects[0].content else { panic!("expected text") };
        assert_eq!(text.text, "resolved");
    }

    /// A template of one title and `n` instances of it, a second each, the
    /// i-th saying "card i".
    fn instances(n: usize) -> (Sequence, Sequence) {
        let mut inner = make_seq("template");
        let mut title_track = Track::new("V1", TrackKind::Video);
        let mut clip = Clip::gap(t(0), t(24));
        clip.kind = ClipKind::Title(crate::TitleData::single_text(crate::TextBox { text: "default".into(), ..Default::default() }));
        title_track.clips.push(clip);
        inner.tracks.push(title_track);
        let mut outer = make_seq("outer");
        outer.tracks.push(Track::new("V1", TrackKind::Video));
        for i in 0..n as i64 {
            let mut pre = Clip::precomposition(inner.id, t(24 * i), t(24 * i + 24));
            pre.kind = ClipKind::TemplatePrecomposition {
                sequence_id: inner.id,
                template_instance: klipp_core::TemplateInstance {
                    template_id: uuid::Uuid::new_v4(),
                    bindings: BTreeMap::from([("text".into(), "object:0.text".into())]),
                    overrides: BTreeMap::from([("text".into(), klipp_core::TemplateValue::Text(format!("card {i}")))]),
                    media_slots: BTreeMap::new(),
                    links: Vec::new(),
                },
            };
            outer.tracks[0].clips.push(pre);
        }
        (outer, inner)
    }

    fn same(a: &Sequence, b: &Sequence) -> bool {
        serde_json::to_value(a).unwrap() == serde_json::to_value(b).unwrap()
    }

    /// #3117: the cached flattening is the flattening; a second pass takes
    /// every scene from the cache; an edit to one instance makes only its
    /// scene again, a move makes none, an edit to the template makes every
    /// one again; scenes no longer placed are dropped.
    #[test]
    fn a_cached_flattening_makes_again_only_the_scenes_an_edit_changed() {
        let (mut outer, mut inner) = instances(20);
        let mut cache = FlattenCache::default();
        let flat = |outer: &Sequence, inner: &Sequence, cache: &mut FlattenCache| {
            let all = [outer.clone(), inner.clone()];
            let cached = flatten_sequence_cached(outer, &all, cache);
            assert!(same(&cached, &flatten_sequence(outer, &all)), "the cached flattening is the flattening");
            cached
        };
        let counts = |c: &FlattenCache| (c.hits, c.misses);
        flat(&outer, &inner, &mut cache);
        assert_eq!(counts(&cache), (0, 20), "every scene made the first time");
        flat(&outer, &inner, &mut cache);
        assert_eq!(counts(&cache), (20, 20), "and taken from the cache the second");

        outer.tracks[0].clips[3].timeline_in = t(24 * 3 + 5);
        flat(&outer, &inner, &mut cache);
        assert_eq!(counts(&cache), (40, 20), "a trim makes no scene again");

        let ClipKind::TemplatePrecomposition { template_instance, .. } = &mut outer.tracks[0].clips[7].kind else { unreachable!() };
        template_instance.overrides.insert("text".into(), klipp_core::TemplateValue::Text("changed".into()));
        let out = flat(&outer, &inner, &mut cache);
        assert_eq!(counts(&cache), (59, 21), "an instance's override makes its scene again, only its");
        let said = |s: &Sequence| s.tracks.iter().flat_map(|t| &t.clips).find_map(|c| match &c.kind { ClipKind::Title(td) => Some(td.text()), _ => None });
        assert_eq!(out.tracks[0].clips[7].precomp.as_deref().and_then(said).as_deref(), Some("changed"));

        let ClipKind::Title(td) = &mut inner.tracks[0].clips[0].kind else { unreachable!() };
        let crate::TitleObjectContent::TextBox(text) = &mut td.objects[0].content else { unreachable!() };
        text.font_size = 99.0;
        flat(&outer, &inner, &mut cache);
        assert_eq!(counts(&cache), (59, 41), "an edit to the template makes every scene again");

        outer.tracks[0].clips.truncate(5);
        flat(&outer, &inner, &mut cache);
        assert_eq!(cache.scenes.len(), 5, "the scenes no longer placed are dropped");
    }

    fn t(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 24)
    }

    fn media_clip(in_f: i64, out_f: i64, src_in_f: i64, src_out_f: i64) -> Clip {
        Clip::media(MediaId::new(), t(in_f), t(out_f), t(src_in_f), t(src_out_f))
    }

    #[test]
    fn flatten_two_media_clips_in_compound() {
        let mut child = make_seq("child");
        let mut child_track = Track::new("V1", TrackKind::Video);
        child_track.clips.push(media_clip(0, 24, 0, 24));   // 0..1s
        child_track.clips.push(media_clip(24, 48, 0, 24));  // 1..2s
        child.tracks.push(child_track);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut parent_track = Track::new("V1", TrackKind::Video);
        // Compound clip placed at 100..148 frames (4..6.16s) on parent
        parent_track
            .clips
            .push(Clip::sequence(child_id, t(100), t(148)));
        parent.tracks.push(parent_track);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        assert_eq!(flat.tracks.len(), 1);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 2);
        // First leaf: parent_in = 100 + (0 - 0) = 100, parent_out = 100 + 24 = 124
        assert_eq!(clips[0].timeline_in, t(100));
        assert_eq!(clips[0].timeline_out, t(124));
        assert_eq!(clips[0].source_in, t(0));
        assert_eq!(clips[0].source_out, t(24));
        // Second leaf: parent_in = 124, parent_out = 148
        assert_eq!(clips[1].timeline_in, t(124));
        assert_eq!(clips[1].timeline_out, t(148));
    }

    #[test]
    fn compound_trimmed_clips_inner() {
        // Inner sequence has a 4-second clip [0..96 frames]
        // Compound clip's source window only exposes [24..72] (1..3s)
        // Expect: one leaf clip on parent, source range 24..72, parent timeline width 48
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 96, 0, 96));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        let mut compound = Clip::sequence(child_id, t(200), t(248));
        compound.source_in = t(24);
        compound.source_out = t(72);
        ptr.clips.push(compound);
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].timeline_in, t(200));
        assert_eq!(clips[0].timeline_out, t(248));
        assert_eq!(clips[0].source_in, t(24));
        assert_eq!(clips[0].source_out, t(72));
    }

    #[test]
    fn nested_compound_two_levels() {
        // grandchild contains a media clip
        // child contains a compound clip referring to grandchild
        // parent contains a compound clip referring to child
        let mut grandchild = make_seq("gc");
        let mut gtr = Track::new("V1", TrackKind::Video);
        gtr.clips.push(media_clip(0, 24, 0, 24));
        grandchild.tracks.push(gtr);
        let gc_id = grandchild.id;

        let mut child = make_seq("child");
        let mut ctr = Track::new("V1", TrackKind::Video);
        ctr.clips.push(Clip::sequence(gc_id, t(0), t(24)));
        child.tracks.push(ctr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(50), t(74)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(
            &parent,
            &[parent.clone(), child.clone(), grandchild.clone()],
        );
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].timeline_in, t(50));
        assert_eq!(clips[0].timeline_out, t(74));
    }

    #[test]
    fn depth_limit_does_not_panic() {
        // Build a self-referential chain of MAX_COMPOUND_DEPTH + 2 sequences.
        // Each contains one compound clip pointing to the next.
        let mut seqs: Vec<Sequence> = (0..(MAX_COMPOUND_DEPTH as usize + 2))
            .map(|i| make_seq(&format!("s{i}")))
            .collect();
        // Wire each seq[i] to contain seq[i+1] as a compound clip.
        for i in 0..seqs.len() - 1 {
            let next_id = seqs[i + 1].id;
            let mut tr = Track::new("V1", TrackKind::Video);
            tr.clips.push(Clip::sequence(next_id, t(0), t(24)));
            seqs[i].tracks.push(tr);
        }
        // Tail leaf: actual media clip
        let mut tail_tr = Track::new("V1", TrackKind::Video);
        tail_tr.clips.push(media_clip(0, 24, 0, 24));
        seqs.last_mut().unwrap().tracks.push(tail_tr);

        let parent = seqs[0].clone();
        let _flat = flatten_sequence(&parent, &seqs); // must not panic
    }

    #[test]
    fn compound_speed_propagates() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        let mut compound = Clip::sequence(child_id, t(0), t(24));
        compound.speed = 2.0;
        ptr.clips.push(compound);
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert!((clips[0].speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn unknown_sequence_id_returns_empty() {
        let parent = make_seq("parent");
        let bogus = SequenceId::new();
        let compound = Clip::sequence(bogus, t(0), t(24));
        let result = flatten_clip(&compound, bogus, std::slice::from_ref(&parent), 0);
        assert!(result.is_empty());
    }

    #[test]
    fn gap_clips_in_inner_sequence_are_skipped() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(Clip::gap(t(0), t(24)));
        tr.clips.push(media_clip(24, 48, 0, 24));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(48)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1, "Gap should be skipped, only media remains");
    }

    #[test]
    fn disabled_inner_clips_are_skipped() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        let mut c = media_clip(0, 24, 0, 24);
        c.enabled = false;
        tr.clips.push(c);
        tr.clips.push(media_clip(24, 48, 0, 24));
        child.tracks.push(tr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(48)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 1);
    }

    #[test]
    fn empty_inner_sequence_yields_empty() {
        let child = make_seq("child");
        let child_id = child.id;
        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(24)));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        assert!(flat.tracks[0].clips.is_empty());
    }

    #[test]
    fn audio_track_inner_clips_carry_compound_speed() {
        // Pre-existing export bug: compound.speed was dropped on the audio
        // path. The shared flattener now propagates it for all track kinds.
        let mut child = make_seq("child");
        let mut atr = Track::new("A1", TrackKind::Audio);
        atr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(atr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut patr = Track::new("A1", TrackKind::Audio);
        let mut compound = Clip::sequence(child_id, t(0), t(24));
        compound.speed = 2.0;
        patr.clips.push(compound);
        parent.tracks.push(patr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        assert_eq!(flat.tracks[0].clips.len(), 1);
        assert!((flat.tracks[0].clips[0].speed - 2.0).abs() < 1e-9);
    }

    #[test]
    fn track_kind_filter_audio_excludes_video_inner_clips() {
        // An audio compound clip should pull only audio inner clips,
        // even when the inner sequence has both video and audio.
        let mut child = make_seq("child");
        let mut vtr = Track::new("V1", TrackKind::Video);
        vtr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(vtr);
        let mut atr = Track::new("A1", TrackKind::Audio);
        atr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(atr);
        let child_id = child.id;

        let compound = Clip::sequence(child_id, t(0), t(24));
        let leaves = flatten_clip_for_track(
            &compound,
            child_id,
            &[child.clone()],
            TrackKind::Audio,
            0,
        );
        assert_eq!(leaves.len(), 1, "audio parent track must yield only audio inner clips");
    }

    #[test]
    fn track_kind_filter_video_excludes_audio_inner_clips() {
        let mut child = make_seq("child");
        let mut vtr = Track::new("V1", TrackKind::Video);
        vtr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(vtr);
        let mut atr = Track::new("A1", TrackKind::Audio);
        atr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(atr);
        let child_id = child.id;

        let compound = Clip::sequence(child_id, t(0), t(24));
        let leaves = flatten_clip_for_track(
            &compound,
            child_id,
            &[child.clone()],
            TrackKind::Video,
            0,
        );
        assert_eq!(leaves.len(), 1, "video parent track must yield only video inner clips");
    }

    /// A precomposition is the one nested kind flattening leaves alone:
    /// the clip comes through as itself, with its own id, while the
    /// compound beside it is expanded as before.
    #[test]
    fn a_precomposition_is_not_flattened() {
        let mut child = make_seq("child");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        child.tracks.push(tr);

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::precomposition(child.id, t(0), t(24)));
        ptr.clips.push(Clip::sequence(child.id, t(24), t(48)));
        let precomp_id = ptr.clips[0].id;
        parent.tracks.push(ptr);

        let flat = flatten_sequence(&parent, &[parent.clone(), child.clone()]);
        let clips = &flat.tracks[0].clips;
        assert_eq!(clips.len(), 2, "one precomposition kept, one compound expanded to its one clip");
        assert!(matches!(clips[0].kind, ClipKind::Precomposition { sequence_id, .. } if sequence_id == child.id));
        assert_eq!(clips[0].id, precomp_id, "the precomposition keeps its own identity");
        assert!(matches!(clips[1].kind, ClipKind::Media { .. }), "the compound is flattened as before");
    }

    /// A scene laid in twice is two sets of clips: each instance's ids
    /// are its own, and a link inside the scene points at the copy in
    /// its own instance. The scene itself is untouched.
    #[test]
    fn a_scene_laid_in_twice_is_two_sets_of_clips_linked_within_themselves() {
        let mut scene = make_seq("scene");
        let mut tr = Track::new("V1", TrackKind::Video);
        let anchor = media_clip(0, 48, 0, 48);
        let mut follower = media_clip(0, 48, 0, 48);
        follower.transform_link = Some(crate::TransformLink::new(anchor.id));
        let (anchor_id, follower_id) = (anchor.id, follower.id);
        tr.clips.push(anchor);
        tr.clips.push(follower);
        scene.tracks.push(tr);

        let mut parent = make_seq("parent");
        let mut a = Track::new("V1", TrackKind::Video);
        a.clips.push(Clip::precomposition(scene.id, t(0), t(24)));
        let mut b = Track::new("V2", TrackKind::Video);
        b.clips.push(Clip::precomposition(scene.id, t(0), t(24)));
        parent.tracks = vec![a, b];

        let flat = flatten_sequence(&parent, &[parent.clone(), scene.clone()]);
        let instances: Vec<&Sequence> = flat.tracks[..2].iter().map(|t| t.clips[0].precomp.as_deref().expect("resolved")).collect();
        let ids = |s: &Sequence| -> Vec<ClipId> { s.tracks[0].clips.iter().map(|c| c.id).collect() };
        assert_ne!(ids(instances[0]), ids(instances[1]), "two instances, two sets of ids");
        assert!(!ids(instances[0]).contains(&anchor_id) && !ids(instances[0]).contains(&follower_id), "neither is the scene's own");
        for inst in &instances {
            let [anchor, follower] = inst.tracks[0].clips.as_slice() else { panic!("two clips") };
            assert!(
                matches!(follower.transform_link.as_ref().map(|l| &l.target), Some(crate::TransformLinkTarget::Clip(id)) if *id == anchor.id),
                "the link follows the anchor of its own instance",
            );
        }
        assert_eq!(scene.tracks[0].clips[0].id, anchor_id, "the scene keeps its ids");
        assert_eq!(instance_id(flat.tracks[0].clips[0].id, anchor_id), instances[0].tracks[0].clips[0].id, "and an instance id is derived, not drawn");
    }

    /// A scene's sound reaches the mix once per source, at the
    /// instance's ids, sliced to the precomposition's window.
    #[test]
    fn a_precompositions_sound_reaches_the_mix_once_per_source() {
        let mut scene = make_seq("scene");
        let mut v = Track::new("V1", TrackKind::Video);
        let mut a = Track::new("A1", TrackKind::Audio);
        let mut vc = media_clip(0, 96, 0, 96);
        let mut ac = media_clip(0, 96, 0, 96);
        vc.linked_clip_id = Some(ac.id);
        ac.linked_clip_id = Some(vc.id);
        // The picture's sound modifier goes with its sound; its effect's
        // modifier animates a picture the sound clip doesn't draw (#1074).
        let modifier = klipp_core::ModifierStack::default();
        vc.edit_param_modifiers(|v| v.insert(klipp_core::ParamId::Volume, modifier.clone()));
        vc.edit_param_modifiers(|v| v.insert(klipp_core::ParamId::Effect { effect_index: 0, param_name: "radius".into() }, modifier));
        v.clips.push(vc);
        a.clips.push(ac);
        scene.tracks = vec![v, a];

        let mut parent = make_seq("parent");
        let mut p = Track::new("V1", TrackKind::Video);
        p.clips.push(Clip::precomposition(scene.id, t(0), t(24)));
        parent.tracks.push(p);

        let flat = flatten_sequence(&parent, &[parent.clone(), scene.clone()]);
        let sound = flat.tracks.iter().find(|t| t.precomposition_sound).expect("a sound track");
        assert_eq!(sound.kind, TrackKind::Audio);
        assert_eq!(sound.clips.len(), 1, "the linked pair sounds once, through its audio half");
        let instance = flat.tracks[0].clips[0].precomp.as_ref().unwrap();
        let audio_half = &instance.tracks[1].clips[0];
        assert_eq!(sound.clips[0].id, audio_half.id, "at the instance's id");
        assert_eq!(sound.clips[0].timeline_out, t(24), "sliced to the precomposition's window");
        let keys: Vec<&klipp_core::ParamId> = sound.clips[0].param_modifiers().keys().collect();
        assert_eq!(keys, [&klipp_core::ParamId::Volume], "the picture's sound modifier, not its effect's");
    }

    /// A scene that contains itself, at any remove, is refused where the
    /// loop closes; a missing scene resolves to nothing.
    #[test]
    fn a_scene_that_contains_itself_is_refused_where_the_loop_closes() {
        let mut a = make_seq("a");
        let mut b = make_seq("b");
        let mut at = Track::new("V1", TrackKind::Video);
        at.clips.push(Clip::precomposition(b.id, t(0), t(24)));
        a.tracks.push(at);
        let mut bt = Track::new("V1", TrackKind::Video);
        bt.clips.push(Clip::precomposition(a.id, t(0), t(24)));
        bt.clips.push(Clip::precomposition(SequenceId::new(), t(24), t(48)));
        b.tracks.push(bt);

        let flat = flatten_sequence(&a, &[a.clone(), b.clone()]);
        let b_in_a = flat.tracks[0].clips[0].precomp.as_ref().expect("b resolves inside a");
        assert!(b_in_a.tracks[0].clips[0].precomp.is_none(), "a inside b inside a is refused");
        assert!(b_in_a.tracks[0].clips[1].precomp.is_none(), "a scene that does not exist resolves to nothing");
    }

    #[test]
    fn flatten_sequence_yields_no_compound_clips() {
        // After flattening, no clip in any track should be ClipKind::Sequence.
        // This is the core invariant the engine depends on.
        let mut grandchild = make_seq("gc");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        grandchild.tracks.push(tr);
        let gc_id = grandchild.id;

        let mut child = make_seq("child");
        let mut ctr = Track::new("V1", TrackKind::Video);
        ctr.clips.push(Clip::sequence(gc_id, t(0), t(24)));
        child.tracks.push(ctr);
        let child_id = child.id;

        let mut parent = make_seq("parent");
        let mut ptr = Track::new("V1", TrackKind::Video);
        ptr.clips.push(Clip::sequence(child_id, t(0), t(24)));
        ptr.clips.push(media_clip(24, 48, 0, 24));
        parent.tracks.push(ptr);

        let flat = flatten_sequence(
            &parent,
            &[parent.clone(), child.clone(), grandchild.clone()],
        );
        for track in &flat.tracks {
            for clip in &track.clips {
                assert!(
                    !matches!(clip.kind, ClipKind::Sequence { .. }),
                    "flattened sequence must not contain ClipKind::Sequence clips"
                );
            }
        }
    }

    #[test]
    fn no_compound_clips_is_structurally_identical() {
        let mut seq = make_seq("plain");
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(media_clip(0, 24, 0, 24));
        tr.clips.push(media_clip(24, 48, 0, 24));
        seq.tracks.push(tr);

        let flat = flatten_sequence(&seq, &[seq.clone()]);
        assert_eq!(flat.id, seq.id);
        assert_eq!(flat.tracks.len(), seq.tracks.len());
        assert_eq!(flat.tracks[0].clips.len(), seq.tracks[0].clips.len());
        for (a, b) in flat.tracks[0].clips.iter().zip(&seq.tracks[0].clips) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.timeline_in, b.timeline_in);
            assert_eq!(a.timeline_out, b.timeline_out);
        }
    }

    // ─── slice_inner_clip_to_compound ───────────────────────────────────────

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn slice_inner_fully_inside_window() {
        // Compound: parent [100..148], source window [0..48], speed 1.0.
        // Inner clip on inner timeline at [12..36], source [0..24].
        let inner = media_clip(12, 36, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, /*parent_in*/ 100.0 / 24.0,
            /*src_in*/ 0.0, /*src_out*/ 48.0 / 24.0,
            1.0, 24,
        )
        .expect("inside window should slice");
        // visible_in=12/24, visible_out=36/24 → parent_in=100/24+12/24, parent_out=100/24+36/24
        assert!(approx_eq(sliced.timeline_in.to_secs_f64(), (100.0 + 12.0) / 24.0));
        assert!(approx_eq(sliced.timeline_out.to_secs_f64(), (100.0 + 36.0) / 24.0));
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 0.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 24.0 / 24.0));
        assert!(approx_eq(sliced.speed, 1.0));
    }

    #[test]
    fn slice_inner_outside_left_returns_none() {
        // Inner clip at inner timeline [0..24]; compound source window [48..72].
        let inner = media_clip(0, 24, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 48.0 / 24.0, 72.0 / 24.0, 1.0, 24,
        );
        assert!(sliced.is_none());
    }

    #[test]
    fn slice_inner_outside_right_returns_none() {
        // Inner clip at inner timeline [72..96]; compound source window [0..48].
        let inner = media_clip(72, 96, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 0.0, 48.0 / 24.0, 1.0, 24,
        );
        assert!(sliced.is_none());
    }

    #[test]
    fn slice_inner_clipped_at_left_advances_source_in() {
        // Inner [0..48] with source [0..48]. Compound source window [12..48].
        // Visible window: [12..48]; left-clipped by 12.
        // source_in advances from 0 to 12 (in inner-timeline secs == source secs).
        let inner = media_clip(0, 48, 0, 48);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 12.0 / 24.0, 48.0 / 24.0, 1.0, 24,
        )
        .expect("partial overlap should slice");
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 12.0 / 24.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 48.0 / 24.0));
        // parent_in starts at 0 (the trim starts at the compound's left edge)
        assert!(approx_eq(sliced.timeline_in.to_secs_f64(), 0.0));
    }

    #[test]
    fn slice_inner_clipped_at_right_pulls_source_out_back() {
        // Inner [0..48] with source [0..48]. Compound source window [0..36].
        // Visible window: [0..36]; right-clipped by 12.
        let inner = media_clip(0, 48, 0, 48);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 0.0, 36.0 / 24.0, 1.0, 24,
        )
        .expect("partial overlap should slice");
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 0.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 36.0 / 24.0));
    }

    #[test]
    fn slice_compound_speed_two_halves_parent_interval() {
        // Inner [0..24] with source [0..24]. Compound source window [0..24].
        // Compound speed 2.0 → parent_out = parent_in + 24/2 = parent_in + 12.
        let inner = media_clip(0, 24, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 0.0, 0.0, 24.0 / 24.0, 2.0, 24,
        )
        .expect("speed=2 should still slice");
        // parent timeline duration is half the inner-timeline window (24/2=12 frames=0.5s)
        assert!(approx_eq(
            sliced.timeline_out.to_secs_f64() - sliced.timeline_in.to_secs_f64(),
            12.0 / 24.0,
        ));
        // speed = inner.speed (1.0) * compound.speed (2.0)
        assert!(approx_eq(sliced.speed, 2.0));
        // source window unchanged (the inner clip's own media still spans [0..24])
        assert!(approx_eq(sliced.source_in.to_secs_f64(), 0.0));
        assert!(approx_eq(sliced.source_out.to_secs_f64(), 24.0 / 24.0));
    }

    #[test]
    fn slice_compound_speed_one_matches_old_additive_math() {
        // Regression: speed=1.0 must match the old additive flatten math.
        let inner = media_clip(12, 36, 0, 24);
        let sliced = slice_inner_clip_to_compound(
            &inner, 100.0 / 24.0, 0.0, 48.0 / 24.0, 1.0, 24,
        )
        .expect("speed=1 should slice");
        let parent_in_secs = 100.0 / 24.0 + 12.0 / 24.0;
        let parent_out_secs = 100.0 / 24.0 + 36.0 / 24.0;
        assert!(approx_eq(sliced.timeline_in.to_secs_f64(), parent_in_secs));
        assert!(approx_eq(sliced.timeline_out.to_secs_f64(), parent_out_secs));
        assert!(approx_eq(sliced.speed, 1.0));
    }
}
