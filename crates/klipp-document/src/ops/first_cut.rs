//! First Cut (#3308): a talking-head rough cut in one step, made only of
//! edits an assistant could ask for one by one, so the app and an
//! assistant make the same cut. The pauses, filler words and retakes in
//! what is said come out as cuts across the timeline; every other piece of
//! a shot is pushed in (`refit`'s punch); captions are made from the
//! transcript; a music bed goes under, ducked under the speech.
//!
//! The plan lists each change with where it is on the timeline as it was,
//! so one can be left out ([`Op::FirstCut`]'s `exclude`): the plan without
//! it, applied to the sequence as it was, is that change taken back.

use super::*;

/// How tight the pauses are cut.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tightness {
    Gentle,
    #[default]
    Normal,
    Tight,
}

impl Tightness {
    /// The shortest pause cut, and what is left of it at each end, seconds.
    pub fn pause(self) -> (f64, f64) {
        match self {
            Tightness::Gentle => (1.0, 0.25),
            Tightness::Normal => (0.6, 0.15),
            Tightness::Tight => (0.35, 0.08),
        }
    }
}

/// What a change does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Silence,
    Filler,
    Retake,
    PunchIn,
    Captions,
    Music,
}

impl Kind {
    /// Whether the change takes a stretch out of the timeline.
    pub fn cuts(self) -> bool {
        matches!(self, Kind::Silence | Kind::Filler | Kind::Retake)
    }
}

/// One change of a First Cut: what it does, the stretch of the timeline
/// (as it was) it takes out or starts at, and what it says.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub kind: Kind,
    pub from_secs: f64,
    pub to_secs: f64,
    pub what: String,
}

/// Which steps run.
#[derive(Debug, Clone, PartialEq)]
pub struct Steps {
    pub silences: bool,
    pub fillers: bool,
    pub retakes: bool,
    pub punch: bool,
    pub captions: bool,
    pub music: Option<MediaId>,
    pub tightness: Tightness,
}

impl Default for Steps {
    fn default() -> Self {
        Steps { silences: true, fillers: true, retakes: true, punch: true, captions: true, music: None, tightness: Tightness::default() }
    }
}

/// How much a shot is pushed in on every other piece.
pub const PUNCH: f32 = 1.15;
/// Sentences back a retake may repeat.
const RETAKE_REACH: usize = 3;
/// The share of the shorter sentence's words two takes must share, in order.
const RETAKE_MATCH: f64 = 0.8;
/// The fewest words a sentence needs to be told a retake.
const RETAKE_WORDS: usize = 3;
/// The words a later take may say before it opens with an earlier one.
const RETAKE_LEAD_IN: usize = 2;
/// A pause between words that ends a sentence, seconds.
const SENTENCE_PAUSE: f64 = 1.0;
/// The shortest piece of a clip a cut may leave, seconds: a shorter one
/// with nothing said in it goes with the cut beside it (#3441).
const MIN_PIECE: f64 = 0.5;

/// A clip heard speaking, and its file's words in its range.
struct Speaker<'a> {
    clip: &'a Clip,
    item: &'a klipp_media::MediaItem,
    words: Vec<SpokenWord>,
}

/// The clips heard speaking, each sound once: a picture and its linked
/// sound are one. Speech is a speech role, or words in its transcript;
/// a retimed clip is left out, as its file's times don't land on its
/// frames.
fn speakers<'a>(seq: &'a Sequence, pool: &'a MediaPool, transcripts: &dyn Transcripts) -> Vec<Speaker<'a>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for track in seq.tracks.iter().filter(|t| !t.locked) {
        for clip in &track.clips {
            if !is_heard(seq, pool, track, clip) || !seen.insert(sound_settings_clip(seq, clip).id) {
                continue;
            }
            let ClipKind::Media { media_id } = &clip.kind else { continue };
            let Some(item) = pool.get(media_id) else { continue };
            if (clip.speed - 1.0).abs() > 1e-6 || !clip.speed_keyframes.is_empty() || clip.hold.is_some() || clip.is_reversed() {
                continue;
            }
            let (s_in, s_out) = (secs(clip.source_in), secs(clip.source_out));
            let words: Vec<SpokenWord> = transcripts.words(item).unwrap_or_default().into_iter().filter(|w| w.end_secs > s_in && w.start_secs < s_out).collect();
            let speech_role = sound_settings_clip(seq, clip).role.as_ref().is_some_and(|r| r.is_speech());
            if speech_role || !words.is_empty() {
                out.push(Speaker { clip, item, words });
            }
        }
    }
    out
}

