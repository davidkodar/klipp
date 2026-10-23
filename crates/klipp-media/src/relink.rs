//! Finding offline media again: score the files in a folder against what
//! the project remembers about each missing item, and propose a relink
//! only where one file clearly wins.
//!
//! Matching on the filename alone gets the common cases wrong in both
//! directions. A card transcoded from `A001C001.MXF` to `A001C001.mov`
//! never matches, and two cards that both hold `C0001.MP4` match twice,
//! so one clip silently relinks to the other card's shot. This module
//! scores every candidate on several pieces of evidence instead — the
//! file's content, reel, start timecode, name, name without extension and
//! duration — rejects a candidate whose facts contradict the item, and
//! refuses to choose between two that score the same.
//!
//! The pure part ([`Wanted`], [`Candidate`], [`propose`]) does no I/O and
//! is what the tests exercise. [`scan`] walks a folder and reads each
//! plausible file, and is what a UI runs on a worker thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use klipp_core::MediaId;

use crate::MediaItem;

/// One piece of evidence that a file is the missing item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Criterion {
    /// Same size and the same first 64 KB: a copy of the same file.
    Content,
    /// The same reel or tape name.
    Reel,
    /// The same start timecode.
    Timecode,
    /// The same filename, extension included.
    Name,
    /// The same filename once the extension is dropped: a transcode.
    NameStem,
    /// The same duration, within a frame.
    Duration,
    /// The same filename under folders of the same names, counted from
    /// the file up: the same place on a drive mounted somewhere else, and
    /// what tells two cards' `C0001.MP4` apart when each sits in its
    /// card's folder. Only a tiebreak, never an identity: a camera's own
    /// layout (`PRIVATE/M4ROOT/CLIP`, `DCIM/100CANON`) is the same on
    /// every card, so a matching folder says little on its own.
    Folder,
}

impl Criterion {
    pub fn label(self) -> &'static str {
        match self {
            Criterion::Content => "content",
            Criterion::Reel => "reel",
            Criterion::Timecode => "timecode",
            Criterion::Name => "name",
            Criterion::NameStem => "name (other extension)",
            Criterion::Duration => "duration",
            Criterion::Folder => "folder",
        }
    }

    /// How much this criterion says about identity on its own. Reel and
    /// timecode are what professional conform is built on; a name is
    /// weaker because cameras reuse them; a duration only confirms.
    fn weight(self) -> u32 {
        match self {
            Criterion::Content => 1000,
            Criterion::Reel => 300,
            Criterion::Timecode => 200,
            Criterion::Name => 150,
            Criterion::NameStem => 100,
            Criterion::Duration => 60,
            // Per matching folder; see `score`.
            Criterion::Folder => 10,
        }
    }
}

/// Which criteria the user trusts for this footage. A criterion switched
/// off neither scores nor disqualifies: reel names a camera never set
/// properly should not veto an otherwise good match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Criteria {
    pub name: bool,
    pub name_any_extension: bool,
    pub reel: bool,
    pub timecode: bool,
    pub duration: bool,
}

impl Default for Criteria {
    fn default() -> Self {
        Self { name: true, name_any_extension: true, reel: true, timecode: true, duration: true }
    }
}

/// What the project remembers about a missing item.
#[derive(Debug, Clone, Default)]
pub struct Wanted {
    pub media_id: MediaId,
    pub filename: String,
    /// Where the file was: its folder's name is evidence too.
    pub original_path: PathBuf,
    pub file_size: u64,
    pub fingerprint: Option<u64>,
    pub reel: Option<String>,
    pub start_timecode: Option<String>,
    /// Seconds; `None` for a still or when never read.
    pub duration_secs: Option<f64>,
    pub fps: Option<f64>,
    /// The picture's size upright, as the project shows it.
    pub resolution: Option<(u32, u32)>,
    /// Degrees the file's picture is turned to stand upright (0, 90, 180,
    /// 270): a file reads its size as stored, on its side at 90 and 270.
    pub rotation: i32,
}

impl Wanted {
    pub fn from_item(item: &MediaItem) -> Self {
        let duration = item.duration.to_secs_f64();
        let fps = item.fps.as_f64();
        let (w, h) = (item.resolution.width, item.resolution.height);
        Self {
            media_id: item.id,
            filename: item.filename.clone(),
            original_path: item.path.clone(),
            file_size: item.file_size_bytes,
            fingerprint: item.fingerprint,
            reel: item.shot.reel_name.clone(),
            start_timecode: item.shot.start_timecode.clone(),
            duration_secs: (!item.is_still && duration > 0.0).then_some(duration),
            fps: (fps > 0.0).then_some(fps),
            resolution: (w > 0 && h > 0).then_some((w, h)),
            rotation: item.source_rotation,
        }
    }
}

/// What reading a file found. Everything is optional: a file ffmpeg
/// cannot open still has a name and a size.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub duration_secs: Option<f64>,
    pub fps: Option<f64>,
    pub resolution: Option<(u32, u32)>,
    pub reel: Option<String>,
    pub start_timecode: Option<String>,
}

/// A file found in the search folder.
#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub path: PathBuf,
    pub file_size: u64,
    /// Computed only where the size matches some wanted item; `None`
    /// otherwise.
    pub fingerprint: Option<u64>,
    /// `None` when the file was not read (a name-only candidate the
    /// criteria do not need facts for, or one ffmpeg could not open).
    pub facts: Option<Facts>,
}

