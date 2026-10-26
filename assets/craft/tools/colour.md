# Colour, grading and nodes

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as colour-tools.

## klipp_add_node

Add a colour node to a clip's grade graph. Kind is a NodeKind name, e.g. ColorBalance, Corrector, RGBCurves, Qualifier, Mask, MaskCutout, LayerMixer, or ofx:<identifier> for an OFX plug-in; an unknown one is refused with the list. Replies with the new node's node_id. A new node is unconnected — wire it with klipp_connect_nodes.

## klipp_adopt_look

Take a look's numbers off a reference -- a file in the media pool, or the timeline's own best-graded shot -- and make them the target every colour judgement is held to. This is how a grade is briefed on a real job: somebody points at a picture and says "like this". The named presets are the fallback for when there is no such picture, not the normal path. apply false reports what it measured and which preset is nearest; apply true writes it to the brief, and check, grade_to_target and match_scenes all read it from there.

## klipp_colour_selfcheck

Send a published 24-patch ColorChecker through the render path and read it back, to find out whether the path itself moves colour. Run it once before grading to measured targets: if the chart does not come back where it went in, every cast and skin-tone reading is partly a measurement of the pipeline, and a grading loop will chase it for ever. Checks the CPU and GPU paths and holds them against each other, because the viewer shows one and an export may take the other.

## klipp_frame_stats

The scope readings of the frame at a time, the numbers a colourist grades to: the black, mid and white luma points in IRE, the RGB balance and any cast, the saturation, how much is clipped at white and black — and in CIELAB: lightness percentiles, contrast, chroma, the cast on the neutrals (a*, b*, named), and every face read as a grid of skin patches (hue against the 40–65° skin band, chroma, lightness, spread), each judged against the brief's look with the knob that fixes it. Read it before and after a grade — grade to the numbers, not by eye, and match a shot to its neighbours by them.

## klipp_grade_chain

A clip's grade chain in the order the picture flows through it, each node named with the stage it belongs to -- input transform, balance, secondary, look, output -- and whatever the chain does out of that order. A balance sitting after a look is correcting the look's own cast and will never settle; a key pulled before the balance drifts as the balance moves under it. Read this when a grading loop is not converging.

## klipp_grade_to_target

Measure the top picture track or clip against the brief look in CIELAB: black/white/mid lightness, contrast, chroma, neutral cast and each face skin hue. Adjust lift/black, gain/white, exposure/median, temperature+tint/cast and saturation/chroma; remeasure for up to three rounds. Strength limits changes from the starting grade. Revert and stop a shot if sky/foliage/skin hues drift or colours become garish. Preview reports current measurements and first adjustments, grouping shots from one file as a scene. apply true makes one undo step. Screen recordings are checked only for cast/clipping. Follow with klipp_skin_balance for skin and klipp_look for style.

## klipp_look

Apply a bundled 3D LUT after the grade on a picture clip, as one undo step. Choose category/name or identifying words from klipp_looks; opacity 0-1 defaults to 1; remove clears it. Correct exposure/continuity first; verify with frame_at. on timeline applies after clip grades. on {group} uses the group post-clip grade (create via grade_group); one call updates the whole scene/piece and later changes affect every member.

This standalone tool takes `name` (a bundled name or an absolute `.cube` path), for example `{"clip_id":"CLIP_ID","name":"fuji_xtrans_iii_classic_chrome","opacity":0.25}` with a real timeline clip ID. The `look` op inside klipp_plan/klipp_apply instead requires `lut_path`; its optional `name` is only a display label. It does not resolve library names. See topic `op.look` for a place+look batch using `@0`.

## klipp_looks

The bundled looks — 3D LUTs — an agent can apply, by category: cinematic and film emulations (B&W, slide, negative, print, instant, Fuji). Read it to see the names klipp_look takes; pass a category to see all of its looks. A creative choice a piece asks for, on top of the grade, never a substitute for grading to the numbers.

## klipp_match_grade

Match picture clips using scope measurements: adjust target exposure/lift/gain/temperature/tint toward reference black/mid/white and colour balance. secs/reference_secs select timeline frames inside each clip; otherwise use the midpoint of each longest detected shot, avoiding cuts. One undo step; verify this first pass with klipp_check color or klipp_frame_stats, then refine with grade.

## klipp_match_scenes

Group the timeline's shots into scenes by how they were lit, pick the shot in each that is already closest to the look as the hero, grade the rest onto it, and report how far each still is. This is the job that fills most of a colourist's day and it is not the same as grading each shot well: two shots can each be defensible alone and still cut badly, because the eye tracks a change across a cut far more readily than an absolute error inside a shot. Matched on the face first, then exposure, then the cast on the neutrals, then contrast. apply false reports; apply true grades, the whole pass as one undo step.

## klipp_node_types

The node types Klipp's registry describes: each effect's id, name and category, its ports, and its parameters with the key keyframes are stored under, the unit, the default and the accepted range. Read it to know what an effect takes before setting one; pass a category (Colour, BlurDetail, LightGlow, Geometry, FilmTemporal, Key, PlugIn, Grade) to narrow the list.