/// Whether First Cut has anything to cut: a clip heard speaking. What the
/// button's showing goes by (#3308): it shows only then.
pub fn has_speech(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> bool {
    !speakers(seq, pool, transcripts).is_empty()
}

/// A word as compared between takes: lower case, letters and digits.
fn token(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The longest run of `a`'s words found in order in `b`.
fn common_in_order(a: &[String], b: &[String]) -> usize {
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        let mut prev = 0;
        for (j, y) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = if x == y { prev + 1 } else { row[j + 1].max(row[j]) };
            prev = here;
        }
    }
    row[b.len()]
}

/// Whether `later` says `earlier` again: most of both lines' words, in
/// order, or `earlier` given up on and `later` opening with all of it, an
/// "um" or two before. Most of the shorter line alone took two sentences that
/// only opened alike for one, "The wizard is the AI." for a take of "The
/// wizard is the frontier level of intelligence.", and cut a line meant to
/// be said.
fn same_line(earlier: &[String], later: &[String]) -> bool {
    if earlier.len().min(later.len()) < RETAKE_WORDS {
        return false;
    }
    let common = common_in_order(earlier, later) as f64;
    let most_of = |line: &[String]| common >= RETAKE_MATCH * line.len() as f64;
    let opens_with_it = later.windows(earlier.len()).take(RETAKE_LEAD_IN + 1).any(|w| w == earlier);
    (most_of(earlier) && most_of(later)) || opens_with_it
}

/// The sentences of `words`, by their index ranges: a sentence ends at a
/// full stop, a question or an exclamation, or a pause.
fn sentences(words: &[SpokenWord]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, w) in words.iter().enumerate() {
        let ends = w.text.trim_end().ends_with(['.', '?', '!']) || words.get(i + 1).is_some_and(|n| n.start_secs - w.end_secs > SENTENCE_PAUSE);
        if ends || i + 1 == words.len() {
            out.push(start..i + 1);
            start = i + 1;
        }
    }
    out
}

/// The retakes in `words`: (where the earlier take starts, where the
/// last starts, the line), in the file's seconds. A line said again
/// within [`RETAKE_REACH`] sentences keeps its last take; the earlier
/// one, and anything said between, goes.
pub fn retakes(words: &[SpokenWord]) -> Vec<(f64, f64, String)> {
    let sents = sentences(words);
    let tokens: Vec<Vec<String>> = sents.iter().map(|r| words[r.clone()].iter().map(|w| token(&w.text)).filter(|t| !t.is_empty()).collect()).collect();
    let mut out: Vec<(f64, f64, String)> = Vec::new();
    // Sentences already in a retake, so two takes of one line aren't
    // counted twice.
    let mut taken = 0;
    for k in 1..sents.len() {
        let reach = k.saturating_sub(RETAKE_REACH).max(taken);
        if let Some(j) = (reach..k).rev().find(|&j| same_line(&tokens[j], &tokens[k])) {
            let line = words[sents[k].clone()].iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" ");
            let from = words[sents[j].start].start_secs;
            let to = words[sents[k].start].start_secs;
            // A third take of the same line takes the second with it.
            match out.last_mut() {
                Some(last) if (last.1 - from).abs() < 1e-9 => last.1 = to,
                _ => out.push((from, to, line)),
            }
            taken = k;
        }
    }
    out
}

