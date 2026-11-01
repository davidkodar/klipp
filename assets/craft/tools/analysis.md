# Analysis and looking at the footage

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as analysis-tools.

## klipp_analyze

Background analysis: shot boundaries, EBU R128 loudness/true peak, silence, and word-timed transcript/language if the speech model is installed. Beat sheet and OCR require screen footage (no camera metadata or tagged screen via klipp_tag_media) and their installed models. A still is read for its people and faces (framing), so refit and the grading tools can use them. Omit media_id for the pool; analysed items are free. An item nothing applies to is done at once with nothing_ran saying why. Also runs on import without downloads. wait true waits, then read klipp_media_analysis. download_models fetches missing enabled-in-preferences models with user knowledge: speech hundreds of MB, OCR pair about 10 MB.

Arguments:
- `download_models`: Fetch missing models first (the user should know a download happens)
- `media_id`: From klipp_list_media; omit for every item
- `timeout_ms`: Maximum wait, default one hour; timeout does not cancel analysis
- `wait`: Wait until all requested items end; inspect finished and succeeded

## klipp_audio_plugins

The LV2 and CLAP audio plug-ins installed on this machine (found in the LV2_PATH and CLAP_PATH folders when the app starts), those whose name or id holds query when one is given: each id (lv2:<uri>, clap:<id>), name and number of parameters, thirty at most. When query is one plug-in's id, or holds only one, its parameters come with their keys, ranges and defaults, thirty at most; param picks those whose name or key holds a word ("threshold"). Put one on a clip's sound with the clip_audio op, or on a whole track's with track_audio: effect is its id, params by name or key, each inside its range, the rest at their defaults; bypass and remove work as for the built-in effects. A read; changes nothing.

## klipp_audio_spectrum

A file's sound as frequency against time: where a hum starts, when a hiss creeps in, which stretch has the air conditioning in it. The scrolling analyser in the app shows what is playing; this reads the whole file at once, which is the only way to answer *when*. Answers with named findings and the band levels a second at a time -- rumble, hum, voice, sibilance, hiss -- each judged relative to the voice's own level rather than as an absolute, because a quiet recording of a clean room and a loud one of a noisy room have the same noise floor and different problems.

## klipp_auto_redact

Hide everything the screen text found sensitive — emails, keys, passwords, card numbers — wherever the timeline shows it: one redact op per finding (a cropped, pixelated copy of the footage on a video track above), resolved as a plan. A dry run by default; with apply true it is done as one undo step. Needs the OCR pass (klipp_analyze with the OCR models). Look at a frame afterwards to see each box covers its text.

## klipp_beat_sheet

A screencast or demo's beat sheet: where the picture is settled or changing (idle/action), the cut points (a scene change, or where a change just settled: cut TO a settle and hold until it reads, never mid-action) and the title/hold windows (settled stretches a title can sit on). From the shots and motion passes, so analyze first. With the OCR models downloaded each settled beat and reveal is named by what the screen showed ("6.13-14.50 s SEARCH & FILTERING"), so a plan can work by feature name. Per second: the times say where to look; confirm with frame_at or contact_sheet where it matters.

## klipp_beats

The tempo and beat grid of a clip's sound: BPM, a confidence, and the beat times in seconds. Run it on the music you are cutting to, then place or trim cuts to a beat time for a musical rhythm. A low confidence means no clear beat (speech or ambience). For music it also gives the track's sections (intro, build, peak, breakdown, outro, body) on phrase lines and its energy bar by bar, for putting chapter changes on musical turns. With target_secs it fits the track to that length by taking out or repeating whole bars where the join is cleanest, keeping its start and end, and returns place ops (on track, from at_secs) to apply. A read; changes nothing.

Arguments:
- `at_secs`: where the fitted track's place ops start on the timeline (0)
- `skip_intro`: with target_secs: leave the intro out and fit from where it ends
- `target_secs`: fit the track to this length, seconds
- `track`: the track for the place ops: an index, or a kind such as "audio" (the first audio track)

## klipp_check

