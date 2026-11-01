//! Auditions (#3209): the `alternatives` op. A clip's slot holds several
//! takes ([`klipp_timeline::audition`]); this makes one, adds, removes and
//! renames takes, switches the slot to one, and finalizes it to a plain
//! clip. Each is one undo step.
//!
//! A switch puts the take's clip and its linked sound in the slot, keeping
//! the slot's id, start and track, so clips connected to it keep their
//! anchors; the one it replaces is stored as a take as it stood, edits
//! made to it and all. On the primary storyline the clips after it move
//! by the change in length, and the clips connected to those move with
//! them; elsewhere they move only with `ripple`, and a longer take that
//! would run into the next clip is refused.

use super::*;
use klipp_core::TakeId;
use klipp_timeline::audition::{Audition, Take};
use klipp_timeline::TrackRole;

/// What the op does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlternativesAction {
    Create,
    Add,
    Remove,
    Rename,
    Switch,
    Finalize,
}

/// A take to add: a file from the pool, from `from` to `to` seconds of it
/// (from its start, as long as the slot, when left out); or, with no
/// `media_id`, a copy of the clip in the slot as it is now, to vary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakeSpec {
    #[serde(default)]
    pub media_id: Option<MediaId>,
    #[serde(default)]
    pub from: Option<f64>,
    #[serde(default)]
    pub to: Option<f64>,
    #[serde(default)]
    pub name: Option<String>,
}

/// The longest name a take is given.
const NAME_MAX: usize = 80;

#[allow(clippy::too_many_arguments)]
pub(super) fn alternatives(
    seq: &Sequence,
    pool: &MediaPool,
    fps: Fps,
    i: usize,
    action: AlternativesAction,
    clip_ref: &ClipRef,
    takes: &[TakeSpec],
    take: Option<&str>,
    name: Option<&str>,
    ripple: bool,
) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let (ti, _, clip) = resolve_clip(seq, fps, clip_ref, i)?;
    if seq.tracks[ti].locked {
        return Err(refuse(i, format!("track {ti} is locked"), "unlock it first"));
    }
    let label = clip_name(seq, pool, clip);
    let replaced = |description: &'static str, audition: Option<Box<Audition>>| {
        let mut new = clip.clone();
        new.audition = audition;
        EditCommand::ReplaceClip { description, old: Box::new(clip.clone()), new: Box::new(new) }
    };
    match action {
        AlternativesAction::Create => {
            if let Some(a) = &clip.audition {
                return Err(refuse(i, format!("{label} already holds {} takes", a.takes.len()), "add to it with action add"));
            }
            if !matches!(clip.kind, ClipKind::Media { .. } | ClipKind::Title(_) | ClipKind::Generator(_) | ClipKind::Sequence { .. }) {
                return Err(refuse(i, format!("{label} is not a clip that holds takes"), "make an audition of a media, title, generator or compound clip"));
            }
            let first = Take { id: TakeId::new(), name: name_or(name, "Take 1", i)?, clip: None, sound: Vec::new() };
            let mut a = Audition { active: first.id, takes: vec![first] };
            let specs: &[TakeSpec] = if takes.is_empty() { &[TakeSpec { media_id: None, from: None, to: None, name: None }] } else { takes };
            for spec in specs {
                let t = take_of(seq, pool, fps, i, ti, clip, &a, spec)?;
                a.takes.push(t);
            }
            let names: Vec<&str> = a.takes.iter().map(|t| t.name.as_str()).collect();
            Ok((format!("make {label} an audition of {} takes: {}", a.takes.len(), names.join(", ")), replaced("Create Audition", Some(Box::new(a))), vec![clip.id]))
        }
        AlternativesAction::Add => {
            let mut a = audition_of(clip, &label, i)?.clone();
            if takes.is_empty() {
                return Err(refuse(i, "no take to add", "give takes: a media_id with from and to, or {} for a copy of the clip to vary"));
            }
            let mut added = Vec::new();
            for spec in takes {
                let t = take_of(seq, pool, fps, i, ti, clip, &a, spec)?;
                added.push(t.name.clone());
                a.takes.push(t);
            }
            Ok((format!("add {} to {label}'s audition, {} takes now", added.join(", "), a.takes.len()), replaced("Add to Audition", Some(Box::new(a))), vec![clip.id]))
        }
        AlternativesAction::Remove => {
            let mut a = audition_of(clip, &label, i)?.clone();
            let t = find_take(&a, take, &label, i)?;
            if t.id == a.active {
                return Err(refuse(i, format!("{} is the take in the slot", t.name), "switch to another take first, or finalize"));
            }
            let (id, gone) = (t.id, t.name.clone());
            a.takes.retain(|t| t.id != id);
            Ok((format!("remove {gone} from {label}'s audition, {} takes left", a.takes.len()), replaced("Remove from Audition", Some(Box::new(a))), vec![clip.id]))
        }
        AlternativesAction::Rename => {
            let mut a = audition_of(clip, &label, i)?.clone();
            let t = find_take(&a, take, &label, i)?;
            let id = t.id;
            let old = t.name.clone();
            let new = name_or(name, "", i)?;
            if new.is_empty() {
                return Err(refuse(i, "no name given", "give name"));
            }
            if a.takes.iter().any(|t| t.id != id && t.name.eq_ignore_ascii_case(&new)) {
                return Err(refuse(i, format!("another take is named {new:?}"), "give a name no other take has"));
            }
            if let Some(t) = a.takes.iter_mut().find(|t| t.id == id) {
                t.name = new.clone();
            }
            Ok((format!("rename {old} to {new} in {label}'s audition"), replaced("Rename Take", Some(Box::new(a))), vec![clip.id]))
        }
        AlternativesAction::Finalize => {
            let a = audition_of(clip, &label, i)?;
            let kept = a.active_take().map_or_else(|| "the take in the slot".to_string(), |t| t.name.clone());
            Ok((format!("finalize {label} as {kept}, {} other take(s) dropped", a.takes.len().saturating_sub(1)), replaced("Finalize Audition", None), vec![clip.id]))
        }
        AlternativesAction::Switch => switch(seq, pool, fps, i, ti, clip, take, ripple),
    }
}

