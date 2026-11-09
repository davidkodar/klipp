# Colour, grading and nodes

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as colour-tools.

## klipp_add_node

Add a colour node to a clip's grade graph. Kind is a NodeKind name, e.g. ColorBalance, Corrector, RGBCurves, Qualifier, Mask, MaskCutout, LayerMixer, or ofx:<identifier> for an OFX plug-in; an unknown one is refused with the list. Replies with the new node's node_id. A new node is unconnected — wire it with klipp_connect_nodes.

Arguments:
- `kind`: NodeKind name, e.g. ColorBalance

## klipp_adopt_look

Take a look's numbers off a reference -- a file in the media pool, or the timeline's own best-graded shot -- and make them the target every colour judgement is held to. This is how a grade is briefed on a real job: somebody points at a picture and says "like this". The named presets are the fallback for when there is no such picture, not the normal path. apply false reports what it measured and which preset is nearest; apply true writes it to the brief, and check, grade_to_target and match_scenes all read it from there.

## klipp_colour_selfcheck

Send a published 24-patch ColorChecker through the render path and read it back, to find out whether the path itself moves colour. Run it once before grading to measured targets: if the chart does not come back where it went in, every cast and skin-tone reading is partly a measurement of the pipeline, and a grading loop will chase it for ever. Checks the CPU and GPU paths and holds them against each other, because the viewer shows one and an export may take the other.

## klipp_frame_stats

The scope readings of the frame at a time, the numbers a colourist grades to: the black, mid and white luma points in IRE, the RGB balance and any cast, the saturation, how much is clipped at white and black — and in CIELAB: lightness percentiles, contrast, chroma, the cast on the neutrals (a*, b*, named), and every face read as a grid of skin patches (hue against the 25–70° skin band, chroma, lightness, spread), each judged against the brief's look with the knob that fixes it. Read it before and after a grade — grade to the numbers, not by eye, and match a shot to its neighbours by them.

## klipp_grade_chain

A clip's grade chain in the order the picture flows through it, each node named with the stage it belongs to -- input transform, balance, secondary, look, output -- and whatever the chain does out of that order. A balance sitting after a look is correcting the look's own cast and will never settle; a key pulled before the balance drifts as the balance moves under it. Read this when a grading loop is not converging.

## klipp_grade_to_target

Measure the top picture track or clip against the brief look in CIELAB: black/white/mid lightness, contrast, chroma, neutral cast and each face skin hue. Adjust lift/black, gain/white, exposure/median, temperature+tint/cast and saturation/chroma; remeasure for up to three rounds. Strength limits changes from the starting grade. Revert and stop a shot if sky/foliage/skin hues drift or colours become garish. Preview reports current measurements and first adjustments, grouping shots from one file as a scene. apply true makes one undo step. Screen recordings are checked only for cast/clipping. Follow with klipp_skin_balance for skin and klipp_look for style.

Arguments:
- `strength`: how far each knob may move from where the shot started; gentle unless asked

## klipp_look

Apply a bundled 3D LUT after the grade on a picture clip, as one undo step. Choose category/name or identifying words from klipp_looks; opacity 0-1 defaults to 1; remove clears it. Correct exposure/continuity first; verify with frame_at. on timeline applies after clip grades. on {group} uses the group post-clip grade (create via grade_group); one call updates the whole scene/piece and later changes affect every member.

This standalone tool takes `name` (a bundled name or an absolute `.cube` path), for example `{"clip_id":"CLIP_ID","name":"fuji_xtrans_iii_classic_chrome","opacity":0.25}` with a real timeline clip ID. The `look` op inside klipp_plan/klipp_apply instead requires `lut_path`; its optional `name` is only a display label. It does not resolve library names. See topic `op.look` for a place+look batch using `@0`.

The bundled looks are the LUTs the person sees under Effects → LUTs and in the Colour tab's LUT presets: one they put on a clip there is this look on it, in its grade, and get_grade shows it. The Colour tab's Timeline LUT (a file over the whole picture, exported) and the viewing LUT in Colour Management (on screen only, never exported) are not looks and this tool does not change them.

Arguments:
- `name`: a look name from klipp_looks, or an absolute .cube path
- `on`: instead of clip_id: "timeline", {"group": name, "stage": "post" (default) or "pre"}, or {"track": index or kind, "from"?, "to"?} for every picture clip of a track, with "itself": true for the track's own grade

