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
7. **Deliver.** `klipp_export` to a file, then `klipp_follow_job` with
   its job ID: it waits until the render ends (ten minutes unless you
   give `max_wait_ms`) and replies how it ended — completed with the file
   and its measured loudness, or failed and why — with the way there.
   If it is still running when the wait is up, the reply says so; call it
   again. Progress notifications go to your client, not to you: you see
   only the replies. To tell the person how far the render is as it goes,
   run `klipp-mcp follow <job_id>` as a background task where your
   harness hears a background command's output: it prints one line per
   step ("render 40%: Encoding, about 0:31 left") and a last line when the
   job ends, and exits 0 only when the file is done. `klipp_job_status`
   reads the state at once, its first line in words; a finished render
   stays readable for an hour even after the person clears it from the
   render queue. `klipp_status` also shows active work.
   `klipp_export_subtitles` writes caption sidecars, tagged with
   each subtitle track's language; with several subtitle tracks (a
   translation beside its source) it writes a file each,
   `name.<language>.ext`, listed in `files`.
   `klipp_import_subtitles` sets the new track's language from the
   file where its format names one (`language` in the reply).
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
- Within one apply, `"@N"` names the clip step N made or changed (0 is
  the first step): place a shot and set its role, level and effects on
  `"@0"` in the same apply, one undo step, without reading ids back. Only
  a field that names a clip reads it so; a name, a role or a title's
  words spelled "@0" stay as written.
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

- `place {media_id, track, at, source_in?, source_out?, insert?, with_audio?, sync_source_secs?}`:
  a media item onto a track (`"video"`, `"audio"`, `"text"` for the
  first of that kind, or a track index from the timeline view) at an
  anchor. Its sound comes along, linked, on the first audio track,
  which is made if there is none; `with_audio: false` leaves it out
  and mutes the clip, which is what B-roll over a voice wants. An empty sequence takes the
  first picture's size and rate. With `insert`, every unlocked track is
  rippled later to make room and a clip straddling the anchor is split
  there. `sync_source_secs` is the moment of the file (seconds from its
  start, as `search_sfx` reports `heard.peak_secs`) that lands on `at`:
  the clip starts that much earlier, to the sample on an audio track,
  and the step says where the moment lands.
- `add_track {kind, name?}`: a video or text track on top, which
  becomes track 0 and moves every other index down by one; an audio or
  subtitle track at the bottom.
- `remove_track {track, with_clips?}`: a track taken out, by index or
  kind; every track below moves up by one. Refused while it holds clips
  unless `with_clips`, which takes them too and unlinks anything on
  another track from them. One undo step puts it back.
- `set_track {track, visible?, language?, locked?}`: a track's own
  settings, one undo step. `visible` false hides its picture or captions
  (its eye), true shows them; `language` is a BCP 47 tag (`de`, `pt-br`;
  empty for none) a subtitle export tags its file with; `locked` true
  keeps every edit off its clips, so an insert or a cut leaves a music
  bed whole, and false lets them be edited again. A translated caption track
  starts hidden so two languages don't draw over each other: show it,
  and hide its source, to see it.
- `track_automation {track, param?, mode?, keys?}`: an audio track's own
  automation, one undo step. `mode` is off, read (the default), touch,
  latch or write; `keys` for `param` volume, `[{at, db}]` (0 unity, up
  to +6, -60 or below silence), or pan, `[{at, pan}]` (-1 left to 1
  right), at sequence seconds with straight moves between, replacing
  that control's keys (empty clears them). They stay put when clips
  move: a fade-out across several clips is one track lane, where
  `volume` keys a single clip.
- `captions {track?, from?, to?, max_chars?, max_lines?, replace?}`: captions for
  what is heard on the timeline, from the transcripts. Each word lands
  where its clip puts it; a cue closes at a sentence, a pause or a cut;
  two lines of 42 characters unless you say otherwise, fewer on a tall
  frame, where captions are larger and sit up in the lower third (a
  `refit` after `format` gives existing captions that look). They go on the
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
  `track` (an index or a kind) instead of `clip_id`, with `from?`/`to?`,
  sets every clip heard on that track in one step (the dialogue track
  3 dB down); a picture and its linked sound are set once.
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
- `match {clip_id, keep?}`: start and end a title, effect or sound with the clip under it; `keep` keeps it so as the shot is edited.
- `role {clip_id, role}`: what a clip is to the mix and the checks:
  dialogue, voiceover, music, sfx, ambience, foley, broll or graphics.
  Mark a camera's own sound `ambience` (or the shot `broll`) when it
  runs under music, so a `duck` does not take it for speech; narration
  is marked dialogue when `narrate` places it.
- `label {clip_id, name?, colour?}`: the clip's own name on the
  timeline, shown in place of its media's (a blank name gives the
  media's back), and its colour: none, red, orange, yellow, green, blue
  or purple. Name the parts of a long cut ("cold open", "interview A")
  and colour them by state, so the editor finds them; `timeline` and
  `list_clips` show both.
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
  To open onto the shot, give the title `exit: {kind: grow, scale: 30,
  anchor: "solid"}`: the letters fly into their most solid stroke until
  the picture fills the frame, and the cut to the full shot is seamless.
  Without `anchor` a word grows about its centre, which for two lines (or
  two words) is the gap between them: the frame goes black instead.
  An entrance or exit is `{kind, duration_secs?, delay_secs?, easing?,
  stagger?, stagger_secs?, distance?, scale?, anchor?}`: `duration_secs`
  up to 3600 (0.6 when left out); `delay_secs` after the title starts for an
  entrance, before it ends for an exit; `easing` one of linear,
  out_cubic, out_quint, out_expo, out_back, out_bounce, in_out_cubic,
  in_cubic, or a curve of its own as `cubic-bezier(x1, y1, x2, y2)` (CSS's
  handles: x is time within 0..1, a y above 1 overshoots); `stagger` by char, word or line, `stagger_secs` apart;
  `distance` (font sizes) for rise, drop and the slides only; `scale`
  for grow, shrink, scale_up and scale_down only: grow above 1 and at
  most 50, shrink at least 0 and below 1. Whether a grown word matte
  fills the frame at its last frame depends on the letters (a thin one,
  a small size or an anchor near an edge needs more scale); a plan the
  app accepts does not show that, so look at the last frame
  (`frame_at`) before cutting on it.
