//! The Pace Strip's hints (#3313): where a cut may lose its viewer. A
//! slow hook (nothing changes in the first 3 s), dead air (over a second
//! where nothing is heard), shots long for where the piece goes (and in
//! which nothing moves or speaks), and text on screen too briefly to read.
//! Hints only, never errors, from the analysis that exists: a file not
//! analysed says nothing.
//!
//! The hook and long shots need a destination, the brief's platform: a
//! piece with none gets neither. Text too briefly to read is the Delivery
//! Check's own rule, so the strip and the check agree.

use std::sync::OnceLock;

use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};
use serde::{Deserialize, Serialize};

use crate::ops::Transcripts;

/// How long the opening has to change something in, seconds.
pub const HOOK_SECS: f64 = 3.0;
/// The shortest stretch with nothing heard that is dead air, seconds.
pub const DEAD_AIR_SECS: f64 = 1.0;
/// The motion analyser's frame-to-frame difference under which a picture
/// is still (4 and up is a moving camera).
const STILL_MOTION: f32 = 4.0;

/// What a hint is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaceKind {
    Hook,
    DeadAir,
    LongShot,
    BriefText,
}

/// One hint: what it is about, the stretch of the timeline (seconds),
/// what was found and what might help.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PaceMark {
    pub kind: PaceKind,
    pub from_secs: f64,
    pub to_secs: f64,
    pub what: String,
    pub hint: String,
}

/// A destination's limits: the platforms that name it in a brief, and the
/// longest a still, silent shot runs there.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub id: String,
    pub platforms: Vec<String>,
    pub long_shot_secs: f64,
    /// When the limit was last thought through.
    pub checked: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    destination: Vec<Destination>,
}

/// The destinations, in the file's order, read once; none when the file
/// doesn't read, which its test keeps from shipping.
pub fn destinations() -> &'static [Destination] {
    static ALL: OnceLock<Vec<Destination>> = OnceLock::new();
    ALL.get_or_init(|| toml::from_str::<File>(include_str!("../../../assets/presets/pace/builtin.toml")).map(|f| f.destination).unwrap_or_default())
}

/// The destination `seq`'s brief names, the first whose platform words
/// its platform has; `None` for a piece with no destination.
pub fn destination_of(seq: &Sequence) -> Option<&'static Destination> {
    let platform = seq.brief.as_ref()?.platform.to_lowercase();
    if platform.trim().is_empty() {
        return None;
    }
    destinations().iter().find(|d| d.platforms.iter().any(|p| platform.contains(p.as_str())))
}

fn secs(t: klipp_core::Timecode) -> f64 {
    t.to_secs_f64()
}

/// Whether `c` is a picture: not a gap.
fn is_picture(c: &Clip) -> bool {
    !matches!(c.kind, ClipKind::Gap)
}

/// The picture on top at `t`: the first visible video track's clip there.
fn shown_at(seq: &Sequence, t: f64) -> Option<&Clip> {
    seq.tracks
        .iter()
        .filter(|tr| tr.kind == TrackKind::Video && tr.visible)
        .find_map(|tr| tr.clips.iter().find(|c| is_picture(c) && secs(c.timeline_in) <= t && t < secs(c.timeline_out)))
}

/// A clip's file second as a timeline second, for a clip at its own speed;
/// `None` for a retimed one, whose file times don't land one to one.
fn at_speed(c: &Clip) -> bool {
    (c.speed - 1.0).abs() < 1e-6 && c.speed_keyframes.is_empty() && c.hold.is_none() && !c.is_reversed()
}

/// Where the shown picture changes, inside the timeline: each edge of a
/// clip on a visible picture track, and each shot change analysed inside
/// a file.
fn cuts(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts, end: f64) -> Vec<f64> {
    let mut out = Vec::new();
    for c in seq.tracks.iter().filter(|t| t.kind == TrackKind::Video && t.visible).flat_map(|t| &t.clips).filter(|c| is_picture(c)) {
        let (t_in, t_out) = (secs(c.timeline_in), secs(c.timeline_out));
        out.extend([t_in, t_out]);
        if let (ClipKind::Media { media_id }, true) = (&c.kind, at_speed(c)) {
            if let Some(item) = pool.get(media_id) {
                let (s_in, s_out) = (secs(c.source_in), secs(c.source_out));
                out.extend(transcripts.shot_changes(item).into_iter().filter(|s| *s > s_in && *s < s_out).map(|s| s - s_in + t_in));
            }
        }
    }
    out.retain(|t| *t > 1e-3 && *t < end - 1e-3);
    out.sort_by(f64::total_cmp);
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-3);
    out
}