/// One candidate as it scored against one item.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Index into the candidate list given to [`propose`].
    pub candidate: usize,
    pub matched: Vec<Criterion>,
    pub score: u32,
    /// Differences worth telling the user about that do not disqualify:
    /// a proxy at another resolution, a different frame rate.
    pub warnings: Vec<String>,
}

impl Scored {
    /// Whether this is evidence enough to relink without the user
    /// choosing: the same content, or two independent identities
    /// agreeing (reel, timecode, name), or a name the duration confirms.
    ///
    /// A reel alone is not enough: it names a card, not a clip, and a
    /// sibling on the same card with the same length would be ticked. A
    /// timecode alone is not either: jam-synced cameras on a remote start
    /// record the same one. A bare name or duration is only a suggestion.
    pub fn is_strong(&self) -> bool {
        let has = |c| self.matched.contains(&c);
        let named = has(Criterion::Name) || has(Criterion::NameStem);
        let identities = usize::from(named) + usize::from(has(Criterion::Reel)) + usize::from(has(Criterion::Timecode));
        has(Criterion::Content) || identities >= 2 || (named && has(Criterion::Duration))
    }
}

/// What to do about one missing item.
#[derive(Debug, Clone, PartialEq)]
pub enum Proposal {
    /// One candidate clearly wins.
    Found(Scored),
    /// Two or more candidates score the same; the user chooses. Best
    /// first.
    Ambiguous(Vec<Scored>),
    /// Nothing matched. The notes say why candidates that looked close
    /// were turned down, when any were.
    NotFound { rejected: Vec<String> },
}

/// Score `c` against `w`: `Ok` with what matched, or `Err` with the fact
/// that rules it out (empty when nothing matched) and whether the file
/// looked like the item anyway (same content or name), which is when a
/// search reports it.
fn score(w: &Wanted, c: &Candidate, criteria: Criteria) -> Result<Scored, (String, bool)> {
    let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut matched = Vec::new();
    let mut warnings = Vec::new();
    let mut contradiction = None;

    let same_content = c.file_size == w.file_size
        && c.file_size > 0
        && matches!((c.fingerprint, w.fingerprint), (Some(a), Some(b)) if a == b);
    if same_content {
        matched.push(Criterion::Content);
    }

    if criteria.name && name == w.filename {
        matched.push(Criterion::Name);
        if shared_folders(&w.original_path, &c.path) > 0 {
            matched.push(Criterion::Folder);
        }
    } else if criteria.name_any_extension && stem(&name).eq_ignore_ascii_case(stem(&w.filename)) && !stem(&name).is_empty() {
        matched.push(Criterion::NameStem);
    }

    if let Some(facts) = &c.facts {
        if criteria.reel {
            match (norm_reel(w.reel.as_deref()), norm_reel(facts.reel.as_deref())) {
                (Some(a), Some(b)) if a == b => matched.push(Criterion::Reel),
                (Some(a), Some(b)) => contradiction = Some(format!("{name}: reel {b}, not {a}")),
                _ => {}
            }
        }
        if criteria.timecode {
            match (norm_tc(w.start_timecode.as_deref()), norm_tc(facts.start_timecode.as_deref())) {
                (Some(a), Some(b)) if a == b => matched.push(Criterion::Timecode),
                (Some(_), Some(_)) => {
                    contradiction = contradiction.or(Some(format!(
                        "{name}: starts at {}, not {}",
                        facts.start_timecode.as_deref().unwrap_or_default(),
                        w.start_timecode.as_deref().unwrap_or_default()
                    )))
                }
                _ => {}
            }
        }
        if criteria.duration {
            if let (Some(a), Some(b)) = (w.duration_secs, facts.duration_secs) {
                // Within a frame is a match. Beyond that is not yet a
                // contradiction: a transcode's audio padding (AAC primes
                // up to ~21 ms) lengthens the container by more than a
                // frame at 50p. Only a difference no transcode makes
                // rules the file out.
                let diff = (a - b).abs();
                if diff <= frame_tolerance(w.fps) {
                    matched.push(Criterion::Duration);
                } else if diff > DURATION_VETO_SECS {
                    contradiction = contradiction.or(Some(format!("{name}: {b:.2} s long, not {a:.2} s")));
                } else {
                    warnings.push(format!("{:.0} ms {}", diff * 1000.0, if b > a { "longer" } else { "shorter" }));
                }
            }
        }
        if let (Some((ww, wh)), Some((cw, ch))) = (w.resolution, facts.resolution) {
            // Read as stored: a picture turned a quarter is its upright
            // size on its side, and the same file (#2480).
            let (ww, wh) = if w.rotation.rem_euclid(180) == 90 { (wh, ww) } else { (ww, wh) };
            if (ww, wh) != (cw, ch) {
                warnings.push(format!("{cw}×{ch}, not {ww}×{wh}"));
            }
        }
        if let (Some(a), Some(b)) = (w.fps, facts.fps) {
            if (a - b).abs() > 0.01 {
                warnings.push(format!("{b:.3} fps, not {a:.3}"));
            }
        }
    }

    if let Some(why) = contradiction {
        // Only worth reporting for a file that looked like the item; the
        // rest of the folder differing from it is not news.
        let looked_like_it = matched
            .iter()
            .any(|c| matches!(c, Criterion::Content | Criterion::Name | Criterion::NameStem));
        return Err((why, looked_like_it));
    }
    if matched.is_empty() {
        return Err((String::new(), false));
    }
    let mut score: u32 = matched.iter().map(|c| c.weight()).sum();
    if matched.contains(&Criterion::Folder) {
        // The deeper the shared path, the likelier the same card.
        score += Criterion::Folder.weight() * (shared_folders(&w.original_path, &c.path).min(8) as u32 - 1);
    }
    if !same_content && c.file_size == w.file_size && c.file_size > 0 {
        // Byte-identical size is a tiebreak between otherwise equal copies.
        score += 5;
    }
    Ok(Scored { candidate: 0, matched, score, warnings })
}