## klipp_looks

The bundled looks — 3D LUTs — an agent can apply, by category: cinematic and film emulations (B&W, slide, negative, print, instant, Fuji). Read it to see the names klipp_look takes; pass a category to see all of its looks. A creative choice a piece asks for, on top of the grade, never a substitute for grading to the numbers.

Arguments:
- `category`: a category to list in full, e.g. cinematic or film-print

## klipp_match_grade

Match picture clips using scope measurements: adjust target exposure/lift/gain toward reference black/mid/white, read the picture again, then temperature/tint toward its colour balance: brightness first, since colour read off a picture too dark or too bright is off. secs/reference_secs select timeline frames inside each clip; otherwise use the midpoint of each longest detected shot, avoiding cuts. One undo step. It is a first pass and the reply says so: tell the person to compare the shots and fine-tune by eye for an exact match; verify with klipp_check color or klipp_frame_stats, then refine with grade. The same match again changes nothing while the clip and reference still stand: `already_matched`.

Arguments:
- `reference_clip_id`: the shot to match it to
- `reference_secs`: the reference frame to read, timeline seconds inside it
- `secs`: the target frame to read, timeline seconds inside the clip

## klipp_match_scenes

Group the timeline's shots into scenes by how they were lit, pick the shot in each that is already closest to the look as the hero, grade the rest onto it, and report how far each still is. This is the job that fills most of a colourist's day and it is not the same as grading each shot well: two shots can each be defensible alone and still cut badly, because the eye tracks a change across a cut far more readily than an absolute error inside a shot. Matched on the face first, then exposure, then the cast on the neutrals, then contrast. apply false reports; apply true grades, the whole pass as one undo step. A shot a match would not help is left as it was, with `left_as_it_was` saying why: one that comes out no closer, or one that would need more than one lighting setup asks of a shot, as a sunset read beside a daylight street does. Grade those by eye. The same match again changes nothing while every shot still stands: `already_matched`, with the last report. `clip_ids` matches only those shots (two or more on the first video track); without it, the whole timeline.

## klipp_node_types

The node types Klipp's registry describes: each effect's id, name and category, its ports, and its parameters with the key keyframes are stored under, the unit, the default and the accepted range. Read it to know what an effect takes before setting one; pass a category (Colour, BlurDetail, LightGlow, Geometry, FilmTemporal, Key, PlugIn, Grade) to narrow the list.

Arguments:
- `category`: a category to list, e.g. Colour

## klipp_scope

A scope image of a frame, GPU-drawn the way the app's own scopes are: the waveform, the RGB parade, the vectorscope, or the histogram, as a PNG. It reads the frame at secs (the playhead when omitted) without disturbing the scopes the viewer is looking at. The colourist's second eye; frame_stats gives the same frame as numbers to grade to.

Arguments:
- `mode`: waveform, parade, vectorscope, or histogram (waveform by default)
- `path`: Absolute PNG path; temp dir when omitted
- `secs`: timeline seconds; the playhead when omitted

## klipp_set_log_format

Set the camera log (or HDR) encoding a clip was recorded in, for footage whose file metadata does not say (most camera log files do not). Klipp then decodes it to scene-linear for grading and shows it converted to normal contrast. Applies to the clip's media, so every clip of the same file follows. klipp_list_media shows each file's detected and effective format.

Arguments:
- `format`: Srgb for ordinary video; SLog3, VLog, LogC3, CanonLog3, … for camera log

## klipp_set_node_param

Set one numeric parameter on a grade node, e.g. temperature, tint, hue, global_mix, saturation, contrast. Call klipp_get_grade first to see what a node currently carries: node ids belong to one clip, so the id of another clip's Corrector is refused rather than applied.

To ride a value over the shot, key it: give `at`, a time in seconds on the timeline inside the clip, and the value there. Each call adds or updates one key, one undo step; keys on one parameter blend between them. `interpolation` shapes the move at that key: linear (the default), ease_in (slow into it), ease_out (slow out of it), ease_both, bezier, or hold (no move until the next key). `remove: true` with `at` takes that key away. Once a parameter has keys, its plain value no longer shows: change it by keying. A colour wheel is three parameters (lift_x, lift_y, lift_master): key all three at a time to move the wheel. klipp_get_grade reports each node's keys.