- `volume {clip_id, keys: [{at, db}]}`: a clip's volume drawn by hand,
  levels in decibels from the clip's own level (0 as set, -9 a duck,
  -60 near silence) at timeline
  seconds with straight moves between. It replaces the clip's volume
  keyframes. For the dip `duck` cannot find, or a swell into a moment.
- `grade {clip_id | on, exposure?, contrast?, saturation?, temperature?, tint?, lift?, gamma?, gain?, highlights?, shadows?, reset?}`:
  a picture clip's primary grade. What you give is set, the rest stays.
  Instead of `clip_id`, `on: {group, stage}` grades a grade group (`stage`
  `pre` or `post`, as `grade_group` made it) and `on: "timeline"` the
  timeline grade, after every clip's own. `on: {"track": n, "from"?,
  "to"?}` grades every picture clip of that track (or that stretch of
  it), each in its own grade, in one step; with `from` it copies that
  clip's grade onto the track's other clips.
  `from: <clip_id>` instead of knobs copies that clip's whole grade onto
  this one (a colourist's "apply grade from"); grade again after to adjust.
  Every knob is 0 at neutral, except `saturation` and `contrast`, whose
  neutral is 1. `exposure` is in stops; `temperature` and `tint` are the
  white balance, in mireds of assumed light (a unit moves a grey about
  0.2 b*: +25 a clear warm, +50 strong; `tint` + is light assumed more
  magenta, so the picture goes green, and − turns it magenta); `lift`, `gamma`, `gain` move the shadows, mids and
  highlights; `shadows` (-1 to 1) lifts or sinks the quarter tones,
  black and mid grey held, and `highlights` (-1 to 1) the tones from mid
  grey up, most at white: ±0.3 is a moderate move, ±1 a strong
  recovery or lift; `reset` takes the clip to neutral first. Match shots to
  each other before making any of them look better.
- `effect {clip_id, kind, amount?}`: a finishing effect on a picture
  clip, native generator or title (on a title it works on the drawn
  text: blur softens it, glow lights around it). `amount` is 0 to 1, one scale for every kind: about 0.3 subtle, 0.5 clearly there, 1 strong; a second of the same kind replaces the first.
  With `on: "timeline"` or `on: {"group": name}` instead of `clip_id` it
  goes on every picture clip there in one step (a vignette and grain over
  the whole film); `on: {"track": n or "video", "from"?, "to"?}` does the
  same for one track, or a stretch of it (the B camera's clips). Each
  clip gets its own copy, so a shot added later needs it too. `look`
  takes the same `on: {"track": …}`. The reply says how many clips were
  taken, how many others on the track were left, and any that refused.
  With `"itself": true` (and no `from`/`to`) the effect, look or grade
  goes on the track itself instead: one effect the track runs after
  every clip's own, clips added later included (a look for one camera's
  track, a vignette over a whole reel).
  Kinds: `vignette`, `grain` (`film_grain`), `glow`, `bloom`, `diffusion`
  (`pro_mist`), `halation` (`film_halation`), `light_rays` (`god_rays`),
  `lens_flare`, `anamorphic` (`anamorphic_streak`, `streak`), `light_wrap`,
  `blur`, `radial_blur` (`zoom_blur`), `spin_blur`, `directional_blur`
  (`motion_blur`), `sharpen`, `chromatic_aberration` (`rgb_split`,
  `fringe`), `tilt_shift`, `mosaic` (`pixelate`), `posterize`, `invert`,
  `mirror`, `lens_distortion` (`barrel`), `displace` (`ripple`,
  `heat_haze`: each pixel moved by a drifting noise field), `reflection`
  (`water_reflection`, and its surfaces `calm_water`, `rippled_water`,
  `glossy_floor`, `wet_street`, `glass`: see below), `emboss`, `edge_detect`
  (`edges`), `denoise`, `vhs`, `old_film`, `flicker` (random jumps of
  the opacity; `exposure_flicker` of the light), `binocular`
  (`binocular_vignette`: two eyepieces, a fringe at their rims) and the
  particles `dust`, `snow`, `rain`, `sparks` and `embers` (`particles`
  is dust; more of them with the amount). `corners`
  (`rounded_corners`) rounds a layer's alpha mask, `shadow`
  (`drop_shadow`) places a shadow behind it, and `outer_glow` lights its
  silhouette; compose the picture smaller than the frame to see these.
  For example, `{op: effect, clip_id: ID, kind: corners, amount: 0.4}`.
  `glow` lights round a picture's highlights (a title glows whole);
  `invert` is the full negative unless an amount blends it back;
  `edge_detect` draws the edges on black; `lens_distortion` bends the
  picture and scales it so its edges stay whole; `binocular` is black
  outside the two eyepieces. Place the light of `lens_flare` with
  `params` `position_x`, `position_y`, and of `light_rays` with
  `center_x`, `center_y` (frame fractions from the top left).
  `params` sets an effect's own settings by name over what `kind` and
  `amount` give, each inside its range, a choice by name:
  `{op: effect, clip_id: ID, kind: displace, params: {amount: 8, size:
  160, speed: 0.5, direction: across}}` sways the picture slowly
  sideways (`amount` and `size` in canvas pixels; `direction` `both`,
  `across` or `up_down`; `seed` another pattern). A wrong name is
  refused with the settings the effect has.
  A `reflection` mirrors a layer (a title, a logo, a picture) about a
  waterline below it, as on water: put it on the layer, over the shot
  of the water. `kind` picks the surface (`calm_water` is the default
  of `reflection`: a slow sideways wobble that breaks into horizontal
  bands further down); `amount` its strength. Its `params`:
  `waterline` (the line, a fraction of the frame's height from the top,
  0.6 by default; put it where the letters' base is), `gap` (canvas
  pixels from that base down to the water: 0 rests the layer on it,
  more floats it above, the reflection starting as much lower),
  `ripple_amount` and `ripple_size` (canvas pixels), `ripple_speed`,
  `breakup` (strips and gaps further down, 0 to 1), `smear` (streaks
  down, 0 to 1), `fade` (how quickly it fades with distance, 0 to 1),
  `opacity`, `darken` (how much darker than the layer, 0 to 1: water
  reflects little, so 0.45 on water), `contact` (the thin dark band
  where the layer meets the water, its shadow, 0 to 1), `blend` (`normal`, the water's
  default, `screen`, `soft_light` or `overlay`: how it mixes with the
  picture under it),
  `tint` (`#rrggbb`, the water's colour) with `tint_amount`, `submerge`
  (how far the layer's own pixels below the line are under the water,
  0 to 1: 1 hides them, letters dipping into the water; on water 0.7,
  a little of them showing through, rippled and coloured like the
  water; elsewhere 0, over their reflection) and `seed`. The reflection
  starts whole at the line and breaks up further down. For example `{op: effect, clip_id: TITLE,
  kind: calm_water, params: {waterline: 0.62, tint: "#1d3b4f"}}`.
  On water (`calm_water`, `rippled_water`) the tint and darkness come
  from the water itself: the water under the reflection is measured once
  from the footage below it, and the tint and darkness follow it
  (`tint_auto`, `darken_auto`, both on). klipp_list_clips shows the clip's
  `water` (its colour, and `outdated` when the footage, range or waterline
  has changed since). `klipp_measure_water {clip_id}` measures it again; it
  is never redone on its own. Giving `tint` or `darken` by hand, set
  `tint_auto`/`darken_auto` to 0 so a later measurement keeps them.
  A key is `chroma_key` (`green_screen`, `blue_screen`) on one clip, the
  app's Chroma Key: `color` `#rrggbb` (green, or blue for `blue_screen`),
  `tolerance` (0.2), `softness` (0.1) and `spill` (0.6), each 0 to 1; a
  second key on the clip replaces the first. `luma_key` keys by
  brightness, the app's Luma Key: `low` to `high` is keyed out (0 to
  0.1, a black background), `softness` (0.05) fades it back in past
  them, each 0 to 1, and `invert: true` keeps that range and keys out
  the rest (fire or sparks shot on black over a picture: `low` 0.6,
  `high` 1, `invert`). Put the keyed clip on a track above what shows
  through it.
  `fill_bars` (`blanking_fill`, `blur_fill`, `blurred_background`) fills
  the black bars round a picture that does not fill the frame (an
  upright phone shot in a wide sequence, a wide one in a vertical
  sequence) with the picture itself, enlarged and blurred, so the frame
  is full and the whole picture still shows; more `amount` is softer
  and darker. Put it on every such clip when asked to fill the bars.
