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
   shots, motion, loudness (with the spectral bands that reveal wind,
   rumble, hum and hiss), silences and transcript;
   `klipp_search_transcripts` to find where something is said;
   `klipp_find_shots` to find shots by framing, people, objects,
   motion, speech or length. `klipp_transcript` for a stretch of the
   speech with every word's time and the measured silences: cut on a
   word's edge or, better, in a silence. `klipp_cut_points` goes
   further: it ranks the clean places to cut — the pauses first, then
   sentence ends and shot changes, then phrase breaks — and, given a
   `near_secs`, says whether that moment is clean, mid-sentence or
   mid-word and where the nearest clean point is. Use it before you
   trim or place a spoken clip, so a cut never lands through a word or
   a sentence. `klipp_selects` goes one further for a long take: it
   splits the speech into the runs between real pauses, marks each keep
   or drop (filler and dead air are drops), and reports how much is worth
   keeping — build a tight cut by placing the keep ranges in order.
   `klipp_beats` reads a clip's tempo and beat grid, so cuts can land on
   the beat of your music. `klipp_style_profile` reads a reference
   video's whole editing style — its cut cadence and whether it cuts on
   the beat, its tempo, its look off the picture, its motion — so you can
   build a clip in the same feel from your own footage, and your own or
   licensed music, never the reference's track.
   `klipp_job_status` says which analyser is at work and for how long;
   `klipp_analysis_status` also says which models are
   installed; a transcript takes about a third of the file's length. `klipp_contact_sheet` to see a range with
   times burnt in; `klipp_audio_excerpt` to hear one.
   Import and multicam sync also return a job ID. Import completes when
   the media enters the pool; its result gives the media ID and, if asked,
   the analysis job ID to follow next. Sync completes when the new group
   appears. A queued analysis job or a named render can be stopped with
   `klipp_cancel_job`; check job status before reporting cancellation.
3. **Plan.** `klipp_plan` with a list of ops: a dry run that says what
   each step would do and each touched clip's range afterwards.
   Nothing changes. If a step cannot be done the reply says which,
   why, and what would help; fix the ops and plan again.
   For a requested cut, use `klipp_prepare_cut` on a short timeline
   range first. It maps measured pauses, words, picture changes and
   settled screen frames through trims and speed changes to exact
   sequence frames. Choose from its evidence; if it says ambiguous,
   inspect the suggested contact sheet or audio excerpt. Carry its
   revision and project identity into plan and apply.
4. **Apply.** `klipp_apply` with the same ops and the plan's
   `expected_revision` (and `expected_project_identity` when available): resolved again,
   applied as one undo step, refused whole if the timeline or project
   changed since you planned. Give the call an `idempotency_key` you can
   reuse for the exact same request. If Klipp reports `applied: false`
   after the UI wait times out, the cut is still queued: use its
   `operation_id` with `klipp_operation_status` until it says `applied`
   or `refused`. Never send the same cut with a new key while it is
   queued. Klipp also caches an error reply for that key for up to one
   hour; after an error, inspect the project and use a new key only for
   a deliberate new attempt. Outcomes and keys are kept only while Klipp
   runs: after it restarts, an earlier operation id is answered as from
   before the restart, and an old key no longer stops a retry, so read
   the timeline to see whether that edit landed before sending it
   again. A project identity from before a restart never matches the
   project after it. `klipp_undo` takes the whole action back.
   A read sent alongside a write may see the timeline before that write;
   read again after the write's reply before using its revision or clip IDs.
5. **Look.** `klipp_frame_at` for the frame at a time (the playhead
   comes back where it was), `klipp_screenshot` for the viewer,
   `klipp_contact_sheet` over the range you cut. `klipp_frame_stats`
   for the scope readings a colourist grades to: the black, mid and
   white points, the balance, the clipping, the skin tones. Grade to
   those numbers, and match a shot to its neighbours by them.
   `klipp_scope` draws the actual scope of a frame — waveform, parade,
   vectorscope or histogram — as an image, GPU-drawn the way the app's
   own scopes are, without disturbing the viewer's; the numbers guide the
   grade, the picture confirms it.
   `klipp_match_grade` does that match in one call: it reads both shots
   and sets the target's exposure, lift, gain, temperature and tint to
   bring it toward a reference shot — the continuity grade first, then
   the look. Verify with `klipp_check color` and refine.