/// Whether cutting `from..to` (timeline seconds) takes a whole frame: a cut
/// goes to whole frames inward, so a shorter one takes nothing. What is left
/// of a pause at a cut already made is that short.
fn takes_a_frame(from: f64, to: f64, rate: f64) -> bool {
    ((to * rate) + 1e-6).floor() > ((from * rate) - 1e-6).ceil()
}

/// `t` (seconds) as m:ss.
fn at(t: f64) -> String {
    let t = t.max(0.0);
    format!("{}:{:04.1}", (t / 60.0).floor() as u64, t % 60.0)
}

/// The changes a First Cut makes, in the order of the timeline as it is:
/// pauses, filler words and retakes in what is heard, then the punch-in,
/// captions and music bed. Pure: the same sequence and transcripts give
/// the same list.
pub fn plan(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts, steps: &Steps) -> Vec<Change> {
    let speaking = speakers(seq, pool, transcripts);
    if speaking.is_empty() {
        return Vec::new();
    }
    let mut all: Vec<Change> = Vec::new();
    let (min_pause, padding) = steps.tightness.pause();
    let rate = seq.fps.as_f64();
    for s in speaking {
        let (s_in, s_out) = (secs(s.clip.source_in), secs(s.clip.source_out));
        let (t_in, t_out) = (secs(s.clip.timeline_in), secs(s.clip.timeline_out));
        let tl = |f: f64| (f.clamp(s_in, s_out) - s_in + t_in).clamp(t_in, t_out);
        let mut cuts: Vec<Change> = Vec::new();
        if steps.silences {
            for &(a, b) in transcripts.silences(s.item).iter().filter(|(a, b)| b - a >= min_pause) {
                let (from, to) = (tl(a + padding), tl(b - padding));
                // Shorter than this would read "0.0 s pause".
                if takes_a_frame(from, to, rate) && to - from >= 0.05 {
                    cuts.push(Change { kind: Kind::Silence, from_secs: from, to_secs: to, what: format!("{:.1} s pause at {}", to - from, at(from)) });
                }
            }
        }
        if steps.fillers {
            let words = klipp_core::lang::words(transcripts.language(s.item).as_deref()).ok();
            for w in s.words.iter().filter(|w| words.as_ref().is_some_and(|l| l.is_filler(&w.text))) {
                let (from, to) = (tl(w.start_secs), tl(w.end_secs));
                if takes_a_frame(from, to, rate) {
                    cuts.push(Change { kind: Kind::Filler, from_secs: from, to_secs: to, what: format!("\"{}\" at {}", w.text.trim(), at(from)) });
                }
            }
        }
        if steps.retakes {
            for (a, b, line) in retakes(&s.words) {
                let (from, to) = (tl(a), tl(b));
                if takes_a_frame(from, to, rate) {
                    cuts.push(Change { kind: Kind::Retake, from_secs: from, to_secs: to, what: format!("earlier take of \"{line}\" at {}", at(from)) });
                }
            }
        }
        let said: Vec<(f64, f64)> = s.words.iter().map(|w| (tl(w.start_secs), tl(w.end_secs))).collect();
        close_slivers(&mut cuts, &said, t_in, t_out);
        all.extend(cuts);
    }
    let mut cuts = all;
    cuts.sort_by(|a, b| a.from_secs.total_cmp(&b.from_secs).then(a.to_secs.total_cmp(&b.to_secs)));
    let mut out = cuts;
    if steps.punch {
        out.push(Change { kind: Kind::PunchIn, from_secs: 0.0, to_secs: 0.0, what: format!("every other piece of a shot pushed in to {PUNCH}") });
    }
    // Captions only where there are none to replace: a person's own
    // captions are never taken.
    let captioned = seq.tracks.iter().any(|t| t.kind == TrackKind::Subtitle && !t.clips.is_empty());
    if steps.captions && !captioned {
        out.push(Change { kind: Kind::Captions, from_secs: 0.0, to_secs: 0.0, what: "captions from what is said".into() });
    }
    if let Some(media) = &steps.music {
        let name = pool.get(media).map(|m| m.filename.clone()).unwrap_or_else(|| "music".into());
        out.push(Change { kind: Kind::Music, from_secs: 0.0, to_secs: 0.0, what: format!("{name} under the speech, ducked") });
    }
    out
}