Arguments:
- `at`: Key it at this timeline second
- `node_id`: From klipp_get_grade for THIS clip
- `param`: Parameter name, e.g. temperature
- `remove`: Take the key at `at` away

## klipp_read_chart

A ColorChecker Classic in the shot, read against its published values (in the app, the Chart button in Colour Balance on the Colour page, with corners to drag in the viewer): give the four outer corners of its patches, in frame fractions, in order round the chart starting at the brown patch (read them with klipp_frame_at). It reads all 24 patches through the perspective the corners give, and says how far each sits from its published value as mean and worst ΔE2000, the neutral grey's a* b* and L*, and the temperature, tint and exposure it asks for. With apply true it sets them on top of the clip's grade, measured after each of up to two rounds, as one undo step, and says the error before and after; exposure false leaves exposure alone. It sets white balance and exposure only, from the chart's greys, so colour patches can stay off: that is the camera's own colour. Corners given from the far end are read the right way round and said to be. Refused when the corners do not read as a chart, whose six greys must run white to black along one long side beside coloured patches, or when it is too small to read. A chart of another make, whose greys read as a ramp and whose colours are not a ColorChecker's, is said to be: only the white balance from its greys is set, no exposure, and no ΔE is quoted. Give its corners so that the greys fall on the long side the ColorChecker's do, white first: the corner by the far colour patch at the white end, then along that side. An unconverted log or flat picture is said to be: apply the camera's conversion first.

Arguments:
- `clip_id`: From klipp_list_clips
- `corners`: Four [x, y], 0 to 1 across and down the frame
- `secs`: Timeline second, inside the clip; else its middle
- `apply`: Set the correction (default false, read only)
- `exposure`: false leaves exposure alone

## klipp_pick_grey

White balance from a point you know is grey, as the Colour page's eyedropper in Colour Balance does: `x` and `y` are fractions of the frame, read with klipp_frame_at, at `secs` on the timeline or the clip's middle. Temperature and tint go on top of the clip's grade, as one undo step, and the reply says what the point reads after. This is the exact way when klipp_match_grade or an automatic balance cannot tell a grey from a coloured surface, and exact only if the point really is grey: a white or grey that is not blown out, not a surface that only looks neutral. Refused on a point clipped white or too dark to read.

Arguments:
- `clip_id`: From klipp_list_clips
- `x`, `y`: The point, 0 to 1 across and down the frame
- `secs`: Timeline second, inside the clip

## klipp_skin_balance

Skin onto the skin band, by measurement: the faces in the clip's frame (the framing analysis's people, the head of each) are read as a grid of skin patches in CIELAB (8×8 for a face that fills the frame, down to 3×3 for a small one); the face's median hue is held to the band 25–70° that real skin of every kind sits in as photographed, aiming at 45°. With apply true a skin secondary (a qualifier keyed on skin, a hue shift) turns the hue onto the band, measured again after each of up to three rounds. Refused when the face is lit by two colours of light; when the scene's neutrals carry the same cast the reply says to white balance first. Read a face at any time with klipp_frame_stats.

## klipp_track_mask

