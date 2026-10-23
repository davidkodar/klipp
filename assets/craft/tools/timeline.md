# The timeline, markers, multicam and the edit plan

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as timeline-tools.

## klipp_add_marker

Put a marker on the timeline: how you and the editor leave each other notes. Mark what you are unsure of or want looked at, with a label and a note saying why, and the editor jumps through them. at_secs is timeline seconds; duration_secs makes it a range. color: red, orange, yellow, green, blue, purple; kind: chapter, todo, comment, best_take, section (chapters export as chapter points). Answers with the marker's id.

## klipp_attach_audio

Give a clip the audio of its own media file, linked, spanning exactly what the clip spans. Repairs a clip that was added before appending brought the audio along; refused if the clip already has its audio.

## klipp_build_edit_plan

Build the approved edit plan onto the timeline after whatever is down, as a dry run by default — the reply says what it would build, step by step, and changes nothing. With apply true it is built as one undo step (undo takes it all back). Refused while the plan is unapproved. `detail` adds every step as JSON.

## klipp_create_multicam

Make a multicam group from pool items (two or more angles of one event): synced onto one clock and laid out at the playhead, each angle on tracks of its own, as Create Synced Multicam does. sync is "timecode", "audio" or "timecode_then_audio" (default: timecode where a clip has it, sound for the rest). Seek first to where it should go. The sync runs in the background: read klipp_status (its multicam entry) or klipp_timeline once it is laid out.

## klipp_detect_scene_cuts

Split a clip at the cuts in its picture: a flattened movie, a montage someone exported, a long take with edits in it, back into its shots on the timeline. Runs in the background; the clip is then split at each hard cut, its linked sound with it, as one undo step. Hard cuts are found well; slow dissolves may be missed. Read klipp_timeline after a while for the shots.

## klipp_generator

The `data` argument:

Preview a native generator clip; apply true places it as one undo step. Give at_secs, secs (positive, at most one day), optional video track index, and data in the native generator format: {SolidColor:{color:[r,g,b,a]}}, {Gradient:{top:[r,g,b,a],bottom:[r,g,b,a],angle_deg:0}}, {LivingGradient:{a:[r,g,b,a],b:[r,g,b,a],angle_deg:20,drift_deg_per_sec:4,glow:0.35}}, {Grain:{color:[r,g,b,a],amount:0.35,size_px:2,speed:24}}, {Polygon:{sides:24,color:[r,g,b,a],fill:true}} for a circle, {Placeholder:{label:missing,color:[r,g,b,a]}}, or the string Countdown. Colour channels are bytes 0..255. Omit track to use/create a bottom Background video track. Overlaps and locked tracks are refused.

## klipp_export_timeline