/// One clip's cuts reaching over the pieces they would leave shorter than
/// [`MIN_PIECE`] with nothing said in them: between two cuts, and at the
/// clip's ends (`t_in`, `t_out`). `said` is its words, timeline seconds.
fn close_slivers(cuts: &mut [Change], said: &[(f64, f64)], t_in: f64, t_out: f64) {
    if cuts.is_empty() {
        return;
    }
    cuts.sort_by(|a, b| a.from_secs.total_cmp(&b.from_secs));
    let sliver = |a: f64, b: f64| b - a > 1e-6 && b - a < MIN_PIECE && !said.iter().any(|&(x, y)| y > a && x < b);
    if sliver(t_in, cuts[0].from_secs) {
        cuts[0].from_secs = t_in;
    }
    // The cut reaching furthest so far: the piece after it is the gap.
    let mut reach = 0;
    for i in 1..cuts.len() {
        if sliver(cuts[reach].to_secs, cuts[i].from_secs) {
            cuts[reach].to_secs = cuts[i].from_secs;
        }
        if cuts[i].to_secs > cuts[reach].to_secs {
            reach = i;
        }
    }
    if sliver(cuts[reach].to_secs, t_out) {
        cuts[reach].to_secs = t_out;
    }
}

/// An op from its JSON, as an assistant would send it: the defaults are
/// the op's own.
fn op(v: serde_json::Value) -> Op {
    serde_json::from_value(v).expect("a First Cut op is well formed")
}

/// The ops that make `changes` but those at the indices in `exclude`: the
/// stretches taken out, latest first so none moves another, each to whole
/// frames inward, those that touch made one; then the punch-in, captions
/// and music bed over the cut.
pub fn ops(seq: &Sequence, changes: &[Change], exclude: &[usize], music: Option<&MediaId>) -> Vec<Op> {
    let rate = seq.fps.as_f64();
    let kept = |i: usize| !exclude.contains(&i);
    let mut spans: Vec<(i64, i64)> = changes
        .iter()
        .enumerate()
        .filter(|(i, c)| kept(*i) && c.kind.cuts())
        .map(|(_, c)| (((c.from_secs * rate) - 1e-6).ceil() as i64, ((c.to_secs * rate) + 1e-6).floor() as i64))
        .filter(|(a, b)| b > a)
        .collect();
    spans.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    let mut out: Vec<Op> = merged.iter().rev().map(|&(a, b)| op(serde_json::json!({"op": "cut", "from": {"frame": a}, "to": {"frame": b}}))).collect();
    let wants = |k: Kind| changes.iter().enumerate().any(|(i, c)| kept(i) && c.kind == k);
    if wants(Kind::PunchIn) {
        out.push(op(serde_json::json!({"op": "refit", "markers": false, "punch": PUNCH})));
    }
    if wants(Kind::Captions) {
        out.push(op(serde_json::json!({"op": "captions"})));
    }
    if let (true, Some(media)) = (wants(Kind::Music), music) {
        let removed: i64 = merged.iter().map(|(a, b)| b - a).sum();
        let end = seq.tracks.iter().flat_map(|t| &t.clips).map(|c| (secs(c.timeline_out) * rate).round() as i64).max().unwrap_or(0);
        let length = (end - removed).max(1) as f64 / rate;
        // An audio track goes at the bottom: its index is the count now.
        let track = seq.tracks.len();
        out.push(op(serde_json::json!({"op": "add_track", "kind": "audio", "name": "Music"})));
        out.push(op(serde_json::json!({"op": "place", "media_id": media, "track": track, "at": {"secs": 0.0}, "source_out": length})));
        out.push(op(serde_json::json!({"op": "duck", "clip_id": {"track": track, "secs": 0.5 / rate}})));
    }
    out
}