## klipp_scope

A scope image of a frame, GPU-drawn the way the app's own scopes are: the waveform, the RGB parade, the vectorscope, or the histogram, as a PNG. It reads the frame at secs (the playhead when omitted) without disturbing the scopes the viewer is looking at. The colourist's second eye; frame_stats gives the same frame as numbers to grade to.

## klipp_set_log_format

Set the camera log (or HDR) encoding a clip was recorded in, for footage whose file metadata does not say (most camera log files do not). Klipp then decodes it to scene-linear for grading and shows it converted to normal contrast. Applies to the clip's media, so every clip of the same file follows. klipp_list_media shows each file's detected and effective format.

## klipp_set_node_param

Set one numeric parameter on a grade node, e.g. temperature, tint, hue, global_mix, saturation, contrast. Call klipp_get_grade first to see what a node currently carries: node ids belong to one clip, so the id of another clip's Corrector is refused rather than applied.

To ride a value over the shot, key it: give `at`, a time in seconds on the timeline inside the clip, and the value there. Each call adds or updates one key, one undo step; keys on one parameter blend between them. `interpolation` shapes the move at that key: linear (the default), ease_in (slow into it), ease_out (slow out of it), ease_both, bezier, or hold (no move until the next key). `remove: true` with `at` takes that key away. Once a parameter has keys, its plain value no longer shows: change it by keying. A colour wheel is three parameters (lift_x, lift_y, lift_master): key all three at a time to move the wheel. klipp_get_grade reports each node's keys.

## klipp_skin_balance

Skin onto the skin band, by measurement: the faces in the clip's frame (the framing analysis's people, the head of each) are read as an 8×8 grid of skin patches in CIELAB; the face's median hue is held to the band 40–65° that real skin of every kind sits in. With apply true a skin secondary (a qualifier keyed on skin, a hue shift) turns the hue onto the band, measured again after each of up to three rounds. Refused when the face is lit by two colours of light; when the scene's neutrals carry the same cast the reply says to white balance first. Read a face at any time with klipp_frame_stats.

## klipp_track_surface

Track a planar surface in perspective for the pin op. Supply click or corners (see fields), measured with klipp_frame_at at at_secs. A click finds the four-sided surface round it with a straight edge along every side that also moves as one plane over the next frames, and the reply gives its corners (check them with klipp_frame_at: a surface's outer edge is found, a screen's bezel included); or it refuses with why (no closed region with straight edges round the click, or the region doesn't move as one flat surface): then give corners. The whole surface is followed, its outline included, so a plain surface (a single-colour canopy, a wall) tracks: put the corners on its edges. Runs asynchronously as one undo step; klipp_list_clips reports the surface id, the tracked range, its mean confidence (0-1) and the stretches followed uncertainly (`low`) or lost (`lost`: the last good place is held there). After a loss the surface is searched for and picked up again when it shows. Check frames in low and lost stretches with klipp_frame_at; correct a frame with klipp_surface_key, which keeps the rest of the track, or continue a track lost at its end by giving corners on a later frame with the same surface and direction forward/backward. The new range replaces overlapping tracking and preserves the rest. A larger area helps only when more of the same flat plane is in view round the surface (the whole wall round a small sign, a screen's housing): follow it with `track_corners`, its four corners in frame fractions like `corners`; the surface stays where `corners` put it on that plane. Another face of the same object is another plane (a boat's hull for its canopy) and drifts, and an area mostly out of frame holds nothing. A plain stripe or canopy is tracked by itself, its corners on its edges, and corrected with klipp_surface_key across what passes in front of it. `exclude` leaves parts of the area out: polygons of three or more [x,y] points, on the area but not of the plane (a screen playing in a wall, a window's reflection); they move with the surface. A surface tracked again or continued keeps its own track area and exclusions when these are left out; `exclude: []` clears them. Things passing in front need no exclusion: the tracker discounts them.

## klipp_surface_key

Correct a tracked surface where klipp_list_clips shows it `low` or `lost`, or where klipp_frame_at shows it off the surface: give its corners on that frame (frame fractions, as track_surface's `corners`, measured with klipp_frame_at at at_secs) and the surface's id or name. That frame is held as given, set by hand, and the track either side is solved again from it as far as the next anchor each way (the reference frame or another correction): forward and backward, blended by confidence and nearness, so frames near a correction follow it. The rest of the track is kept. Runs in the background as one undo step; klipp_list_clips lists the surface's `corrections` (timeline seconds) and what is still low or lost. Work from the worst stretch: correct a frame in it, check, and add another correction only where it is still off. To start over from new corners instead, track_surface with the surface replaces the whole track.

## klipp_get_grade

The clip's colour node graph as JSON, including every node id and its parameters, and what runs with it on the picture: its grade group's stages, its track's grade and the timeline grade. Read this before changing a grade — node ids come from here.

## klipp_measure_region

One box of the frame read on its own, in the same numbers as klipp_frame_stats: a face against its background, a sky, a product, a corner of a screen. x, y, w and h are fractions of the frame. Use it when the whole-frame reading is the average of two different things.