Write the open sequence to an interchange timeline file, for another editor or a round trip through klipp_import_timeline. path is absolute, in a folder that exists, and its extension names the format: .fcpxml (Final Cut Pro X), .edl (CMX 3600), .otio (OpenTimelineIO), .aaf (written through OpenTimelineIO's otioconvert and its AAF adapter, which must be installed) or .kdenlive. format, if given, must match the extension. An existing file is refused unless overwrite is true: ask before replacing one. The reply names the format, the clip count, and in dropped what the format could not carry (OpenTimelineIO and AAF list titles, grades and effects left out); the other formats carry what they can without a list. Not part of a batch.

## klipp_import_timeline

Import a timeline and inspect its report.

Call `klipp_import_timeline` with an absolute `path` to an OpenTimelineIO,
FCPXML, Final Cut Pro 7 XML (xmeml), Kdenlive/MLT or CMX 3600 EDL timeline. The format is told from the
file's content, so a misnamed extension still opens. It appends a sequence to
the current project and switches to it; it does not replace or save the current
project. With `new_project: true` the timeline opens as a new, unsaved project
instead, the way Open Project from Another Editor does; that is refused while the
open project has unsaved changes (save it first). AAF is recognised but not
read yet: the error says to export OpenTimelineIO, FCPXML or an EDL instead. Missing media is listed in the report; relink
it in the app (Relink Media…, or the report's Relink…) or with `klipp_relink_media`.

The response includes the same `report` shown in the app: imported sequence,
track, clip, transition, marker and title counts; warnings grouped by kind with
up to three original examples; all warning details; and missing media ids,
names and paths. Warning counts count notices, not individual effects: one
notice may describe several dropped items. Preserve this distinction when
summarising the import.

If the UI has not finished before the reply, read `klipp_import_timeline` without
`path` until its `request_id` matches the returned id. It then contains `report`
or `error`. Do not submit the import again. A read without `path` never imports.

The report opens after import. File → Last Import Report reopens it, and its
Relink… button opens the existing relink flow for offline media. It records the
state at import; subsequent relinking does not rewrite that historical report.
Saving the project keeps the report for reopening, including Save As.

## klipp_list_clips

Every track and its clips, one line a clip: clip id, kind, range in seconds, name (file or generator, or the clip's own) and media id, with speed, off, graded and effects only where set. A title or caption has its whole `text`, line breaks as `\n`, in both forms: read it back to correct a cue with set_text. Narrow it to a track, a stretch of time or one media item; full adds source ranges, frames, blend, grade group and paths. Each track lists its transitions too: kind, length, the stretch they cover and their two clips. Clip ids from here are what the grade tools take.

## klipp_prepare_cut

Prepare a frame-exact cut in a bounded TIMELINE range. Maps source pauses, word and sentence edges, shot changes, and settled screen reveals through trims and retiming onto sequence frames. Returns ranked candidates with evidence and uncertainty, relevant clips, linked sound, locks, handles, revision and project identity. Give the editing intent and an optional target moment. This only reads; choose a frame, plan the cut, and apply with its revision precondition. If ambiguous, inspect a contact sheet or audio excerpt first.

## klipp_export_subtitles

Write the subtitle clips (the text clips when there are none) to an absolute path, in the format its extension names: .srt, .vtt, .ass, .ssa, .ttml (IMSC), .scc (608) or .stl (EBU). format, if given, must match the extension.

## klipp_update_marker

Change a marker's label, note, colour or kind, by its id from klipp_list_markers. Mark a todo done by changing its kind or colour, rather than removing the note.

## klipp_append_clip

Append a media pool item to the end of the timeline as a new clip. A clip at a frame rate other than the project's is added without changing the project's rate.

## klipp_translate_captions

Translate a subtitle (or text) track's captions into another language onto a new subtitle track under it, at the same times and in the source track's caption style, as one undo step. The new track is in `to`'s language, which klipp_export_subtitles tags its file with. `to` is an ISO 639-1 code: from English (the default `from`) into de, fr, es, sv, it, nl, fi or da, or into en from de, fr, es, sv, it or nl. `track` is the track's index, by default the first subtitle track with captions. It runs as a job: the reply gives `job_id` at once; follow it with klipp_follow_job or klipp_job_status (kind translation, phase downloading, loading, translating or applying), and stop it with klipp_cancel_job. One translation runs at a time. Captions are translated as whole sentences (joined until a full stop, a pause over a second, or about 240 characters) and each sentence's translation is shared back over its captions by length, so a caption may carry other words than its source did. The finished job's `result` names the new track and the captions to check: those that read faster than 20 characters a second, run past two lines of 42, or whose sentence the translator cut short. Each pair's model (OPUS-MT, Helsinki-NLP, CC BY 4.0, about 300 MB) downloads only with `download_models` true; it runs locally.

## klipp_set_append_audio

Source patching: whether appending a clip also brings its audio. On suits camera footage; off is for cutting to music, where every clip would otherwise lay down room tone. Shows as V+A / V only in the timeline toolbar, and resets to on when the app restarts.

## klipp_set_brief

The `brief` argument:

Replace the brief as one undo step. With merge true, keep omitted fields; null clears a field. Fields (all optional): title, about, tone, platform, target_secs, width, height, loudness_lufs (web -14, broadcast -23), captions/music (bool), cast [{name,role,described}], features [{title,description,rank}] (rank 1 headline; plan segments reference titles), arc (e.g. hook/features/CTA), cta, brand_colors ["#rrggbb",...] (first is title default), notes. type_styles: [{name,font?,bold?,size?,color?,fill?,backing?,full_frame?,x?,y?,align?,outline?,outline_color?,glow?,glow_color?,gradient?,animate?,animate_secs?,stagger?,enter?,exit?,loop_effect?,shadow?}] (shadow as on a title: {x?,y?,blur?,color?,opacity?}). Title ops/plan titles select style by name and override its fields. Built-ins heading/callout/card can be replaced. enter/exit use title motion objects; kind none disables. look: {name} preset broadcast_neutral|web_punchy|film_soft|warm_interview|golden|cool_tech|screencast, or black_l,white_l,mid_l,contrast_range,chroma_mean,cast_a,cast_b,skin_hue, and protect: the colours it keeps where they were, any of sky, foliage, skin (all when left out; golden keeps skin only, so it warms a sky); style_profile supplies a reference look_target. vocabulary: correctly spelt names/products/terms for speech recognition; run klipp_analyze after setting. language: ISO 639-1 speech code (en,sv); omitted detects per file. Lower thirds use cast names; klipp_check checks the brief; exports inherit omitted size/loudness. Unknown fields refused.

## klipp_set_edit_plan

The `plan` argument:

Propose the paper edit, as one undo step: the cut said before a clip is placed, so the user can read, change and approve it while changing it is cheap. `plan` is {segments: [...], music?, notes?}. Each segment, in cut order: {media_id, source_in_secs, source_out_secs (stills may hold past their import duration), feature? (a title from the brief's features, as spelt there), note? (what it shows and why — write one), placement? {x?, y?, width?, height?, rotation_deg?, opacity?} uses compose coordinates (equal width and height); background? {generator: solid|gradient|living_gradient|grain|circle|countdown|placeholder|color_bars, colors?: [#rrggbb, ...]} places a backdrop below the picture, taking the brief palette when colors are omitted; with_audio? (the file's own sound; off by default), title? {text, secs?, style? heading|callout|card (card covers the shot by default), x? and y? as frame fractions, full_frame? (false overlays), color?, fill?, glow? 0-1, glow_color?, gradient? [top, bottom], animate? (the title op's), animate_secs?, stagger? char|word|line, enter?, exit? (the title op motion objects)}, transition? ("cross_dissolve", "dip_to_black": how it joins the segment before), zoom? {from?, to, x?, y?}, sfx? {media_id (from search_sfx), at_secs? into the segment, source_in_secs?, secs?, volume_db?}}. Music is {media_id, duck_db?, source_in_secs?, volume_db?, fade_in_secs?, fade_out_secs?}. With snap_to_bars true the bed's beats are read and every segment is rounded to whole bars (half bars under one) and the bed starts on its first beat; the reply says what moved. A segment becomes a scene with layers? [...] over its picture, each timed from the segment's start, later ones on top: {kind: "media", media_id, at_secs, secs?, source_in_secs?, placement? (equal width and height), enter?/exit? {kind: fade|rise|drop|slide_left|slide_right|scale_up, secs?, ease?}} for a card; {kind: "title", at_secs, title} for a title; {kind: "connector", from: [x, y], to: [x, y], at_secs, secs?, color?, stroke_width?, reveal_secs?} for a line drawn on from `from`. secs defaults to the segment's end; stagger cards by their at_secs. The plan text gives each layer's timeline span and where layers overlap; they build on "Scene cards" and "Scene titles" tracks. Give consecutive montage segments section: "montage" (or "montage: name"). These snap to beats instead of bars. Once built from an approved plan, their screen reading minimum is relaxed and short titles use word-count timing. A montage title exactly matching a brief feature counts as its label when feature is omitted; an explicit feature takes precedence. Ground the stretches in klipp_beat_sheet: cut to a settled reveal and hold until it reads; put titles in a hold window. The plan is stored unapproved; the reply says which of the brief's features it leaves out. Show its text to the user and ask.

## klipp_set_multicam_sound

Choose whose sound the multicam group at at_secs (or the playhead) plays. Each angle brings its own sound, and heard together they echo, so a group plays one: an angle counted from 1 (the default is angle 1, the sound a synced group was matched to; pick the lav or the camera nearest the speaker), "follow" to cut the sound with the picture, or "off" when a recorder's track outside the group carries the sound. Playback and export hear the same. klipp_status's multicam entry says which is set.

## klipp_switch_angle

Cut the multicam group in play to an angle, counted from 1 as keys 1-9 are, at at_secs or at the playhead: from there on the program shows that camera, until the next cut. One undo step per cut. klipp_status's multicam entry lists the group's angles, the one in effect and the cut list; klipp_timeline marks each clip's angle and the primary's cuts. Cutting to a transcript: switch to the speaker's angle at each change of speaker.

## klipp_timeline

The timeline as text: tracks top first, every clip with its range in seconds and frames, what it is (media file, title text, generator), its grade and effects in a word, its id, the transitions on each track, and the markers. Read this before planning an edit; klipp_list_clips is the same as JSON with more fields. On a long cut, read one track or a stretch at a time with track, from_secs and to_secs.

## klipp_timeline_sheet

A contact sheet of the timeline as one image: a tile for each shot (each stretch between cuts on the picture tracks), or frames tiles evenly spaced, over a range of it. The look at a whole cut — framing, grade continuity, titles — in one call instead of a frame_at per shot. The reply lists each tile's time and what is on it.

## klipp_apply

Apply a list of editorial operations as ONE undo step, after resolving them the way klipp_plan does; pass its expected_revision and optionally expected_project_identity to refuse the whole edit if the timeline or project changed since planning. Replies with the steps and the revision. Ops: klipp_guide topic ops.

## klipp_list_markers

Every marker on the timeline in time order, with its id, time, range, label, note, colour and kind: the editor's notes to you, and yours to them. Read them at the start of a session: a todo marker is work asked for.

## klipp_frame_at

The frame at a time on the timeline, as a PNG, without leaving the playhead where it was: seeks there, waits for the frame, captures, seeks back. Use it to look at what you did at a specific point. Seconds from timeline start.

## klipp_fonts

The font families a title or a brief's type style can name: those bundled with Klipp, always there, and those installed on this machine. Any other name falls back to Inter; set_brief, plan and apply warn when one is named.

## klipp_screenshot

Capture what the viewer is currently showing, as a PNG. Returns the file path, and inlines the image when it is small enough. This is how to actually see the result of a grade rather than infer it.