Read-only pre-export checks: black/flash frames, uncovered jump cuts, titles spanning cuts or too short, captions under lower thirds, cuts inside words, unducked music, mismatched picture/audio ends, offline media; brief length/size/captions/cast/music. With screen beat sheets: cuts during changes, short reading holds, titles over changing screens (suggests hold windows), flat cadence; cuts to settled screens are not jump cuts. With plans: missing brief features or under-three-second coverage, approved plans unbuilt or drifted. Each finding gives where, why and the fix; fix errors and weigh warnings before klipp_export. Slower: color true renders a frame a top-track shot for clipping, crushed blacks, dullness, oversaturation and skin-line drift; loudness true meters the mix: its integrated loudness and true peak (an error for overs that would be delivered, or for a brief's target the export can't reach under the ceiling; a warning for a mix far from the target), and sections far under the rest. The colour findings come grouped by shot, a fix given once. min_severity warning (or error) lists only findings at that severity or above; the counts still count every finding, and below_floor says how many were left out. Words in titles and captions no dictionary knows are warnings (rule spelling); a language with no dictionary installed is a note. Footage used twice is a note (rule reused_footage), or a warning when the brief's reused_footage_warns is set: klipp_reuse_report lists it.

## klipp_reuse_report

Where the timeline uses the same footage twice: "did I use anything twice?". Each pair is two picture clips showing the same frames of one file, wherever they sit: on a track, inside a compound clip, or inside a precomposition. Holds, speed ramps and reversed clips count by the frames they really show. One file imported twice counts once (by its fingerprint). Stills aren't footage, and a clip that shows a frame twice itself isn't counted against itself.

- `pairs`: each side gives the clip on the sequence's track (`clip`, `track`), the clip showing the footage and the compound clips it is inside (`inner`, `inside`), the file's `name`, the reused `timeline` span and the `source` span in seconds, and the shared length (`secs`).
- `text`: the pairs in words, or "Nothing is used twice."

Shots that look the same but come from different files (a re-export, a duplicate download) aren't matched yet: only the same frames of one file.

The timeline marks the shared spans with a thin strip at each clip's foot (timeline options: Show reused footage). klipp_check lists each pair as a note, or as a warning with the brief's `reused_footage_warns` set, for stock-heavy deliveries where each use may be licensed. Read-only.

## klipp_spelling

