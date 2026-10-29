# The timeline, markers, multicam and the edit plan

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as timeline-tools.

## klipp_add_marker

Put a marker on the timeline: how you and the editor leave each other notes. Mark what you are unsure of or want looked at, with a label and a note saying why, and the editor jumps through them. at_secs is timeline seconds; duration_secs makes it a range. color: red, orange, yellow, green, blue, purple; kind: chapter, todo, comment, best_take, section (chapters export as chapter points). Answers with the marker's id.

## klipp_attach_audio

Give a clip the audio of its own media file, linked, spanning exactly what the clip spans. Repairs a clip that was added before appending brought the audio along; refused if the clip already has its audio.

Arguments:
- `clip_id`: From klipp_list_clips

## klipp_build_edit_plan

Build the approved edit plan onto the timeline after whatever is down, as a dry run by default — the reply says what it would build, step by step, and changes nothing. With apply true it is built as one undo step (undo takes it all back). Refused while the plan is unapproved. `detail` adds every step as JSON.

## klipp_create_multicam

Make a multicam group from pool items (two or more angles of one event): synced onto one clock and laid out at the playhead, each angle on tracks of its own, as Create Synced Multicam does. sync is "timecode", "audio" or "timecode_then_audio" (default: timecode where a clip has it, sound for the rest). Seek first to where it should go. The sync runs in the background: read klipp_status (its multicam entry) or klipp_timeline once it is laid out.

Arguments:
- `media_ids`: the angles, from klipp_list_media

## klipp_detect_scene_cuts

Split a clip at the cuts in its picture: a flattened movie, a montage someone exported, a long take with edits in it, back into its shots on the timeline. Runs in the background; the clip is then split at each hard cut, its linked sound with it, as one undo step. Hard cuts are found well; slow dissolves may be missed. Read klipp_timeline after a while for the shots.

## klipp_generator

The `data` argument:

Preview a native generator clip; apply true places it as one undo step. Give at_secs, secs (positive, at most one day), optional video track index, and data in the native generator format, one of these 14 kinds (colour channels are bytes 0..255, `[r,g,b,a]`):

- `{SolidColor:{color}}`: one colour.
- `{Gradient:{top,bottom,angle_deg?,grain?}}`: two colours across the frame at angle_deg (0 top to bottom); `four_point:{top_left,top_right,bottom_left,bottom_right}` in place of top and bottom blends four corners. grain 0 to its maximum, a little by default.
- `{LivingGradient:{a,b,angle_deg,drift_deg_per_sec,glow,grain?}}`: a gradient that drifts slowly, with a soft glow.
- `{Grain:{color,amount,size_px,speed}}`: a moving grain over a colour.
- `{Polygon:{sides,color,outline_width_px?,fill?}}`: a regular polygon in the middle of the frame; sides 64 or more is a true circle, smooth at any size; fill false with an outline_width_px draws only its outline (a ring for the circle).
- `{Star:{points,inner_radius_pct,color,outline_width_px?,fill?}}`: a star of points, its inner corners at inner_radius_pct (0.05 to 0.95) of the outer.
- `{Placeholder:{label,color}}`: a labelled stand-in for a shot still to come.
- `"ColorBars"`, `"ColorBars100"`: the seven SMPTE bars, at 75 % or 100 %.
- `"ColorBarsSmpte"`: the full SMPTE pattern, the 75 % bars over the castellations and the −I / white / +Q / PLUGE row, for setting a monitor's black and white.
- `"Countdown"`: a leader counting down the seconds.
- `{Timer:{mode,initial_seconds,format,text_color,background_color}}`: a clock, mode CountUp or CountDown from initial_seconds, format Seconds, MmSs, HhMmSs or HhMmSsMs; its digits fit the frame's width in any aspect.
- `{Slate:{project,scene,take,director,camera,date,framerate,notes,text_color,background_color}}`: a production slate, each field text, laid out in label and value columns that fill the frame in any aspect; empty fields are left out.
- `{StillFrame:{png_path,width,height}}`: a still from a PNG file (absolute path) of that size, fitted inside the frame at its own aspect, the rest clear.

Omit track to use/create a bottom Background video track. Overlaps and locked tracks are refused.

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