/// Propose what to relink each of `wanted` to, from `candidates`.
///
/// A candidate needs at least one criterion and nothing that contradicts
/// the item; the best-scoring one is proposed when nothing else scores
/// the same. Candidates that tie are all returned for the user to choose
/// from, which is what keeps two cards' `C0001.MP4` from relinking by
/// accident.
///
/// Copies with the same content tie by construction — a backup folder
/// beside the footage — and are the same file, so the first is proposed
/// with a note rather than offered as a choice.
pub fn propose(wanted: &[Wanted], candidates: &[Candidate], criteria: Criteria) -> Vec<Proposal> {
    let index = Index::new(candidates);
    wanted
        .iter()
        .map(|w| {
            let mut hits = Vec::new();
            let mut rejected = Vec::new();
            for i in index.candidates_for(w, criteria) {
                let c = &candidates[i];
                match score(w, c, criteria) {
                    Ok(mut s) => {
                        s.candidate = i;
                        hits.push(s);
                    }
                    Err((why, true)) if !why.is_empty() => rejected.push(why),
                    Err(_) => {}
                }
            }
            hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.candidate.cmp(&b.candidate)));
            match hits.len() {
                0 => Proposal::NotFound { rejected },
                1 => Proposal::Found(hits.remove(0)),
                _ if hits[0].score > hits[1].score => Proposal::Found(hits.remove(0)),
                _ => {
                    let top = hits[0].score;
                    hits.retain(|h| h.score == top);
                    if hits.iter().all(|h| h.matched.contains(&Criterion::Content)) {
                        let copies = hits.len();
                        let mut first = hits.remove(0);
                        first.warnings.push(format!("{copies} identical copies found; this is the first"));
                        Proposal::Found(first)
                    } else {
                        Proposal::Ambiguous(hits)
                    }
                }
            }
        })
        .collect()
}

/// What a file picked by hand for `w` says about being it.
#[derive(Debug, Clone, PartialEq)]
pub enum PickVerdict {
    /// Evidence enough to be sure ([`Scored::is_strong`]), with any
    /// differences worth mentioning.
    Confirmed(Vec<String>),
    /// Nothing against it, nothing much for it either.
    Unconfirmed(Vec<String>),
    /// A fact that rules it out: another reel, another start timecode, a
    /// length no transcode explains.
    Contradicted(String),
}

/// Judge a file the user picked for `w` by the rules a search uses.
/// Relinking one clip by hand chose among nothing, so two cards'
/// `C0001.MP4` went through unchecked; this says when the file picked is
/// not the clip. Reads the file (ffmpeg), so call it off the UI thread.
pub fn check_pick(w: &Wanted, path: &Path) -> PickVerdict {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let fingerprint = (file_size == w.file_size && file_size > 0).then(|| fingerprint(path)).flatten();
    let c = Candidate { path: path.to_path_buf(), file_size, fingerprint, facts: read_facts(path) };
    match score(w, &c, Criteria::default()) {
        Ok(s) if s.is_strong() => PickVerdict::Confirmed(s.warnings),
        Ok(s) => PickVerdict::Unconfirmed(s.warnings),
        Err((why, _)) if !why.is_empty() => PickVerdict::Contradicted(why),
        Err(_) => PickVerdict::Unconfirmed(Vec::new()),
    }
}

/// How many folders `a` and `b` share, counted up from the file until
/// the names differ.
fn shared_folders(a: &Path, b: &Path) -> usize {
    let dirs = |p: &Path| p.parent().map(|d| d.components().rev().map(|c| c.as_os_str().to_os_string()).collect::<Vec<_>>()).unwrap_or_default();
    dirs(a).iter().zip(dirs(b).iter()).take_while(|(x, y)| x == y && !x.is_empty() && x.as_os_str() != "/").count()
}

/// One frame at `fps`, and never tighter than a container's rounding.
fn frame_tolerance(fps: Option<f64>) -> f64 {
    fps.map_or(0.05, |f| (1.0 / f).max(0.02))
}

/// A duration difference past this rules a candidate out. Transcodes,
/// rewraps and audio padding all stay well inside it.
pub const DURATION_VETO_SECS: f64 = 0.25;

/// Candidates grouped by the keys a match can come from, so each wanted
/// item is scored against the files that share something with it rather
/// than against every file in the folder.
struct Index {
    by_stem: HashMap<String, Vec<usize>>,
    by_size: HashMap<u64, Vec<usize>>,
    by_reel: HashMap<String, Vec<usize>>,
    by_tc: HashMap<String, Vec<usize>>,
    /// Sorted by duration.
    by_duration: Vec<(f64, usize)>,
}

impl Index {
    fn new(candidates: &[Candidate]) -> Self {
        let mut ix = Index {
            by_stem: HashMap::new(),
            by_size: HashMap::new(),
            by_reel: HashMap::new(),
            by_tc: HashMap::new(),
            by_duration: Vec::new(),
        };
        for (i, c) in candidates.iter().enumerate() {
            let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ix.by_stem.entry(stem(&name).to_ascii_lowercase()).or_default().push(i);
            if c.file_size > 0 {
                ix.by_size.entry(c.file_size).or_default().push(i);
            }
            if let Some(f) = &c.facts {
                if let Some(r) = norm_reel(f.reel.as_deref()) {
                    ix.by_reel.entry(r).or_default().push(i);
                }
                if let Some(t) = norm_tc(f.start_timecode.as_deref()) {
                    ix.by_tc.entry(t).or_default().push(i);
                }
                if let Some(d) = f.duration_secs {
                    ix.by_duration.push((d, i));
                }
            }
        }
        ix.by_duration.sort_by(|a, b| a.0.total_cmp(&b.0));
        ix
    }

