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

## Ops

- `place {media_id, track, at, source_in?, source_out?}`: a media item
  onto a track (`"video"`, `"audio"`, `"text"` for the first of that
  kind, or a track index from the timeline view) at an anchor.
- `trim {clip_id, edge, to}`: the in or out edge to an anchor; the
  other edge stays; the source moves with the edge.
- `split {clip_id, at}`: into two.
- `remove {clip_id, ripple?}`: off the track; with ripple, later clips
  close the gap.
- `move {clip_id, to}`: the clip's start to an anchor; a linked audio
  or video partner goes with it.

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