Arguments:
- `from_secs`: Only clips that end after this
- `full`: Every field (default false)
- `media_id`: Only clips of this media item
- `to_secs`: Only clips that start before this
- `track`: A track's index or name

## klipp_prepare_cut

Prepare a frame-exact cut in a bounded TIMELINE range. Maps source pauses, word and sentence edges, shot changes, and settled screen reveals through trims and retiming onto sequence frames. Returns ranked candidates with evidence and uncertainty, relevant clips, linked sound, locks, handles, revision and project identity. Give the editing intent and an optional target moment. This only reads; choose a frame, plan the cut, and apply with its revision precondition. If ambiguous, inspect a contact sheet or audio excerpt first.

## klipp_export_subtitles

Write the subtitle clips (the text clips when there are none) to an absolute path, in the format its extension names: .srt, .vtt, .ass, .ssa, .ttml (IMSC), .scc (608) or .stl (EBU). format, if given, must match the extension.

## klipp_update_marker

Change a marker's label, note, colour or kind, by its id from klipp_list_markers. Mark a todo done by changing its kind or colour, rather than removing the note.

## klipp_append_clip

Append a media pool item to the end of the timeline as a new clip. A clip at a frame rate other than the project's is added without changing the project's rate.

Arguments:
- `media_id`: From klipp_list_media
- `track`: Track index; omit for the default track

## klipp_translate_captions

Translate a subtitle (or text) track's captions into another language onto a new subtitle track under it, at the same times and in the source track's caption style, as one undo step. The new track is in `to`'s language, which klipp_export_subtitles tags its file with. `to` is an ISO 639-1 code: from English into de, fr, es, sv, it, nl, fi or da, or into en from de, fr, es, sv, it or nl. `from` is the captions' language: by default the track's own language tag (set_track language), else English. `track` is the track's index, by default the first subtitle track with captions. It runs as a job: the reply gives `job_id` at once; follow it with klipp_follow_job or klipp_job_status (kind translation, phase downloading, loading, translating or applying), and stop it with klipp_cancel_job. One translation runs at a time. Captions are translated as whole sentences (joined until a full stop, a pause over a second, or about 240 characters) and each sentence's translation is shared back over its captions by length, so a caption may carry other words than its source did. The finished job's `result` names the new track and the captions to check: those that read faster than 20 characters a second, run past two lines of 42, or whose sentence the translator cut short. The brief's cast names and vocabulary are kept as written, and `glossary` (an object, source term to its translation) sets how a recurring term comes out, e.g. {"application": "program"}; terms match whole words in any case, each form given on its own. A caption whose kept term the model dropped is among those to check. A sentence that comes back with under half its words is translated again a clause at a time, and the fuller one kept. Each pair's model (OPUS-MT, Helsinki-NLP, CC BY 4.0, about 300 MB) downloads only with `download_models` true; it runs locally.

Arguments:
- `from`: default track language, else en
- `to`: de fr es sv it nl fi da, or en

## klipp_set_append_audio

Source patching: whether appending a clip also brings its audio. On suits camera footage; off is for cutting to music, where every clip would otherwise lay down room tone. Shows as V+A / V only in the timeline toolbar, and resets to on when the app restarts.

Arguments:
- `enabled`: true brings audio, false is video only

## klipp_set_brief

The `brief` argument:

Replace the brief as one undo step. With merge true, keep omitted fields; null clears a field. Fields (all optional): title, about, tone, platform, target_secs, width, height, loudness_lufs (web -14, broadcast -23), captions/music (bool), cast [{name,role,described}], features [{title,description,rank}] (rank 1 headline; plan segments reference titles), arc (e.g. hook/features/CTA), cta, brand_colors ["#rrggbb",...] (first is title default), notes. type_styles: [{name,font?,bold?,size?,color?,fill?,backing?,full_frame?,x?,y?,align?,outline?,outline_color?,glow?,glow_color?,gradient?,animate?,animate_secs?,stagger?,enter?,exit?,loop_effect?,shadow?,material?}] (shadow as on a title: {x?,y?,blur?,color?,opacity?}; material a metal's name: gold, silver, steel, copper, rose_gold, bronze, chrome, brushed_metal). Title ops/plan titles select style by name and override its fields. Built-ins heading/callout/card can be replaced. enter/exit use title motion objects; kind none disables. look: {name} preset broadcast_neutral|web_punchy|film_soft|warm_interview|golden|cool_tech|screencast, or black_l,white_l,mid_l,contrast_range,chroma_mean,cast_a,cast_b,skin_hue, and protect: the colours it keeps where they were, any of sky, foliage, skin (all when left out; golden keeps skin only, so it warms a sky); style_profile supplies a reference look_target. vocabulary: correctly spelt names/products/terms for speech recognition; run klipp_analyze after setting. language: ISO 639-1 speech code (en,sv); omitted detects per file. Lower thirds use cast names; klipp_check checks the brief; exports inherit omitted size/loudness. Unknown fields refused.

Arguments:
- `merge`: change only the fields given; the rest stay

## klipp_set_edit_plan

The `plan` argument:

Propose the paper edit, as one undo step: the cut said before a clip is placed, so the user can read, change and approve it while changing it is cheap. `plan` is {segments: [...], music?, notes?}. Each segment, in cut order: {media_id, source_in_secs, source_out_secs (stills may hold past their import duration), feature? (a title from the brief's features, as spelt there), note? (what it shows and why — write one), placement? {x?, y?, width?, height?, rotation_deg?, opacity?} uses compose coordinates; background? {generator: solid|gradient|living_gradient|grain|circle|countdown|placeholder|color_bars, colors?: [#rrggbb, ...]} places a backdrop below the picture, taking the brief palette when colors are omitted; with_audio? (the file's own sound; off by default), title? {text, secs?, style? heading|callout|card (card covers the shot by default), x? and y? as frame fractions, full_frame? (false overlays), color?, fill?, glow? 0-1, glow_color?, gradient? [top, bottom], animate? (the title op's), animate_secs?, stagger? char|word|line, enter?, exit? (the title op motion objects)}, transition? ("cross_dissolve", "dip_to_black": how it joins the segment before), zoom? {from?, to, x?, y?}, sfx? {media_id (from search_sfx), at_secs? into the segment, source_in_secs?, secs?, volume_db?}}. Music is {media_id, duck_db?, source_in_secs?, volume_db?, fade_in_secs?, fade_out_secs?}. With snap_to_bars true the bed's beats are read and every segment is rounded to whole bars (half bars under one) and the bed starts on its first beat; the reply says what moved. A segment becomes a scene with layers? [...] over its picture, each timed from the segment's start, later ones on top: {kind: "media", media_id, at_secs, secs?, source_in_secs?, placement? (compose coordinates), enter?/exit? {kind: fade|rise|drop|slide_left|slide_right|scale_up, secs?, ease?}} for a card; {kind: "title", at_secs, title} for a title; {kind: "connector", from: [x, y], to: [x, y], at_secs, secs?, color?, stroke_width?, reveal_secs?} for a line drawn on from `from`. secs defaults to the segment's end; stagger cards by their at_secs. The plan text gives each layer's timeline span and where layers overlap; they build on "Scene cards" and "Scene titles" tracks. Give consecutive montage segments section: "montage" (or "montage: name"). These snap to beats instead of bars. Once built from an approved plan, their screen reading minimum is relaxed and short titles use word-count timing. A montage title exactly matching a brief feature counts as its label when feature is omitted; an explicit feature takes precedence. Ground the stretches in klipp_beat_sheet: cut to a settled reveal and hold until it reads; put titles in a hold window. The plan is stored unapproved; the reply says which of the brief's features it leaves out. Show its text to the user and ask.

## klipp_set_multicam_sound

Choose whose sound the multicam group at at_secs (or the playhead) plays. Each angle brings its own sound, and heard together they echo, so a group plays one: an angle counted from 1 (the default is angle 1, the sound a synced group was matched to; pick the lav or the camera nearest the speaker), "follow" to cut the sound with the picture, or "off" when a recorder's track outside the group carries the sound. Playback and export hear the same. klipp_status's multicam entry says which is set.

Arguments:
- `at_secs`: timeline seconds; the playhead when left out
- `sound`: an angle number from 1, "follow" or "off"

## klipp_switch_angle

Cut the multicam group in play to an angle, counted from 1 as keys 1-9 are, at at_secs or at the playhead: from there on the program shows that camera, until the next cut. One undo step per cut. klipp_status's multicam entry lists the group's angles, the one in effect and the cut list; klipp_timeline marks each clip's angle and the primary's cuts. Cutting to a transcript: switch to the speaker's angle at each change of speaker.

Arguments:
- `at_secs`: timeline seconds; the playhead when left out

## klipp_timeline

The timeline as text: tracks top first, every clip with its range in seconds and frames, what it is (media file, title text, generator), its grade and effects in a word, its id, the transitions on each track, and the markers. Read this before planning an edit; klipp_list_clips is the same as JSON with more fields. On a long cut, read one track or a stretch at a time with track, from_secs and to_secs.

Arguments:
- `from_secs`: Only what plays from here on the timeline
- `max_chars`: Cut on a line boundary at this many characters (default 12000)
- `to_secs`: Only what plays before here
- `track`: One track only, by index (0 is the top)

## klipp_timeline_sheet

A contact sheet of the timeline as one image: a tile for each shot (each stretch between cuts on the picture tracks), or frames tiles evenly spaced, over a range of it. The look at a whole cut — framing, grade continuity, titles — in one call instead of a frame_at per shot. The reply lists each tile's time and what is on it.

Arguments:
- `cols`: Tiles to a row (default 6)
- `frames`: Evenly spaced tiles instead of one per shot (1-60)
- `inline`: Also return the picture in the reply; by default only when no path is given
- `path`: Absolute PNG path; temp dir when omitted

## klipp_apply

Apply a list of editorial operations as ONE undo step, after resolving them the way klipp_plan does; pass its expected_revision and optionally expected_project_identity to refuse the whole edit if the timeline or project changed since planning. Replies with the steps and the revision. Ops: klipp_guide topic ops.

The reply, of klipp_plan and klipp_apply alike, is the plan as text with long runs of one kind of step folded; `detail` true gives every step in full and each touched clip's range as JSON.

Arguments:
- `detail`: every step as JSON; see topic apply
- `expected_project_identity`: from klipp_plan; refuse if the project changed
- `expected_revision`: Revision from klipp_plan or a timeline read; refuse if it changed
- `label`: A name for the history entry

## klipp_list_markers

Every marker on the timeline in time order, with its id, time, range, label, note, colour and kind: the editor's notes to you, and yours to them. Read them at the start of a session: a todo marker is work asked for.

## klipp_frame_at

The frame at a time on the timeline, as a PNG, without leaving the playhead where it was: seeks there, waits for the frame, captures, seeks back. Use it to look at what you did at a specific point. Seconds from timeline start.

Left out, max_width gives the viewer's frame, at most 1280 wide; the viewer decodes at the playback size, which can be smaller. A max_width wider than the viewer's frame is drawn from the files at that width, up to the timeline's, as the export draws it, for judging small type and fine detail at full size; the reply's drawn says so.

channel shows one channel of the frame in grey: red, green or blue of the picture as it shows, alpha (black where nothing covers the frame, white where it is opaque) or luma. node shows a node of a clip's grade or of its chain (an effect after the grade, a merge) in place of the finished frame, its picture as it leaves that node, by its path as graph_read gives it ({root: {clip}, nodes: [...]}); a node beside the chain is refused. channel alpha checks a key or a matte directly: with nothing below the keyed clip, a chroma key over its screen is black on the screen and white on the subject. Both are for that one frame: the viewer's own channel and node are put back after, and the frame is the viewer's size.

Arguments:
- `path`: Absolute PNG path; temp dir when omitted

## klipp_fonts

The font families a title or a brief's type style can name: those bundled with Klipp, always there, and those installed on this machine. Any other name falls back to Inter; set_brief, plan and apply warn when one is named. The text names the bundled families and counts the installed ones, which are listed in installed; query (a word, e.g. "serif" or "mono") lists only the families whose name holds it, and the catalogue families, fetched or not, whose tags have every word of it, after the name matches; matched_tags says, for each family found by its tags, the tags it was found by.

Klipp's catalogue holds about seventy more open-licence families (OFL, Apache), fetched on request: engraved and high-contrast serifs (Cinzel, Bodoni Moda, Playfair Display), condensed news sans (Oswald, Roboto Condensed, Barlow Condensed), rounded and friendly sans (Nunito, Fredoka), scripts and handwriting (Great Vibes, Caveat, Permanent Marker), monospace (IBM Plex Mono), and three or more families each, in serif, sans, display and handwritten styles where the writing has them, for Cyrillic, Greek, Arabic, Hebrew, Devanagari, Bengali, Tamil, Thai, Chinese, Japanese and Korean (query the script's name: "arabic serif", "japanese handwritten"). catalogue lists each with its tags: style (what it is), mood (what it suits) and scripts (the writing it covers), its size and whether it is fetched. query matches them by name or by every word being a tag, so "condensed news sans", "wedding script" or "japanese" picks families. fetch: name downloads one (checked against its SHA-256, its licence kept beside it, in the user's data folder); then a title is set in it by its name. A plan that names a catalogue family not fetched yet says so, with the fetch to make. A project opened whose titles name catalogue families fetches them in the background.

Arguments:
- `fetch`: a catalogue family to fetch

## klipp_brand_kit

A brand kit keeps a creator's look once for every project: `colours` (`primary`, the colour titles take, `secondary`, `accent`, each `#rrggbb`), `fonts` (a `heading` and a `body` family: bundled, installed or in the font catalogue), `type_styles` (named title looks as the brief holds them), `logo` (`file`, a picture in the kit's folder, and its watermark place: `corner` top_left, top_right, bottom_left or bottom_right, `margin` and `scale` as fractions of the frame's width, `opacity`), `intro` and `outro` (each a composition template by `template` name, or a clip by `file`), `lower_third` and `end_screen` (templates by name) and `captions` (a caption look). Three starter kits ship with Klipp, read-only: Clean, Bold and Editorial.

`list: true` lists every kit with its `source` (`mine` or `builtin`); `get: name` gives one with all its fields; `set: {name, ...fields}` keeps a kit as your own, replacing one of its name (a starter's name makes your own copy), with `files: ["/absolute/path/logo.png", ...]` for the logo and any intro or outro clip it names, copied into its folder; a font no title can draw is said in the reply. `remove: name` removes one of your kits (not a starter). `apply: name` takes a kit into the open sequence: with `parts` (`brief` and `captions` when left out, its look, one undo step) `brief`, its colours become the brand colours (primary first) and its heading, callout and card looks are set in its fonts and colours, their size and place kept (its own type styles instead when it has them), titles that follow those looks restyled; `captions`, its caption look (its own, or the default in its body font) goes onto every subtitle track that has no look of its own, its captions restyled. The sequence's parts are another undo step, asked for by name: `intro` opens the piece with the kit's intro (a composition template held its own length, or its clip), everything moved on by that length; `outro` closes it at the end; `watermark` puts its logo on a video track of its own above everything, for the whole piece, `scale` of the frame's width wide in its own shape, `margin` in from both edges of its `corner`, at its `opacity`. A kit's files are imported first; a part the kit has nothing for is said, not refused. A kit's colours must be `#rrggbb`, its numbers in range and its files plain names of a picture (png, jpg, webp) or a clip (mp4, mov, mkv, webm); anything else is refused with why.

## klipp_title_templates

The title templates, built-in by category and saved, with their fields by label and their values: what the template op places and sets. compositions lists the composition templates (a stack of clips saved as one) with their length, parameters (name, label, default, choices), slots (name, label, kind), builtin (shipped with Klipp, never written; one saved under its name is the user's copy) and formats (the frames it has its own layout for: wide, and tall and square where made; placed in a sequence it takes the layout for that frame); the template op places one by name, sets its parameters by name or label and fills a media slot with a media item's id. `save: {clip_ids, name, category?, fields?, replace?}` saves clips of the open sequence together as a composition template, placed as one clip: fields names by label what it exposes (each text, its colour and font, a shape's colour, an image layer's picture, and each media clip as a slot); left out, its texts and slots. `save: {clip_id, nodes, name, category?, controls?, replace?}` saves nodes of that clip's picture (their ids, from graph_read) as a recipe under your own: each node as it is, the wires among them and from the clip's source, grade or display step kept, a behaviour on a parameter too; a node they are wired to and leave out is refused, named. controls publish numbers, each `{label, min, max, drives: [{target: "node:<id>.<param>", to: [a, b]}]}`, at rest where the node is now; the recipe op then places it on any clip. Its `controls: [{label, min?, max?, default?, drives: [{target, to: [a, b], curve?}]}]` are macro sliders (0 to 1 when min and max are left out): each drives numbers of the layers, a target named by label (`Text 1 size`, `Text 1 opacity`, `x`, `y`, `rotation`, `tracking`, `line spacing`, `entrance length`, `exit length`) across `to`, on a curve (linear, ease_in, ease_out, ease_in_out). The template op sets a control like a field; the template_link op sets it later, or unlinks, relinks or offsets one target. With `save: {clip_id, name, category?, fields?, replace?}` it saves a title on the timeline as a template instead: it joins the Titles browser and the template op places it by name. fields names, by label, what stays editable once it is placed: each text and its colour, the bar's colour, Speed and Size, and also each text's font (`Text 1 font`), each picture by its layer's name, and Duration. Left out, the title's defaults and Duration are kept. Duration lays the motion over the time it is placed for: placed for 8 s a template made over 4 s enters and leaves at half the pace, its animator keys stretched too; the template op sets it from secs unless fields give one. category groups it in the browser (My Templates when left out). A name already saved is refused unless replace is true. klipp_title_templates lists the fields of each template with their values. Templates come from three sources, each listed with `source`: `builtin` (shipped with Klipp, read-only), `mine` (what the user saves) and `pack:<name>` (an installed pack, read-only). The same name may be in more than one; the template op finds the user's own first, then a pack's, then Klipp's, unless it names `source`. Saving a built-in or a pack's template keeps a copy as the user's own. `pack: {path, name, author?, version?, licence?, templates?, licences?}` writes the user's saved templates (all, or those named: title templates, composition templates and recipes) to one .klipppack file at an absolute path: each picture they draw goes in with its licence text, given in licences by file name or as a <file>.licence.txt beside it (a picture without one is refused, named); a catalogue font goes by name and is fetched where the pack is used, an installed one is carried when a licence sits beside it, and the reply names any left out. `pack: {import: true, path}` installs a pack as a folder of its own, read-only, its templates listed as `pack:<name>`; one of the same name already installed is refused, and `pack: {update: true, path}` replaces it whole. The reply names what is missing and what is drawn instead (a font as Noto Sans, a picture left empty). `pack: {list: true}` lists the installed packs (name, author, version, licence, how many templates), and `pack: {remove: name}` removes one, its folder and nothing else.

recipes lists the recipes: node groups the recipe op builds into a clip's picture as ordinary nodes (green screen, picture in picture, matted glow, sky, text behind subject, screen replacement, replace background (clean plate), colour hold, camera shake, glitch, gradient map, wave, twirl, bulge or pinch), each with its slots (name, label, kind: media, colour, title, subject_matte, surface) and its controls (name, label, default, range). The recipe op fills a slot by name or label: a media slot with a media item's id, a colour slot with `#rrggbb`, a title slot with the text or the title op's own fields (`{text, style, size...}`, made by the title op over the clip's span), a subject_matte slot with `subject` or `foreground`, a surface slot with the name or id of a surface tracked in the clip (`klipp_track_surface` tracks one: a sign's or a screen's four corners; the picture follows it frame by frame, and the inset control keeps it inside a screen's bezel); and sets controls by name or label. Colour hold keeps the colour its colour control picks and greys the rest at its own brightness (range, softness, amount); camera shake wiggles the picture's position and turn (strength in sequence pixels, speed, rotation), a little enlarged so no edge shows at a gentle strength; glitch tears rows sideways with a colour split and static (amount, speed); gradient map lays brightness along three colours; wave, twirl and bulge or pinch bend the picture (their amounts, sizes, centres and, for the wave, its speed and direction). Replace background (clean plate) lays a new background wherever the shot looks as it does at its clean frame, a moment without the subject: set its clean frame control to the seconds into the clip where the background is empty (kept as the media's own time, so a trim leaves it; a time past the clip's end is refused). A subject matte is the one the clip's media already has: when frames are missing, the reply says how many and that `klipp_track_subject` with that model makes only those (it downloads a model only if asked). A recipe sizes and places in fractions of the frame, so it fits any sequence.

## klipp_screenshot

Capture what the viewer is currently showing, as a PNG. Returns the file path, and inlines the image when it is small enough. This is how to actually see the result of a grade rather than infer it.

Arguments:
- `max_width`: Downscale to this width (default 1280)
- `path`: Where to write the PNG; omit for a temp file
- `timeout_ms`: How long to wait for that frame (default 3000)
