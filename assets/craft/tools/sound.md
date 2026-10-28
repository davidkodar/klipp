# Sound, music and narration

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as sound-tools.

## klipp_add_music

Download a searched music track OR sound effect into the media pool, keeping its licence and attribution beside the file for your export credits. Pass a klipp_search_music result's fields, or several as tracks. Then place it on an audio track (place marks it music) and duck it under speech. Honour the licence — keep non-commercial tracks out of commercial work.

max_secs (on the call, or per track) fetches only the first that many seconds of an MP3, for an ambience bed out of a long recording: `{tracks: [{..., max_secs: 60}]}`. An original recording comes whole. A long download is waited for while it moves; one that stalls for 30 s, or runs far past its size's time, is tried once more, and the error says how much came and in how long.

## klipp_freesound_sign_in

Signs in to the person's own Freesound account, so klipp_add_music brings a sound's original recording (WAV, FLAC, AIFF at its own rate and depth) instead of the preview MP3. It needs their Freesound API credential, made at freesound.org/apiv2/apply with the callback URL http://127.0.0.1:47861/freesound/callback, its client id and secret in ~/.config/klipp/keys.toml as freesound_client_id and freesound_client_secret; without them the refusal says so. Called with nothing, it opens the approval page in the browser and listens for Freesound's answer; ask the person to approve, then call it again to see it finished (signed in as their name). Where Freesound shows a code instead of coming back, pass it as code. sign_out forgets the sign-in. Signed in, add_music takes the original by default (quality preview for the preview); signed out, the preview, and quality original is refused rather than quietly a preview. Every import reply says what it brought, measured: preview or original, codec, bitrate, rate, channels and depth, and the credit beside the file says it too.

## klipp_align_audio

Line a clip's sound up with another's — dual-system sound, a separate recorder against the camera's scratch track. It cross-correlates the two clips' audio, finds the offset, and moves clip_id so it plays in sync with reference_clip_id; it reports the match confidence and refuses when the two share no common sound. max_offset_secs bounds the search (30 by default). Both clips must carry audio.

Arguments:
- `clip_id`: the clip to move into sync (e.g. the external audio)
- `reference_clip_id`: the clip to sync to (e.g. the camera)

## klipp_audio_excerpt

Hear a stretch of sound, from_secs to to_secs, as a 16 kHz mono 16-bit WAV: enough to judge speech, a beat or a mix, not a master. With media_id it is that file's own sound, its times the file's (clamped to its length). Without one (or with sequence true) it is the timeline's mix as the export mixes it, before loudness normalisation, at most sixty seconds from from_secs; the reply's source says so. Give both media_id and sequence and it is refused. A small excerpt (about thirty seconds) comes inline; otherwise read the WAV at the reply's path, or give path (absolute) to write it there. Use it to check a cut's sound, find where a line starts, or hear what the mix does under a title; for loudness numbers use klipp_mix_loudness, and for words klipp_transcript.

## klipp_audition

Play sound through Klipp's own output for the person at the machine to hear, when they should judge it by ear: a pool file's stretch (media_id with from_secs and to_secs) or a clip's (clip_id, optionally narrowed to timeline seconds) at the source's own sample rate and channels; the timeline's mix (sequence true, from and to) at the sequence's rate in stereo; or up to five search results' preview_url values before importing anything, played one after another. At most a minute of a stretch. You do not hear it: say what you played and ask the person what they hear. action stop ends it and status says what is playing. For numbers rather than listening, search's listen measures previews and klipp_mix_loudness the mix.

## klipp_auto_sfx

Preview sound design; apply true commits one undo step: whooshes on cuts, clicks on reveals, riser into last cut. on cuts is default; transitions selects cuts with transitions; stylised selects zoom/whip/glitch/film-burn, excluding dissolve/dip. Optional media ids select sounds; otherwise preview built-in whooshes, then synthesise/import on apply. Skip conflicting placements, preserve existing clips. Place on SFX track with sfx role. When both sources are analysed, whooshes are 3 dB above the loudest underlying sound and clicks 3 dB below; otherwise -6/-8 dB.

## klipp_narrate

The `voice` argument:

Narration from text, made on the machine. voice is a manifest id; the default is kokoro_voice_af_heart (natural, female, US English; Apache-2.0). The other Kokoro voices are kokoro_voice_ + af_bella, af_nicole, am_michael, am_fenrir, am_puck (US); bf_emma, bf_isabella, bm_george, bm_fable (UK); they need the espeak-ng program and one 87 MB download for all ten. Lighter, more robotic Piper voices (run by the Piper engine: a piper program on the machine, else Piper's official release, a 27 MB download fetched with download_models true) are piper_ + en_us_ljspeech_high, en_us_kristin_medium, en_us_norman_medium, en_us_john_medium, en_us_libritts_high (CC BY: credit LibriTTS), en_gb_cori_high, sv_se_nst_medium (Swedish); piper_en_us_lessac_medium is research only, never for paid work. Where Kokoro cannot run, the default is piper_en_us_ljspeech_high. Nothing downloads unless download_models is true (or the person downloads it in Window > Narrate); a refusal names the size. The take goes into the pool, and with apply true onto a Voice track at at_secs as dialogue. speed (0.5 to 2; 0.85 reads unhurried) and pause_secs (0 to 3, between sentences) shape the delivery. Then duck the music under it; transcript gives the words' times. A scratch voice to cut to, replaced by a recording later.

Arguments:
- `fit_secs`: seconds the take must last

## klipp_search_music

Search Jamendo royalty-free music by mood/genre, best textual match first. Results give title, artist, duration, tags, pace and licence/commercial clearance. listen previews up to five without importing (count from first or track-id list), reporting tempo, beat clarity and energy evolution. Import via klipp_add_music. Requires Jamendo client id in user key config. Match reference energy using style_profile; never reuse its track.

To cut, fit or loop a track its licence has to allow edits: edits_only true leaves out every ND licence, and licences narrows to families as the results label them (CC0, CC BY, CC BY-SA, CC BY-NC, CC BY-NC-SA, CC BY-ND, CC BY-NC-ND; "by-sa" works too). Both are applied before the ranking and the limit, so a limit of eight gives eight that pass. commercial_only is the same for paid work.

A search ranks the same candidates whatever its limit, so the best match is first at limit 1 as at 20. listen with ids hears those tracks even when the results don't hold them: they come back as listened_by_id, after the results, and an id the library doesn't know is in not_found.

Arguments:
- `commercial_only`: only tracks whose licence allows commercial use
- `instrumental`: only instrumental tracks, for a promo with its own titles
- `listen`: hear previews (at most 5): the first this-many, or [track_id, …]; tempo, beat and energy, nothing imported

## klipp_search_sfx

Search royalty-free sound effects by keyword (e.g. "whoosh", "door slam", "rain", "impact") from Freesound. Each result shows its licence and whether it clears commercial use. max_secs keeps to short sounds (a transition's whoosh, not a long design file); listen (a count, or [track_id, …], at most 5) hears previews and says each one's length, when it peaks and how loud it is, which is where it goes: the peak on the cut. Then add it with klipp_add_music (it takes a sound effect the same as a track). Needs a Freesound token in the user's key config.

Arguments:
- `commercial_only`: only sounds whose licence allows commercial use
- `listen`: hear previews (at most 5): the first this-many, or [track_id, …]; length, peak time, loudness
- `max_secs`: only sounds this long or shorter

## klipp_normalize

Bring clips to a loudness or peak target by their gain (one undo step; the media is not changed): each on its own, or group true for one gain that keeps their balance. Never raises a clip past the true-peak ceiling. Answers each clip's measured LUFS, sample peak and true peak and the gain set; apply false only answers.

Arguments:
- `apply`: false answers what it would set. Default true
- `ceiling_dbtp`: True-peak ceiling, default -1 dBTP
- `clip_ids`: From klipp_list_clips
- `group`: One gain for all the clips. Default false
- `mode`: loudness: integrated LUFS (ITU-R BS.1770); peak: sample peak dBFS. Default loudness
- `target`: LUFS or dBFS. Default -14 LUFS (web) or -1 dBFS; -16 podcast, -23 EBU R128, -24 ATSC A/85
