//! Freezing a track (#3207): what a frozen frame is, which frames of a
//! track can be frozen, and the keys that say whether a frozen frame is
//! still the one the track renders.
//!
//! A frozen frame is the track's layer as the stack takes it before
//! blending: the active clip (or transition) drawn with everything on it
//! — the track's effects are baked onto its clips in the render copy —
//! with neither the clip's opacity nor its blend mode applied. Those, and
//! the track's opacity, are applied live over it, as over a live layer, so
//! a frozen track keeps them as controls.
//!
//! A frame whose layer reads or writes the picture of other tracks can't
//! be frozen: it is left live, and [`live_reason`] says why.
//!
//! Keys are stable hashes of everything a frame's picture depends on
//! ([`slot_key`] for one track, [`frame_key`] for the whole frame, which
//! the background render of #3212 caches by). A frame is shown from a
//! cache only while its key is the one it was stored under; anything else
//! renders live. Keys take the sequence the renderer draws: the engine's
//! render copy (`flatten_sequence`, which bakes the tracks' effects and
//! grade groups onto the clips and lays the nested scenes in).
//!
//! A clip's opacity is part of its frames' key for now: changing a frozen
//! clip's opacity marks those frames stale, and they render live until
//! frozen again, though the frozen layer would still serve. Its blend
//! mode, the track's opacity and everything on other tracks are not.
//!
//! Stored frames are half floats (RGBA f16, straight alpha): stored as
//! 8-bit sRGB with 8-bit alpha, a graded, blurred clip laid by Screen at
//! 60% came out up to 6 levels from live; as half floats every case is
//! within 1 (`tests/freeze_parity.rs`).

use std::hash::{Hash, Hasher};

use klipp_core::Timecode;
use klipp_effects::BuiltinEffect;
use klipp_timeline::canonical_hash::canonical_hash;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

use crate::render::{active_item, contrast_protect, is_visual, TrackItem};

/// Bumped whenever the compositor draws a layer differently, so a frame
/// frozen by an older build is never taken for a current one.
pub const FREEZE_RENDER_VERSION: u64 = 1;

/// Nests deeper than this are hashed as "too deep": the renderer stops
/// there too.
const MAX_NEST: usize = klipp_timeline::flatten::MAX_COMPOUND_DEPTH as usize;

/// What a key reads beyond the sequence: the project's other sequences
/// (for nested ones), the identity of each media item and plug-in, and
/// the render's environment.
pub struct FreezeEnv<'a> {
    /// Every sequence of the project, for the nested ones a clip shows.
    pub sequences: &'a [Sequence],
    /// A media item's identity (its file's path, length and modification
    /// time, log format, proxy), by its id; `None` when the pool hasn't it.
    pub media: &'a dyn Fn(&str) -> Option<u64>,
    /// An installed plug-in's identity (file, modification time, version),
    /// by its effect id (`frei0r:...`, `ofx:<name>`); `None` when missing.
    pub plugin: &'a dyn Fn(&str) -> Option<u64>,
    /// The render's environment: working space, LUT interpolation, proxies,
    /// the global LUT, and the size frames are drawn at.
    pub environment: u64,
}

/// Why `track`'s frame at `time` can't be frozen, or `None` when it can (or
/// nothing is there). Each reason is a layer that reads or writes the
/// picture of other tracks, or a caption, placed against other tracks.
pub fn live_reason(seq: &Sequence, track: usize, time: Timecode) -> Option<&'static str> {
    let t = seq.tracks.get(track)?;
    if t.kind == TrackKind::Subtitle {
        return Some("a caption track, placed against the other tracks' titles");
    }
    match active_item(seq, t, time)? {
        TrackItem::Clip(c) => clip_live_reason(c),
        TrackItem::Transition { a, b, .. } => clip_live_reason(a).or_else(|| clip_live_reason(b)),
    }
}

fn clip_live_reason(c: &Clip) -> Option<&'static str> {
    if matches!(c.kind, ClipKind::AdjustmentLayer) {
        return Some("an adjustment layer, which grades the tracks below it");
    }
    if crate::resolve::reads_below(c) {
        return Some("a clip that reads the picture below it");
    }
    if c.matte_source.is_some() {
        return Some("matted by another track's clip");
    }
    if c.matte_stencil.is_some() || c.consumed_as_matte {
        return Some("another clip's matte or stencil");
    }
    if contrast_protect(c) != 0 {
        return Some("a title that protects its contrast against the picture below");
    }
    if c.surface_pin.is_some() {
        return Some("pinned to a surface it takes its light from");
    }
    None
}

/// The key of `track`'s layer at `time` (see the module), or `None` when
/// the track shows nothing there or the frame stays live.
pub fn slot_key(seq: &Sequence, track: usize, time: Timecode, env: &FreezeEnv<'_>) -> Option<u64> {
    let t = seq.tracks.get(track)?;
    if !is_visual(t) || live_reason(seq, track, time).is_some() {
        return None;
    }
    let item = active_item(seq, t, time)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("slot", FREEZE_RENDER_VERSION, env.environment).hash(&mut h);
    (time.value, time.rate).hash(&mut h);
    item_hash(seq, &item, env, false, &mut h);
    Some(h.finish())
}