Spelling in the titles and captions, against the Hunspell dictionaries installed on the system. Each text is checked in its own language: the text's, else its track's (a subtitle track's language), else the brief's, else the system's. Not marked: the brief's vocabulary and cast names, words of the media's file names, brand kit names, the person's word list, numbers, web and e-mail addresses, @handles, #tags, and words in another script. action list (the default) gives each word marked: the word, clip_id, track, object, its byte range start and end in that text, at_secs, the language and up to five suggestions; missing names the languages with no dictionary and the package to install. ignore takes word as right in this project (it goes on the brief's vocabulary, one undo step, which the speech-to-text also uses); learn adds it to the person's word list, for every project. Correct a word with a title op's text; ask before ignoring or learning a name you are not sure of.

Arguments:
- `action`: list, ignore or learn
- `word`: for ignore and learn, the one word

## klipp_review_finding

Every klipp_check finding has an `id`. Save a decision about one by it: `accepted` keeps it on purpose and needs a `rationale` (why it is deliberate); `to_fix` notes it and leaves it counted; `clear` takes the decision back. One undo step, saved with the project. Check then lists accepted findings under "Accepted on purpose", not counted as errors or warnings, with their reason and the revision they were accepted at. The id holds while what the finding judged is unchanged: change a colour finding's clip (its media, range, grade or group look), or what a craft or sound finding says, and the finding returns under a new id, unreviewed, while the old decision is listed as no longer holding. Accept only what is deliberate; fix the rest.

## klipp_contact_sheet

Look at a media file: a grid of frames evenly spaced over a range (the whole file by default), each with its time in the file burnt in, as one JPEG, inlined when small enough. Use it to see what a clip is before planning, and to check a range you cut to. Twelve frames, four to a row, 240 px tiles by default; the reply lists each tile's time.

Arguments:
- `inline`: Also return the picture in the reply; by default only when no path is given
- `path`: Absolute JPEG path; temp dir when omitted

## klipp_cut_points

The clean places to cut in a media file, ranked: the measured pauses (strength 3, the cleanest), the sentence ends and shot changes (strength 2), the phrase breaks (strength 1). Pass near_secs and it also says whether that moment is clean, mid-sentence or mid-word, and the nearest clean point to move to. Use it before you trim or place a spoken clip, so a cut never lands through a word or the middle of a sentence. from_secs and to_secs narrow the range.

## klipp_find_shots

Shots across every analysed item, every condition given holding: framing (close-up, medium close-up, medium, wide, very wide, inset: a small figure in a corner or a face cam over a screen; none for no people, as B-roll wants), at least so many people, a detector object (cat, car, cup…), motion (static, gentle, moving, busy; needs decoded measurements, and a missing bin is not static), speech or not, a minimum length. Each hit: the media id and the shot's start and end in the file. A take over 12 s is also searched moment by moment, split where the camera settles or moves or the framing changes; those hits carry a moment number and steady (still enough to use), so static or gentle finds a steady stretch inside a walk.

## klipp_analysis_status

Where analysis stands. With wait true, the reply is held until the selected items end or timeout_ms passes (an hour by default; the wait never cancels the work). It reports finished, succeeded and timed_out, and failed items carry their errors.

Arguments:
- `timeout_ms`: Maximum wait, default one hour; does not cancel work

## klipp_search_transcripts

Find where something is said: every transcript segment containing the query, matched without regard to case, with its media item and its start and end in the file's seconds. max_hits caps the list.

## klipp_media_analysis

What is known about a media item, as text: its shots with their ranges, its loudness, range and true peak, and its silences. The timeline view shows the same facts per clip in brief. Times are seconds from the file's start; a clip's source_in maps them to the timeline.

Arguments:
- `max_chars`: Cut at this many characters (default 8000)

## klipp_mix_loudness

How loud the mix is over a stretch of the timeline, as the export mixes it (before loudness normalisation): every step_secs (0.5) its short-term loudness and the loudest 400 ms inside the step, with the steps that stand out 4 dB over the median marked. Use it to hear whether a whoosh or a hit lands over the music at its cut, and whether music sits under speech. A read; about a tenth of the stretch's length.

## klipp_review

The cut as an editor scrubs it, in one call: a draft render of the whole timeline (max_width 640 by default, fast), a contact sheet of the draft (frames tiles in cols columns, one every couple of seconds), and the check with its colour pass. Read the sheet image for the shape of the cut — the rhythm, the titles, the cards — and the check for what to fix; frame_at for a closer look at one frame. Takes as long as the draft render (seconds to a minute).

## klipp_screen_text

What was on the screen of a screen recording and when: every line OCR read, folded into spans with the seconds it stayed up and where on the frame (x, y, w, h as fractions), between from_secs and to_secs. Says which screen a beat shows — "Search & Filtering" up 12–18 s — so a plan can name the right moment. SENSITIVE lines (an email, a key, a password, a card number) are listed masked with where to blur. Needs the two OCR models (about 10 MB): if the reply says they are not downloaded, ask the user before klipp_analyze with download_models true.

Arguments:
- `all`: every line read, not the summary of headings and lines that stayed up

## klipp_selects

Cut a long take down to its selects: the runs of speech between the real pauses, each with the time it runs from and to and what is said, marked keep or drop (a run that is only filler — um, uh — or too short to use is a drop). Filler is read in the transcript's language, from Klipp's word list for it (English and Swedish ship; more go in lang/<code>/words.toml in Klipp's data folder); `language` in the reply names it, and the text says when the language has no list, so no run was dropped as filler. Use it to build a tight cut from a loose recording: place each keep range in order and close the gaps. min_gap_secs sets the pause that starts a new select (0.6 by default). A take with next to no speech (under a tenth of a take of half a minute or more), or whose word times collapsed, is cut by its picture instead: one select a shot, `by` "picture" in the reply. A read; changes nothing.

Arguments:
- `min_gap_secs`: the pause length that splits one select from the next, seconds

## klipp_find_moments

The moments of a long talk, podcast, stream or vlog that stand on their own as short clips, best first, never overlapping. Each starts on a sentence after a real pause and ends on one, about `length` seconds long (30 by default; from half to twice it), with its hook (the first line a viewer hears), its full text, a score from 0 to 1 and why: a question or a strong first line, a number up front, whether it opens mid-thought, a turn that lands its point, a laugh, its pace. `count` sets how many (5); `include` keeps only moments holding one of those words, `exclude` leaves out any holding one. It reads the transcript, so analyze the file first. The ranking reads words, not meaning: read each moment's text and rank again by what it says before keeping any. The hook words are read in the transcript's language, from its word list (`language` in the reply); in a language with no list, moments are ranked by their shape alone, and the text says so. A read; changes nothing.

Arguments:
- `length`: seconds aimed for (30)

## klipp_make_shorts

Build a vertical short for each moment you kept from klipp_find_moments (from_secs, to_secs and its hook line), each in a sequence of its own at 1080x1920 named after its hook. The moment's speech is placed on clean word edges; `tighten` (true by default, the platforms' norm) takes the pauses and runs of filler out (filler in the transcript's language, as klipp_selects reads it; `language` in the reply), and every other piece is pushed in a little so each jump reads as a cut. Each picture fills the upright frame and follows the speaker (faces, then people, from the framing analysis: analyze first), as transform keys on the GPU. Captions are made from the transcript, `hook_title` puts the hook over the first three seconds, and the sound is brought to `platform`'s loudness (shorts, reels or tiktok, -14 LUFS). A moment outside the file or with nothing said in it refuses the whole request before any sequence is made. Each short is its own undo steps; look at each afterwards, its cuts and captions, and reframe a picture with no clear speaker by hand.

Arguments:
- `moments`: [{from_secs, to_secs, hook?}]

## klipp_style_profile

A reference video's editing style as facts, to build a clip in its feel from other footage: its cut cadence (shots, their lengths, cuts a minute, on the beat or not), its tempo, its look off the scopes (blacks, whites, contrast, saturation, warmth), its motion and detected holds. Cut cadence is not animation pacing: look at changes within shots before choosing hold lengths, as motion statistics name no animation events. Read only. Match the feel with your own footage and your own or licensed music, never the reference's track. Import and analyse the reference first.

## klipp_track_subject

A matte that follows the subject through one clip, frame by frame: model subject (one detected subject, followed; the default) or foreground (everything in front). It is added to the clip's grade as an External Matte gating its correction (the selected one, else the first, a new one when there is none), as one undo step, and filled in the background; the viewer shows each part as it is made. Its job_id is `<media id>:subject_matte`: klipp_job_status reads its progress, klipp_cancel_job stops it after the frame it is on and keeps the frames made. Running it again tracks only what is missing. An export is refused until the matte covers every frame the clip shows. The model is never fetched unasked: without it the call is refused, naming it; download_model true fetches it (tell the person, it is large).

With into "cut_out" the matte cuts the clip out instead: it is the alpha the clip's grade leaves with, so the tracks below show everywhere but the subject; the same matte node serves both uses when asked for each.

With transition_id in place of clip_id, it tracks the subject a piece transition (strips, panels, halves, shatter) cuts round as its own piece (`pieces.subject` on the transition op): only the frames of the subject side the transition shows, by the model the transition's subject names, and nothing is added to a grade. Without the matte the pieces are plain and the viewer says why; an export waits for it.

To undo a subject or single-frame AI cut-out, use `klipp_track_subject` with `clip_id` and `into: "whole"` (Restore Background). It removes only that matte's Output alpha connection, as one undoable step, retaining the matte if another part of the grade still reads it. It starts no tracking pass and downloads no model. Other hand-wired alpha keys are refused, as are transition targets. One Undo restores the cut-out. “Track the rest” on an existing matte card only continues its pass; it never reconnects a removed matte or creates a correction.

Arguments:
- `clip_id`: From klipp_list_clips
- `download_model`: Fetch the model first when missing (the user should know a download happens)
- `model`: subject: one detected subject, followed (default); foreground: the whole foreground
- `transition_id`: In place of clip_id: a piece transition whose pieces cut round the subject

## klipp_transcript

What is said in a media file between two times, with when, in seconds of the file: the segments, and with words true every word with its start and end, and the stretches measured silent in that range. Use it to put a cut on the edge of a word or in a silence (a cut in measured silence is clean), and to read a stretch the analysis text cut short.

## klipp_analysis_queue

Put media_id first in the pending analysis queue. With cancel_pending true, cancel other queued items (all if no ID). Running work finishes normally. Call analyze first for an item that has no active request.

## klipp_search_screen_text

The screen recording's Cmd-F: every moment a word or phrase was on screen, across all footage that has had its screen text read (klipp_screen_text). Each hit has the media item, the seconds it stayed up, the text and where on the frame. max_hits caps the list.

## klipp_search_visual

Footage found by what its picture shows: a description such as "a dog on a beach" or "a close-up of hands on a keyboard", matched against frames sampled every two seconds of each analysed file (picture search must be downloaded, and analysis must have run). Hits come best first: the media item, the stretch of the file where neighbouring frames match, its best frame and the match score, a cosine similarity. max_hits caps the list; min_score (default 0.2) drops weaker matches. With nothing analysed yet it says how to start.