6. **Check.** `klipp_check` reads the finished timeline the way an
   editor watches it once: black frames, flashes, jump cuts with
   nothing over them, titles across a cut, sound cut inside a word,
   music at full level under speech, and the brief. Fix the errors;
   decide about the warnings, and say which you kept and why. Pass
   `color:true` to also read a frame of every shot through the scopes,
   for clipped highlights, crushed blacks, a dull or over-saturated
   picture, and skin off the skin-tone line — so the grade is checked to
   the numbers, not by eye.
   Check always reads the sound faults too — wind, rumble, mains hum,
   hiss — measured from the analysis bands, and names the `clean_audio`
   flag that fixes each.
7. **Deliver.** `klipp_export` to a file, then follow its job ID with
   `klipp_job_status` or `klipp_follow_job`. The job reports the render
   percentage, phase, frames done and total, ETA, failure or output path.
   With an MCP progress token, `klipp_follow_job` sends throttled progress
   notifications as it runs; without one, it returns after a status change
   so you can tell the person what happened and call again. `klipp_status`
   also shows active work. Read the terminal state before saying the file
   is ready. `klipp_export_subtitles` writes caption sidecars.
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
- Every write returns the document revision after it. Pass the revision
  from a plan to apply, with its project identity when available; if
  either moved, plan again.
