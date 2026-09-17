//! The check before a piece goes out: what an editor would catch on a
//! last watch, read off the timeline and the brief. Nothing here
//! changes anything; every finding says where, what, and what would
//! put it right.

use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TitleObjectContent, TrackKind};
use serde::Serialize;

use crate::ops::{is_heard, linear_to_db, sound_settings_clip, SpokenWord, Transcripts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The piece should not go out like this.
    Error,
    /// An editor would look at this and decide.
    Warning,
    /// Worth knowing; nothing wrong.
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// The rule that found it, for a caller that wants to filter.
    pub rule: &'static str,
    /// Where on the timeline, when it is somewhere.
    pub at_secs: Option<f64>,
    pub what: String,
    pub hint: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub duration_secs: f64,
    pub errors: usize,
    pub warnings: usize,
    pub findings: Vec<Finding>,
}

impl Report {
    /// The report as lines of text, the worst first.
    pub fn text(&self) -> String {
        let mut out = vec![format!(
            "Checked {:.2} s: {} error{}, {} warning{}, {} note{}.",
            self.duration_secs,
            self.errors,
            if self.errors == 1 { "" } else { "s" },
            self.warnings,
            if self.warnings == 1 { "" } else { "s" },
            self.findings.len() - self.errors - self.warnings,
            if self.findings.len() - self.errors - self.warnings == 1 { "" } else { "s" },
        )];
        for f in &self.findings {
            let tag = match f.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Note => "note",
            };
            let at = f.at_secs.map(|t| format!(" {t:.2} s")).unwrap_or_default();
            out.push(format!("[{tag}]{at}  {}. {}", f.what, f.hint));
        }
        out.join("\n")
    }
}

fn secs(t: klipp_core::Timecode) -> f64 {
    t.to_secs_f64()
}

fn clip_label(pool: &MediaPool, c: &Clip) -> String {
    match &c.kind {
        ClipKind::Media { media_id } => pool.get(media_id).map(|m| m.filename.clone()).unwrap_or_else(|| "a missing file".into()),
        ClipKind::Title(td) => format!("title \"{}\"", td.text().lines().next().unwrap_or("")),
        ClipKind::Generator(_) => "a generator".into(),
        _ => "a clip".into(),
    }
}

/// Whether a clip puts a picture on screen.
fn is_picture(c: &Clip) -> bool {
    c.enabled && !matches!(c.kind, ClipKind::Gap | ClipKind::AdjustmentLayer)
}