Follow a mask on a clip's grade from the frame under the playhead (klipp_seek onto the clip first), both ways through the clip, in the background. It starts a job: follow `job_id` with klipp_follow_job or klipp_job_status. The job's result gives `followed_secs` (from, to, on the clip's own clock), `clip_secs`, `lost`, and `text` in words. `lost` true means the track stopped early because the picture no longer matched where it started; it stops rather than guess, and the mask stays still outside that stretch, so check the result rather than assume the shot is covered. A re-track on a plain area stops at the same place: move the mask onto something with detail, or track from another frame. The mask's keys are written when the job ends, as the Track button writes them. It follows only what you give it: it does not find a subject. Check a frame with klipp_frame_at afterwards.

Arguments:
- `mask_index`: Which shape on the node; else the first
- `node_id`: The Mask node; else the first with a shape
- `perspective`: Follow four corners on a flat surface, not a move, size and turn

## klipp_track_surface

Track a planar surface in perspective for the pin op. Supply click or corners (see fields), measured with klipp_frame_at at at_secs. A click finds the four-sided surface round it with a straight edge along every side that also moves as one plane over the next frames, and the reply gives its corners (check them with klipp_frame_at: a surface's outer edge is found, a screen's bezel included); or it refuses with why (no closed region with straight edges round the click, or the region doesn't move as one flat surface): then give corners. The whole surface is followed, its outline included, so a plain surface (a single-colour canopy, a wall) tracks: put the corners on its edges. Runs asynchronously as one undo step; klipp_list_clips reports the surface id, the tracked range, its mean confidence (0-1) and the stretches followed uncertainly (`low`) or lost (`lost`: the last good place is held there). After a loss the surface is searched for and picked up again when it shows. Check frames in low and lost stretches with klipp_frame_at; correct a frame with klipp_surface_key, which keeps the rest of the track, or continue a track lost at its end by giving corners on a later frame with the same surface and direction forward/backward. The new range replaces overlapping tracking and preserves the rest. A larger area helps only when more of the same flat plane is in view round the surface (the whole wall round a small sign, a screen's housing): follow it with `track_corners`, its four corners in frame fractions like `corners`; the surface stays where `corners` put it on that plane. Another face of the same object is another plane (a boat's hull for its canopy) and drifts, and an area mostly out of frame holds nothing. A plain stripe or canopy is tracked by itself, its corners on its edges, and corrected with klipp_surface_key across what passes in front of it. `exclude` leaves parts of the area out: polygons of three or more [x,y] points, on the area but not of the plane (a screen playing in a wall, a window's reflection); they move with the surface. A surface tracked again or continued keeps its own track area and exclusions when these are left out; `exclude: []` clears them. Things passing in front need no exclusion: the tracker discounts them. A plain surface such as a blank screen has only its outline to prove a fit, so its frames read `low` unless every side of the outline lines up; tracking marks on it, or corrections at weak frames, help most. A plain surface that doesn't hold is tried again by itself with more of what is round it, which takes longer; the track can then go on after a loss where it stopped before.

Arguments:
- `at_secs`: Timeline second of the corners; else the playhead
- `click`: [x,y] inside the surface, frame fractions; instead of corners
- `corners`: [[x,y] TL, TR, BR, BL], frame fractions
- `direction`: One way only, merged into its track; needs surface
- `exclude`: Polygons [[x,y],...] to leave out; [] clears
- `name`: Its name, e.g. Phone screen
- `surface`: Id or name of a surface to track again
- `track_corners`: Corners of a larger area of the plane to follow

## klipp_measure_water

Measure the water under a clip's reflection again (#2795): the footage below the reflected clip is rendered at three moments and the water's colour read just below the waterline, glints and ripple troughs left out. Where the reflection takes its tint and darkness from the water (`tint_auto`, `darken_auto`; on for the water surfaces), they follow the measurement, the darkness kept within bounds. A reflection is measured once on its own when added; after the footage under it, its range or its waterline changes, klipp_list_clips shows its `water` as `outdated` and this measures it again. One undo step.

## klipp_surface_key

Correct a tracked surface where klipp_list_clips shows it `low` or `lost`, or where klipp_frame_at shows it off the surface: give its corners on that frame (frame fractions, as track_surface's `corners`, measured with klipp_frame_at at at_secs) and the surface's id or name. That frame is held as given, set by hand, and the track either side is solved again from it as far as the next anchor each way (the reference frame or another correction): forward and backward, blended by confidence and nearness, so frames near a correction follow it. The rest of the track is kept. Runs in the background as one undo step; klipp_list_clips lists the surface's `corrections` (timeline seconds) and what is still low or lost. Work from the worst stretch: correct a frame in it, check, and add another correction only where it is still off. To start over from new corners instead, track_surface with the surface replaces the whole track.

Arguments:
- `at_secs`: Timeline second; else the playhead
- `corners`: Where it is on that frame, as track_surface's corners
- `surface`: Id or name of the tracked surface

## klipp_get_grade

The clip's colour node graph as JSON, including every node id and its parameters, and what runs with it on the picture: its grade group's stages, its track's grade and the timeline grade. Read this before changing a grade — node ids come from here.

## klipp_measure_region

One box of the frame read on its own, in the same numbers as klipp_frame_stats: a face against its background, a sky, a product, a corner of a screen. x, y, w and h are fractions of the frame. Use it when the whole-frame reading is the average of two different things.
