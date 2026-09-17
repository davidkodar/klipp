# Working in Klipp as an agent

Klipp is a video editor. Through these tools you read what is in the
footage, plan an edit in editorial terms, apply it as one undo step,
look at what you did, and deliver a file. You never need a person to
click for you, and you never need to do frame arithmetic: you name
clips and media by id and places by anchors, and Klipp resolves the
frames.

## The loop

1. **Read.** `klipp_status` for the build, the revision and the
   playhead. `klipp_timeline` for the tracks and clips with their
   ranges in seconds and frames, each clip's id, and what analysis
   knows about it in brief. `klipp_list_media` for the pool.
2. **Learn the footage.** `klipp_analyze` once (it also runs on import
   and on project open), then `klipp_media_analysis` for a file's
   shots, motion, loudness, silences and transcript;
   `klipp_search_transcripts` to find where something is said;
   `klipp_find_shots` to find shots by framing, people, objects,
   motion, speech or length. `klipp_contact_sheet` to see a range with
   times burnt in; `klipp_audio_excerpt` to hear one.
3. **Plan.** `klipp_plan` with a list of ops: a dry run that says what
   each step would do and each touched clip's range afterwards.
   Nothing changes. If a step cannot be done the reply says which,
   why, and what would help; fix the ops and plan again.
4. **Apply.** `klipp_apply` with the same ops: resolved again, applied
   as one undo step, refused whole if the timeline changed since you
   planned. `klipp_undo` takes the whole action back.
5. **Look.** `klipp_frame_at` for the frame at a time (the playhead
   comes back where it was), `klipp_screenshot` for the viewer,
   `klipp_contact_sheet` over the range you cut.
6. **Deliver.** `klipp_export` to a file, `klipp_export_status` until
   it is complete, `klipp_export_subtitles` for captions.

## Units and ids

- Times are seconds from the start of the timeline, or frames at the
  sequence's rate. Never ticks.
- Every clip, track and media item has an id; the views show them.
  A split makes two new clips with new ids, reported in the reply.
- Every write returns the document revision after it. If you hold a
  plan and the revision moved, plan again.

## Anchors

`{"secs": 12.5}`, `{"frame": 300}`, `{"clip_start": id}`,
`{"clip_end": id}`, `{"marker": "Intro"}`, or
`{"offset": {"from": anchor, "secs": -0.5}}`.

A clip is named by its id string, or by where it is:
`{"track": 1, "secs": 30.0}` is the clip covering that time on that
track as the plan stands at that step. That is how a later step names
the half a split made or the clip a place put down, whose ids do not
exist until the plan runs.

## Ops

- `place {media_id, track, at, source_in?, source_out?, insert?, with_audio?}`:
  a media item onto a track (`"video"`, `"audio"`, `"text"` for the
  first of that kind, or a track index from the timeline view) at an
  anchor. Its sound comes along, linked, on the first audio track,
  which is made if there is none; `with_audio: false` leaves it out
  and mutes the clip, which is what B-roll over a voice wants. An empty sequence takes the
  first picture's size and rate. With `insert`, every unlocked track is
  rippled later to make room and a clip straddling the anchor is split
  there.
- `add_track {kind, name?}`: a video or text track on top, which
  becomes track 0 and moves every other index down by one; an audio
  track at the bottom.
- `trim {clip_id, edge, to, ripple?}`: the in or out edge to an anchor;
  the other edge stays; the source moves with the edge. With `ripple`,
  the later clips on the track close up by the change; a head trim
  then keeps the clip's start and shortens it from the front.
- `split {clip_id, at}`: into two, the linked sound with it.
- `remove {clip_id, ripple?}`: off the track; with ripple, later clips
  close the gap.
- `move {clip_id, to}`: the clip's start to an anchor; a linked audio
  or video partner goes with it.
- `j_cut {incoming_clip, frames}`: the incoming clip's sound starts so
  many frames before its picture. `l_cut {outgoing_clip, frames}`: the
  outgoing clip's sound stays so many frames past its picture. Both
  need the clips' linked audio, adjacent on one audio track.

Ops run in order and each sees what the ones before did. Overlaps,
locked tracks, edges past the media and cuts outside a clip are refused.

## Batches

Several tool calls that are one action in your plan go in
`klipp_batch`, one undo step; grade parameters, appends and removes.
Plans are batches of their own.

## Manners

- Analysis on a long file takes a while; ask, do something else, then
  read it.
- Prefer the plan's refusal to guessing a time.
- Do not download models without saying so; `klipp_analyze` with
  `download_models` fetches them once.
- Leave the user's markers, favourites and rejects alone; the ones
  analysis makes are its own kind.
