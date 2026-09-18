# Working in Klipp as an agent

Klipp is a video editor. Through these tools you read what is in the
footage, plan an edit in editorial terms, apply it as one undo step,
look at what you did, and deliver a file. You never need a person to
click for you, and you never need to do frame arithmetic: you name
clips and media by id and places by anchors, and Klipp resolves the
frames.

## The loop

0. **Orient.** `klipp_orient` first: where you are in one call — the
   build, the project, the brief, the footage and its analysis, the
   sequence, and what to do next. Call it again whenever you lose the
   thread.
0. **The brief.** `klipp_brief` says what the piece is for: its
   length, platform, loudness, whether it is captioned, and the cast
   with their names and roles. If there is none, ask the user what it
   should say and set it with `klipp_set_brief`. Names on screen come
   from its cast list, the check holds the piece to it, and an export
   takes its size and loudness from it.
1. **Read.** `klipp_status` for the build, the revision and the
   playhead. `klipp_timeline` for the tracks and clips with their
   ranges in seconds and frames, each clip's id, and what analysis
   knows about it in brief. `klipp_list_media` for the pool.
2. **Learn the footage.** `klipp_analyze` once (it also runs on import
   and on project open), then `klipp_media_analysis` for a file's
   shots, motion, loudness, silences and transcript;
   `klipp_search_transcripts` to find where something is said;
   `klipp_find_shots` to find shots by framing, people, objects,
   motion, speech or length. `klipp_transcript` for a stretch of the
   speech with every word's time and the measured silences: cut on a
   word's edge or, better, in a silence. `klipp_analysis_status` says
   which analyser is at work and for how long, and which models are
   installed; a transcript takes about a third of the file's length. `klipp_contact_sheet` to see a range with
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
6. **Check.** `klipp_check` reads the finished timeline the way an
   editor watches it once: black frames, flashes, jump cuts with
   nothing over them, titles across a cut, sound cut inside a word,
   music at full level under speech, and the brief. Fix the errors;
   decide about the warnings, and say which you kept and why.
7. **Deliver.** `klipp_export` to a file, `klipp_export_status` until
   it is complete, `klipp_export_subtitles` for captions.
8. **Save.** `klipp_rename_sequence` so the timeline is called what
   the piece is, then `klipp_save_project` with a path the first time,
   without one after. The user carries on from your project, and an unsaved one
   is gone when the app closes. `klipp_status` says where the project
   is saved and whether it has unsaved changes; `klipp_open_project`
   opens another, and will not throw unsaved work away unless told to.

## Units and ids

- Times are seconds from the start of the timeline, or frames at the
  sequence's rate. Never ticks.
- Every clip, track and media item has an id; the views show them.
  A split makes two new clips with new ids, reported in the reply.
- Every write returns the document revision after it. If you hold a
  plan and the revision moved, plan again.

## Anchors

`{"secs": 12.5}`, `{"frame": 300}`, `{"clip_start": clip}`,
`{"clip_end": clip}`, `{"track_end": "video"}` (where the last clip on
a track ends, which is where an assembly's next select goes),
`{"marker": "Intro"}`, or `{"offset": {"from": anchor, "secs": -0.5}}`.

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
  becomes track 0 and moves every other index down by one; an audio or
  subtitle track at the bottom.
- `captions {track?, from?, to?, max_chars?, max_lines?, replace?}`: captions for
  what is heard on the timeline, from the transcripts. Each word lands
  where its clip puts it; a cue closes at a sentence, a pause or a cut;
  two lines of 42 characters unless you say otherwise. They go on the
  first subtitle track, made at the bottom if there is none. The reply
  lists every cue: read them, and correct one with `set_text`. Do it
  last, once the speech is where it will stay: a trim afterwards does
  not move them, and `replace: true` makes them again. They say what the transcript says, mishearings and
  all: read them, and correct a cue with `set_text`.
- `audio {clip_id, volume_db?, gain_db?, pan?, fade_in?, fade_out?}`:
  a clip's sound. Volume and pre-fader gain in decibels, pan from -1
  (left) to 1, fades in seconds; what you leave out stays as it is.
  Name the clip you mean: for an audio clip linked to a video clip the
  settings are kept on the video clip, and the op puts them there.
- `duck {clip_id, by_db?, fade_secs?}`: music under speech. The clip's
  volume comes down, nine decibels unless you say otherwise, wherever
  another clip is heard over it, and moves there and back over half a
  second. Set the music's own volume first and place the speech first:
  the duck is written as keyframes from what is on the timeline then.
- `style {clip_id | track, from?, to?, size?, color?, font?, bold?, align?, x?, y?, backing?, outline?}`:
  the look and place of one title, or of every title on a track
  (`"subtitle"` for all the captions), between `from` and `to` when
  given. `size` is pixels of a 1080-line frame, `color` is `#rrggbb`,
  `x` and `y` are where the text's centre goes (0 to 1 of the frame),
  `backing` is how dark a panel sits behind the text (0 none, 1 black),
  `outline` a width in pixels. Lift the captions with it for as long as
  a lower third is up, so the two do not share the bottom of the frame.
- `lower_third {name, role?, at, secs?, track?, side?, x?, y?}`: a name
  and a role bottom left in title-safe, four seconds unless you say
  otherwise, fading in and out, on a text track with room (one is made
  on top if none has, which moves every other index down by one).
  Refused across a shot change, and the refusal names the cut. Look at
  the frame first: where the speaker is on the right, or the picture
  has a name of its own burnt in down there (a split-screen podcast),
  `side: "right"` sets it against the right edge, and `x` (that edge)
  and `y` (the name's line) place it, as fractions of the frame inside
  title-safe. A name under the wrong person is worse than none.
- `set_text {clip_id, text}`: the words of a title or a caption; its
  look, place and timing stay. For the cue the transcript misheard and
  the name it cannot spell: read the captions once they are made
  (`klipp_timeline`) and put right what is wrong.
- `trim {clip_id, edge, to, ripple?, linked?}`: the in or out edge to an
  anchor; the other edge stays; the source moves with the edge. The
  clip it is linked to, a picture's sound, is trimmed with it by the
  same amount, so they stay in step and a J-cut keeps its lead;
  `linked: false` moves the one clip alone. With `ripple`,
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