/// The stretches of `0..end` that none of `spans` covers, each at least `min` long.
fn uncovered(mut spans: Vec<(f64, f64)>, end: f64, min: f64) -> Vec<(f64, f64)> {
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut at = 0.0;
    for (a, b) in spans {
        if a - at >= min {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if end - at >= min {
        out.push((at, end));
    }
    out
}

/// Check `seq` against its brief and the craft's rules.
pub fn check(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Report {
    let mut f: Vec<Finding> = Vec::new();
    let mut add = |severity, rule, at_secs: Option<f64>, what: String, hint: &str| {
        f.push(Finding { severity, rule, at_secs, what, hint: hint.to_string() });
    };
    let fps = seq.fps.as_f64().max(1.0);
    let frame = 1.0 / fps;
    let duration = secs(seq.total_duration());
    let video_tracks: Vec<(usize, &klipp_timeline::Track)> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video && t.visible).collect();
    let pictures: Vec<(usize, &Clip)> = video_tracks.iter().flat_map(|(ti, t)| t.clips.iter().filter(|c| is_picture(c)).map(move |c| (*ti, c))).collect();

    if pictures.is_empty() {
        add(Severity::Error, "empty", None, "there is no picture on the timeline".into(), "Place the piece first");
    }

    // Media that is not there.
    for t in &seq.tracks {
        for c in t.clips.iter().filter(|c| c.enabled) {
            if let ClipKind::Media { media_id } = &c.kind {
                match pool.get(media_id) {
                    None => add(Severity::Error, "offline", Some(secs(c.timeline_in)), "a clip's media is not in the pool".into(), "Remove the clip or import its file"),
                    Some(m) if m.offline => add(Severity::Error, "offline", Some(secs(c.timeline_in)), format!("{} is offline", m.filename), "Relink the file before delivering"),
                    _ => {}
                }
            }
        }
    }

    // Black: time with no picture under it.
    if !pictures.is_empty() {
        let picture_end = pictures.iter().map(|(_, c)| secs(c.timeline_out)).fold(0.0, f64::max);
        for (a, b) in uncovered(pictures.iter().map(|(_, c)| (secs(c.timeline_in), secs(c.timeline_out))).collect(), picture_end, frame * 0.9) {
            add(Severity::Error, "black", Some(a), format!("no picture from {a:.2} to {b:.2} s: {:.0} frame{} of black", (b - a) * fps, if (b - a) * fps >= 1.5 { "s" } else { "" }), "Close the gap, or cover it");
        }
        // Picture and sound ending apart.
        let sound_end = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(t, c)| is_heard(seq, pool, t, c))
            .map(|(_, c)| secs(c.timeline_out))
            .fold(0.0, f64::max);
        if sound_end > 0.0 && (sound_end - picture_end).abs() > frame * 1.5 {
            let (first, then) = if sound_end > picture_end { ("picture", "sound") } else { ("sound", "picture") };
            add(Severity::Warning, "ends_apart", Some(picture_end.min(sound_end)), format!("the {first} ends at {:.2} s and the {then} runs on to {:.2} s", picture_end.min(sound_end), picture_end.max(sound_end)), "End them together, or fade the one that runs on");
        }
    }

    // Flash frames and jump cuts, track by track.
    for (ti, t) in &video_tracks {
        let clips: Vec<&Clip> = t.clips.iter().filter(|c| is_picture(c)).collect();
        for c in &clips {
            let frames = ((secs(c.timeline_out) - secs(c.timeline_in)) * fps).round() as i64;
            if frames < 8 {
                add(Severity::Warning, "flash", Some(secs(c.timeline_in)), format!("{} is on screen for {frames} frame{}", clip_label(pool, c), if frames == 1 { "" } else { "s" }), "A shot that short reads as a flash: lengthen it or take it out");
            }
        }
        for pair in clips.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let cut = secs(b.timeline_in);
            let same_media = matches!((&a.kind, &b.kind), (ClipKind::Media { media_id: x }, ClipKind::Media { media_id: y }) if x == y);
            let meets = (secs(a.timeline_out) - cut).abs() < frame * 0.5;
            let jumps = (secs(b.source_in) - secs(a.source_out)).abs() > 0.2;
            if !(same_media && meets && jumps) {
                continue;
            }
            // Covered when a picture on a track above runs across the cut.
            let covered = pictures.iter().any(|(tj, c)| tj < ti && secs(c.timeline_in) <= cut - 0.2 && secs(c.timeline_out) >= cut + 0.2);
            if !covered {
                add(Severity::Warning, "jump_cut", Some(cut), format!("a jump cut: {} cuts to itself, {:.1} s later in the file, with nothing over the join", clip_label(pool, a), secs(b.source_in) - secs(a.source_out)), "Cover the join with B-roll or a reaction, or keep it if jump cuts are the piece's style");
            }
        }
    }

    // Titles: across a cut, too short to read, sharing the bottom of the frame with captions.
    let cuts: Vec<f64> = pictures.iter().flat_map(|(_, c)| [secs(c.timeline_in), secs(c.timeline_out)]).collect();
    let titles: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Text && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled && matches!(c.kind, ClipKind::Title(_))).collect();
    let captions: Vec<&Clip> = seq.tracks.iter().filter(|t| t.kind == TrackKind::Subtitle && t.visible).flat_map(|t| t.clips.iter()).filter(|c| c.enabled).collect();
    for c in &titles {
        let ClipKind::Title(td) = &c.kind else { continue };
        let (a, b) = (secs(c.timeline_in), secs(c.timeline_out));
        if let Some(cut) = cuts.iter().copied().filter(|e| *e > a + frame && *e < b - frame).reduce(f64::min) {
            add(Severity::Warning, "title_across_cut", Some(cut), format!("{} is up from {a:.2} to {b:.2} s and the picture changes at {cut:.2} s", clip_label(pool, c)), "A title does not run across a shot change: move it inside one shot");
        }
        let words: usize = td.objects.iter().filter_map(|o| match &o.content { TitleObjectContent::TextBox(tb) if o.visible => Some(tb.text.split_whitespace().count()), _ => None }).sum();
        let needs = (words as f64 * 0.3).max(1.5);
        if b - a + 1e-6 < needs {
            add(Severity::Warning, "title_short", Some(a), format!("{} has {words} word{} and {:.1} s to be read in", clip_label(pool, c), if words == 1 { "" } else { "s" }, b - a), &format!("Hold it at least {needs:.1} s: about a third of a second a word, never under a second and a half"));
        }
        let low = td.objects.iter().any(|o| o.visible && o.transform.center.y > 0.72);
        if low {
            if let Some(cap) = captions.iter().find(|k| secs(k.timeline_in) < b && secs(k.timeline_out) > a) {
                let from = a.max(secs(cap.timeline_in));
                add(Severity::Warning, "captions_under_title", Some(from), format!("{} and the captions share the bottom of the frame from {from:.2} s", clip_label(pool, c)), "Put the title up where nobody is speaking, or take the captions out from under it");
            }
        }
    }

    // Sound: cuts inside a word, music left at full level under speech.
    let mut speech_spans: Vec<(f64, f64)> = Vec::new();
    let mut musics: Vec<&Clip> = Vec::new();
    let mut cache: std::collections::HashMap<klipp_core::MediaId, Option<Vec<SpokenWord>>> = std::collections::HashMap::new();
    for t in &seq.tracks {
        for c in t.clips.iter().filter(|c| is_heard(seq, pool, t, c)) {
            let Some(media_id) = c.media_id() else { continue };
            let Some(item) = pool.get(&media_id) else { continue };
            let words = cache.entry(media_id).or_insert_with(|| transcripts.words(item));
            let (s_in, s_out) = (secs(c.source_in), secs(c.source_out));
            let inside: Vec<&SpokenWord> = words.as_deref().unwrap_or(&[]).iter().filter(|w| w.end_secs > s_in && w.start_secs < s_out).collect();
            if inside.is_empty() {
                if words.is_some() || item.video_codec.is_empty() {
                    musics.push(c);
                }
                continue;
            }
            speech_spans.push((secs(c.timeline_in), secs(c.timeline_out)));
            const INTO: f64 = 0.06;
            for (edge, at, name) in [(s_in, secs(c.timeline_in), "starts"), (s_out, secs(c.timeline_out), "ends")] {
                if let Some(w) = inside.iter().find(|w| w.start_secs + INTO < edge && w.end_secs - INTO > edge) {
                    let to = if name == "starts" { w.start_secs } else { w.end_secs };
                    add(Severity::Warning, "word_cut", Some(at), format!("the sound {name} inside the word \"{}\"", w.text.trim()), &format!("Trim that edge to {to:.2} s of the file, the word's own edge, plus a beat"));
                }
            }
        }
    }
    let has_music = !musics.is_empty();
    for m in musics {
        let (a, b) = (secs(m.timeline_in), secs(m.timeline_out));
        let settings = sound_settings_clip(seq, m);
        let under_speech = speech_spans.iter().any(|(x, y)| *x < b && *y > a);
        if under_speech && settings.volume_keyframes.is_empty() && settings.volume > 0.26 {
            add(Severity::Warning, "music_level", Some(a), format!("{} plays at {:.0} dB under speech and never comes down", clip_label(pool, m), linear_to_db(settings.volume)), "Duck it under the speech, six to twelve decibels, with a fade");
        }
    }

    // The brief.
    match &seq.brief {
        None => add(Severity::Note, "no_brief", None, "there is no brief, so length, size, loudness and names are not checked".into(), "Set one with the piece's length, platform, loudness and cast"),
        Some(brief) => {
            if let (Some(target), Some(tol)) = (brief.target_secs, brief.length_tolerance_secs()) {
                if (duration - target).abs() > tol {
                    add(Severity::Error, "length", None, format!("the piece runs {duration:.1} s and the brief asks for {target:.0} s (within {tol:.0})"), if duration > target { "Take time out" } else { "It runs short: add to it, or change the brief" });
                }
            }
            if let (Some(w), Some(h)) = (brief.width, brief.height) {
                if (seq.resolution.width, seq.resolution.height) != (w, h) {
                    add(Severity::Warning, "size", None, format!("the sequence is {}x{} and the brief asks for {w}x{h}", seq.resolution.width, seq.resolution.height), "Export at the brief's size (the picture is fitted, not stretched), or reframe for it");
                }
            }
            if brief.captions && captions.is_empty() {
                add(Severity::Warning, "captions_missing", None, "the brief asks for captions and there are none".into(), "Add them with the captions op once the speech is where it will stay");
            }
            for member in &brief.cast {
                let named = titles.iter().any(|c| matches!(&c.kind, ClipKind::Title(td) if td.objects.iter().any(|o| matches!(&o.content, TitleObjectContent::TextBox(tb) if tb.text.trim().eq_ignore_ascii_case(member.name.trim())))));
                if !named {
                    add(Severity::Note, "cast_unnamed", None, format!("{} is in the cast and is never named on screen", member.name), "A lower third on their first appearance, if they appear");
                }
            }
            if brief.music && !has_music {
                add(Severity::Note, "music_missing", None, "the brief asks for music and none is heard".into(), "Place a music bed on an audio track of its own and duck it");
            }
            if let Some(l) = brief.loudness_lufs {
                add(Severity::Note, "loudness", None, format!("the delivery's loudness is {l:.0} LUFS"), "An export with no target of its own takes this one; the export's status reports what the file measures");
            }
        }
    }

    f.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.at_secs.unwrap_or(-1.0).partial_cmp(&b.at_secs.unwrap_or(-1.0)).unwrap_or(std::cmp::Ordering::Equal)));
    let errors = f.iter().filter(|x| x.severity == Severity::Error).count();
    let warnings = f.iter().filter(|x| x.severity == Severity::Warning).count();
    Report { duration_secs: duration, errors, warnings, findings: f }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, RationalTime, Timecode};
    use klipp_timeline::{Brief, CastMember, Track};

    fn tc(s: f64) -> Timecode {
        RationalTime::from_secs_f64(s, 25)
    }

    /// An interview file that says a sentence, and a music file that says nothing.
    struct Spoken(klipp_core::MediaId);

    impl Transcripts for Spoken {
        fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            if item.id != self.0 {
                return Some(Vec::new());
            }
            Some("one two three four five six seven eight nine ten".split(' ').enumerate().map(|(k, w)| SpokenWord { start_secs: k as f64, end_secs: k as f64 + 0.6, text: w.into() }).collect())
        }
    }

    /// Two selects of one interview meeting at 4 s, their sound linked under them.
    fn piece() -> (Sequence, MediaPool, klipp_core::MediaId, klipp_core::MediaId) {
        let mut pool = MediaPool::new();
        let mut add = |path: &str, video: bool| {
            let mut item = klipp_media::MediaItem::synthetic_for_test(path.into(), None);
            item.duration = tc(60.0);
            item.has_audio = true;
            item.video_codec = if video { "hevc".into() } else { String::new() };
            pool.add(item)
        };
        let talk = add("/tmp/talk.mov", true);
        let music = add("/tmp/music.wav", false);
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut v = Track::new("V1", TrackKind::Video);
        let mut a = Track::new("A1", TrackKind::Audio);
        for (t0, t1, s0) in [(0.0, 4.0, 0.0), (4.0, 8.0, 4.0)] {
            let mut pic = Clip::media(talk, tc(t0), tc(t1), tc(s0), tc(s0 + t1 - t0));
            let mut snd = Clip::media(talk, tc(t0), tc(t1), tc(s0), tc(s0 + t1 - t0));
            pic.linked_clip_id = Some(snd.id);
            snd.linked_clip_id = Some(pic.id);
            v.clips.push(pic);
            a.clips.push(snd);
        }
        seq.tracks = vec![v, a];
        (seq, pool, talk, music)
    }

    fn rules(r: &Report) -> Vec<&'static str> {
        r.findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn a_clean_piece_has_nothing_to_say_but_that_it_has_no_brief() {
        let (seq, pool, talk, _) = piece();
        // The two selects run on from each other in the file: a cut, not a jump. The
        // words sit inside the seconds, so no edge is inside one.
        let r = check(&seq, &pool, &Spoken(talk));
        assert_eq!(rules(&r), vec!["no_brief"], "{}", r.text());
        assert_eq!((r.errors, r.warnings), (0, 0));
        assert!(r.text().starts_with("Checked 8.00 s: 0 errors, 0 warnings, 1 note."), "{}", r.text());
    }

    #[test]
    fn what_an_editor_would_catch_is_caught() {
        let (mut seq, mut pool, talk, music) = piece();
        let _ = &mut pool;
        // The second select comes from later in the file and starts a second late: a
        // jump cut after a second of black, its sound starting inside the word "six".
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[1];
            (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (tc(5.0), tc(9.0), tc(5.3), tc(9.3));
        }
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"black") && r.errors == 1, "{}", r.text());
        let word = r.findings.iter().find(|f| f.rule == "word_cut").expect("a cut inside a word");
        assert!(word.what.contains("\"six\"") && word.hint.contains("5.00 s"), "{word:?}");

        // Closed up, it is a jump cut; B-roll across the join covers it.
        for t in seq.tracks.iter_mut() {
            let c = &mut t.clips[1];
            (c.timeline_in, c.timeline_out, c.source_in, c.source_out) = (tc(4.0), tc(8.0), tc(20.0), tc(24.0));
        }
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"jump_cut") && !rules(&r).contains(&"black"), "{}", r.text());
        let mut broll = Track::new("V2", TrackKind::Video);
        let mut cover = Clip::media(music, tc(3.0), tc(5.0), tc(0.0), tc(2.0));
        cover.volume = 0.0;
        broll.clips = vec![cover];
        seq.tracks.insert(0, broll);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(!rules(&r).contains(&"jump_cut"), "{}", r.text());

        // A five-frame shot is a flash.
        seq.tracks[0].clips.push(Clip::media(music, tc(6.0), tc(6.2), tc(0.0), tc(0.2)));
        seq.tracks[0].clips[1].volume = 0.0;
        assert!(rules(&check(&seq, &pool, &Spoken(talk))).contains(&"flash"));
        seq.tracks[0].clips.pop();

        // Music under the speech at full level is said; ducked, it is not.
        let mut bed = Track::new("A2", TrackKind::Audio);
        bed.clips = vec![Clip::media(music, tc(0.0), tc(8.0), tc(0.0), tc(8.0))];
        seq.tracks.push(bed);
        let r = check(&seq, &pool, &Spoken(talk));
        assert!(rules(&r).contains(&"music_level"), "{}", r.text());
        klipp_core::keyframe::upsert_keyframe(&mut seq.tracks.last_mut().unwrap().clips[0].volume_keyframes, 0.0, 0.3);
        assert!(!rules(&check(&seq, &pool, &Spoken(talk))).contains(&"music_level"));
    }

    #[test]
    fn the_brief_is_what_the_piece_is_held_to() {
        let (mut seq, pool, talk, _) = piece();
        seq.brief = Some(Brief {
            target_secs: Some(60.0),
            width: Some(1080),
            height: Some(1920),
            loudness_lufs: Some(-14.0),
            captions: true,
            music: true,
            cast: vec![CastMember { name: "Ada".into(), role: "Engineer".into(), described: String::new() }],
            ..Default::default()
        });
        let r = check(&seq, &pool, &Spoken(talk));
        let got = rules(&r);
        for rule in ["length", "size", "captions_missing", "cast_unnamed", "music_missing", "loudness"] {
            assert!(got.contains(&rule), "{rule} missing from {}", r.text());
        }
        assert!(!got.contains(&"no_brief"));
        assert_eq!(r.findings[0].rule, "length", "errors come first: {}", r.text());
        // As JSON for a caller that wants to filter.
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["findings"][0]["severity"], "error");
    }
}