/// The key of the whole frame at `time`: each visible track's layer as it
/// is laid (with its clip's and its own opacity and blend), the stack's
/// own work over them (stencils, adjustment layers, the timeline grade),
/// the camera, and the environment. The background render (#3212) keys
/// its frames by it.
pub fn frame_key(seq: &Sequence, time: Timecode, env: &FreezeEnv<'_>) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("frame", FREEZE_RENDER_VERSION, env.environment).hash(&mut h);
    (time.value, time.rate).hash(&mut h);
    (seq.resolution.width, seq.resolution.height, seq.fps.num, seq.fps.den).hash(&mut h);
    hash_serialised(&seq.timeline_graph, &mut h);
    hash_serialised(&seq.camera, &mut h);
    for (i, t) in seq.tracks.iter().enumerate().filter(|(_, t)| is_visual(t)) {
        let Some(item) = active_item(seq, t, time) else { continue };
        (i, t.kind == TrackKind::Subtitle, t.opacity.to_bits()).hash(&mut h);
        item_hash(seq, &item, env, true, &mut h);
    }
    h.finish()
}

/// What `item` draws: its clips, a transition's own settings, and, with
/// `laid`, how each clip is laid (its blend mode).
fn item_hash(seq: &Sequence, item: &TrackItem<'_>, env: &FreezeEnv<'_>, laid: bool, h: &mut impl Hasher) {
    match item {
        TrackItem::Clip(c) => clip_hash(seq, c, env, laid, 0, h),
        TrackItem::Transition { a, b, kind, progress, params, shutter } => {
            "transition".hash(h);
            hash_serialised(kind, h);
            hash_serialised(params, h);
            (progress.to_bits(), shutter.0.to_bits(), shutter.1).hash(h);
            clip_hash(seq, a, env, laid, 0, h);
            clip_hash(seq, b, env, laid, 0, h);
        }
    }
}

/// `c` as it draws: the clip (its place, kind, title or generator, every
/// field it serialises) and its picture, which a picture-truth clip
/// doesn't serialise; the nested sequence it shows, recursively; its media
/// and plug-ins' identities. Its blend mode only when `laid`: a frozen
/// layer is drawn without it.
fn clip_hash(seq: &Sequence, c: &Clip, env: &FreezeEnv<'_>, laid: bool, depth: usize, h: &mut impl Hasher) {
    if laid {
        hash_serialised(c, h);
    } else {
        let mut drawn = c.clone();
        drawn.blend_mode = klipp_core::BlendMode::Normal;
        hash_serialised(&drawn, h);
    }
    match c.picture_v2() {
        Ok(p) => {
            hash_serialised(&p.picture, h);
            hash_serialised(&p.versions, h);
        }
        Err(why) => why.hash(h),
    }
    let nested = match &c.kind {
        ClipKind::Sequence { sequence_id } | ClipKind::Precomposition { sequence_id } | ClipKind::TemplatePrecomposition { sequence_id, .. } => Some(*sequence_id),
        _ => None,
    };
    if let Some(id) = nested {
        match env.sequences.iter().chain(std::iter::once(seq)).find(|s| s.id == id) {
            Some(_) if depth >= MAX_NEST => "too deep".hash(h),
            Some(inner) => sequence_hash(inner, env, depth + 1, h),
            None => "missing sequence".hash(h),
        }
    }
    for media in clip_media(c) {
        (env.media)(&media).hash(h);
    }
    for plugin in clip_plugins(c) {
        (env.plugin)(&plugin).hash(h);
    }
}

/// A nested sequence as a whole: its own fields and every clip on it as it
/// draws, whatever the time (a nest is drawn at its own times).
fn sequence_hash(s: &Sequence, env: &FreezeEnv<'_>, depth: usize, h: &mut impl Hasher) {
    hash_serialised(s, h);
    for c in s.all_clips() {
        clip_hash(s, c, env, true, depth, h);
    }
}

/// The media items `c` reads: its own and those its picture lays beside it.
fn clip_media(c: &Clip) -> Vec<String> {
    let mut out: Vec<String> = c.media_id().map(|m| m.to_string()).into_iter().collect();
    out.extend(crate::resolve::media_sources(c).into_iter().map(|(m, _)| m));
    out
}

/// The plug-ins `c` runs, by effect id.
fn clip_plugins(c: &Clip) -> Vec<String> {
    let mut out: Vec<String> = c
        .picture_effects()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| match e.effect {
            BuiltinEffect::Frei0r { type_id, .. } => Some(type_id),
            _ => None,
        })
        .collect();
    out.extend(c.picture_ofx().unwrap_or_default().into_iter().map(|p| format!("ofx:{}", p.entry.plugin_name)));
    out
}

fn hash_serialised<T: serde::Serialize + ?Sized>(value: &T, h: &mut impl Hasher) {
    match canonical_hash(value) {
        Ok(v) => v.hash(h),
        Err(why) => why.to_string().hash(h),
    }
}