/// Where words are said on the timeline, from the transcripts of the
/// clips that are heard: (start, end) seconds.
fn words_on_timeline(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for track in &seq.tracks {
        for c in track.clips.iter().filter(|c| at_speed(c) && crate::ops::is_heard(seq, pool, track, c)) {
            let ClipKind::Media { media_id } = &c.kind else { continue };
            let Some(item) = pool.get(media_id) else { continue };
            let (s_in, s_out, t_in) = (secs(c.source_in), secs(c.source_out), secs(c.timeline_in));
            for w in transcripts.words(item).unwrap_or_default().into_iter().filter(|w| w.end_secs > s_in && w.start_secs < s_out) {
                out.push((w.start_secs.max(s_in) - s_in + t_in, w.end_secs.min(s_out) - s_in + t_in));
            }
        }
    }
    out
}

/// Where something is heard: each heard clip's span less its measured
/// silences, as stretches that may overlap. A file whose sound isn't
/// analysed is heard throughout, so it is never taken for dead air.
fn heard(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for track in &seq.tracks {
        for c in track.clips.iter().filter(|c| crate::ops::is_heard(seq, pool, track, c)) {
            let (t_in, t_out) = (secs(c.timeline_in), secs(c.timeline_out));
            let silent: Vec<(f64, f64)> = match (&c.kind, at_speed(c)) {
                (ClipKind::Media { media_id }, true) => {
                    let s_in = secs(c.source_in);
                    pool.get(media_id).map(|item| transcripts.silences(item).into_iter().map(|(a, b)| (a - s_in + t_in, b - s_in + t_in)).collect()).unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let mut at = t_in;
            let mut gaps: Vec<(f64, f64)> = silent.into_iter().map(|(a, b)| (a.max(t_in), b.min(t_out))).filter(|(a, b)| b > a).collect();
            gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (a, b) in gaps {
                if a > at {
                    out.push((at, a));
                }
                at = at.max(b);
            }
            if t_out > at {
                out.push((at, t_out));
            }
        }
    }
    out
}

/// The hints for `seq`, in time order.
pub fn marks(seq: &Sequence, pool: &MediaPool, transcripts: &dyn Transcripts) -> Vec<PaceMark> {
    let end = secs(seq.total_duration());
    if end <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let dest = destination_of(seq);
    let cuts = cuts(seq, pool, transcripts, end);
    let words = words_on_timeline(seq, pool, transcripts);

    // The hook: only for a piece that goes somewhere, and long enough to
    // have an opening.
    if dest.is_some() && end > HOOK_SECS {
        let cut = cuts.iter().any(|t| *t < HOOK_SECS);
        let text = seq.tracks.iter().filter(|t| t.visible).flat_map(|t| &t.clips).any(|c| matches!(c.kind, ClipKind::Title(_)) && secs(c.timeline_in) < HOOK_SECS);
        let speech = words.iter().any(|(a, _)| *a < HOOK_SECS);
        let moves = shown_at(seq, 0.0).filter(|c| at_speed(c)).and_then(|c| match &c.kind {
            ClipKind::Media { media_id } => pool.get(media_id).and_then(|item| {
                let s_in = secs(c.source_in);
                transcripts.motion_between(item, s_in, s_in + HOOK_SECS.min(secs(c.timeline_out) - secs(c.timeline_in)))
            }),
            _ => None,
        });
        if !cut && !text && !speech && moves.is_none_or(|m| m < STILL_MOTION) {
            out.push(PaceMark {
                kind: PaceKind::Hook,
                from_secs: 0.0,
                to_secs: HOOK_SECS,
                what: "Nothing changes in the first 3 s: no cut, no words, no title and no movement".into(),
                hint: "Open on a cut, a line, a title or movement: viewers decide in the first seconds".into(),
            });
        }
    }

    // Dead air: over a second where nothing is heard at all.
    let mut sound = heard(seq, pool, transcripts);
    sound.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut at = 0.0;
    let mut quiet: Vec<(f64, f64)> = Vec::new();
    for (a, b) in sound {
        if a > at {
            quiet.push((at, a));
        }
        at = f64::max(at, b);
    }
    if end > at {
        quiet.push((at, end));
    }
    for (a, b) in quiet.into_iter().filter(|(a, b)| b - a > DEAD_AIR_SECS) {
        out.push(PaceMark { kind: PaceKind::DeadAir, from_secs: a, to_secs: b, what: format!("{:.1} s with nothing heard", b - a), hint: "Close it up, or put sound under it".into() });
    }

    // Long shots: over the destination's limit, with nothing moving and
    // nothing said in them. Unanalysed motion says nothing.
    if let Some(d) = dest {
        let mut edges = vec![0.0];
        edges.extend(cuts.iter().copied());
        edges.push(end);
        for w in edges.windows(2) {
            let (a, b) = (w[0], w[1]);
            if b - a <= d.long_shot_secs {
                continue;
            }
            let Some(c) = shown_at(seq, (a + b) / 2.0).filter(|c| at_speed(c)) else { continue };
            let ClipKind::Media { media_id } = &c.kind else { continue };
            let Some(item) = pool.get(media_id) else { continue };
            let (s_in, t_in) = (secs(c.source_in), secs(c.timeline_in));
            let still = transcripts.motion_between(item, a - t_in + s_in, b - t_in + s_in).is_some_and(|m| m < STILL_MOTION);
            let said = words.iter().any(|(x, y)| *x < b && *y > a);
            if still && !said {
                out.push(PaceMark {
                    kind: PaceKind::LongShot,
                    from_secs: a,
                    to_secs: b,
                    what: format!("A {:.1} s shot where nothing moves or speaks, long for {}", b - a, d.id),
                    hint: format!("Shots here run under {:.0} s when nothing happens in them: cut it shorter or cut away", d.long_shot_secs),
                });
            }
        }
    }

    // Text too briefly to read: the Delivery Check's own rule.
    for f in crate::qc::check(seq, pool, transcripts).findings.into_iter().filter(|f| matches!(f.rule, "title_short" | "too_short_to_read")) {
        let Some(at) = f.at_secs else { continue };
        let to = seq.tracks.iter().flat_map(|t| &t.clips).find(|c| (secs(c.timeline_in) - at).abs() < 1e-3).map(|c| secs(c.timeline_out)).unwrap_or(at + 0.5);
        out.push(PaceMark { kind: PaceKind::BriefText, from_secs: at, to_secs: to, what: f.what, hint: f.hint });
    }

    out.sort_by(|a, b| a.from_secs.total_cmp(&b.from_secs));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::SpokenWord;
    use klipp_core::{Fps, MediaId, RationalTime};
    use klipp_timeline::{Brief, Track};

    /// What the analysis says of every file: its words, measured silences,
    /// motion (none when not analysed) and shot changes, file seconds.
    #[derive(Default)]
    struct Room {
        words: Vec<(f64, f64)>,
        silences: Vec<(f64, f64)>,
        motion: Option<f32>,
        shots: Vec<f64>,
    }

    impl Transcripts for Room {
        fn words(&self, _item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
            Some(self.words.iter().map(|&(a, b)| SpokenWord { start_secs: a, end_secs: b, text: " word".into(), confidence: 1.0 }).collect())
        }
        fn silences(&self, _item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
            self.silences.clone()
        }
        fn motion_between(&self, _item: &klipp_media::MediaItem, _from: f64, _to: f64) -> Option<f32> {
            self.motion
        }
        fn shot_changes(&self, _item: &klipp_media::MediaItem) -> Vec<f64> {
            self.shots.clone()
        }
    }

    fn tc(secs: f64) -> RationalTime {
        RationalTime::from_secs_f64(secs, 25)
    }

    /// A camera file with sound, `len` seconds.
    fn camera(pool: &mut MediaPool, len: f64) -> MediaId {
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/take.mov".into(), None);
        item.duration = tc(len);
        item.has_audio = true;
        item.video_codec = "h264".into();
        pool.add(item)
    }

    /// Clips of `media` one after another, each `(start, end)` on the
    /// timeline from 0 s of the file, on one picture track; to `platform`.
    fn piece(platform: Option<&str>, cuts: &[(f64, f64)]) -> (Sequence, MediaPool) {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(25, 1);
        let mut pool = MediaPool::new();
        let media = camera(&mut pool, 60.0);
        let mut v = Track::new("V1", TrackKind::Video);
        v.clips = cuts.iter().map(|&(a, b)| klipp_timeline::Clip::media(media, tc(a), tc(b), tc(0.0), tc(b - a))).collect();
        seq.tracks = vec![v];
        seq.brief = platform.map(|p| Brief { platform: p.into(), ..Brief::default() });
        (seq, pool)
    }

    fn kinds(marks: &[PaceMark], kind: PaceKind) -> Vec<(f64, f64)> {
        marks.iter().filter(|m| m.kind == kind).map(|m| ((m.from_secs * 100.0).round() / 100.0, (m.to_secs * 100.0).round() / 100.0)).collect()
    }

    #[test]
    fn the_limits_file_reads_and_a_brief_finds_its_destination() {
        assert!(destinations().len() >= 3, "the file reads");
        for d in destinations() {
            assert!(d.long_shot_secs > 0.0 && !d.checked.is_empty() && !d.platforms.is_empty(), "{}", d.id);
        }
        let (mut seq, _) = piece(Some("YouTube Shorts, Instagram Reels, TikTok"), &[(0.0, 5.0)]);
        assert_eq!(destination_of(&seq).map(|d| (d.id.as_str(), d.long_shot_secs)), Some(("vertical", 4.0)), "Shorts before YouTube");
        seq.brief = Some(Brief { platform: "YouTube".into(), ..Brief::default() });
        assert_eq!(destination_of(&seq).map(|d| d.long_shot_secs), Some(10.0));
        seq.brief = Some(Brief { platform: "Instagram feed".into(), ..Brief::default() });
        assert_eq!(destination_of(&seq).map(|d| d.long_shot_secs), Some(6.0));
        seq.brief = None;
        assert!(destination_of(&seq).is_none());
    }

    #[test]
    fn a_still_silent_opening_is_a_slow_hook_only_with_a_destination() {
        let still = Room { motion: Some(1.0), words: vec![(5.0, 5.4)], ..Room::default() };
        let (seq, pool) = piece(Some("YouTube"), &[(0.0, 8.0)]);
        assert_eq!(kinds(&marks(&seq, &pool, &still), PaceKind::Hook), vec![(0.0, 3.0)]);
        // A cut at 1.5 s, a word at 1 s, or movement: something changes.
        let (cut, cut_pool) = piece(Some("YouTube"), &[(0.0, 1.5), (1.5, 8.0)]);
        assert!(kinds(&marks(&cut, &cut_pool, &still), PaceKind::Hook).is_empty());
        let spoken = Room { motion: Some(1.0), words: vec![(1.0, 1.3)], ..Room::default() };
        assert!(kinds(&marks(&seq, &pool, &spoken), PaceKind::Hook).is_empty());
        let moving = Room { motion: Some(9.0), ..Room::default() };
        assert!(kinds(&marks(&seq, &pool, &moving), PaceKind::Hook).is_empty());
        // No destination: no hook hint.
        let (nowhere, nowhere_pool) = piece(None, &[(0.0, 8.0)]);
        assert!(kinds(&marks(&nowhere, &nowhere_pool, &still), PaceKind::Hook).is_empty());
    }

    #[test]
    fn dead_air_is_over_a_second_with_nothing_heard_at_all() {
        let quiet = Room { silences: vec![(2.0, 3.4), (5.0, 5.5)], ..Room::default() };
        let (seq, pool) = piece(None, &[(0.0, 8.0)]);
        // The 1.4 s silence, not the half second.
        assert_eq!(kinds(&marks(&seq, &pool, &quiet), PaceKind::DeadAir), vec![(2.0, 3.4)]);
        // A music bed over it, its file never measured silent: heard.
        let mut with_music = seq.clone();
        let mut bed_pool = pool.clone();
        let mut song = klipp_media::MediaItem::synthetic_for_test("/tmp/bed.wav".into(), None);
        song.duration = tc(8.0);
        song.has_audio = true;
        let bed = bed_pool.add(song);
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips = vec![klipp_timeline::Clip::media(bed, tc(0.0), tc(8.0), tc(0.0), tc(8.0))];
        with_music.tracks.push(a);
        let bed_heard = Room { silences: vec![(2.0, 3.4)], ..Room::default() };
        // Both files say the same silences here, so give the bed its own:
        // a file that isn't silent there is heard.
        struct Split;
        impl Transcripts for Split {
            fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
                None
            }
            fn silences(&self, item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
                if item.filename.contains("bed") { Vec::new() } else { vec![(2.0, 3.4)] }
            }
        }
        assert!(kinds(&marks(&with_music, &bed_pool, &Split), PaceKind::DeadAir).is_empty(), "music counts as heard");
        assert_eq!(kinds(&marks(&with_music, &bed_pool, &bed_heard), PaceKind::DeadAir), vec![(2.0, 3.4)], "unless it is silent there too");
    }

    #[test]
    fn a_long_shot_is_one_where_nothing_moves_or_speaks_past_its_destinations_limit() {
        let still = Room { motion: Some(1.0), ..Room::default() };
        // A 7 s second shot: long for a vertical piece, not for YouTube.
        let cuts = [(0.0, 1.0), (1.0, 8.0)];
        let (vertical, pool) = piece(Some("TikTok"), &cuts);
        assert_eq!(kinds(&marks(&vertical, &pool, &still), PaceKind::LongShot), vec![(1.0, 8.0)]);
        let (youtube, pool) = piece(Some("YouTube"), &cuts);
        assert!(kinds(&marks(&youtube, &pool, &still), PaceKind::LongShot).is_empty());
        let (nowhere, pool) = piece(None, &cuts);
        assert!(kinds(&marks(&nowhere, &pool, &still), PaceKind::LongShot).is_empty(), "no destination, no limit");
        // Someone speaks in it, it moves, or its motion isn't analysed: not long.
        let spoken = Room { motion: Some(1.0), words: vec![(4.0, 4.5)], ..Room::default() };
        let (vertical, pool) = piece(Some("TikTok"), &cuts);
        assert!(kinds(&marks(&vertical, &pool, &spoken), PaceKind::LongShot).is_empty());
        assert!(kinds(&marks(&vertical, &pool, &Room { motion: Some(8.0), ..Room::default() }), PaceKind::LongShot).is_empty());
        assert!(kinds(&marks(&vertical, &pool, &Room::default()), PaceKind::LongShot).is_empty());
        // A shot change the analysis found inside the clip splits it.
        let split = Room { motion: Some(1.0), shots: vec![4.0], ..Room::default() };
        assert!(kinds(&marks(&vertical, &pool, &split), PaceKind::LongShot).is_empty());
    }

    #[test]
    fn text_too_brief_to_read_is_the_delivery_checks_own_finding() {
        let (mut seq, pool) = piece(None, &[(0.0, 8.0)]);
        let mut t = Track::new("Text 1", TrackKind::Text);
        let words = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Read all of these words before they go".into(), ..Default::default() });
        t.clips = vec![klipp_timeline::Clip { kind: ClipKind::Title(words), ..klipp_timeline::Clip::gap(tc(2.0), tc(3.0)) }];
        seq.tracks.insert(0, t);
        let found = kinds(&marks(&seq, &pool, &Room::default()), PaceKind::BriefText);
        assert_eq!(found, vec![(2.0, 3.0)]);
        let check = crate::qc::check(&seq, &pool, &Room::default());
        assert!(check.findings.iter().any(|f| matches!(f.rule, "title_short" | "too_short_to_read") && f.at_secs == Some(2.0)), "the check finds it too");
    }
}