fn audition_of<'a>(clip: &'a Clip, label: &str, i: usize) -> Result<&'a Audition, Refusal> {
    clip.audition.as_deref().ok_or_else(|| refuse(i, format!("{label} is not an audition"), "make one with action create"))
}

fn find_take<'a>(a: &'a Audition, key: Option<&str>, label: &str, i: usize) -> Result<&'a Take, Refusal> {
    let names = || a.takes.iter().map(|t| format!("{} ({})", t.name, t.id)).collect::<Vec<_>>().join(", ");
    let Some(key) = key else {
        return Err(refuse(i, "no take named", format!("give take, by name or id: {}", names())));
    };
    a.find(key).ok_or_else(|| refuse(i, format!("{label} has no take {key:?}"), format!("its takes are {}", names())))
}

/// `name` trimmed, or `default`; refused when too long.
fn name_or(name: Option<&str>, default: &str, i: usize) -> Result<String, Refusal> {
    let n = name.map(str::trim).unwrap_or(default).to_string();
    if n.chars().count() > NAME_MAX {
        return Err(refuse(i, format!("a take name of {} characters", n.chars().count()), format!("give a name of at most {NAME_MAX}")));
    }
    Ok(n)
}

/// The sound clip linked to `clip`, with its track and place there.
fn sound_of<'a>(seq: &'a Sequence, clip: &Clip) -> Option<(usize, usize, &'a Clip)> {
    clip.linked_clip_id.and_then(|id| find_clip(seq, id)).filter(|(_, _, s)| s.linked_clip_id == Some(clip.id))
}