- On MCP 2025-06-18, status, timeline, plan and apply also return their
  exact fields in `structuredContent`; use those fields for revisions,
  frames, clip ids and write status. Older clients receive readable text.
  The tool catalog gives short descriptions; this guide is the full op
  reference. Common edit ops reject misspelled fields and malformed
  anchors before sending anything to Klipp.

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
- `remove_track {track, with_clips?}`: a track taken out, by index or
  kind; every track below moves up by one. Refused while it holds clips
  unless `with_clips`, which takes them too and unlinks anything on
  another track from them. One undo step puts it back.
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
  It comes down for speech only: not for clips marked music, sfx,
  ambience, foley or broll, nor for any set twelve decibels or more
  under the music (a camera's street sound under a bed). A clip whose
  file is transcribed is speech where its words are, not along its
  whole length, and a lone word the transcriber was unsure of is not
  speech; a clip marked voiceover is speech throughout.
- `role {clip_id, role}`: what a clip is to the mix and the checks:
  dialogue, voiceover, music, sfx, ambience, foley, broll or graphics.
  Mark a camera's own sound `ambience` (or the shot `broll`) when it
  runs under music, so a `duck` does not take it for speech; narration
  is marked dialogue when `narrate` places it.
- `matte {clip_id, source?, channel?, invert?, feather_px?, choke_px?, show_source?, clear?}`:
  the clip seen only through another, a track matte. Without
  `source`, the clip above it at its time, on a picture or text track.
  `channel` is `auto` (a title's shape, anything else its brightness),
  `alpha` or `luma`; `invert` shows it everywhere but there; the
  source is hidden unless `show_source`; `clear` takes it off. The
  common one, footage inside a word, is two ops in one apply: a
  `title` over the clip, big and heavy (`size` up to 1000, a word
  that fills the frame wants 700 to 1000 at 1080p, `font` "Bebas Neue"
  or `bold`) with `over_faces: true`, since a window may cover a face
  where a caption must not, then `matte` on the clip by its id. Anything below the
  clip shows around the letters; nothing there is black.
- `volume {clip_id, keys: [{at, db}]}`: a clip's volume drawn by hand,
  levels in decibels from the clip's own level (0 as set, -9 a duck,
  -60 near silence) at timeline
  seconds with straight moves between. It replaces the clip's volume
  keyframes. For the dip `duck` cannot find, or a swell into a moment.
- `grade {clip_id, exposure?, contrast?, saturation?, temperature?, tint?, lift?, gamma?, gain?, highlights?, shadows?, reset?}`:
  a picture clip's primary grade. What you give is set, the rest stays.
  Every knob is 0 at neutral, except `saturation` and `contrast`, whose
  neutral is 1. `exposure` is in stops; `temperature` and `tint` are the
  white balance; `lift`, `gamma`, `gain` move the shadows, mids and
  highlights; `reset` takes the clip to neutral first. Match shots to
  each other before making any of them look better.
- `effect {clip_id, kind, amount?}`: a finishing effect on a picture
  clip. `amount` is 0 to 1; a second of the same kind replaces the first.
  Kinds: `vignette`, `grain` (`film_grain`), `glow`, `bloom`, `diffusion`
  (`pro_mist`), `halation` (`film_halation`), `light_rays` (`god_rays`),
  `lens_flare`, `anamorphic` (`anamorphic_streak`, `streak`), `light_wrap`,
  `blur`, `radial_blur` (`zoom_blur`), `spin_blur`, `directional_blur`
  (`motion_blur`), `sharpen`, `chromatic_aberration` (`rgb_split`,
  `fringe`), `tilt_shift`, `mosaic` (`pixelate`), `posterize`, `invert`,
  `mirror`, `lens_distortion` (`barrel`), `emboss`, `edge_detect`
  (`edges`), `denoise`, `vhs`, and `old_film`. `corners`
  (`rounded_corners`) rounds a layer's alpha mask, `shadow`
  (`drop_shadow`) places a shadow behind it, and `outer_glow` lights its
  silhouette; compose the picture smaller than the frame to see these.
  For example, `{op: effect, clip_id: ID, kind: corners, amount: 0.4}`.
  `fill_bars` (`blanking_fill`, `blur_fill`, `blurred_background`) fills
  the black bars round a picture that does not fill the frame (an
  upright phone shot in a wide sequence, a wide one in a vertical
  sequence) with the picture itself, enlarged and blurred, so the frame
  is full and the whole picture still shows; more `amount` is softer
  and darker. Put it on every such clip when asked to fill the bars.
- `stabilize {clip_id, smoothing?}`: steady a shaky clip; it crops the
  edges a little to hide the movement.
- `retime {clip_id, speed, ripple?}`: change a clip's speed, its linked
  sound with it. Above 1 is faster and shorter, below 1 slower and
  longer, negative plays it reversed. Its sound plays at the speed, its
  pitch moving with it as tape's does, backward when reversed; music and
  other tracks are untouched. (A speed ramp's own sound is silent.)
  With `ripple` the clips after it on its tracks move by the change in
  length; without it a gap is left where it shrinks, and a clip that
  would grow into the next one is refused.
- `reframe {clip_id, scale?, x?, y?}`: punch in and reposition a
  picture. `scale` zooms (1 as shot, up to 4); `x` and `y` pan the frame
  from -1 to 1. Use it to salvage a wide or badly-framed shot, to fit a
  vertical delivery, or to set a slow push. It sits before other
  effects, so a vignette or grade after it sees the reframed picture.
- `qualify {clip_id, target?|hue?, …, exposure?, saturation?, hue_shift?,
  temperature?}`: a secondary. Key a colour or tone — a named `target`
  (skin, sky, foliage, reds, blues…) or an explicit `hue` in degrees,
  narrowed by `hue_width` and saturation/luminance windows — and correct
  only what is keyed. `invert` grades everything but the key; `remove`
  clears it. Fix a sky, calm a loud shirt, keep skin warm while cooling
  the rest. Grade the primary first, then reach for a secondary.
- `look` (the `klipp_look` tool): put a creative look — a bundled 3D
  LUT — on a picture clip, as one node after the grade so it sits on the
  corrected image. `name` is a look from `klipp_looks` (category/name, or
  any words that pick one out); `opacity` fades it in; `remove` clears
  it. Grade for exposure and continuity first, then a look for style.
  `klipp_looks` lists the library: cinematic and film emulations.
- `search_music` / `search_sfx` / `add_music` (the `klipp_search_music`,
  `klipp_search_sfx`, `klipp_add_music` tools): find royalty-free music
  (Jamendo, by mood or genre) and sound effects (Freesound, by keyword)
  and add either to the pool. Each result shows its licence and whether
  it clears commercial use; preview before you commit, and the credit is
  kept beside the file for your export notes. Match the reference's energy
  with legally-usable music, never its own track; reach for effects on
  transitions, cuts and atmosphere.
- `clean_audio {clip_id, voice?, reduction_db?, wind?, hiss?, hum?, deess?}`:
  clean a clip's sound. Each flag adds the right tool. `voice` is Voice
  Isolation, an AI model that separates speech from wind, traffic and
  room noise; it is the best cleanup wherever someone speaks, and covers
  wind and hiss too. `reduction_db` sets how far the rest goes down:
  12 keeps the place audible, 24 (the default) is broadcast-clean, 60
  removes all it can. Leave `voice` off where nobody speaks, or it takes
  the ambience with the noise; there `wind` (gusts and rumble) and
  `hiss` do the job. `hum` removes mains buzz and `deess` harsh
  sibilance. The presenter on a beach in a gale wants `voice`; the
  empty beach itself wants `wind`. Voice Isolation's model downloads
  only when the user asks; the reply says when it is missing.
- `align_audio` (the `klipp_align_audio` tool): line a clip's sound up
  with a reference clip's — dual-system sound, a separate recorder
  against the camera's scratch track. It cross-correlates the two,
  reports the match confidence, and moves the clip into sync; it refuses
  when the two share no common sound.
- `transition {clip_id, kind?, secs?}`: a transition between a clip and
  the one after it, `secs` long, on the cut. `cross_dissolve` is the
  default and the only one an interview or a documentary wants; a
  dissolve marks a passage of time or place. `dip_to_black` and
  `dip_to_white` mark a break. A stylised kind (`zoom`, `whip`,
  `glitch`, `film_burn`) is a style choice the brief has to ask for.
  Both clips need footage beyond the cut to overlap.
- `style {clip_id | track, from?, to?, size?, color?, font?, bold?, align?, x?, y?, backing?, outline?, shadow?}`:
  the look and place of one title, or of every title on a track
  (`"subtitle"` for all the captions), between `from` and `to` when
  given. `size` is pixels of a 1080-line frame, `color` is `#rrggbb`,
  `x` and `y` are where the text's centre goes (0 to 1 of the frame),
  `backing` is how dark a panel sits behind the text (0 none, 1 black),
  `outline` a width in pixels, `shadow {x?, y?, blur?, color?, opacity?}`
  a soft drop shadow behind the words: offset and blur in pixels of a
  1080-line frame (0, 4 and 8 when left out), `#rrggbb`, opacity 0 to 1
  (0.6), and opacity 0 takes it off. Bright text over a bright sky or
  sand wants the shadow; a dark glow barely shows. Lift the captions with it for as long as
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
- `title {text, at, secs?, style?, track?, x?, y?, size?, color?, font?, bold?, align?, backing?, backing_color?, full_frame?, outline?, outline_color?, glow?, glow_color?, gradient?, animate?, animate_secs?, stagger?, loop?, glitch?, chromatic?, enter?, exit?, over_faces?, shadow?}`:
  a title from scratch: a heading, a callout or a hero card, on a text
  track (one is made on top if left out). `style` names a look, one of
  the brief's type styles or `heading`, `callout` or `card`, and its
  fields fill every one left out here. `x`, `y`, `size`, `color`,
  `backing`, `outline` and `shadow` read as in `style`; `full_frame` is a solid
  `#rrggbb` card behind everything, `gradient` a two-colour fill
  (`#top,#bottom`). `animate` is the entrance (fade, rise, drop,
  slide_left, slide_right, scale_up, scale_down, blur, mask_rise,
  mask_drop, typewriter) and `loop` a continuous effect (pulse, flash,
  neon, wave, shake, color_cycle). `size` is pixels at 1080 on the
  frame's short side, so a size and the built-in styles read the same
  on a vertical frame as on a wide one, and the whole title is kept
  inside title-safe; a word too wide for the frame shrinks the line
  rather than breaking. Look at the frame all the same.
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
- `slip {clip_id, secs}`: what a clip shows moved, not the clip: its
  source in and out both by `secs`, later in the file for positive, the
  timeline untouched, its grade and id kept. Its linked sound slips by as
  much through its own range, so a J or L cut stays one. Refused past
  either end of the file, the sound's included.
- `slide {clip_id, secs}`: the clip moved along the timeline between its
  neighbours, its picture and length unchanged: the clip before it gets
  `secs` longer (later for positive) and the clip after it that much
  shorter, so nothing else moves. Its sound and theirs go with it; one
  undo step. Refused where a neighbour would run out of its file or of
  its length; a clip touching nothing either side is moved, not slid.
- `join {clip_id, side?}`: a split undone later, or a scene cut found
  where the shot did not change: the clip and the piece of the same shot
  on its `side` ("next", the default, or "previous") made one again, its
  sound with it, when that piece is the same file at the same speed and
  the source runs on. Name the edit: after a scene split every edit of a
  film runs on in its file, so join the clip either side of the false
  cut, not any clip. The first piece's grade and effects carry the whole.
- `remove {clip_id, ripple?, linked?}`: off the track, and its linked
  sound (or picture) with it unless `linked` is false; with ripple,
  later clips close the gap on each track. A ripple is refused when
  the linked pair's timeline ranges differ (a J or L cut) or the
  partner's track is locked, since the tracks would close up by
  different amounts; lift it with `ripple: false` instead.
- `move {clip_id, to}`: the clip's start to an anchor; a linked audio
  or video partner goes with it.
- `j_cut {incoming_clip, frames}`: the incoming clip's sound starts so
  many frames before its picture.
- `l_cut {outgoing_clip, frames}`: the outgoing clip's sound stays so
  many frames past its picture. Both J and L cuts need linked audio,
  adjacent on one audio track.
- `cut {from, to}`: remove that stretch from every unlocked track,
  splitting clips and their linked halves at the edges; move everything
  after it earlier by the removed length. This is a text-based delete.
  It is refused through a retimed clip.
- `format {width, height, fps?}`: set the sequence's picture size and
  optionally its rate. Fit the picture rather than stretching it.
- `redact {clip_id, x, y, w, h, from?, to?, kind?, amount?}`: hide a
  patch of picture for a stretch. Coordinates are fractions of the
  frame from the top left, as `klipp_screen_text` reports them. The
  default kind is `mosaic`; `blur` is also available. The range
  defaults to the clip edges; `klipp_auto_redact` handles all findings.
- `grade_group {group, clips:[clip_id…], leave?}`: put clips in a named
  group, or take them out with `leave`. Group grades run before (`pre`)
  or after (`post`) each clip's own grade. `grade` and `look` can target
  `{group, stage}` instead of a clip, or `"timeline"` after all clips.
  Use one group look for a scene.
- `remove_grade_group {group}`: end a group and leave its clips' own
  grades in place.
- `zoom {clip_id, from?, to, x?, y?}`: animate a zoom over the clip,
  from one scale to another (1 as shot, up to 4). `x,y` can move the
  focus toward a frame point (-1 to 1), for a Ken Burns move on a
  still. It renders on the GPU.
- `pan {clip_id, keys:[{secs, scale?, x?, y?}]}`: animate a crop through
  poses in the clip's own seconds. `scale` is 1 to 4, centred on frame
  point `x,y` from 0 to 1; Klipp eases between poses without crossing
  the picture edge.
- `follow {clip_id, scale?, hold_secs?}`: make a pan from the beat
  sheet's spots, pushing to each event, holding, then easing out.
- `highlight {at, secs?, x, y, w, h, kind?, color?, stroke?, track?}`:
  draw attention to a region. `x,y` are its centre and `w,h` its size
  as frame fractions; `kind` is `box`, `ring` or `spot` (dim outside).
  The highlight fades in and out.
- `ramp {clip_id, from, to, secs?}`: ramp speed from `from` times to
  `to` times over timeline seconds, then hold the new speed; the clip's
  length follows. The ramp's sound is silent.
- `compose {clip_id, poses:[{secs?, x?, y?, width?, height?, rotation_deg?, opacity?}]}`:
  place a picture inside the frame. One pose holds; more animate.
  `secs` is relative to the clip; `x,y` place its centre as frame
  fractions (-1 to 2, default 0.5). `width` and `height` must be equal
  (0.01 to 4, default 1); rotation is -360 to 360 degrees and opacity
  0 to 1. Example: `{op: compose, clip_id: ID, poses:[{x:0.7,
  y:0.7, width:0.4, height:0.4}]}`.
- `camera {keys:[{secs?, x?, y?, zoom?, roll_deg?}]}`: move the whole
  sequence, including all its layers. `secs` is timeline time, not a
  clip offset. `x,y` are the look point (-1 to 2, default 0.5), `zoom`
  is 0.1 to 8 and `roll_deg` -360 to 360. One key holds; `keys:[]`
  clears the camera.
- `crop {clip_id, left?, right?, top?, bottom?}`: take those fractions
  off the picture's sides. The remainder is transparent, so a cropped
  screen window can be composed as a card and rounded with `corners`.
  All four zero removes the crop.
- `generator {data, at, secs?|end?, track?}`: place native generator
  data as in `klipp_generator`. Give exactly one duration, `secs` or
  an `end` anchor. Without a track it uses a bottom Background video
  track. Example: `{op: generator, data:{SolidColor:{color:
  [16,24,32,255]}}, at:{secs:0}, secs:3}`.
- `depth {clip_id, depth}`: set how far back the clip sits for the
  sequence camera. `1` is the composed plane, `3` drifts a third as
  much and grows less under a push, `0.5` is nearer, and `0` fixes it
  to the screen while the scene moves behind it.
- `motion_blur {shutter_deg, samples?}`: blur everything that moves.
  `shutter_deg` is the open shutter's share of a frame (180 is the film
  look, 360 a full frame, 0 off); `samples` defaults to 8, and higher
  values are smoother and slower.
- `vector_path {clip_id, object_id?, path}`: add or replace a title's
  editable path layer. `path` has `pose:{contours:[{closed,vertices:
  [{point:[x,y],incoming:[x,y],outgoing:[x,y]}]}],fill:[r,g,b,a],
  stroke:[r,g,b,a],stroke_width,stroke_glow?:{radius,strength}}`,
  optional `keyframes:[{time_secs,pose,easing}]` and optional
  `trim:{range:{start,end,offset},keyframes:[{time_secs,range,easing}]}`.
  Coordinates are normalized to the title box; keyed contours must
  keep their topology. Open paths clip, closed paths wrap trim offset,
  and the glow follows only the revealed stroke. Trim leaves the fill
  unchanged; strokes use round caps and joins.

Ops run in order and each sees what the ones before did. Overlaps,
locked tracks, edges past the media and cuts outside a clip are refused.

## Batches

Several tool calls that are one action in your plan go in
`klipp_batch`, one undo step; grade parameters, appends and removes.
Plans are batches of their own.

## Manners

- Talk to the person in their words, not the tools'. Asked whether
  Klipp is connected, answer from `klipp_status`'s summary; asked what
  they can do here, answer from `klipp_guide` topic `abilities`.

- Analysis on a long file takes a while; ask, do something else, then
  read it.
- Prefer the plan's refusal to guessing a time.
- Do not download models without saying so; `klipp_analyze` with
  `download_models` fetches them once.
- Leave the user's markers, favourites and rejects alone; the ones
  analysis makes are its own kind.
