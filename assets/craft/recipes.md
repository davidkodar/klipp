# Recipes: worked plans for common pieces

A recipe is the order of work and the ops it needs, for someone who has
read the craft reference. Times, names and choices come from the brief
and from what analysis found; nothing here is a number to copy.

## An interview with B-roll

The brief says: length, tone, what the piece is about, who is who.

1. **Learn the footage.** `klipp_analyze`, then `klipp_media_analysis`
   for each interview file: its transcript, its silences, its framing.
   `klipp_find_shots` with `with_speech: false` for the B-roll shots,
   `framing: "wide"` for establishing shots.
2. **Pull selects.** Read the transcript. Choose the sentences that
   carry the piece; note each one's file, start and end (sentence
   boundaries, plus a beat). Check a doubtful take with
   `klipp_audio_excerpt`.
3. **Assemble the spine.** For each select, `place` it on the video
   track at `{"track_end": "video"}`, with `source_in` and
   `source_out` at the sentence's edges; the audio track gets the same
   ranges, so speech is never cut mid-word. Plan first, apply once.
4. **Cover the joins.** Where two selects from the same set-up meet,
   the cut is a jump cut. `place` a B-roll shot on the track above,
   starting a beat after the join and long enough to read, chosen so
   its motion and framing suit the moment. Where a reaction exists,
   prefer it.
5. **Open and close.** A wide shot at the top, held to read the place;
   the last line, then a beat, then out.
6. **Titles.** `lower_third` on each speaker's first appearance, name
   and title from the brief, four seconds, a beat after the cut; it is
   refused across a shot change, B-roll included, so place it once the
   cover is down.
7. **Sound.** Music under the open and close if the brief wants it,
   ducked under speech. Loudness to the delivery.
8. **Check.** `klipp_contact_sheet` over each join; `klipp_frame_at` on
   the titles; `klipp_timeline` for the whole.
9. **Deliver.** `klipp_export` with the brief's loudness;
   `klipp_export_subtitles`; read `klipp_export_status` until complete.

## A social cut-down of a longer piece

The brief says: platform, length (thirty to ninety seconds), aspect.

1. Read the transcript of the master; find the one idea the cut-down
   is about, and the two or three sentences that say it.
2. `place` those sentences in the order that tells it, tightest
   first; leave out set-up, keep the payoff.
3. Open on the strongest line, not the introduction; the platform's
   viewer decides in two seconds.
4. If the aspect changes, reframe every shot so the speaker stays in
   the frame (a task of its own until the reframe op exists: check
   each shot with a contact sheet).
5. `captions`, burnt in: most viewers will not hear it. Last, once
   the speech is where it will stay.
6. Loudness to the platform (-14 LUFS is the usual), export, check the
   length against the brief.

## A highlight montage to music

The brief says: length, the music file, the mood, what must be in.

1. Analyse the music too: its silences and loudness give its shape;
   the beat is by ear until the beat analyser exists (an audio excerpt
   at the start tells you the tempo).
2. `klipp_find_shots` by motion and framing to build a pool: wide and
   moving for energy, close and static for feeling.
3. Lay the music on the audio track first; the picture is cut to it.
4. `place` shots in turn, each cut on or just after a beat, lengths
   varied, a wide after every few close shots. Nothing shorter than
   half a second unless the mood asks.
5. End on the music's end, or on a held last shot as it fades.
6. Loudness to the delivery, export, watch it once.

## What every recipe shares

- Plan the whole assembly with `klipp_plan` before applying; read the
  refusals, they are the resolver telling you where the timeline does
  not agree with your arithmetic.
- Apply in a few large actions, not many small ones: each apply is one
  undo step for the user.
- Look at what you did before you deliver.