/// The take `spec` makes for `clip`'s slot on track `ti`.
#[allow(clippy::too_many_arguments)]
fn take_of(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, clip: &Clip, a: &Audition, spec: &TakeSpec) -> Result<Take, Refusal> {
    let mut name = name_or(spec.name.as_deref(), "", i)?;
    if name.is_empty() {
        name = a.next_name();
    }
    if a.takes.iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
        return Err(refuse(i, format!("another take is named {name:?}"), "give a name no other take has"));
    }
    let Some(media_id) = spec.media_id else {
        // A copy of the slot as it is, its sound with it, to vary.
        if spec.from.is_some() || spec.to.is_some() {
            return Err(refuse(i, "from and to without a media_id", "give the media_id they are times of, or leave them out for a copy of the clip"));
        }
        let mut copy = clip.clone();
        copy.audition = None;
        copy.id = ClipId::new();
        let sound = sound_of(seq, clip).map(|(sti, _, s)| {
            let mut s = s.clone();
            s.id = ClipId::new();
            s.linked_clip_id = Some(copy.id);
            (seq.tracks[sti].id, s)
        });
        copy.linked_clip_id = sound.as_ref().map(|(_, s)| s.id);
        return Ok(Take { id: TakeId::new(), name, clip: Some(Box::new(copy)), sound: sound.into_iter().collect() });
    };
    let Some(item) = pool.get(&media_id) else {
        return Err(refuse(i, format!("no media {media_id} in the pool"), "give a media_id list_media shows"));
    };
    let duration = item.duration.to_secs_f64();
    let from = spec.from.unwrap_or(0.0);
    let to = spec.to.unwrap_or_else(|| (from + secs(clip.timeline_out - clip.timeline_in)).min(if duration > 0.0 { duration } else { f64::INFINITY }));
    if !from.is_finite() || !to.is_finite() || from < 0.0 || to <= from || (duration > 0.0 && to > duration + 1e-6) {
        return Err(refuse(i, format!("a take from {from} to {to} s of a file {duration:.2} s long"), "give from and to inside the file, from before to"));
    }
    let (src_in, src_out) = (snap(from, fps), snap(to, fps));
    if src_out <= src_in {
        return Err(refuse(i, "a take shorter than a frame", "give a longer stretch of the file"));
    }
    let tl_in = clip.timeline_in;
    let tl_out = tl_in + (src_out - src_in);
    let mut picture = Clip::media(media_id, tl_in, tl_out, src_in, src_out);
    // Its sound on the track the slot's sound is on, else the first sound
    // track, when the file has sound and the slot is a picture.
    let sound_track = (item.has_audio && seq.tracks[ti].kind != TrackKind::Audio)
        .then(|| sound_of(seq, clip).map(|(sti, ..)| sti).or_else(|| seq.tracks.iter().position(|t| t.kind == TrackKind::Audio)))
        .flatten();
    let sound = sound_track.map(|sti| {
        let mut s = Clip::media(media_id, tl_in, tl_out, src_in, src_out);
        s.linked_clip_id = Some(picture.id);
        (seq.tracks[sti].id, s)
    });
    picture.linked_clip_id = sound.as_ref().map(|(_, s)| s.id);
    Ok(Take { id: TakeId::new(), name, clip: Some(Box::new(picture)), sound: sound.into_iter().collect() })
}

