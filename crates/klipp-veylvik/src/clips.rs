//! Several of Klipp's clips in one composition: a precomposition's, its
//! composition on the precomposition clip. Each clip of its scene is a
//! footage node named by the clip's key ([`clip_key`]), which Klipp answers
//! with that clip's footage at the scene's time, through its trim, speed
//! and direction, and clear where the clip isn't. The timing is Klipp's:
//! the clips are moved and trimmed in the precomposition, as any clip is,
//! and the composition only sees pictures. [`crate::CLIP_MEDIA_KEY`] is
//! the scene as Klipp composes it.

use std::collections::BTreeSet;

use klipp_core::ClipId;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};
use veylvik_document::{nodes, Composition, Node, Operation};

/// What a composition names the scene's clip `id` by: its id as the scene
/// keeps it, not as one instance of the scene renames it.
pub fn clip_key(id: ClipId) -> String {
    format!("clip:{}", id.0)
}

/// The scene's clip `key` names, if it names one.
pub fn clip_of_key(key: &str) -> Option<ClipId> {
    key.strip_prefix("clip:").and_then(|id| uuid::Uuid::parse_str(id).ok()).map(ClipId)
}

/// The keys of the scene's clips `composition` has footage of.
pub fn clip_keys(composition: &Composition) -> BTreeSet<String> {
    composition
        .nodes
        .values()
        .filter_map(|n| match &n.operation {
            Operation::Footage(f) if clip_of_key(&f.asset).is_some() => Some(f.asset.clone()),
            _ => None,
        })
        .collect()
}

/// Whether the composition in `bytes` may have footage of a scene's
/// clips: their key is in it.
pub fn bytes_name_clips(bytes: &[u8]) -> bool {
    bytes.windows(6).any(|w| w == b"\"clip:")
}

/// The scene's clips a composition can lay in: those with video media,
/// the lowest track's first (the first track is the top one), and in a
/// track by time.
pub fn scene_clips(scene: &Sequence) -> Vec<&Clip> {
    scene
        .tracks
        .iter()
        .rev()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| {
            let mut clips: Vec<&Clip> = t.clips.iter().filter(|c| matches!(c.kind, ClipKind::Media { .. })).collect();
            clips.sort_by_key(|c| c.timeline_in);
            clips
        })
        .collect()
}

/// A composition of `scene`'s clips at its size and rate: each clip's
/// footage, named `label`, fitted to the frame and laid over those before
/// it, as the scene's tracks stack. Refused when no clip has video media.
pub fn of_clips(scene: &Sequence, label: impl Fn(&Clip) -> String) -> Result<Composition, String> {
    let clips = scene_clips(scene);
    let first = clips.first().ok_or("none of the clips has video media")?;
    let size = [scene.resolution.width.max(1), scene.resolution.height.max(1)];
    let fps = scene.fps.as_f64();
    let rate = crate::compose::rate_of(fps);
    let frames = ((scene.total_duration().to_secs_f64() * fps).round() as u32).max(1);
    let mut c = Composition::from_host_footage(&clip_key(first.id), &label(first), None, size, rate, frames).map_err(|e| e.to_string())?;
    for clip in &clips[1..] {
        let name = label(clip);
        let footage = Node::new(
            &name,
            Operation::Footage(nodes::Footage {
                asset: clip_key(clip.id),
                timing: Default::default(),
                placement: veylvik_document::media::Placement::Fit,
                colour: Default::default(),
                layer: None,
                fallback: String::new(),
            }),
            &[],
        );
        let merge = Node::new(&format!("{name} over"), Operation::Merge(nodes::Merge {}), &[("background", c.output), ("foreground", footage.id)]);
        c.output = merge.id;
        c.nodes.insert(footage.id, footage);
        c.nodes.insert(merge.id, merge);
    }
    c.validate().map_err(|e| e.to_string())?;
    Ok(c)
}

/// Where in its media `clip` of a scene at `fps` is at `scene_secs` into
/// the scene, as Klipp's renders read it, or `None` where it isn't.
pub fn media_secs(clip: &Clip, scene_secs: f64, fps: f64) -> Option<f64> {
    let (start, end) = (clip.timeline_in.to_secs_f64(), clip.timeline_out.to_secs_f64());
    // A hair's grace: a frame's time, rounded on its way, can fall just before it.
    let at = scene_secs + 1e-6;
    (at >= start && at < end).then(|| clip.source_secs_at(scene_secs - start, fps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::Track;

    fn at(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    #[test]
    fn the_scenes_clips_are_laid_in_from_the_lowest_track_up_each_by_its_key() {
        let mut scene = Sequence::new_default();
        let (mut top, mut low) = (Track::new("V2", TrackKind::Video), Track::new("V1", TrackKind::Video));
        let a = Clip::media(MediaId(uuid::Uuid::from_u128(1)), at(0.0), at(2.0), at(0.0), at(2.0));
        let b = Clip::media(MediaId(uuid::Uuid::from_u128(2)), at(1.0), at(3.0), at(0.0), at(2.0));
        let later = Clip::media(MediaId(uuid::Uuid::from_u128(3)), at(2.0), at(3.0), at(0.0), at(1.0));
        low.clips = vec![later.clone(), a.clone()];
        top.clips = vec![b.clone()];
        scene.tracks = vec![top, low, Track::new("A1", TrackKind::Audio)];
        assert_eq!(scene_clips(&scene).iter().map(|c| c.id).collect::<Vec<_>>(), vec![a.id, later.id, b.id]);
        let c = of_clips(&scene, |c| format!("{:?}", c.media_id())).unwrap();
        assert_eq!(clip_keys(&c), BTreeSet::from([clip_key(a.id), clip_key(b.id), clip_key(later.id)]));
        // The top track's clip is laid over everything.
        let Operation::Footage(f) = &c.nodes[&c.nodes[&c.output].inputs["foreground"]].operation else { panic!("footage on top") };
        assert_eq!(f.asset, clip_key(b.id));
        assert!(bytes_name_clips(&serde_json::to_vec(&c).unwrap()));
        assert_eq!(clip_of_key(&clip_key(a.id)), Some(a.id));
        assert_eq!(clip_of_key(crate::CLIP_MEDIA_KEY), None);
        assert!(of_clips(&Sequence::new_default(), |_| String::new()).is_err());
    }

    #[test]
    fn a_clip_is_at_its_media_time_inside_it_and_nowhere_outside() {
        // At 1 s into the scene, from 3 s into its media.
        let c = Clip::media(MediaId(uuid::Uuid::from_u128(1)), at(1.0), at(2.0), at(3.0), at(4.0));
        assert_eq!(media_secs(&c, 0.5, 24.0), None);
        assert!((media_secs(&c, 1.0, 24.0).unwrap() - 3.0).abs() < 1e-6);
        assert!((media_secs(&c, 1.5, 24.0).unwrap() - 3.5).abs() < 1e-6);
        assert_eq!(media_secs(&c, 2.0, 24.0), None);
    }
}