    /// Every candidate that could score on some criterion for `w`.
    fn candidates_for(&self, w: &Wanted, criteria: Criteria) -> Vec<usize> {
        let mut out = Vec::new();
        let mut add = |v: Option<&Vec<usize>>| out.extend(v.into_iter().flatten().copied());
        if criteria.name || criteria.name_any_extension {
            add(self.by_stem.get(&stem(&w.filename).to_ascii_lowercase()));
        }
        add(self.by_size.get(&w.file_size));
        if criteria.reel {
            add(norm_reel(w.reel.as_deref()).and_then(|r| self.by_reel.get(&r)));
        }
        if criteria.timecode {
            add(norm_tc(w.start_timecode.as_deref()).and_then(|t| self.by_tc.get(&t)));
        }
        if criteria.duration {
            if let Some(d) = w.duration_secs {
                let tol = frame_tolerance(w.fps);
                let from = self.by_duration.partition_point(|(x, _)| *x < d - tol);
                out.extend(self.by_duration[from..].iter().take_while(|(x, _)| *x <= d + tol).map(|(_, i)| *i));
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

fn norm_reel(r: Option<&str>) -> Option<String> {
    let r = r?.trim();
    (!r.is_empty()).then(|| r.to_ascii_uppercase())
}

/// Timecode digits only, so `01:00:00;00` and `01:00:00:00` agree: the
/// separator records drop-frame counting, not a different instant label.
///
/// All zeros is no timecode: it is where a record-run camera starts every
/// card, and where a file with none is often stamped, so it identifies
/// nothing and contradicts nothing.
fn norm_tc(t: Option<&str>) -> Option<String> {
    let digits: String = t?.chars().filter(|c| c.is_ascii_digit()).collect();
    (digits.chars().any(|c| c != '0')).then_some(digits)
}

/// FNV-1a over the first 64 KB and the file size: the content key a
/// `MediaItem` stores as its fingerprint.
pub fn fingerprint(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 65536];
    let mut n = 0;
    // One read may return short; fill the buffer or reach the end.
    loop {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => {
                n += k;
                if n == buf.len() {
                    break;
                }
            }
            Err(_) => return None,
        }
    }
    buf.truncate(n);
    let file_size = std::fs::metadata(path).ok()?.len();
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for &byte in buf.iter().chain(file_size.to_le_bytes().iter()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    Some(hash)
}

/// How far a [`scan`] has got, for a progress line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub files_seen: usize,
    pub to_read: usize,
    pub read: usize,
}

/// What a [`scan`] found.
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub candidates: Vec<Candidate>,
    /// Folders not entered: unreadable ones, and symlinked ones (which
    /// can loop).
    pub skipped_dirs: usize,
    /// The walk stopped at [`MAX_FILES`]; media past it was not seen.
    pub hit_file_limit: bool,
    pub cancelled: bool,
}

/// The most files a scan looks at. Far more than a project's media, and
/// small enough that pointing it at `/` ends in seconds rather than hours.
pub const MAX_FILES: usize = 200_000;

/// How many folders below the one searched a search goes.
pub const MAX_DEPTH: usize = 12;

/// Walk `dir` for files that could be any of `wanted`, and read what is
/// needed to score them.
///
/// Every media file is a candidate. Its size is always read; its
/// fingerprint only when the size equals some wanted item's; its header
/// (duration, rate, resolution, reel, timecode) when it shares a name or
/// a size with something wanted, and otherwise only when `criteria`
/// match on reel, timecode or duration — which the defaults do, so by
/// default every video and audio file is read. See [`needs_headers`]. There is no depth limit — a card's `PRIVATE/M4ROOT/CLIP`
/// is four levels down before anyone has organised anything — but
/// symlinked folders are not followed.
pub fn scan(
    dir: &Path,
    wanted: &[Wanted],
    criteria: Criteria,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> ScanResult {
    let mut out = ScanResult::default();
    let mut p = Progress::default();
    // No deeper than MAX_DEPTH folders below `dir`: a folder picked too
    // high up is searched near its top, not to the bottom of the disk.
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    let mut files = Vec::new();
    while let Some((d, depth)) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            return out;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            out.skipped_dirs += 1;
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_symlink() && path.is_dir() {
                out.skipped_dirs += 1;
            } else if ft.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                } else {
                    out.skipped_dirs += 1;
                }
            } else if klipp_core::media_extensions::path_is_media(&path) {
                p.files_seen += 1;
                files.push(path);
                if files.len() >= MAX_FILES {
                    out.hit_file_limit = true;
                    stack.clear();
                    break;
                }
                if p.files_seen % 256 == 0 {
                    progress(p);
                }
            }
        }
    }

    let sizes: std::collections::HashSet<u64> = wanted.iter().map(|w| w.file_size).filter(|&s| s > 0).collect();
    let names: std::collections::HashSet<String> =
        wanted.iter().map(|w| stem(&w.filename).to_ascii_lowercase()).collect();
    let needs_facts = needs_headers(criteria);
    for path in files {
        let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let named = names.contains(&stem(&name).to_ascii_lowercase());
        let sized = sizes.contains(&file_size);
        // A still carries no duration or timecode worth reading, and a
        // file that shares neither a name nor a size with anything wanted
        // only matters for reel and timecode matching.
        let read = named || sized || (needs_facts && !klipp_core::media_extensions::path_is_image(&path));
        out.candidates.push(Candidate { path, file_size, fingerprint: None, facts: read.then(Facts::default) });
    }
    p.to_read = out.candidates.iter().filter(|c| c.facts.is_some()).count();
    progress(p);

    for c in &mut out.candidates {
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            return out;
        }
        if sizes.contains(&c.file_size) {
            c.fingerprint = fingerprint(&c.path);
        }
        if c.facts.is_some() {
            c.facts = read_facts(&c.path);
            p.read += 1;
            if p.read % 8 == 0 {
                progress(p);
            }
        }
    }
    progress(p);
    out
}

