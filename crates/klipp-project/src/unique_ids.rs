//! A clip id twice in one sequence, given an id of its own before saving
//! (#2457). The file keys each clip's picture by its sequence and id: a
//! repeated id kept no stored picture for either clip, and was written as
//! a lift of its fields, which loses what only a picture-truth clip's
//! graph holds (an effect's mix or mask, a published grade port). Each
//! later occurrence is saved under a new id instead, the same one every
//! time for the same place, and what named it (a linked clip, a
//! transition) names it still. Nothing is lost, and the open project is
//! left as it is.

use klipp_core::ClipId;
use klipp_timeline::Sequence;
use std::collections::HashMap;

/// `sequences` with every clip id once per sequence, or `None` when they
/// already are.
pub(crate) fn unique_clip_ids(sequences: &[Sequence]) -> Option<Vec<Sequence>> {
    if !sequences.iter().any(has_repeats) {
        return None;
    }
    let mut out = sequences.to_vec();
    for seq in out.iter_mut().filter(|s| has_repeats(s)) {
        repair(seq);
    }
    Some(out)
}

fn has_repeats(seq: &Sequence) -> bool {
    let mut seen = std::collections::HashSet::new();
    seq.tracks.iter().flat_map(|t| &t.clips).any(|c| !seen.insert(c.id))
}

fn repair(seq: &mut Sequence) {
    // Each occurrence of a repeated id: the first keeps it, every later one
    // takes one made from the id and its place, so a second save of the
    // same timeline writes the same ids.
    let mut count: HashMap<ClipId, usize> = HashMap::new();
    let mut renamed: HashMap<(usize, usize), ClipId> = HashMap::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for (ci, clip) in track.clips.iter().enumerate() {
            let n = count.entry(clip.id).or_default();
            if *n > 0 {
                let name = format!("{}:{ti}:{ci}", seq.id);
                renamed.insert((ti, ci), ClipId(uuid::Uuid::new_v5(&clip.id.0, name.as_bytes())));
            }
            *n += 1;
        }
    }
    let repeated: std::collections::HashSet<ClipId> = count.iter().filter(|(_, n)| **n > 1).map(|(id, _)| *id).collect();
    // Where each repeated id is now: (track, clip index, its id, its start).
    let mut places: HashMap<ClipId, Vec<(usize, usize, ClipId, klipp_core::Timecode)>> = HashMap::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for (ci, clip) in track.clips.iter().enumerate().filter(|(_, c)| repeated.contains(&c.id)) {
            let now = renamed.get(&(ti, ci)).copied().unwrap_or(clip.id);
            places.entry(clip.id).or_default().push((ti, ci, now, clip.timeline_in));
        }
    }
    // A link to a repeated id goes to the occurrence beside it: the one on
    // another track that starts closest to the linking clip.
    let mut links: Vec<(usize, usize, ClipId)> = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for (ci, clip) in track.clips.iter().enumerate() {
            let Some(target) = clip.linked_clip_id.filter(|l| repeated.contains(l)) else { continue };
            let gap = |at: klipp_core::Timecode| (at - clip.timeline_in).abs();
            if let Some((_, _, now, _)) = places[&target].iter().filter(|(t, _, _, _)| *t != ti).min_by_key(|(_, _, _, at)| gap(*at)) {
                links.push((ti, ci, *now));
            }
        }
    }
    // A transition joins two clips that meet on its track: the occurrence
    // of a repeated id that meets the other one there.
    for (ti, track) in seq.tracks.iter_mut().enumerate() {
        let id_at = |ci: usize| track.clips.get(ci).map(|c| renamed.get(&(ti, ci)).copied().unwrap_or(c.id));
        let fixes: Vec<(usize, ClipId, ClipId)> = track
            .transitions
            .iter()
            .enumerate()
            .filter_map(|(k, tr)| {
                let pair = (0..track.clips.len().saturating_sub(1)).find(|&ci| track.clips[ci].id == tr.clip_a_id && track.clips[ci + 1].id == tr.clip_b_id)?;
                Some((k, id_at(pair)?, id_at(pair + 1)?))
            })
            .collect();
        for (k, a, b) in fixes {
            (track.transitions[k].clip_a_id, track.transitions[k].clip_b_id) = (a, b);
        }
    }
    for ((ti, ci), id) in &renamed {
        let clip = &mut seq.tracks[*ti].clips[*ci];
        tracing::warn!("clip id {} is in sequence \"{}\" twice; the later one is saved as {}", clip.id.0, seq.name, id.0);
        clip.id = *id;
    }
    for (ti, ci, to) in links {
        seq.tracks[ti].clips[ci].linked_clip_id = Some(to);
    }
}