- `track_audio {track, effect, params?, bypass?, remove?}`: the track's
  own audio chain, run after every clip's own: an EQ and a compressor on
  the dialogue track, a limiter on the music. `effect` is `eq`,
  `compressor`, `limiter`, `gate`, `high_pass`, `denoiser`,
  `voice_isolation`, `deesser`, `hum_removal`, `noise_reduction`,
  `declick`, `normalize`, `reverb`, `delay`, `chorus`, `pitch_shift` or
  `multiband_compressor`; `params` by name set it (the rest stay at their
  defaults), on the one there or a new one at the end (a name the effect
  lacks is refused; topic `op.track_audio` lists them); `bypass` switches
  it off or on; `remove` takes it off. Set once, it holds for every clip
  of the track. `effect` may also be an installed LV2 or CLAP plug-in's
  id (`lv2:…`, `clap:…`; `audio_plugins` lists them), its `params` by
  name or key and inside their ranges.
- `clip_audio {clip_id, effect, params?, bypass?, remove?}`: the same on
  one clip's own chain (a linked pair's, either clip named): a built-in
  effect or an installed plug-in. Five voice tools in it: `normalize`
  brings the whole clip to `target_lufs` integrated by one gain, its
  dynamics kept (measured in the background, none until then; `mode: 0`
  rides the level instead, a leveler, and is the only mode a track
  takes); `noise_reduction` turns each band down while it sits near its
  own noise floor, the voice over it untouched (a sound held steady
  longer than about 1.5 s counts as floor, so not on music); `deesser`
  calms an "s" that stands out from the voice, its `threshold` in dB
  relative to the voice; `declick` bridges clicks and pops;
  `pitch_shift` moves the pitch by `semitones`, up positive.
  A clip's `reverb`, `delay` and `chorus` ring on past its out point,
  into a gap or under the next clip, until they fall below −80 dB (at
  most 10 s), in playback and the export alike; its fade-out goes in
  before them, so a reverb still rings after a fade. Playback that
  starts inside a tail (after a seek or a pause) plays it from there.
- `opacity_mask {clip_id, shape, feather?, invert?, clear?}`: fade a
  clip's opacity across the frame, whole at one line or ring and gone at
  another, so the track below shows through: a sky fading out, a picture
  melting into the next layer, a soft round spotlight. `shape` is
  `{linear: {start: [x, y], end: [x, y]}}` (whole at `start`, gone at
  `end`) or `{radial: {center: [x, y], inner: r, outer: r}}` (whole
  inside `inner`, gone past `outer`, radii in frame widths). Points are
  frame fractions from the top left. `feather` 1 (the default) fades
  all the way between them, 0 is a hard edge midway; `invert` swaps
  which side shows. A second replaces the first; `clear: true` takes it
  off. For example `{op: opacity_mask, clip_id: ID, shape: {linear:
  {start: [0.5, 0.4], end: [0.5, 0.9]}}}` fades a clip out toward the
  bottom. A clip whose alpha is already another matte (a cut-out) is
  refused.