/// The tracks a First Cut of `changes` but `exclude` makes of `before`
/// (the sequence as it was): what taking a change back, or putting it
/// back, swaps in, as one step.
pub fn tracks_without(before: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts, changes: &[Change], exclude: &[usize], music: Option<&MediaId>) -> Result<Vec<klipp_timeline::Track>, Refusal> {
    let plan = resolve_with(before, pool, &ops(before, changes, exclude, music), transcripts)?;
    let mut seqs = vec![before.clone()];
    let mut pool = pool.clone();
    let mut fps = before.fps;
    let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
    for step in &plan.steps {
        view.apply_forward(&step.edit);
    }
    Ok(seqs.remove(0).tracks)
}

/// Where change `at` of `changes` is on the timeline once those not in
/// `exclude` are made, in seconds: its place as it was, less the cuts
/// kept before it. What a click on a change goes to.
pub fn now_at(changes: &[Change], exclude: &[usize], at: usize) -> f64 {
    let Some(this) = changes.get(at) else { return 0.0 };
    let mut taken: Vec<(f64, f64)> = changes.iter().enumerate().filter(|(i, c)| *i != at && !exclude.contains(i) && c.kind.cuts() && c.from_secs < this.from_secs).map(|(_, c)| (c.from_secs, c.to_secs.min(this.from_secs))).collect();
    taken.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Cuts that overlap are one stretch, counted once.
    let mut removed = 0.0;
    let mut reach = f64::NEG_INFINITY;
    for (a, b) in taken {
        let a = a.max(reach);
        if b > a {
            removed += b - a;
        }
        reach = reach.max(b);
    }
    (this.from_secs - removed).max(0.0)
}