/// Whether `criteria` need every file's header read, not only those
/// sharing a name or size with something wanted. A scan made without
/// them cannot serve criteria that need them; search again.
pub fn needs_headers(criteria: Criteria) -> bool {
    criteria.reel || criteria.timecode || criteria.duration
}

/// Read a file's duration, rate, resolution, reel and start timecode —
/// the same tags the import probe reads, and the camera's sidecar.
pub fn read_facts(path: &Path) -> Option<Facts> {
    let mut facts = Facts::default();
    if !klipp_core::media_extensions::path_is_image(path) {
        let ctx = ffmpeg_next::format::input(path).ok()?;
        let dur = ctx.duration();
        if dur > 0 {
            facts.duration_secs = Some(dur as f64 / 1_000_000.0);
        }
        // The file's picture, not its cover art (#1892): an MP3's album
        // art read as a 400×400 picture.
        if let Some(stream) = crate::picture_stream(&ctx).and_then(|i| ctx.stream(i)) {
            let fr = stream.avg_frame_rate();
            if fr.numerator() > 0 && fr.denominator() > 0 {
                facts.fps = Some(fr.numerator() as f64 / fr.denominator() as f64);
            }
            let video = ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())
                .ok()
                .and_then(|c| c.decoder().video().ok());
            if let Some(v) = video.filter(|v| v.width() > 0 && v.height() > 0) {
                facts.resolution = Some((v.width(), v.height()));
            }
        }
        let tag = |names: &[&str]| -> Option<String> {
            let hit = |m: ffmpeg_next::DictionaryRef| {
                m.iter().find(|(k, _)| names.iter().any(|n| k.eq_ignore_ascii_case(n))).map(|(_, v)| v.to_string())
            };
            hit(ctx.metadata()).or_else(|| ctx.streams().find_map(|s| hit(s.metadata())))
        };
        facts.start_timecode = tag(&["timecode"]);
        facts.reel = tag(&["reel_name", "com.apple.proapps.reel", "reel"]);
    }
    if let Some(sidecar) = crate::sidecar::read_sidecar(path) {
        facts.start_timecode = facts.start_timecode.or(sidecar.shot.start_timecode);
        facts.reel = facts.reel.or(sidecar.shot.reel_name);
    }
    Some(facts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(name: &str) -> Wanted {
        Wanted {
            filename: name.into(),
            file_size: 1000,
            fingerprint: Some(42),
            duration_secs: Some(10.0),
            fps: Some(25.0),
            resolution: Some((1920, 1080)),
            ..Wanted::default()
        }
    }

    fn cand(path: &str, facts: Facts) -> Candidate {
        Candidate { path: path.into(), file_size: 999, fingerprint: None, facts: Some(facts) }
    }

    fn ten_seconds() -> Facts {
        Facts { duration_secs: Some(10.0), fps: Some(25.0), resolution: Some((1920, 1080)), ..Facts::default() }
    }

    #[test]
    fn two_cards_with_the_same_clip_name_are_offered_not_guessed() {
        let w = [wanted("C0001.MP4")];
        let c = [cand("/a/C0001.MP4", ten_seconds()), cand("/b/C0001.MP4", ten_seconds())];
        match &propose(&w, &c, Criteria::default())[0] {
            Proposal::Ambiguous(both) => assert_eq!(both.len(), 2),
            other => panic!("expected a choice, got {other:?}"),
        }
    }

    #[test]
    fn the_reel_settles_which_card_a_clip_came_from() {
        let mut w = wanted("C0001.MP4");
        w.reel = Some("A002".into());
        let a = Facts { reel: Some("A001".into()), ..ten_seconds() };
        let b = Facts { reel: Some("a002".into()), ..ten_seconds() };
        let c = [cand("/a/C0001.MP4", a), cand("/b/C0001.MP4", b)];
        match &propose(&[w], &c, Criteria::default())[0] {
            Proposal::Found(s) => {
                assert_eq!(s.candidate, 1);
                assert!(s.matched.contains(&Criterion::Reel));
                assert!(s.is_strong());
            }
            other => panic!("expected the A002 card, got {other:?}"),
        }
    }

    #[test]
    fn a_transcode_with_another_extension_is_found_by_stem_and_duration() {
        let w = [wanted("A001C001.MXF")];
        let c = [cand("/t/A001C001.mov", ten_seconds()), cand("/t/A001C002.mov", ten_seconds())];
        match &propose(&w, &c, Criteria::default())[0] {
            Proposal::Found(s) => {
                assert_eq!(s.candidate, 0);
                assert_eq!(s.matched, vec![Criterion::NameStem, Criterion::Duration]);
                assert!(s.is_strong());
            }
            other => panic!("expected the transcode, got {other:?}"),
        }
    }

    #[test]
    fn a_name_match_with_the_wrong_duration_is_rejected_and_says_why() {
        let w = [wanted("C0001.MP4")];
        let short = Facts { duration_secs: Some(4.0), ..ten_seconds() };
        match &propose(&w, &[cand("/a/C0001.MP4", short)], Criteria::default())[0] {
            Proposal::NotFound { rejected } => assert!(rejected[0].contains("4.00 s long"), "{rejected:?}"),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn switching_a_criterion_off_stops_it_vetoing() {
        let mut w = wanted("C0001.MP4");
        w.reel = Some("A001".into());
        let wrong_reel = Facts { reel: Some("CAMERA".into()), ..ten_seconds() };
        let c = [cand("/a/C0001.MP4", wrong_reel)];
        assert!(matches!(propose(&[w.clone()], &c, Criteria::default())[0], Proposal::NotFound { .. }));
        let trust_no_reels = Criteria { reel: false, ..Criteria::default() };
        assert!(matches!(propose(&[w], &c, trust_no_reels)[0], Proposal::Found(_)));
    }

    #[test]
    fn a_renamed_copy_is_found_by_its_content() {
        let w = [wanted("C0001.MP4")];
        let mut renamed = cand("/x/interview-take-3.mp4", ten_seconds());
        renamed.file_size = 1000;
        renamed.fingerprint = Some(42);
        match &propose(&w, &[renamed], Criteria::default())[0] {
            Proposal::Found(s) => assert!(s.matched.contains(&Criterion::Content)),
            other => panic!("expected the renamed copy, got {other:?}"),
        }
    }

    #[test]
    fn timecode_separators_do_not_matter_but_the_digits_do() {
        let mut w = wanted("C0001.MP4");
        w.start_timecode = Some("01:00:10;00".into());
        let same = Facts { start_timecode: Some("01:00:10:00".into()), ..ten_seconds() };
        let other = Facts { start_timecode: Some("02:00:10:00".into()), ..ten_seconds() };
        let c = [cand("/a/C0001.MP4", other), cand("/b/C0001.MP4", same)];
        match &propose(&[w], &c, Criteria::default())[0] {
            Proposal::Found(s) => assert_eq!(s.candidate, 1),
            other => panic!("expected the matching timecode, got {other:?}"),
        }
    }

    /// #2480: a phone video turned a quarter is upright in the project and
    /// on its side in the file; the same file is no size mismatch, and
    /// one really another size still is.
    #[test]
    fn a_turned_picture_is_its_own_size_on_its_side() {
        let w = [Wanted { resolution: Some((2160, 3840)), rotation: 90, ..wanted("IMG_0001.MOV") }];
        let stored = Facts { resolution: Some((3840, 2160)), ..ten_seconds() };
        match &propose(&w, &[cand("/p/IMG_0001.MOV", stored)], Criteria::default())[0] {
            Proposal::Found(s) => assert!(s.warnings.is_empty(), "{:?}", s.warnings),
            other => panic!("expected a clean match, got {other:?}"),
        }
        let small = Facts { resolution: Some((1920, 1080)), ..ten_seconds() };
        match &propose(&w, &[cand("/p/IMG_0001.MOV", small)], Criteria::default())[0] {
            Proposal::Found(s) => assert_eq!(s.warnings, vec!["1920×1080, not 3840×2160".to_string()]),
            other => panic!("expected a warned match, got {other:?}"),
        }
    }

    #[test]
    fn a_proxy_at_another_resolution_matches_with_a_warning() {
        let w = [wanted("C0001.MP4")];
        let proxy = Facts { resolution: Some((960, 540)), ..ten_seconds() };
        match &propose(&w, &[cand("/p/C0001.MP4", proxy)], Criteria::default())[0] {
            Proposal::Found(s) => assert_eq!(s.warnings, vec!["960×540, not 1920×1080".to_string()]),
            other => panic!("expected a warned match, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_duration_is_offered_but_not_strong() {
        let w = [wanted("C0001.MP4")];
        match &propose(&w, &[cand("/x/other.mp4", ten_seconds())], Criteria::default())[0] {
            Proposal::Found(s) => {
                assert_eq!(s.matched, vec![Criterion::Duration]);
                assert!(!s.is_strong());
            }
            other => panic!("expected a weak suggestion, got {other:?}"),
        }
    }

    #[test]
    fn scan_walks_deeper_than_five_levels_and_reads_nothing_it_needs_not() {
        let root = std::env::temp_dir().join(format!("klipp-relink-scan-{}", std::process::id()));
        let deep = root.join("a/b/c/d/e/f/g");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("C0001.MP4"), b"not really video").unwrap();
        std::fs::write(root.join("notes.txt"), b"not media").unwrap();
        let w = [wanted("C0001.MP4")];
        let found = scan(&root, &w, Criteria::default(), &AtomicBool::new(false), |_| {});
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(found.candidates.len(), 1, "only media files are candidates");
        assert!(found.candidates[0].path.ends_with("a/b/c/d/e/f/g/C0001.MP4"));
        assert!(!found.hit_file_limit && !found.cancelled);
    }

    /// #1892: a sound with cover art has no picture; its art is told
    /// apart, at its own size.
    #[test]
    fn a_sound_with_cover_art_has_no_picture() {
        let root = std::env::temp_dir().join(format!("klipp-relink-art-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mp3 = root.join("song.mp3");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=1", "-f", "lavfi", "-i", "color=c=red:size=40x30:d=1"])
            .args(["-map", "0:a", "-map", "1:v", "-frames:v", "1", "-c:a", "libmp3lame", "-c:v", "mjpeg", "-disposition:v", "attached_pic"])
            .arg(&mp3)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            std::fs::remove_dir_all(&root).ok();
            eprintln!("skipped: ffmpeg could not write the test song");
            return;
        }
        let facts = read_facts(&mp3).expect("readable");
        let art = crate::artwork_size(&mp3);
        std::fs::remove_dir_all(&root).ok();
        assert_eq!(facts.resolution, None, "the album art is not the picture: {facts:?}");
        assert_eq!(art, Some((40, 30)));
    }

    /// Real files, so the header reader is checked against what ffmpeg
    /// writes: two cards' `C0001.mov` that differ only in timecode, and
    /// the scan telling them apart. Skipped where there is no ffmpeg.
    #[test]
    fn a_real_card_pair_is_told_apart_by_timecode() {
        let root = std::env::temp_dir().join(format!("klipp-relink-real-{}", std::process::id()));
        let make = |card: &str, tc: &str| -> bool {
            let dir = root.join(card);
            std::fs::create_dir_all(&dir).unwrap();
            std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25:duration=2"])
                .args(["-timecode", tc, "-c:v", "mpeg4"])
                .arg(dir.join("C0001.mov"))
                .status()
                .is_ok_and(|s| s.success())
        };
        if !make("A001", "01:00:00:00") || !make("A002", "02:00:00:00") {
            std::fs::remove_dir_all(&root).ok();
            eprintln!("skipped: ffmpeg could not write the test clips");
            return;
        }
        let facts = read_facts(&root.join("A002/C0001.mov")).expect("readable");
        assert_eq!(facts.start_timecode.as_deref(), Some("02:00:00:00"));
        assert_eq!(facts.resolution, Some((320, 180)));
        assert!((facts.duration_secs.unwrap() - 2.0).abs() < 0.05, "{facts:?}");

        let mut w = wanted("C0001.mov");
        w.start_timecode = Some("02:00:00:00".into());
        w.duration_secs = Some(2.0);
        w.resolution = Some((320, 180));
        w.fingerprint = None;
        let found = scan(&root, std::slice::from_ref(&w), Criteria::default(), &AtomicBool::new(false), |_| {});
        let proposals = propose(&[w], &found.candidates, Criteria::default());
        std::fs::remove_dir_all(&root).ok();
        match &proposals[0] {
            Proposal::Found(s) => {
                assert!(found.candidates[s.candidate].path.ends_with("A002/C0001.mov"));
                assert!(s.matched.contains(&Criterion::Timecode));
            }
            other => panic!("expected the A002 card, got {other:?}"),
        }
    }

    /// A clip relinked by hand to the other card's `C0001.mov` is caught
    /// by its timecode; the right card's is confirmed. Real files, skipped
    /// where there is no ffmpeg.
    #[test]
    fn a_hand_picked_file_from_the_wrong_card_is_contradicted() {
        let root = std::env::temp_dir().join(format!("klipp-relink-pick-{}", std::process::id()));
        let make = |card: &str, tc: &str| -> bool {
            let dir = root.join(card);
            std::fs::create_dir_all(&dir).unwrap();
            std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25:duration=2"])
                .args(["-timecode", tc, "-c:v", "mpeg4"])
                .arg(dir.join("C0001.mov"))
                .status()
                .is_ok_and(|s| s.success())
        };
        if !make("A001", "01:00:00:00") || !make("A002", "02:00:00:00") {
            std::fs::remove_dir_all(&root).ok();
            eprintln!("skipped: ffmpeg could not write the test clips");
            return;
        }
        let mut w = wanted("C0001.mov");
        w.start_timecode = Some("02:00:00:00".into());
        w.duration_secs = Some(2.0);
        w.resolution = Some((320, 180));
        w.fingerprint = None;
        let wrong = check_pick(&w, &root.join("A001/C0001.mov"));
        let right = check_pick(&w, &root.join("A002/C0001.mov"));
        std::fs::remove_dir_all(&root).ok();
        match wrong {
            PickVerdict::Contradicted(why) => assert!(why.contains("01:00:00:00"), "{why}"),
            other => panic!("the wrong card was not caught: {other:?}"),
        }
        assert_eq!(right, PickVerdict::Confirmed(Vec::new()));
    }

    #[test]
    fn a_reel_alone_does_not_tick_a_same_card_sibling() {
        let mut w = wanted("C0001.MP4");
        w.reel = Some("A001".into());
        let sibling = Facts { reel: Some("A001".into()), ..ten_seconds() };
        match &propose(&[w], &[cand("/a/C0002.MP4", sibling)], Criteria::default())[0] {
            Proposal::Found(s) => assert!(!s.is_strong(), "{s:?}"),
            other => panic!("expected a weak suggestion, got {other:?}"),
        }
    }

    #[test]
    fn a_contradicting_timecode_rules_the_file_out_not_just_down() {
        let mut w = wanted("C0001.MP4");
        w.start_timecode = Some("01:00:00:00".into());
        let other = Facts { start_timecode: Some("02:00:00:00".into()), ..ten_seconds() };
        match &propose(&[w], &[cand("/a/C0001.MP4", other)], Criteria::default())[0] {
            Proposal::NotFound { rejected } => assert!(rejected[0].contains("starts at 02:00:00:00"), "{rejected:?}"),
            other => panic!("expected a rejection, got {other:?}"),
        }
    }

    #[test]
    fn missing_metadata_on_either_side_is_ignored_not_a_contradiction() {
        let mut w = wanted("C0001.MP4");
        w.reel = Some("A001".into());
        w.start_timecode = None;
        // The file has a timecode the item never read, and no reel.
        let facts = Facts { start_timecode: Some("01:00:00:00".into()), ..ten_seconds() };
        assert!(matches!(propose(&[w], &[cand("/a/C0001.MP4", facts)], Criteria::default())[0], Proposal::Found(_)));
    }

    #[test]
    fn transcode_padding_is_neither_a_match_nor_a_veto() {
        let mut w = wanted("A001C001.MXF");
        w.fps = Some(50.0);
        // 21 ms of AAC priming: more than a frame at 50p.
        let padded = Facts { duration_secs: Some(10.021), fps: Some(50.0), ..ten_seconds() };
        match &propose(&[w], &[cand("/t/A001C001.mov", padded)], Criteria::default())[0] {
            Proposal::Found(s) => {
                assert_eq!(s.matched, vec![Criterion::NameStem]);
                assert!(s.warnings.iter().any(|x| x.contains("21 ms longer")), "{:?}", s.warnings);
            }
            other => panic!("expected the transcode, got {other:?}"),
        }
    }

    #[test]
    fn byte_identical_backups_do_not_make_a_choice() {
        let w = [wanted("C0001.MP4")];
        let copy = |p: &str| Candidate { path: p.into(), file_size: 1000, fingerprint: Some(42), facts: Some(ten_seconds()) };
        match &propose(&w, &[copy("/footage/C0001.MP4"), copy("/backup/C0001.MP4")], Criteria::default())[0] {
            Proposal::Found(s) => {
                assert_eq!(s.candidate, 0);
                assert!(s.warnings.iter().any(|x| x.contains("2 identical copies")));
            }
            other => panic!("expected the first copy, got {other:?}"),
        }
    }

    #[test]
    fn proposing_scales_to_a_large_folder() {
        let w: Vec<Wanted> = (0..500).map(|i| Wanted { duration_secs: Some(1000.0 + i as f64), ..wanted(&format!("C{i:04}.MP4")) }).collect();
        let c: Vec<Candidate> = (0..200_000)
            .map(|i| Candidate {
                path: format!("/big/X{i:06}.MP4").into(),
                file_size: 7 + i as u64,
                fingerprint: None,
                facts: Some(Facts { duration_secs: Some(i as f64 * 0.37), ..Facts::default() }),
            })
            .collect();
        let t = std::time::Instant::now();
        let out = propose(&w, &c, Criteria::default());
        assert_eq!(out.len(), 500);
        // Linear scoring took minutes here; indexed, well under a second.
        // Generous, since the gate runs under load.
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    }

    #[test]
    fn scan_stops_when_cancelled_and_does_not_follow_symlinked_folders() {
        let root = std::env::temp_dir().join(format!("klipp-relink-walk-{}", std::process::id()));
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/C0001.MP4"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("real/loop")).unwrap();
        let w = [wanted("C0001.MP4")];
        let found = scan(&root, &w, Criteria::default(), &AtomicBool::new(false), |_| {});
        assert_eq!(found.candidates.len(), 1);
        #[cfg(unix)]
        assert_eq!(found.skipped_dirs, 1);
        let cancelled = scan(&root, &w, Criteria::default(), &AtomicBool::new(true), |_| {});
        std::fs::remove_dir_all(&root).ok();
        assert!(cancelled.cancelled && cancelled.candidates.is_empty());
    }

    #[test]
    fn the_card_folder_tells_two_cards_same_name_apart() {
        let mut w = wanted("C0001.MP4");
        w.original_path = "/mnt/old/shoot/CARD_B/C0001.MP4".into();
        let c = [cand("/media/new/shoot/CARD_A/C0001.MP4", ten_seconds()), cand("/media/new/shoot/CARD_B/C0001.MP4", ten_seconds())];
        match &propose(&[w], &c, Criteria::default())[0] {
            Proposal::Found(s) => {
                assert_eq!(s.candidate, 1);
                assert!(s.matched.contains(&Criterion::Folder) && s.is_strong());
            }
            other => panic!("expected CARD_B, got {other:?}"),
        }
    }

    #[test]
    fn a_camera_layout_folder_does_not_make_the_wrong_card_strong() {
        let mut w = wanted("C0001.MP4");
        w.original_path = "/mnt/CARD_A/PRIVATE/M4ROOT/CLIP/C0001.MP4".into();
        // Only card B in the search, its header unread.
        let other_card = Candidate { path: "/media/CARD_B/PRIVATE/M4ROOT/CLIP/C0001.MP4".into(), file_size: 5, fingerprint: None, facts: None };
        match &propose(&[w], &[other_card], Criteria::default())[0] {
            Proposal::Found(s) => assert!(!s.is_strong(), "{s:?}"),
            other => panic!("expected a weak suggestion, got {other:?}"),
        }
    }

    #[test]
    fn a_zero_timecode_identifies_nothing() {
        let mut w = wanted("C0002.MP4");
        w.reel = Some("A001".into());
        w.start_timecode = Some("00:00:00:00".into());
        let sibling = Facts { reel: Some("A001".into()), start_timecode: Some("00:00:00:00".into()), ..ten_seconds() };
        match &propose(&[w], &[cand("/a/C0001.MP4", sibling)], Criteria::default())[0] {
            Proposal::Found(s) => {
                assert!(!s.matched.contains(&Criterion::Timecode));
                assert!(!s.is_strong());
            }
            other => panic!("expected a weak suggestion, got {other:?}"),
        }
    }
}