- `stabilize {clip_id, smoothing?}`: steady a shaky clip. Only the
  shake is taken out: a pan, a tracking move or a slow turn is kept, and
  the picture is never leaned or sheared. It enlarges the picture just
  as far as that clip's correction needs, so no edge is ever stretched
  in.
- `retime {clip_id, speed | fit, ripple?}`: change a clip's speed, its linked
  sound with it. Above 1 is faster and shorter, below 1 slower and
  longer, negative plays it reversed. `fit: true` in place of `speed` is
  fit to fill: the speed that makes the clip end exactly where the next
  clip on its track begins, worked out for you (a shot slowed to fill a
  bar, a slot left for it in a cut to music). Its sound plays at the speed, its
  pitch moving with it as tape's does, backward when reversed; music and
  other tracks are untouched. (A speed ramp's own sound is silent.)
  With `ripple` the clips after it on its tracks move by the change in
  length; without it a gap is left where it shrinks, and a clip that
  would grow into the next one is refused. `interpolation` is how the
  frames between the file's are drawn when it is slowed, or when the file
  has a lower frame rate than the timeline at full speed: `sample` (or
  `nearest`) repeats the nearest, `blend` cross-fades the two either side,
  `flow` (or `optical_flow`) follows the motion between them. Given alone
  it changes only that. A clip that will show its pictures held, a 16 fps
  file at 0.6x on a 30 fps timeline showing 9.6 a second, is said in the
  reply and by check (`judder`).
- `replace {clip_id, media_id, source_in?}`: another file, or another part
  of one, in a clip's slot: the same place, length and speed, with its
  grade, effects, role, volume, links and transitions kept and its linked
  sound moved to the same file. `source_in` is where in the file it starts:
  the clip's own when that fits, else the start. For the shot that turns
  out wrong after it is graded and dressed.
- `reframe {clip_id, scale?, x?, y?}`: punch in and reposition a
  picture. `scale` zooms (1 as shot, up to 4); `x` and `y` pan the frame
  from -1 to 1. What you leave out stays as the clip has it, so a re-pan
  names only the axis it moves. Use it to salvage a wide or badly-framed shot, to fit a
  vertical delivery, or to set a slow push. It sits before other
  effects, so a vignette or grade after it sees the reframed picture.
- `qualify {clip_id, target?|hue?, …, exposure?, saturation?, hue_shift?,
  temperature?}`: a secondary. Key a colour or tone — a named `target`
  (skin, sky, foliage for leaves and grass olive to green, greens for
  pure green, reds, blues…) or an explicit `hue` in degrees,
  narrowed by `hue_width` and saturation/luminance windows — and correct
  only what is keyed. `invert` grades everything but the key; `remove`
  clears it. Fix a sky, calm a loud shirt, keep skin warm while cooling
  the rest. Grade the primary first, then reach for a secondary.
- `zones {clip_id | on, zone?, exposure?, hue?, amount?, saturation?,
  boundary?, falloff?, preset?, reset?}`: HDR zones, tonal balance placed
  in stops about mid grey. New zones are placed for the clip: on an SDR
  picture light, highlight and specular start at +0.5, +1.5 and +2.2
  stops (about 25%, 51% and 83% of white), inside its range, and the
  reply says so; on HDR and log they reach on past white (+1, +2.5,
  +4.5). Zones a grade already has keep their boundaries. A
  `zone` (global, black, dark, shadow, light, highlight, specular) takes
  an `exposure` in stops, a colour by `hue` (degrees on the wheel) and
  `amount` (0 to 1), and a `saturation`; `boundary` and `falloff` move
  where it reaches. `preset` starts from a named starting point (HDR
  highlight roll-off, Lift shadows, Deepen blacks, Midtone contrast, Warm
  highlights, cool shadows). Reach for it to calm HDR highlights or open
  shadows without a key.
- `look {clip_id | on, lut_path, name?, opacity?, remove?}`: the batch
  op in `klipp_plan` / `klipp_apply` needs `lut_path`, the resolved
  absolute `.cube` file path. `name` is only a display label; it does
  not resolve a bundled look. `remove: true` clears the look without
  a path. The LUT sits after the grade; `opacity` (0–1, default 1)
  fades it in. Grade for exposure and continuity first.
  The separate `klipp_look` tool resolves `name` from `klipp_looks`
  (category/name or identifying words), or accepts an absolute `.cube`
  path in `name`. For example, call `klipp_look` with
  `{"clip_id":"CLIP_ID","name":"fuji_xtrans_iii_classic_chrome","opacity":0.25}`.
  Use a real clip ID from the timeline. `@N` references belong to batch
  ops, not this standalone tool. For a place+look batch, see topic `op.look`.
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
- `transition {clip_id, kind?, secs?, remove?, color?}`: a transition between a clip and
  the one after it, `secs` long, on the cut. `remove: true` takes the
  transition on that cut off again, the crossfade on its sound with it. `cross_dissolve` is the
  default and the only one an interview or a documentary wants; a
  dissolve marks a passage of time or place. `dip_to_black` and
  `dip_to_white` mark a break. A stylised kind (`zoom`, `whip`,
  `glitch`, `film_burn`) is a style choice the brief has to ask for.
  `color` (`#rrggbb`) sets the colour of one that has one:
  `colorize_flash`, `light_leak`, `fade_to_color`.
  Both clips need footage beyond the cut to overlap.
- `style {clip_id | track, from?, to?, size?, color?, font?, bold?, align?, x?, y?, backing?, backing_color?, outline?, outline_color?, shadow?, glow?, glow_color?, gradient?, glitch?, chromatic?, animate?, animate_secs?, stagger?, enter?, exit?, loop?, roll_shape?, edge_fade?, weight?, axes?, features?, last_line?, valign?, underline?, runs?, animators?, keys?, on_path?, blend?, mask?, three_d?, material?}`:
  the look and place of one title, or of every title on a track
  (`"subtitle"` for all the captions), between `from` and `to` when
  given (anchors, as `{"secs": 44.0}`). `size` is pixels of a 1080-line frame, `color` is `#rrggbb`,
  `x` and `y` are where the text's centre goes (0 to 1 of the frame),
  or, for a left- or right-aligned title, where its left or right edge
  goes, so a chapter title and its kicker share one margin,
  `backing` is how dark a panel sits behind the text (0 none, 1 black),
  `outline` a width in pixels, `shadow {x?, y?, blur?, color?, opacity?}`
  a soft drop shadow behind the words: offset and blur in pixels of a
  1080-line frame (0, 4 and 8 when left out), `#rrggbb`, opacity 0 to 1
  (0.6), and opacity 0 takes it off. Bright text over a bright sky or
  sand wants the shadow; a dark glow barely shows. Lift the captions with it for as long as
  a lower third is up, so the two do not share the bottom of the frame.
  Everything else `title` takes for a look or a motion restyles in place
  too, the title keeping its id: never remove a title and make it again
  to change its glow, gradient or entrance. `glow`, `glitch` and
  `chromatic` 0 take them off; `outline_color` or `glow_color` alone
  recolours (or adds) one; the motion is fitted to each title's length;
  `loop: "none"` stops a loop. The typesetting fields restyle as `title`
  takes them; `runs` replaces a title's runs (`[]` clears them) and needs
  one `clip_id`, since its words are that title's.
- `lower_third {name, role?, at, secs?, track?, side?, x?, y?, backing?, in_picture?}`: a name
  and a role bottom left in title-safe, four seconds unless you say
  otherwise, fading in and out, on a text track with room (one is made
  on top if none has, which moves every other index down by one).
  Refused across a shot change, and the refusal names the cut. Look at
  the frame first: where the speaker is on the right, or the picture
  has a name of its own burnt in down there (a split-screen podcast),
  `side: "right"` sets it against the right edge, and `x` (that edge)
  and `y` (the name's line) place it, as fractions of the frame inside
  title-safe. A name under the wrong person is worse than none.
  `backing` keeps it legible: `shadow` (the default) a close dark
  shadow and outline, `box` a dark box behind each line for a light,
  busy picture, `none` the soft lift alone. Over a pillarboxed shot (a
  tall picture in a wide frame), `in_picture: true` keeps it in from the
  picture's own edge instead of on the bar. A title's `backing` panel
  (style, title) sits behind every line of it.
- `title {text, at, secs?, style?, track?, x?, y?, size?, color?, font?, bold?, align?, backing?, backing_color?, full_frame?, outline?, outline_color?, glow?, glow_color?, gradient?, animate?, animate_secs?, stagger?, loop?, glitch?, chromatic?, enter?, exit?, over_faces?, shadow?, weight?, axes?, features?, last_line?, valign?, underline?, runs?, animators?, keys?, on_path?, blend?, mask?, three_d?, material?}`:
  a title from scratch: a heading, a callout or a hero card, on a text
  track (one is made on top if left out). `style` names a look, one of
  the brief's type styles or `heading`, `callout` or `card`, and its
  fields fill every one left out here. `x`, `y`, `size`, `color`,
  `backing`, `outline` and `shadow` read as in `style`; `full_frame` is a solid
  `#rrggbb` card behind everything, `gradient` a two-colour fill
  (`#top,#bottom`) or `{stops: [[pos 0-1, #rrggbb], ...], angle?}`
  (degrees, 0 top to bottom, 90 left to right) or `{stops, radial: {x, y, r}}`,
  2 to 16 stops mixed in linear light (#1509). `animate` is the entrance (fade, rise, drop,
  slide_left, slide_right, scale_up, scale_down, blur, mask_rise,
  mask_drop, typewriter) and `loop` a continuous effect (pulse, flash,
  neon, wave, shake, color_cycle). `size` is pixels at 1080 on the
  frame's short side, so a size and the built-in styles read the same
  on a vertical frame as on a wide one, and the whole title is kept
  inside title-safe; a word too wide for the frame shrinks the line
  rather than breaking. Look at the frame all the same.
  Typesetting: `weight` 100 (thin) to 900 (black), the family's nearest
  face (400 regular, 700 bold); `axes` a variable font's axes by
  four-letter tag, `{"wdth": 80, "opsz": 36}`; `features` OpenType
  features, `"smcp"` small caps on, `"-liga"` ligatures off, `"ss01=1"`
  a stylistic set (kerning and ligatures are on already); with `align:
  "justify"`, `last_line` (left, center or justify) places each
  paragraph's last line; `valign` (top, middle, bottom) sets the text up
  or down its box; `underline` the whole text. `runs` styles stretches
  of it, each `{text, occurrence?, bold?, italic?, underline?, color?,
  size?, font?, weight?, axes?, features?, ruby?}`: `text` is the words
  it covers, the first time they appear unless `occurrence` counts on
  (1, 2, ...); `bold` and `italic` set its style together, one left out
  being off; `ruby` is a reading set small beside the words (furigana:
  above them, or right of a vertical column). Runs may not overlap; words the title lacks are refused.
  Animators move part of the text unit by unit, over the entrance and
  exit: `animators` is a list (in place of any the text had, `[]`
  clears them), each a `preset` (`wave`, `random_fade_in`, `spin_in`)
  with any field over it, or the fields alone. What it picks: `unit`
  (char, word, line), `start` and `end` (0 to 1, fractions of the
  units) moved by `offset` (-1 to 1), each keyed by `start_keys`,
  `end_keys`, `offset_keys` as `[{at, value}]` (seconds from the
  title's start); `shape` (square, ramp_up, ramp_down, triangle, round,
  smooth); `order` (forward, reverse, centre, random with a `seed`, the
  same order every render); `easing` (as a motion's); `amount` (-1 to
  1, negative picks the units outside); `within` `{unit?, start?, end?}`
  a second range it is limited to. What it does to what it picks
  wholly: `move_x`, `move_y` (font sizes), `scale`, `rotate` (degrees),
  `opacity`, `blur` (font sizes), `color`. The middle word of "one
  MIDDLE two" turning letter by letter in a random order is `{unit:
  "char", order: "random", seed: 7, rotate: 90, within: {start: 0.25,
  end: 0.75}, start_keys: [{at: 0, value: 0}, {at: 2, value: 1}]}`.
  `keys` keys the text over its time: `size`, `tracking`,
  `line_spacing` as `[{at, value}]`, `color` and `gradient_stops` (by
  the stop's index from 0, `{"2": [...]}`) as `[{at, color}]`, colours
  blended in linear light; each field replaces its keys, `[]` clears
  them. `on_path` lays the text along a path layer of the same title
  (`vector_path` adds one to its clip; a new title has none, so style
  it on after): `{layer, contour?, offset? (0 to 1), offset_keys?,
  align? (start, center, end), flip?}`, or `"none"` to lay it straight.
  `blend` mixes the text with the title's layers below it (normal, add,
  multiply, screen, overlay, soft_light, hard_light, difference,
  exclusion, color_dodge, color_burn, darken, lighten); `mask`
  `{layer, kind?}` shows the text only where another layer of the same
  title is drawn (`kind` alpha, the default), is not (inverted_alpha),
  is bright (luma) or is dark (inverted_luma), that layer then a matte
  and not drawn itself; `"none"` takes the mask off.
  `three_d` draws the text as solids, extruded, bevelled, turned under
  a perspective camera and lit: `{look? (block, bevelled, chrome),
  depth? (ems, 0 to 2), bevel? and back_bevel? ({size (ems, up to
  0.2), segments?, profile? (straight, round)} or "none"), turn?,
  tilt?, roll? (degrees), fov? (1 to 170), perspective? (false: none),
  light? {azimuth, elevation, intensity, ambient, highlight,
  shininess}, face_color?, side_color?, bevel_color?, keys?}`, `keys`
  by name (turn, tilt, roll, depth, bevel, fov, light_azimuth,
  light_elevation, light_intensity) as `[{at, value}]`. A look sets
  depth, bevels, light and colours; the fields lay over it, or over the
  text's 3D as it is, so a later style changes only what it names.
  `"none"` takes 3D off. The layer's glow, shadow, outline and backing
  go round the solid. A word turning in y over two seconds:
  `{look: "bevelled", keys: {turn: [{at: 0, value: -40}, {at: 2,
  value: 40}]}}`.
  `material` makes flat text metal, polished as the metal is with
  nothing else set: a name (gold, silver, steel or iron, copper,
  rose_gold, bronze, chrome, brushed_metal), `"none"` to paint it again, or
  `{preset?, tint? (#rrggbb), roughness? (0 mirror, 1 satin), edge?
  (fresnel), highlights? (0 to 2), glints?, bevel? (a fraction of the
  size, up to 0.3), depth?, contour? (a dark line round the letters'
  edge), anisotropy? and anisotropy_angle? (brushed streaks),
  environment? {studio?, sky?, shot? (0 to 2), sky_tint?, shot_blur?
  (a fraction of the frame's height, up to 0.2)},
  key_light? {azimuth, elevation, strength (0 to 4), color},
  rim_light? (the same, or "none")}`, amounts 0 to 1 unless said. The
  fields lay over the named preset, or over the text's metal as it is.
  Lights come from azimuth degrees around (0 the right, 90 below, 180
  the left, -90 above) and elevation degrees towards the viewer: put
  the key light where the shot's light comes from (a low sun on the
  right: `{azimuth: -10, elevation: 15, color: "#ffd8a0"}`). The metal
  reflects a studio, a sky and the shot behind the title (blurred),
  shares of what it sees (scaled to a sum of 1 past it), through a
  bevel cut into the letters: the shot leads by default, so gold over a
  sunset turns warm and over blue water cool; more studio makes it a
  product shot; the layer's effects run over it, so a
  glow glows gold. A 3D text (three_d) is metal on every face, its
  own bevels and sides reflecting. Without a GPU the text shows its
  colour.
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
- `edit_from_selects {from, from_secs, to_secs, at?, overwrite?}`: what
  lies from `from_secs` to `to_secs` in another sequence of the project
  (`from`, its id or name as `klipp_status` lists them), put into the
  open one at `at` (its In point when left out), as Insert and Overwrite
  from Selects do: every track cut and moved along, or over what is there
  with `overwrite`. One undo step. Only `klipp_apply` and `klipp_plan` see
  the other sequences; if the one copied from changes before the edit
  lands, it is refused, so plan again.
- `strobe {clip_id, piece_frames, on?, off?, from?, to?, ripple?}`: a
  flicker: the clip (or `from` to `to` inside it) cut into pieces
  `piece_frames` long, and of every `on` pieces kept the next `off`
  removed (1 and 1). The last piece takes what does not divide, so no
  sliver is left. With `ripple` the gaps close up; without, they stay
  black. The linked sound is cut with it; one undo step. `check` reads
  it as one strobe, a note, not its gaps and pieces as black and flashes.
- `set_length {clips, frames, keep?, ripple?}`: each clip made `frames`
  long, keeping its `start` (the default), `end` or `centre`; with
  `ripple` what follows closes up or makes room. A rapid-fire montage is
  every clip of a stretch at 3 frames with ripple; one undo step.
- `over_edits {clip_id, tracks?, from?, to?, align?, every_clip?}`: a
  copy of a clip (a flash, an adjustment clip) on its own track over
  every cut of the targeted picture tracks (or `tracks`), its `centre`
  (the default), `start` or `end` on the cut; `every_clip` uses each
  clip's start. A cut where a copy would overlap a clip already there is
  skipped and counted in the reply; one undo step.
- `template {name, at, secs?, track?, fields?}`: a saved title template
  by `name`, at `at` for `secs`, on a text track (one is made on top if
  none fits). `fields` sets its published fields by label: a text as its
  words, a colour as #rrggbb, the speed as a number (1 as made).
  `klipp_title_templates` lists the templates and their fields.
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
- `move {clip_id, to, track?}`: the clip's start to an anchor; a linked audio
  or video partner goes with it. With `track` the clip goes onto that
  track (a picture to a video track, a title to a text or video track):
  to put a title above a card, or a shot on the track above.
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
- `pin {clip_id, media_id | layer, surface?, from?, to?, fit?, inset?, on_loss?, fade_secs?, light?, blend?, template?}`:
  a picture, video or still, onto a surface tracked in the clip (a phone
  screen, a sign, a monitor): it goes on a video track above, following
  the surface as the shot moves, from `from` to `to` (the clip's edges
  when left out). Or `layer`, a clip already on the timeline (a title,
  a graphic, a generator), is pinned where it is over its own time,
  without `from`/`to`: text on a wall stays a title, edited with
  `set_text` and its style as before. `on_loss` is what it does where
  the surface's track is lost (klipp_list_clips' `lost`): `hold` the
  last good place (the default), `hide`, or `fade` out over `fade_secs`
  (0.3). A sign that leaves the shot hides or fades; pin again with the
  same layer to change it. `light` 0 to 1 makes it darker or lighter as
  the surface is than on its reference frame (paint on a wall going
  into shade): 1 for something on the surface, lower for something in
  front of it. It needs a surface tracked since the light was measured;
  track an older one again. `blend` is `normal`, `multiply` (ink on
  paper), `screen`, `overlay` or `soft_light`. `template` sets these at
  once, any given standing over it: `text_on_wall` (fitted inside a
  margin, takes the light, fades out where lost), `screen` (stretched
  inside a bezel, in its own light, hidden where lost) or `sign` (fitted,
  most of the light, fades out). Start from one and change only what
  looks wrong. `surface` is the id or name `klipp_track_surface` gave;
  left out, the clip's latest. `fit` is `stretch` (onto the four corners)
  or `fit` (its own shape kept, in the middle); left out, it stretches
  when the shapes nearly match and fits when they do not. `inset` leaves
  a margin inside the edges, 0 to 0.45 each side: about 0.02 for a
  screen tracked by its bezel. Track first; re-tracking moves what is
  pinned with it. Look at a frame after.
- `redact {clip_id, x, y, w, h, from?, to?, kind?, amount?}`: hide a
  patch of picture for a stretch. Coordinates are fractions of the
  frame from the top left, as `klipp_screen_text` reports them. The
  default kind is `mosaic`; `blur` is also available. The range
  defaults to the clip edges; `klipp_auto_redact` handles all findings.
- `grade_group {group, clips:[clip_id…], leave?}`: put clips in a named
  group, or take them out with `leave`. Group grades run before (`pre`)
  or after (`post`) each clip's own grade. `grade` and `look` can target
  `{group, stage}` instead of a clip, or `"timeline"` after all clips.
  Use one group look for a scene. `look_target` (a preset name, `"none"`
  to clear) holds the group's shots to their own look target instead of
  the brief's, in `grade_to_target` and `check`: a film warm in the south
  and cool in the north is two groups with two targets. `departs: true`
  says a group leaves the look on purpose; its shots are judged for
  faults only and the check says so once. Either works on a group alone.
- `remove_grade_group {group}`: end a group and leave its clips' own
  grades in place.
- `zoom {clip_id, from?, to, x?, y?}`: animate a zoom over the clip,
  from one scale to another (1 as shot, up to 4). `x,y` can move the
  focus toward a frame point (-1 to 1), for a Ken Burns move on a
  still. It renders on the GPU.
- `pan {clip_id, keys:[{secs, scale?, x?, y?, ease?}]}`: animate a crop through
  poses in the clip's own seconds. `scale` is 1 to 4, centred on frame
  point `x,y` from 0 to 1; Klipp eases between poses without crossing
  the picture edge. `ease` is the move into the key (see Easing below,
  `in_out` when left out). On a picture composed as a card (smaller
  than the frame, moved, or cropped), it pans inside the card: the card
  keeps its place and size, `x,y` are points across the card, and the
  clip's crop is keyed (one is added when there is none, an ordinary
  Crop the inspector shows). A card that moves is refused: compose it
  with one pose, pan, then compose again to move it with the pan kept.
- Easing, for `compose` poses, `pan` and `camera` keys and `zoom`:
  `ease` is the curve of the move into a key from the one before:
  `linear` (constant speed), `in` (starts slowly), `out` (settles),
  `in_out` (slow at both ends), `expo_out` (a fast arrival that settles
  long) or `back_out` (overshoots a little and settles back). `compose`,
  `pan` and `camera` default to `in_out`, so each key is a rest and keys
  a few tenths of a second apart pulse; `zoom` defaults to `linear`. For
  one continuous move through several points give the keys after the
  first `ease: linear`: `{op: pan, clip_id: ID, keys:[{secs:0, scale:1},
  {secs:2, scale:1.15, x:0.45, y:0.5, ease:linear}, {secs:4, scale:1.3,
  x:0.4, y:0.48, ease:linear}]}`.
- `follow {clip_id, scale?, hold_secs?}`: make a pan from the beat
  sheet's spots, pushing to each event, holding, then easing out.
- `refit {markers?}`: after `format` changes the aspect, fit every picture
  and title to the new frame in one step. Each picture fills the frame and
  follows its subject (faces first, then people, from the framing
  analysis, so analyze first) with a held, eased pan; a still holds one
  pose. Titles are kept inside title-safe and shrunk where they no longer
  fit across. Pictures with no clear subject stay centred, are named in
  the reply, and get a range marker each unless `markers` is false.
- `highlight {at, secs?, x, y, w, h, kind?, color?, stroke?, track?}`:
  draw attention to a region. `x,y` are its centre and `w,h` its size
  as frame fractions; `kind` is `box`, `ring` or `spot` (dim outside).
  The highlight fades in and out.
- `ramp {clip_id, from, to, secs?}`: ramp speed from `from` times to
  `to` times over timeline seconds, then hold the new speed; the clip's
  length follows. The ramp's sound is silent.
- `compose {clip_id, poses:[{secs? | from_end_secs?, x?, y?, width?, height?, rotation_deg?, tilt_x_deg?, tilt_y_deg?, opacity?, ease?}]}`:
  set the clip transform for picture-in-picture, split screens or grids.
  One pose holds; more animate. Unlike `reframe`, `pan` and `zoom`,
  `compose` can shrink a picture below full size.
  A pose with no `x`, `y`, `width` or `height` keeps the clip where it
  is, as reframed or composed before: poses of `opacity` alone fade a
  shot, or a colour card (a generator), in or out without moving it.
  `secs` is relative to the clip; `x,y` place its centre as frame
  fractions (-1 to 2, default 0.5). `width` and `height` must be equal
  (0.01 to 4, default 1); rotation is -360 to 360 degrees and opacity
  0 to 1. `tilt_x_deg` and `tilt_y_deg` tilt in perspective, -80 to 80
  degrees (default 0); these can animate between poses too.
  Example: `{op: compose, clip_id: ID, poses:[{x:0.7,
  y:0.7, width:0.4, height:0.4}]}`. On a clip panned inside its card
  it moves the card and keeps the pan, the card in one place (poses may
  still fade it).
  For a corner inset, use `{x:0.8, y:0.8, width:0.3, height:0.3}`.
  For a 2x2 grid of equal-aspect clips on separate video tracks, apply
  one `compose` per clip with width and height 0.5 and centres
  `(0.25,0.25)`, `(0.75,0.25)`, `(0.25,0.75)` and `(0.75,0.75)`.
  Put these operations in one `plan`/`apply` batch; no project-file
  editing or separate layout operation is needed.
  A clip's ends sit on frames, so it can be up to a frame shorter
  than the length asked for: at 30 fps, placed from 28.15 s to 37.70 s,
  it starts on 28.167 s and is 9.533 s long (the step's `after` gives
  its times as placed). A pose up to one frame past the end is taken as
  the end; for a closing pose use `from_end_secs` (seconds before the
  clip's end, 0 is the end) in place of `secs`.
- `camera {keys:[{secs?, x?, y?, zoom?, roll_deg?}]}`: move the whole
  sequence, including all its layers. `secs` is timeline time, not a
  clip offset. `x,y` are the look point (-1 to 2, default 0.5), `zoom`
  is 0.1 to 8 and `roll_deg` -360 to 360. One key holds; `keys:[]`
  clears the camera.
- `crop {clip_id, left?, right?, top?, bottom?}`: take those fractions
  off the picture's sides. The remainder is transparent, so a cropped
  screen window can be composed as a card and rounded with `corners`.
  All four zero removes the crop. A new crop replaces a pan inside the
  card, which keys the crop.
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
- `connector {at, secs, from: [x, y], to: [x, y], color?, stroke_width?, reveal_secs?, track?}`:
  a line between two points of the frame (fractions, -1 to 2), as a
  title with no words: drawn on from `from` over `reveal_secs` (at once
  when left out), held `secs`, `color` `#rrggbb` (white), `stroke_width`
  in pixels of a 1080 frame (6). An edit plan's scene layers make these.
- `graphics {at, secs, track?}`: create an empty native graphics clip, then
  add shapes with `vector_path {clip_id: "@0", shape: "ellipse", …}` in
  the same apply. No placeholder text or connector is added. It can start
  an empty sequence or extend it, lasts 0.2 to 86400 seconds, and uses a
  text track with room unless `track` is given. Vector geometry and its
  animation remain editable; undo takes the batch back together.
- `vector_path {clip_id, object_id?, path | shape, color?, stroke_width?, x?, y?, w?, h?}`:
  add or replace a title's editable path layer. `shape` is a ready-made
  one instead of a `path`: `"ellipse"`, `"triangle"`, `"arrow"` (pointing
  right), `"line"`, `{"polygon":{"sides":6}}` or
  `{"star":{"points":5,"inner":0.45}}`, painted `color` (`#rrggbb`), its
  box centred at `x`,`y` and `w`,`h` wide and tall (fractions of the
  frame; a new one is as wide as it is tall on screen). Turn it with the
  layer's rotation, morph it with keyframes like any path. `path` has `pose:{contours:[{closed,vertices:
  [{point:[x,y],incoming:[x,y],outgoing:[x,y]}]}],fill:[r,g,b,a],
  stroke:[r,g,b,a],stroke_width,stroke_glow?:{radius,strength}}`,
  optional `keyframes:[{time_secs,pose,easing}]` and optional
  `trim:{range:{start,end,offset},keyframes:[{time_secs,range,easing}]}`.
  `easing` takes the title easings (`linear`, `out_cubic`, …, the saved
  `Linear`, `OutCubic`… too), `linear` when left out. A stroke drawn on
  over a second: `trim:{keyframes:[{time_secs:0, range:{start:0, end:0,
  offset:0}, easing:linear}, {time_secs:1, range:{start:0, end:1,
  offset:0}, easing:linear}]}`.
  Path `fill` and `stroke` RGBA channels are normalized floats from 0 to 1,
  unlike generator colours, which are bytes from 0 to 255. For example,
  `fill:[0.1,0.2,0.3,1]`, `stroke:[1,1,1,1]`, `stroke_width:2`
  gives an opaque dark fill with a white stroke.
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