#[allow(clippy::too_many_arguments)]
fn switch(seq: &Sequence, pool: &MediaPool, fps: Fps, i: usize, ti: usize, clip: &Clip, key: Option<&str>, ripple: bool) -> Result<(String, EditCommand, Vec<ClipId>), Refusal> {
    let label = clip_name(seq, pool, clip);
    let a = audition_of(clip, &label, i)?;
    let to = find_take(a, key, &label, i)?;
    if to.id == a.active {
        return Err(refuse(i, format!("{} is already the take in the slot", to.name), "name another take"));
    }
    let Some(stored) = to.clip.as_deref() else {
        return Err(refuse(i, format!("{} has no clip stored", to.name), "name another take"));
    };
    let len = stored.timeline_out - stored.timeline_in;
    let new_out = clip.timeline_in + len;
    // The take leaving the slot, stored as it stands, its sound with it.
    let old_sound = sound_of(seq, clip);
    let mut leaving = clip.clone();
    leaving.audition = None;
    let mut audition = a.clone();
    for t in &mut audition.takes {
        if t.id == a.active {
            t.clip = Some(Box::new(leaving.clone()));
            t.sound = old_sound.map(|(sti, _, s)| (seq.tracks[sti].id, s.clone())).into_iter().collect();
        } else if t.id == to.id {
            t.clip = None;
            t.sound = Vec::new();
        }
    }
    audition.active = to.id;
    // The take coming in, in the slot: its id, start, track and anchor.
    let mut incoming = stored.clone();
    incoming.id = clip.id;
    incoming.timeline_in = clip.timeline_in;
    incoming.timeline_out = new_out;
    incoming.magnetic_anchor = clip.magnetic_anchor.clone();
    incoming.audition = Some(Box::new(audition));
    let mut sound_in: Vec<(usize, Clip)> = Vec::new();
    for (track_id, s) in &to.sound {
        let sti = seq
            .tracks
            .iter()
            .position(|t| t.id == *track_id)
            .or_else(|| old_sound.map(|(sti, ..)| sti))
            .or_else(|| seq.tracks.iter().position(|t| t.kind == TrackKind::Audio))
            .ok_or_else(|| refuse(i, format!("{}'s sound has no sound track to go on", to.name), "add a sound track first"))?;
        if seq.tracks[sti].locked {
            return Err(refuse(i, format!("track {sti}, for {}'s sound, is locked", to.name), "unlock it first"));
        }
        // Where it lay against its picture (a J or L cut) is kept.
        let mut s = s.clone();
        let (off, slen) = (s.timeline_in - stored.timeline_in, s.timeline_out - s.timeline_in);
        s.timeline_in = clip.timeline_in + off;
        s.timeline_out = s.timeline_in + slen;
        s.linked_clip_id = Some(clip.id);
        sound_in.push((sti, s));
    }
    incoming.linked_clip_id = sound_in.first().map(|(_, s)| s.id);
    // The clips after it: moved on the primary storyline, or with ripple.
    let primary = seq.tracks[ti].magnetic_role == TrackRole::Primary;
    let rippling = ripple || primary;
    // Read without the sound leaving the slot, so a later clip's sound
    // may move into its place.
    let mut without = seq.clone();
    if let Some((sti, sci, _)) = old_sound {
        without.tracks[sti].clips.remove(sci);
    }
    let Some((_, _, here)) = find_clip(&without, clip.id) else { return Err(refuse(i, "no such clip", "name a clip on the timeline")) };
    let Rippled { moves, ids: moved, later, delta, partners } = ripple_after(&without, pool, fps, i, &[(ti, here, new_out)], rippling, "switch").map_err(|mut r| {
        r.hint = r.hint.replace("switch with ripple ", "switch with ripple: true ");
        r
    })?;
    let moving: std::collections::HashSet<ClipId> = moved.iter().copied().collect();
    for (sti, s) in &sound_in {
        let staying = |c: &&Clip| Some(c.id) != old_sound.map(|(_, _, o)| o.id) && !moving.contains(&c.id);
        if let Some(n) = seq.tracks[*sti].clips.iter().filter(staying).find(|c| c.timeline_in < s.timeline_out && s.timeline_in < c.timeline_out) {
            return Err(refuse(
                i,
                format!("{}'s sound would run over {} at {:.2} s on track {sti}", to.name, clip_name(seq, pool, n), secs(n.timeline_in)),
                "switch with ripple: true to move the clips after it, or make room first",
            ));
        }
    }
    // Clips connected to the clips moved go with them, as a magnetic edit
    // carries them.
    let mut connected = Vec::new();
    if delta.value != 0 && !moving.is_empty() {
        for (cti, track) in seq.tracks.iter().enumerate() {
            let carried: Vec<&Clip> = match track.magnetic_role {
                TrackRole::Connected => track.clips.iter().filter(|c| c.magnetic_anchor.as_ref().is_some_and(|a| moving.contains(&a.anchor_clip_id)) && !moving.contains(&c.id)).collect(),
                TrackRole::ConnectedStoryline if track.storyline_anchor.as_ref().is_some_and(|a| moving.contains(&a.anchor_clip_id)) => track.clips.iter().filter(|c| !moving.contains(&c.id)).collect(),
                _ => Vec::new(),
            };
            for c in carried {
                if seq.tracks[cti].locked {
                    return Err(refuse(i, format!("a clip connected to a clip after it is on locked track {cti}"), "unlock it first, or switch without ripple off the primary storyline"));
                }
                connected.push(EditCommand::LegacyMoveClip {
                    clip_id: c.id,
                    old_timeline_in: c.timeline_in,
                    old_timeline_out: c.timeline_out,
                    new_timeline_in: c.timeline_in + delta,
                    new_timeline_out: c.timeline_out + delta,
                    linked_partner: None,
                });
            }
        }
    }
    let mut edits = Vec::new();
    if let Some((sti, sci, s)) = old_sound {
        edits.push(EditCommand::LegacyRemoveClip { clip: s.clone(), track_index: sti, position: sci });
    }
    edits.push(EditCommand::ReplaceClip { description: "Switch Take", old: Box::new(clip.clone()), new: Box::new(incoming) });
    let mut ids = vec![clip.id];
    for (sti, s) in sound_in {
        ids.push(s.id);
        edits.push(EditCommand::AddTitleClip { clip: s, track_index: sti, created_track: false, track_id: None });
    }
    let carried = connected.len();
    edits.extend(moves);
    edits.extend(connected);
    ids.extend(moved);
    let what = format!(
        "switch {label} to {} ({:.2}-{:.2} s, {:+.2} s){}{}{}",
        to.name,
        secs(clip.timeline_in),
        secs(new_out),
        secs(new_out) - secs(clip.timeline_out),
        if later > 0 { format!("; {later} later clip(s) moved {:+.2} s", secs(delta)) } else { String::new() },
        if partners > 0 { format!(", {partners} linked to them with them") } else { String::new() },
        if carried > 0 { format!(", {carried} connected clip(s) with them") } else { String::new() },
    );
    Ok((what, EditCommand::Batch(edits), ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{AnchorAlignment, MagneticAnchor, Track};

    fn tc(secs: f64) -> Timecode {
        RationalTime::from_secs_f64(secs, 25)
    }

    fn media(pool: &mut MediaPool, path: &str, secs: f64) -> MediaId {
        let mut item = klipp_media::MediaItem::synthetic_for_test(path.into(), None);
        item.duration = tc(secs);
        item.video_codec = "hevc".into();
        item.has_audio = true;
        pool.add(item)
    }

    /// Three shots of a file on V1, 0-4, 4-8 and 8-12 s, each with its
    /// sound linked on A1; the shots' ids.
    fn fixture() -> (Sequence, MediaPool, [ClipId; 3]) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let m = media(&mut pool, "/tmp/a.mov", 20.0);
        let (mut v, mut a) = (Track::new("V1", TrackKind::Video), Track::new("A1", TrackKind::Audio));
        let mut ids = [ClipId::nil(); 3];
        for (n, id) in ids.iter_mut().enumerate() {
            let (t0, t1) = (tc(4.0 * n as f64), tc(4.0 * (n + 1) as f64));
            let mut p = Clip::media(m, t0, t1, t0, t1);
            let mut s = Clip::media(m, t0, t1, t0, t1);
            p.linked_clip_id = Some(s.id);
            s.linked_clip_id = Some(p.id);
            *id = p.id;
            v.clips.push(p);
            a.clips.push(s);
        }
        seq.tracks = vec![v, a];
        (seq, pool, ids)
    }

    fn op(action: AlternativesAction, clip: ClipId) -> Op {
        alt(action, clip, Vec::new(), None, None, false)
    }

    fn alt(action: AlternativesAction, clip: ClipId, takes: Vec<TakeSpec>, take: Option<&str>, name: Option<&str>, ripple: bool) -> Op {
        Op::Alternatives { action, clip_id: ClipRef::Id(clip), takes, take: take.map(str::to_string), name: name.map(str::to_string), ripple }
    }

    /// Resolved and applied; the edit, to undo.
    fn run(seq: &mut Sequence, pool: &MediaPool, o: Op) -> Result<(String, EditCommand), Refusal> {
        let plan = resolve(seq, pool, &[o])?;
        let step = plan.steps.into_iter().next().expect("a step");
        let mut seqs = vec![seq.clone()];
        let (mut p, mut f) = (pool.clone(), seq.fps);
        crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut f }.apply_forward(&step.edit);
        *seq = seqs.remove(0);
        Ok((step.what, step.edit))
    }

    fn undo(seq: &mut Sequence, pool: &MediaPool, edit: &EditCommand) {
        let mut seqs = vec![seq.clone()];
        let (mut p, mut f) = (pool.clone(), seq.fps);
        crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut f }.apply_inverse(edit);
        *seq = seqs.remove(0);
    }

    /// Every clip on every track, as saved, the slot's takes aside: what a
    /// switch and back must restore exactly.
    fn timeline(seq: &Sequence) -> serde_json::Value {
        let tracks: Vec<Vec<serde_json::Value>> = seq
            .tracks
            .iter()
            .map(|t| {
                t.clips
                    .iter()
                    .map(|c| {
                        let mut c = c.clone();
                        c.audition = None;
                        serde_json::to_value(&c).expect("a clip")
                    })
                    .collect()
            })
            .collect();
        serde_json::to_value(tracks).expect("the tracks")
    }

    fn clip(seq: &Sequence, id: ClipId) -> &Clip {
        seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).expect("the clip")
    }

    /// #3209: a slot switched to a longer take of another file and back
    /// is restored exactly, picture, sound and the clips after it; an edit
    /// made to a take while it is in the slot stays with it; on a track
    /// that isn't the primary storyline the longer take is refused, naming
    /// the clip it would run into, until ripple is given; every step undoes
    /// exactly.
    #[test]
    fn switching_restores_each_take_exactly() {
        let (mut seq, mut pool, [_, b, c]) = fixture();
        let other = media(&mut pool, "/tmp/other.mov", 10.0);
        let start = timeline(&seq);
        let create = alt(AlternativesAction::Create, b, vec![TakeSpec { media_id: Some(other), from: Some(1.0), to: Some(7.0), name: Some("Wide".into()) }, TakeSpec::default()], None, None, false);
        let (what, made) = run(&mut seq, &pool, create).unwrap();
        assert!(what.contains("3 takes: Take 1, Wide, Take 2"), "{what}");
        assert_eq!(timeline(&seq), start, "making the audition changes nothing on the timeline");
        let a = clip(&seq, b).audition.as_deref().expect("the audition");
        assert_eq!(a.active_take().map(|t| t.name.as_str()), Some("Take 1"));

        let switch = |take: &str, ripple: bool| alt(AlternativesAction::Switch, b, Vec::new(), Some(take), None, ripple);
        let refused = run(&mut seq, &pool, switch("Wide", false)).unwrap_err();
        assert!(refused.why.contains("would run over") && refused.why.contains("8.00 s"), "names the clip it runs into: {}", refused.why);
        assert!(refused.hint.contains("ripple: true"), "{}", refused.hint);

        let (what, to_wide) = run(&mut seq, &pool, switch("Wide", true)).unwrap();
        assert!(what.contains("+2.00 s") && what.contains("1 later clip(s) moved +2.00 s"), "{what}");
        let slot = clip(&seq, b);
        assert_eq!((slot.media_id(), slot.timeline_in, slot.timeline_out, slot.source_in, slot.source_out), (Some(other), tc(4.0), tc(10.0), tc(1.0), tc(7.0)), "the wide take in the slot");
        let sound = clip(&seq, slot.linked_clip_id.expect("its sound"));
        assert_eq!((sound.media_id(), sound.timeline_in, sound.timeline_out, sound.linked_clip_id), (Some(other), tc(4.0), tc(10.0), Some(b)), "its sound with it, linked to the slot");
        assert_eq!((clip(&seq, c).timeline_in, clip(&seq, c).timeline_out), (tc(10.0), tc(14.0)), "the clip after it moved");
        let c_sound = clip(&seq, c).linked_clip_id.unwrap();
        assert_eq!(clip(&seq, c_sound).timeline_in, tc(10.0), "and its sound");
        assert_eq!(seq.tracks[1].clips.len(), 3, "one sound clip for the slot, not two");

        // An edit to the take while it is in the slot.
        let at = seq.tracks[0].clips.iter().position(|x| x.id == b).unwrap();
        seq.tracks[0].clips[at].volume = 0.5;
        let wide = timeline(&seq);

        let (_, back) = run(&mut seq, &pool, switch("Take 1", true)).unwrap();
        assert_eq!(timeline(&seq), start, "switched back: everything as it was, the slot's own sound clip too");
        let (_, again) = run(&mut seq, &pool, switch("wide", true)).unwrap();
        assert_eq!(timeline(&seq), wide, "the wide take as it was left, its edit kept");

        undo(&mut seq, &pool, &again);
        assert_eq!(timeline(&seq), start, "undo of a switch");
        undo(&mut seq, &pool, &back);
        assert_eq!(timeline(&seq), wide);
        let at = seq.tracks[0].clips.iter().position(|x| x.id == b).unwrap();
        seq.tracks[0].clips[at].volume = 1.0;
        undo(&mut seq, &pool, &to_wide);
        assert_eq!(timeline(&seq), start);
        undo(&mut seq, &pool, &made);
        assert!(clip(&seq, b).audition.is_none(), "undo of the create");
    }

    /// #3209: on the primary storyline a switch moves the clips after it
    /// without being asked, and a clip connected to one of them moves with
    /// it; a clip connected to the slot keeps its anchor.
    #[test]
    fn a_switch_on_the_primary_storyline_carries_connected_clips() {
        let (mut seq, mut pool, [_, b, c]) = fixture();
        seq.tracks[0].magnetic_role = TrackRole::Primary;
        let other = media(&mut pool, "/tmp/other.mov", 10.0);
        let mut lane = Track::new("V2", TrackKind::Video);
        lane.magnetic_role = TrackRole::Connected;
        let m = clip(&seq, b).media_id().unwrap();
        let mut on_c = Clip::media(m, tc(9.0), tc(10.0), tc(0.0), tc(1.0));
        on_c.magnetic_anchor = Some(MagneticAnchor { anchor_clip_id: c, offset: tc(1.0), alignment: AnchorAlignment::StartAligned });
        let mut on_b = Clip::media(m, tc(5.0), tc(6.0), tc(0.0), tc(1.0));
        on_b.magnetic_anchor = Some(MagneticAnchor { anchor_clip_id: b, offset: tc(1.0), alignment: AnchorAlignment::StartAligned });
        let (on_c_id, on_b_id) = (on_c.id, on_b.id);
        lane.clips = vec![on_b, on_c];
        seq.tracks.push(lane);
        let create = alt(AlternativesAction::Create, b, vec![TakeSpec { media_id: Some(other), from: Some(0.0), to: Some(5.0), name: None }], None, None, false);
        run(&mut seq, &pool, create).unwrap();
        let (what, _) = run(&mut seq, &pool, alt(AlternativesAction::Switch, b, Vec::new(), Some("Take 2"), None, false)).unwrap();
        assert!(what.contains("1 connected clip(s) with them"), "{what}");
        assert_eq!(clip(&seq, c).timeline_in, tc(9.0), "rippled on the primary storyline without ripple given");
        assert_eq!(clip(&seq, on_c_id).timeline_in, tc(10.0), "the clip connected to it with it");
        assert_eq!(clip(&seq, on_b_id).timeline_in, tc(5.0), "the clip connected to the slot keeps its place");
    }

    /// #3209: takes are added, renamed and removed by name or id; the take
    /// in the slot isn't removed; finalize leaves the take in the slot a
    /// plain clip.
    #[test]
    fn takes_are_edited_and_the_slot_finalized() {
        let (mut seq, pool, [a, ..]) = fixture();
        assert!(run(&mut seq, &pool, op(AlternativesAction::Add, a)).unwrap_err().why.contains("not an audition"));
        run(&mut seq, &pool, op(AlternativesAction::Create, a)).unwrap();
        assert!(run(&mut seq, &pool, op(AlternativesAction::Create, a)).unwrap_err().why.contains("already holds 2 takes"));
        run(&mut seq, &pool, alt(AlternativesAction::Add, a, vec![TakeSpec { name: Some("Slow".into()), ..TakeSpec::default() }], None, None, false)).unwrap();
        let id = clip(&seq, a).audition.as_ref().unwrap().find("slow").unwrap().id.to_string();
        run(&mut seq, &pool, alt(AlternativesAction::Rename, a, Vec::new(), Some(&id), Some("Slower"), false)).unwrap();
        let clash = run(&mut seq, &pool, alt(AlternativesAction::Rename, a, Vec::new(), Some("Slower"), Some("take 1"), false)).unwrap_err();
        assert!(clash.why.contains("another take is named"), "{}", clash.why);
        let active = run(&mut seq, &pool, alt(AlternativesAction::Remove, a, Vec::new(), Some("Take 1"), None, false)).unwrap_err();
        assert!(active.why.contains("the take in the slot"), "{}", active.why);
        let missing = run(&mut seq, &pool, alt(AlternativesAction::Remove, a, Vec::new(), Some("Nope"), None, false)).unwrap_err();
        assert!(missing.hint.contains("Take 1") && missing.hint.contains("Slower"), "names the takes there are: {}", missing.hint);
        run(&mut seq, &pool, alt(AlternativesAction::Remove, a, Vec::new(), Some("Slower"), None, false)).unwrap();
        assert_eq!(clip(&seq, a).audition.as_ref().unwrap().takes.len(), 2);
        let before = timeline(&seq);
        let (what, _) = run(&mut seq, &pool, op(AlternativesAction::Finalize, a)).unwrap();
        assert!(what.contains("as Take 1, 1 other take(s) dropped"), "{what}");
        assert!(clip(&seq, a).audition.is_none());
        assert_eq!(timeline(&seq), before, "the take in the slot stays as it was");
    }

    /// #3209: the check notes an audition left with its takes, naming the
    /// take that is exported; finalized, it is gone.
    #[test]
    fn the_check_notes_an_audition_left_with_its_takes() {
        let (mut seq, pool, [a, ..]) = fixture();
        let notes = |seq: &Sequence| crate::qc::check(seq, &pool, &crate::ops::NoTranscripts).findings.into_iter().filter(|f| f.rule == "audition").collect::<Vec<_>>();
        assert!(notes(&seq).is_empty());
        run(&mut seq, &pool, op(AlternativesAction::Create, a)).unwrap();
        let found = notes(&seq);
        assert_eq!(found.len(), 1);
        assert!(found[0].severity == crate::qc::Severity::Note && found[0].what.contains("2 takes") && found[0].what.contains("Take 1"), "{}", found[0].what);
        run(&mut seq, &pool, op(AlternativesAction::Finalize, a)).unwrap();
        assert!(notes(&seq).is_empty());
    }

    /// #3209: what renders, and so what exports, is the take in the slot,
    /// on a sequence of its own and inside a compound clip: a red card
    /// with a blue take draws red, and switched, blue.
    #[test]
    fn the_take_in_the_slot_is_what_renders() {
        struct Nothing;
        impl klipp_compose::FrameSource for Nothing {
            fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<klipp_compose::SourceFrame> {
                None
            }
        }
        let card = |rgb: [u8; 3]| Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [rgb[0], rgb[1], rgb[2], 255] }), source_in: tc(0.0), source_out: tc(2.0), ..Clip::gap(tc(0.0), tc(2.0)) };
        let mut inner = Sequence::new_default();
        inner.fps = Fps::new(25, 1);
        let mut red = card([255, 0, 0]);
        let blue = card([0, 0, 255]);
        let first = Take { id: TakeId::new(), name: "Red".into(), clip: None, sound: Vec::new() };
        red.audition = Some(Box::new(Audition { active: first.id, takes: vec![first, Take { id: TakeId::new(), name: "Blue".into(), clip: Some(Box::new(blue)), sound: Vec::new() }] }));
        let slot = red.id;
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![red];
        inner.tracks = vec![v1];
        let mut outer = Sequence::new_default();
        outer.fps = inner.fps;
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![Clip { kind: ClipKind::Sequence { sequence_id: inner.id }, ..Clip::gap(tc(0.0), tc(2.0)) }];
        outer.tracks = vec![v1];
        let centre = |seq: &Sequence, all: &[Sequence]| {
            let flat = klipp_timeline::flatten::flatten_sequence(seq, all);
            let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
            r.set_gpu_enabled(false);
            let px = r.render(&flat, tc(1.0), 32, 18, &mut Nothing, klipp_compose::RenderOptions::default());
            let i = (9 * 32 + 16) * 4;
            [px[i], px[i + 1], px[i + 2]]
        };
        let reddish = |p: [f32; 3]| p[0] > 0.5 && p[2] < 0.1;
        let blueish = |p: [f32; 3]| p[2] > 0.5 && p[0] < 0.1;
        assert!(reddish(centre(&inner, std::slice::from_ref(&inner))), "the red take, on its own");
        assert!(reddish(centre(&outer, &[outer.clone(), inner.clone()])), "and in the compound");
        let pool = MediaPool::new();
        run(&mut inner, &pool, alt(AlternativesAction::Switch, slot, Vec::new(), Some("Blue"), None, false)).unwrap();
        assert!(blueish(centre(&inner, std::slice::from_ref(&inner))), "switched: blue, on its own");
        assert!(blueish(centre(&outer, &[outer.clone(), inner.clone()])), "and in the compound");
    }
}