/// The changes as a numbered list, one a line: what `exclude` counts.
pub fn listed(changes: &[Change]) -> String {
    changes.iter().enumerate().map(|(i, c)| format!("{i}: {}", c.what)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::Track;

    fn tc(secs: f64) -> Timecode {
        RationalTime::from_secs_f64(secs, 25)
    }

    fn w(a: f64, b: f64, text: &str) -> SpokenWord {
        SpokenWord { start_secs: a, end_secs: b, text: format!(" {text}"), confidence: 1.0 }
    }

    /// A take of "so what we did was", a pause, an "um", the line again
    /// to its end, then another line and a last pause (file seconds).
    struct Talk;

    impl Transcripts for Talk {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Some(vec![
                w(0.0, 0.2, "So"), w(0.25, 0.5, "what"), w(0.55, 0.7, "we"), w(0.75, 1.0, "did"), w(1.05, 1.6, "was."),
                w(3.0, 3.3, "um"),
                w(3.4, 3.6, "So"), w(3.65, 3.9, "what"), w(3.95, 4.1, "we"), w(4.15, 4.4, "did"), w(4.45, 4.7, "was"), w(4.75, 5.0, "build"), w(5.05, 5.2, "it."),
                w(5.4, 5.7, "Then"), w(5.75, 5.9, "we"), w(5.95, 6.3, "shipped"), w(6.35, 6.6, "it."),
            ])
        }
        fn silences(&self, _item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
            vec![(1.6, 3.0), (6.6, 8.0)]
        }
    }

    /// Nothing said: music, say.
    struct Quiet;

    impl Transcripts for Quiet {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            None
        }
    }

    /// A 10 s talking clip with its sound linked under it.
    fn talking() -> (Sequence, MediaPool) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/talk.mov".into(), None);
        item.duration = tc(10.0);
        item.has_audio = true;
        let media = pool.add(item);
        let mut v = Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        let mut a = Clip::media(media, tc(0.0), tc(10.0), tc(0.0), tc(10.0));
        (v.linked_clip_id, a.linked_clip_id) = (Some(a.id), Some(v.id));
        let mut vt = Track::new("V1", TrackKind::Video);
        vt.clips = vec![v];
        let mut at = Track::new("A1", TrackKind::Audio);
        at.clips = vec![a];
        seq.tracks = vec![vt, at];
        (seq, pool)
    }

    fn end_frames(seq: &Sequence) -> i64 {
        seq.tracks.iter().flat_map(|t| &t.clips).map(|c| (secs(c.timeline_out) * 25.0).round() as i64).max().unwrap_or(0)
    }

    /// `op` applied to `seq` as the app applies a step.
    fn applied(seq: &Sequence, pool: &MediaPool, op: Op) -> Sequence {
        let plan = resolve_with(seq, pool, &[op], &Talk).unwrap_or_else(|r| panic!("{r:?}"));
        assert_eq!(plan.steps.len(), 1, "one undo step");
        let mut seqs = vec![seq.clone()];
        let mut p = pool.clone();
        let mut fps = seq.fps;
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        seqs.remove(0)
    }

    fn first_cut(exclude: Vec<usize>) -> Op {
        Op::FirstCut { silences: true, fillers: true, retakes: true, punch: false, captions: false, music: None, tightness: Tightness::Normal, exclude }
    }

    #[test]
    fn the_plan_lists_the_pauses_fillers_and_retakes_in_time_order() {
        let (seq, pool) = talking();
        let changes = plan(&seq, &pool, &Talk, &Steps::default());
        let kinds: Vec<Kind> = changes.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [Kind::Retake, Kind::Silence, Kind::Filler, Kind::Silence, Kind::PunchIn, Kind::Captions], "{changes:#?}");
        // The earlier take, to where the last one starts (its "um", which
        // goes as a filler); each pause but its padding; the filler's own
        // time. The picture and its linked sound are one speaker: nothing
        // twice.
        let span = |c: &Change| ((c.from_secs * 100.0).round() / 100.0, (c.to_secs * 100.0).round() / 100.0);
        assert_eq!(span(&changes[0]), (0.0, 3.0));
        assert_eq!(span(&changes[1]), (1.75, 2.85));
        assert_eq!(span(&changes[2]), (3.0, 3.3));
        assert_eq!(span(&changes[3]), (6.75, 7.85));
        assert!(changes[0].what.contains("So what we did was build it."), "{}", changes[0].what);
        assert!(!has_speech(&seq, &pool, &Quiet) && plan(&seq, &pool, &Quiet, &Steps::default()).is_empty(), "no speech, no First Cut");
        assert!(has_speech(&seq, &pool, &Talk));
    }

    #[test]
    fn first_cut_is_one_step_and_a_change_left_out_is_taken_back() {
        let (seq, pool) = talking();
        assert_eq!(end_frames(&seq), 250);
        // 0 to 3.0 s (the retake, with the pause in it) and the "um" to
        // 3.3 s touching it, made one, and 6.75 to 7.85 s, to whole frames
        // inward: 82 + 27 frames.
        let cut = applied(&seq, &pool, first_cut(Vec::new()));
        assert_eq!(end_frames(&cut), 250 - 82 - 27);
        // The last pause left out: the retake and the "um" are taken.
        let kept = applied(&seq, &pool, first_cut(vec![3]));
        assert_eq!(end_frames(&kept), 250 - 82);
        // A change that isn't there is refused, saying how many there are.
        let refused = resolve_with(&seq, &pool, &[first_cut(vec![9])], &Talk).unwrap_err();
        assert!(refused.why.contains("no change 9"), "{}", refused.why);
        // No speech: refused, saying so.
        let none = resolve_with(&seq, &pool, &[first_cut(Vec::new())], &Quiet).unwrap_err();
        assert!(none.why.contains("heard speaking"), "{}", none.why);
    }

    #[test]
    fn the_punch_in_captions_and_music_are_the_existing_ops() {
        let (seq, pool) = talking();
        let music = MediaId::new();
        let steps = Steps { music: Some(music), ..Steps::default() };
        let changes = plan(&seq, &pool, &Talk, &steps);
        let made: Vec<String> = ops(&seq, &changes, &[], Some(&music)).iter().map(|o| serde_json::to_value(o).unwrap()["op"].as_str().unwrap().to_string()).collect();
        assert_eq!(made, ["cut", "cut", "refit", "captions", "add_track", "place", "duck"]);
        // The cuts latest first, so neither moves the other.
        let first = serde_json::to_value(&ops(&seq, &changes, &[], None)[0]).unwrap();
        assert_eq!(first["from"]["frame"], 169);
    }

    #[test]
    fn a_change_taken_back_is_the_cut_without_it() {
        let (seq, pool) = talking();
        let changes = plan(&seq, &pool, &Talk, &Steps { punch: false, captions: false, ..Steps::default() });
        // The last pause taken back: the tracks the op with it excluded makes.
        // Compared by each clip's place and source: a split's pieces get
        // new ids each time.
        let shape = |tracks: &[Track]| tracks.iter().map(|t| t.clips.iter().map(|c| [c.timeline_in, c.timeline_out, c.source_in, c.source_out].map(|t| (secs(t) * 25.0).round() as i64)).collect::<Vec<_>>()).collect::<Vec<_>>();
        let back = tracks_without(&seq, &pool, &Talk, &changes, &[3], None).unwrap();
        assert_eq!(shape(&back), shape(&applied(&seq, &pool, first_cut(vec![3])).tracks));
        // Everything taken back: the sequence as it was.
        assert_eq!(shape(&tracks_without(&seq, &pool, &Talk, &changes, &[0, 1, 2, 3], None).unwrap()), shape(&seq.tracks));
    }

    #[test]
    fn a_change_is_found_where_the_cuts_before_it_moved_it() {
        let (seq, pool) = talking();
        let changes = plan(&seq, &pool, &Talk, &Steps::default());
        // The last pause, at 6.75 s as it was: the retake and the "um"
        // before it (0 to 3.3 s, the first pause inside them) moved it up.
        assert!((now_at(&changes, &[], 3) - 3.45).abs() < 1e-9, "{}", now_at(&changes, &[], 3));
        // With the retake taken back, only the pause and the "um" did.
        assert!((now_at(&changes, &[0], 3) - (6.75 - 1.1 - 0.3)).abs() < 1e-9, "{}", now_at(&changes, &[0], 3));
    }

    #[test]
    fn a_line_said_again_keeps_its_last_take() {
        let words = |text: &str, from: f64| text.split(' ').enumerate().map(|(i, t)| w(from + i as f64 * 0.3, from + i as f64 * 0.3 + 0.25, t)).collect::<Vec<_>>();
        // One word changed: the same line.
        let mut said = words("We tried it on the boat.", 0.0);
        said.extend(words("We tried it on the river.", 3.0));
        assert_eq!(retakes(&said).len(), 1);
        // Two different lines are not.
        let mut said = words("We tried it on the boat.", 0.0);
        said.extend(words("Then the rain came in fast.", 3.0));
        assert!(retakes(&said).is_empty());
        // Too short to tell.
        let mut said = words("Right.", 0.0);
        said.extend(words("Right.", 2.0));
        assert!(retakes(&said).is_empty());
    }

    /// Two sentences that open alike are not one line said twice: the later
    /// must repeat the earlier as a whole. A true retake is still one, with a
    /// word changed, or restarted from a line given up on.
    #[test]
    fn a_retake_repeats_the_whole_line_not_its_opening() {
        let words = |text: &str, from: f64| text.split(' ').enumerate().map(|(i, t)| w(from + i as f64 * 0.3, from + i as f64 * 0.3 + 0.25, t)).collect::<Vec<_>>();
        let two = |a: &str, b: &str| {
            let mut said = words(a, 0.0);
            said.extend(words(b, 5.0));
            retakes(&said).len()
        };
        assert_eq!(two("The wizard is the AI.", "The wizard is the frontier level of intelligence."), 0, "two sentences: both stay");
        assert_eq!(two("We went to the river.", "We went to the river and then the sea."), 1, "given up, and said in full");
        assert_eq!(two("We went to the river.", "um We went to the river and then the sea."), 1, "an um before it");
        assert_eq!(two("We went to the river.", "After lunch that day we went to the river."), 0, "said later in another line");
        assert_eq!(two("The wizard is the AI.", "The wizard is the AI."), 1);
        assert_eq!(two("The wizard is really the AI.", "The wizard is truly the AI."), 1, "a word changed");
    }

    /// Run again on a cut timeline, what is left of a pause at each cut is
    /// less than a frame: nothing to cut, and no "0.0 s pause" listed.
    #[test]
    fn a_second_run_lists_nothing_at_the_cuts() {
        let (seq, pool) = talking();
        let cut = applied(&seq, &pool, first_cut(Vec::new()));
        let again = plan(&cut, &pool, &Talk, &Steps { punch: false, captions: false, ..Steps::default() });
        let rate = cut.fps.as_f64();
        for c in &again {
            let frames = ((c.to_secs * rate) + 1e-6).floor() - ((c.from_secs * rate) - 1e-6).ceil();
            assert!(frames >= 1.0, "less than a frame: {c:?}");
            assert!(!c.what.contains("0.0 s"), "{c:?}");
        }
    }

    /// "Um, so, uh, let me show you": fillers that start between frames.
    struct Hesitant;

    impl Transcripts for Hesitant {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Some(vec![w(0.02, 0.31, "Um"), w(0.41, 0.6, "so,"), w(0.63, 0.9, "uh"), w(1.0, 1.2, "let"), w(1.25, 1.4, "me"), w(1.45, 1.7, "show"), w(1.75, 2.0, "you.")])
        }
    }

    /// The captions read what is heard after the cut: a filler cut from
    /// a frame after it starts is not in them (#3440).
    #[test]
    fn the_captions_leave_out_the_fillers_that_were_cut() {
        let (seq, pool) = talking();
        let op = Op::FirstCut { silences: false, fillers: true, retakes: false, punch: false, captions: true, music: None, tightness: Tightness::Normal, exclude: Vec::new() };
        let plan = resolve_with(&seq, &pool, &[op], &Hesitant).unwrap_or_else(|r| panic!("{r:?}"));
        let mut seqs = vec![seq.clone()];
        let (mut p, mut fps) = (pool.clone(), seq.fps);
        let mut view = crate::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut fps };
        view.apply_forward(&plan.steps[0].edit);
        let cut = seqs.remove(0);
        let track = cut.tracks.iter().position(|t| t.kind == TrackKind::Subtitle).expect("a caption track");
        let text: Vec<String> = crate::caption_translation::captions(&cut, track).into_iter().map(|c| c.text).collect();
        assert_eq!(text.join(" "), "so, let me show you.");
    }

    /// An "um" just in, a pause running into another, and a pause near
    /// the end, with nothing said between them.
    struct Slivers;

    impl Transcripts for Slivers {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Some(vec![w(0.1, 0.35, "um"), w(0.45, 0.8, "Hello"), w(0.85, 1.0, "there."), w(2.95, 3.2, "um"), w(3.3, 3.5, "we"), w(3.55, 8.0, "go.")])
        }
        fn silences(&self, _item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
            vec![(1.0, 2.9), (8.0, 9.9)]
        }
    }

    /// Cuts that nearly meet leave no piece shorter than half a second
    /// with nothing said in it, at a clip's ends or between them (#3441).
    #[test]
    fn cuts_that_nearly_meet_leave_no_sliver() {
        let (seq, pool) = talking();
        let changes = plan(&seq, &pool, &Slivers, &Steps { punch: false, captions: false, ..Steps::default() });
        let span = |c: &Change| ((c.from_secs * 100.0).round() / 100.0, (c.to_secs * 100.0).round() / 100.0);
        let spans: Vec<(f64, f64)> = changes.iter().map(span).collect();
        assert_eq!(spans, [(0.0, 0.35), (1.15, 2.95), (2.95, 3.2), (8.15, 10.0)], "{changes:#?}");
    }
}
