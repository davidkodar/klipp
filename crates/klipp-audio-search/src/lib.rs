//! Search royalty-free music and sound effects from within Klipp.
//!
//! The keys live in the user's config, never in the source tree: the
//! `KLIPP_JAMENDO_CLIENT_ID` / `KLIPP_FREESOUND_TOKEN` environment
//! variables win, otherwise `~/.config/klipp/keys.toml` is read for
//! `jamendo_client_id = "…"` / `freesound_token = "…"`. So a public
//! repository never carries a secret, and a user drops their own key in
//! one place.
//!
//! One shape — [`Track`] — covers a music track and a sound effect, so a
//! single library UI lists both. Only search and preview are here;
//! downloading a chosen track into a project is the caller's job (it owns
//! the media pool).

use serde::{Deserialize, Serialize};

/// Which library a result came from, and what kind of sound it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Jamendo — Creative-Commons music tracks.
    JamendoMusic,
    /// Freesound — Creative-Commons / CC0 sound effects.
    Freesound,
}

/// A searchable audio result — a music track or a sound effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub source: Source,
    /// The provider's id for the track, for a later download.
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: f64,
    /// A human licence label, e.g. "CC BY 4.0" or "CC0".
    pub license: String,
    /// The licence deed URL, for the credit line.
    pub license_url: String,
    /// Whether the licence allows commercial use without a further deal.
    pub commercial_ok: bool,
    /// A streamable URL for previewing before download.
    pub preview_url: String,
    /// The URL to download the file into a project (empty if not allowed).
    pub download_url: String,
    /// The page to credit / link back to.
    pub page_url: String,
    /// Cover / waveform image, if any.
    pub image_url: String,
    /// A few descriptive tags.
    pub tags: Vec<String>,
    /// The library's own read of the tempo ("low", "medium", "high"…),
    /// when it gives one.
    #[serde(default)]
    pub pace: String,
    /// What a Freesound sound's original file is, as the library says
    /// (#1894); `None` for music, and for a library that doesn't say.
    #[serde(default)]
    pub original: Option<Original>,
}

/// A sound's original file as the library describes it (#1894).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Original {
    /// Its format: "wav", "flac", "aiff", "mp3", "ogg", "m4a".
    pub kind: String,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub bit_depth: Option<u32>,
    pub bytes: Option<u64>,
}

impl Original {
    /// "WAV 48 kHz 24-bit stereo, 2.1 MB".
    pub fn describe(&self) -> String {
        let mut parts = vec![self.kind.to_uppercase()];
        if let Some(r) = self.sample_rate {
            parts.push(khz(r));
        }
        if let Some(d) = self.bit_depth.filter(|d| *d > 0) {
            parts.push(format!("{d}-bit"));
        }
        if let Some(b) = self.bitrate_kbps.filter(|_| !matches!(self.kind.as_str(), "wav" | "aiff" | "flac")) {
            parts.push(format!("{b} kbps"));
        }
        if let Some(c) = self.channels {
            parts.push(channels(c).into());
        }
        let mut out = parts.join(" ");
        if let Some(n) = self.bytes {
            out.push_str(&format!(", {:.1} MB", n as f64 / 1e6));
        }
        out
    }

    /// The file extension its download is kept under.
    pub fn extension(&self) -> &'static str {
        match self.kind.to_ascii_lowercase().as_str() {
            "wav" => "wav",
            "flac" => "flac",
            "aif" | "aiff" => "aiff",
            "mp3" => "mp3",
            "ogg" => "ogg",
            "m4a" => "m4a",
            _ => "audio",
        }
    }
}

fn khz(rate: u32) -> String {
    let k = rate as f64 / 1000.0;
    if k.fract() == 0.0 { format!("{k:.0} kHz") } else { format!("{k:.1} kHz") }
}

fn channels(n: u32) -> &'static str {
    match n {
        1 => "mono",
        2 => "stereo",
        _ => "multichannel",
    }
}

/// What a download is, measured off the file it wrote (#1894): the
/// preview or the original, and its codec, rate and channels. The figure
/// a credit, an import reply and the media pool give, never a fixed one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Download {
    /// "preview", "original", or "download": the file a music library
    /// offers, which is neither.
    pub quality: String,
    pub codec: String,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub bit_depth: Option<u32>,
}

impl Download {
    /// "preview (MP3 184 kbps, 44.1 kHz stereo)".
    pub fn describe(&self) -> String {
        let mut what = self.codec.to_uppercase();
        if let Some(d) = self.bit_depth.filter(|d| *d > 0) {
            what.push_str(&format!(" {d}-bit"));
        }
        if let Some(b) = self.bitrate_kbps.filter(|_| self.bit_depth.is_none()) {
            what.push_str(&format!(" {b} kbps"));
        }
        let mut rest = Vec::new();
        if let Some(r) = self.sample_rate {
            rest.push(khz(r));
        }
        if let Some(c) = self.channels {
            rest.push(channels(c).to_string());
        }
        if !rest.is_empty() {
            what.push_str(&format!(", {}", rest.join(" ")));
        }
        format!("{} ({what})", self.quality)
    }

    /// The download measured off `path` by ffprobe, as `quality`; with no
    /// ffprobe, the quality alone.
    pub fn measure(path: &std::path::Path, quality: &str) -> Self {
        let mut d = Self { quality: quality.into(), ..Default::default() };
        let Ok(out) = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_name,sample_rate,channels,bit_rate,bits_per_raw_sample,bits_per_sample:format=bit_rate", "-of", "default=nw=1"])
            .arg(path)
            .output()
        else {
            return d;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let num = |k: &str| text.lines().filter_map(|l| l.strip_prefix(&format!("{k}="))).find_map(|v| v.trim().parse::<u64>().ok()).filter(|n| *n > 0);
        d.codec = text.lines().find_map(|l| l.strip_prefix("codec_name=")).unwrap_or("").to_string();
        d.sample_rate = num("sample_rate").map(|n| n as u32);
        d.channels = num("channels").map(|n| n as u32);
        d.bitrate_kbps = num("bit_rate").map(|b| ((b as f64) / 1000.0).round() as u32);
        // A PCM or FLAC file's depth; a lossy one has none.
        if d.codec.starts_with("pcm") || d.codec == "flac" {
            d.bit_depth = num("bits_per_raw_sample").or_else(|| num("bits_per_sample")).map(|n| n as u32);
        }
        d
    }
}

impl Track {
    /// The attribution line to keep with the asset and put in credits.
    pub fn attribution(&self) -> String {
        format!("\"{}\" by {} — {} ({})", self.title, self.artist, self.license, self.page_url)
    }
}

pub mod freesound_auth;
pub mod audition;

/// Percent-encode a query value for a URL (RFC 3986 unreserved kept).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A query value back from [`encode`]'s form: `%XX` and `+`.
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                Ok(b) => {
                    out.push(b);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read a key: the environment variable first, then `keys.toml`.
pub(crate) fn read_key(env: &str, toml_key: &str) -> Option<String> {
    if let Ok(v) = std::env::var(env) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    let path = dirs::config_dir()?.join("klipp").join("keys.toml");
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(toml_key) {
            let val = rest.trim().strip_prefix('=')?.trim().trim_matches('"').trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// A key baked in at build time (release binaries carry the app's own),
/// used only when the user has set none of their own.
fn baked(v: Option<&'static str>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The Jamendo client id: a user-supplied key first (env or config), then
/// the one baked into a release build.
pub fn jamendo_client_id() -> Option<String> {
    read_key("KLIPP_JAMENDO_CLIENT_ID", "jamendo_client_id")
        .or_else(|| baked(option_env!("KLIPP_JAMENDO_CLIENT_ID_BAKED")))
}

/// Whether music search is configured.
pub fn music_available() -> bool {
    jamendo_client_id().is_some()
}

/// A Creative-Commons deed URL to a short label and whether it clears
/// commercial use. Jamendo returns the deed URL per track.
fn cc_label(url: &str) -> (String, bool) {
    let u = url.to_lowercase();
    let commercial = !u.contains("/by-nc") && !u.contains("/by-nc-") && !u.contains("nc/") && !u.contains("noncommercial");
    let name = if u.contains("publicdomain") || u.contains("/zero") || u.contains("cc0") {
        "CC0"
    } else if u.contains("/by-nc-sa") {
        "CC BY-NC-SA"
    } else if u.contains("/by-nc-nd") {
        "CC BY-NC-ND"
    } else if u.contains("/by-nc") {
        "CC BY-NC"
    } else if u.contains("/by-sa") {
        "CC BY-SA"
    } else if u.contains("/by-nd") {
        "CC BY-ND"
    } else if u.contains("/by") {
        "CC BY"
    } else {
        "Creative Commons"
    };
    (name.to_string(), commercial)
}

/// The Freesound API token: a user-supplied key first (env or config),
/// then the one baked into a release build.
pub fn freesound_token() -> Option<String> {
    read_key("KLIPP_FREESOUND_TOKEN", "freesound_token")
        .or_else(|| baked(option_env!("KLIPP_FREESOUND_TOKEN_BAKED")))
}

/// Whether sound-effect search is configured.
pub fn sfx_available() -> bool {
    freesound_token().is_some()
}

/// Search Freesound for sound effects. `query` is free text over names
/// and tags. Previews are public CDN MP3s, so a result plays and imports
/// without any login.
pub fn search_sfx(query: &str, limit: u32) -> Result<Vec<Track>, String> {
    search_sfx_within(query, limit, None)
}

/// [`search_sfx`], only sounds no longer than `max_secs`: a transition's
/// whoosh, not a twenty-second design file (#795). Freesound filters by
/// duration itself, so a page of results is a page of short ones.
pub fn search_sfx_within(query: &str, limit: u32, max_secs: Option<f64>) -> Result<Vec<Track>, String> {
    search_sfx_filtered(query, limit, SfxFilter { max_secs, commercial_only: false }).map(|found| found.tracks)
}

/// What a sound-effect search is narrowed to.
#[derive(Debug, Clone, Copy, Default)]
pub struct SfxFilter {
    /// Only sounds this long or shorter, seconds.
    pub max_secs: Option<f64>,
    /// Only sounds whose licence allows commercial use (#1573).
    pub commercial_only: bool,
}

/// A sound-effect search's results and the words that found them.
#[derive(Debug, Clone, Default)]
pub struct SfxSearch {
    pub tracks: Vec<Track>,
    /// The words the results match: the query itself, or fewer of its
    /// words when nothing matched them all.
    pub matched_query: String,
}

/// The queries tried in turn: the whole phrase, then shorter and
/// shorter runs of its first words. Freesound matches every word, so
/// "reverse cymbal swell" finds nothing where "reverse cymbal" finds
/// plenty (#1573).
fn sfx_attempts(query: &str) -> Vec<String> {
    let words: Vec<&str> = query.split_whitespace().collect();
    (1..=words.len()).rev().map(|n| words[..n].join(" ")).collect()
}

/// [`search_sfx_within`] with the licence filter, falling back to fewer
/// of the query's words when nothing matches them all.
pub fn search_sfx_filtered(query: &str, limit: u32, filter: SfxFilter) -> Result<SfxSearch, String> {
    let token = freesound_token().ok_or_else(|| "no Freesound token set — put freesound_token in ~/.config/klipp/keys.toml or set KLIPP_FREESOUND_TOKEN".to_string())?;
    let limit = limit.clamp(1, 50);
    // A larger page when the licence filter thins it here.
    let page = if filter.commercial_only { (limit * 3).min(150) } else { limit };
    for attempt in sfx_attempts(query) {
        let mut tracks = freesound_page(&token, &attempt, page, filter.max_secs)?;
        if filter.commercial_only {
            tracks.retain(|t| t.commercial_ok);
        }
        tracks.truncate(limit as usize);
        if !tracks.is_empty() {
            return Ok(SfxSearch { tracks, matched_query: attempt });
        }
    }
    Ok(SfxSearch { tracks: Vec::new(), matched_query: query.trim().to_string() })
}

/// A Freesound result's original file, from its `type`, `samplerate`,
/// `channels`, `bitrate`, `bitdepth` and `filesize` (#1894).
fn freesound_original(r: &serde_json::Value) -> Option<Original> {
    let kind = r.get("type").and_then(|v| v.as_str()).filter(|t| !t.is_empty())?.to_ascii_lowercase();
    let num = |k: &str| r.get(k).and_then(|v| v.as_f64()).filter(|n| *n > 0.0);
    Some(Original {
        kind,
        sample_rate: num("samplerate").map(|n| n as u32),
        channels: num("channels").map(|n| n as u32),
        bitrate_kbps: num("bitrate").map(|n| n as u32),
        bit_depth: num("bitdepth").map(|n| n as u32),
        bytes: num("filesize").map(|n| n as u64),
    })
}

/// The fields a Freesound search or sound lookup asks for.
const FREESOUND_FIELDS: &str = "id,name,username,duration,license,previews,url,tags,type,samplerate,channels,bitrate,bitdepth,filesize";

/// One Freesound result as a [`Track`].
fn freesound_track(r: &serde_json::Value) -> Track {
    let str_of = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let license_url = str_of("license");
    let (license, commercial_ok) = if license_url.is_empty() { ("Creative Commons".to_string(), false) } else { cc_label(&license_url) };
    let preview = r.get("previews").and_then(|p| p.get("preview-hq-mp3").or_else(|| p.get("preview-lq-mp3"))).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let tags = r.get("tags").and_then(|t| t.as_array()).map(|a| {
        let mut v: Vec<String> = a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        v.truncate(8);
        v
    }).unwrap_or_default();
    Track {
        source: Source::Freesound,
        id: r.get("id").and_then(|v| v.as_i64()).map(|n| n.to_string()).unwrap_or_default(),
        title: str_of("name"),
        artist: str_of("username"),
        duration_secs: r.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0),
        license,
        license_url,
        commercial_ok,
        preview_url: preview,
        download_url: String::new(),
        page_url: str_of("url"),
        image_url: String::new(),
        tags,
        pace: String::new(),
        original: freesound_original(r),
    }
}

/// A Freesound sound by its id, as a search gives it, its original
/// described (#1894): for an import that was handed only the id.
pub fn freesound_sound(id: &str) -> Result<Track, String> {
    let token = freesound_token().ok_or("no Freesound token set — put freesound_token in ~/.config/klipp/keys.toml or set KLIPP_FREESOUND_TOKEN")?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("{id:?} is not a Freesound sound id"));
    }
    let url = format!("https://freesound.org/apiv2/sounds/{id}/?token={token}&fields={FREESOUND_FIELDS}");
    let text = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("Freesound could not be asked for sound {id}: {}", e.kind()))?
        .into_string()
        .map_err(|e| format!("Freesound returned an unreadable body: {e}"))?;
    let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("Freesound returned unreadable JSON: {e}"))?;
    Ok(freesound_track(&body))
}

fn freesound_page(token: &str, query: &str, page: u32, max_secs: Option<f64>) -> Result<Vec<Track>, String> {
    let q = encode(query.trim());
    let filter = match max_secs.filter(|m| m.is_finite() && *m > 0.0) {
        Some(m) => format!("&filter={}", encode(&format!("duration:[0 TO {m}]"))),
        None => String::new(),
    };
    let url = format!(
        "https://freesound.org/apiv2/search/text/?query={q}&token={token}&page_size={page}&sort=rating_desc&fields={FREESOUND_FIELDS}{filter}"
    );
    let text = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("Freesound request failed: {e}"))?
        .into_string()
        .map_err(|e| format!("Freesound returned an unreadable body: {e}"))?;
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("Freesound returned unreadable JSON: {e}"))?;
    let empty = Vec::new();
    let results = body.get("results").and_then(|r| r.as_array()).unwrap_or(&empty);
    let mut tracks = Vec::with_capacity(results.len());
    for r in results {
        tracks.push(freesound_track(r));
    }
    Ok(tracks)
}

/// Whether a licence forbids derivatives (CC BY-ND, BY-NC-ND): a cut,
/// a fit to length or a remix of the track breaks it, which is what an
/// editor does with music (#1570).
pub fn forbids_edits(license: &str) -> bool {
    license.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part.eq_ignore_ascii_case("ND"))
}

/// The licence families a music search can be narrowed to, as results
/// label them (#1931).
pub const LICENCE_FAMILIES: [&str; 7] = ["CC0", "CC BY", "CC BY-SA", "CC BY-NC", "CC BY-NC-SA", "CC BY-ND", "CC BY-NC-ND"];

/// A licence family by the name an editor gives it, "by-sa" as well as
/// "CC BY-SA"; the label results carry, or why not.
pub fn licence_family(name: &str) -> Result<&'static str, String> {
    let key = |s: &str| s.trim().to_ascii_uppercase().trim_start_matches("CC").trim().replace([' ', '_'], "-");
    LICENCE_FAMILIES
        .iter()
        .find(|f| key(f) == key(name))
        .copied()
        .ok_or_else(|| format!("no licence family {name:?}; the families are {}", LICENCE_FAMILIES.join(", ")))
}

/// What a music search is narrowed to.
#[derive(Debug, Clone, Default)]
pub struct MusicFilter {
    /// Only tracks whose licence allows commercial use.
    pub commercial_only: bool,
    /// Only instrumental tracks: no voice over a promo's own titles.
    pub instrumental: bool,
    /// Only tracks whose licence allows cutting, fitting and remixing:
    /// none that is ND (#1931).
    pub edits_only: bool,
    /// Only these licence families ([`LICENCE_FAMILIES`]); empty is any.
    pub licences: Vec<&'static str>,
}

impl MusicFilter {
    /// Whether `track` passes what the library can't be asked for, or
    /// was asked and is held to again here.
    fn keeps(&self, track: &Track) -> bool {
        (!track.preview_url.is_empty() || !track.download_url.is_empty())
            && (!self.commercial_only || track.commercial_ok)
            && (!self.edits_only || !forbids_edits(&track.license))
            && (self.licences.is_empty() || self.licences.contains(&track.license.as_str()))
    }
}

/// Search Jamendo for music. `query` is matched as fuzzy tags (moods,
/// genres), so "upbeat electronic" finds upbeat electronic tracks; when
/// the tags find nothing, the words are tried as a plain search, then
/// each word alone, so a natural phrase still finds something.
pub fn search_music(query: &str, limit: u32) -> Result<Vec<Track>, String> {
    search_music_filtered(query, limit, MusicFilter::default())
}

pub fn search_music_filtered(query: &str, limit: u32, filter: MusicFilter) -> Result<Vec<Track>, String> {
    let limit = limit.clamp(1, 50);
    // A pool larger than wanted, to rank by the query's own words: the
    // library orders by popularity, so its first answer to "chill
    // acoustic guitar" was a dance track (#739).
    let page = (limit * 3).clamp(30, 200);
    let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
    let mut attempts: Vec<String> = vec![format!("fuzzytags={}", encode(&words.join("+"))), format!("search={}", encode(query.trim()))];
    if words.len() > 1 {
        attempts.extend(words.iter().map(|w| format!("fuzzytags={}", encode(w))));
    }
    let mut tracks = Vec::new();
    for extra in attempts {
        // The filters are asked of the library and applied again here, and
        // a second page is read when they thin the first, so a limit of
        // six gives six, not the two of a page's six that passed (#739).
        for offset in [0, page] {
            let mut got = jamendo_tracks(&extra, page, offset, &filter)?;
            let read = got.len();
            // Before ranking and the limit, so a limit of eight gives
            // eight that pass (#1931).
            got.retain(|t| filter.keeps(t));
            got.retain(|t| !tracks.iter().any(|k: &Track| k.id == t.id));
            tracks.extend(got);
            if tracks.len() >= limit as usize || read < page as usize {
                break;
            }
        }
        if !tracks.is_empty() {
            break;
        }
    }
    rank_by_query(&mut tracks, &words);
    tracks.truncate(limit as usize);
    for t in &mut tracks {
        t.tags.truncate(8);
    }
    Ok(tracks)
}

/// Moods that pull against each other: a track tagged with one side is
/// a poor answer to a query asking for the other.
const CALM: &[&str] = &["chill", "calm", "relaxing", "relax", "ambient", "soft", "peaceful", "mellow", "gentle", "quiet", "slow", "lounge", "dreamy", "meditative", "acoustic"];
const DRIVING: &[&str] = &["energetic", "energy", "dance", "upbeat", "powerful", "aggressive", "epic", "edm", "fast", "hard", "intense", "party", "electro"];

/// Put the tracks that answer the query's words first: a word met in a
/// track's tags or title counts for it, a mood from the opposite side
/// counts against it. Ties keep the library's popularity order.
fn rank_by_query(tracks: &mut [Track], words: &[String]) {
    let score = |t: &Track| -> i32 {
        let tags: Vec<String> = t.tags.iter().map(|x| x.to_lowercase()).collect();
        let title = t.title.to_lowercase();
        let has = |w: &str| tags.iter().any(|x| x == w || x.contains(w)) || title.split(|c: char| !c.is_alphanumeric()).any(|x| x == w);
        let mut s = 0;
        for w in words {
            if has(w) {
                s += 2;
            }
        }
        let asks = |side: &[&str]| words.iter().any(|w| side.contains(&w.as_str()));
        let meets = |side: &[&str]| side.iter().filter(|m| tags.iter().any(|x| x == *m)).count() as i32;
        if asks(CALM) && !asks(DRIVING) {
            s -= 2 * meets(DRIVING);
        }
        if asks(DRIVING) && !asks(CALM) {
            s -= 2 * meets(CALM);
        }
        s
    };
    // A stable sort: equal scores stay in popularity order.
    tracks.sort_by_key(|t| std::cmp::Reverse(score(t)));
}

/// What the library is asked to leave out itself, before it counts a
/// page: `ccnc=false` no non-commercial licences, `ccnd=false` none that
/// forbids edits (#1931).
fn library_filters(filter: &MusicFilter) -> String {
    let mut out = String::new();
    if filter.instrumental {
        out.push_str("&vocalinstrumental=instrumental");
    }
    if filter.commercial_only {
        out.push_str("&ccnc=false");
    }
    if filter.edits_only {
        out.push_str("&ccnd=false");
    }
    out
}

fn jamendo_tracks(extra: &str, limit: u32, offset: u32, filter: &MusicFilter) -> Result<Vec<Track>, String> {
    let cid = jamendo_client_id().ok_or_else(|| "no Jamendo client id set — put jamendo_client_id in ~/.config/klipp/keys.toml or set KLIPP_JAMENDO_CLIENT_ID".to_string())?;
    let asked = library_filters(filter);
    let url = format!(
        "https://api.jamendo.com/v3.0/tracks/?client_id={cid}&format=json&limit={limit}&offset={offset}&order=popularity_total&include=musicinfo+licenses&audioformat=mp32{asked}&{extra}"
    );
    let text = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| format!("Jamendo request failed: {e}"))?
        .into_string()
        .map_err(|e| format!("Jamendo returned an unreadable body: {e}"))?;
    let body: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("Jamendo returned unreadable JSON: {e}"))?;
    if body.get("headers").and_then(|h| h.get("status")).and_then(|s| s.as_str()) != Some("success") {
        let msg = body.get("headers").and_then(|h| h.get("error_message")).and_then(|s| s.as_str()).unwrap_or("unknown error");
        return Err(format!("Jamendo error: {msg}"));
    }
    let empty = Vec::new();
    let results = body.get("results").and_then(|r| r.as_array()).unwrap_or(&empty);
    let mut tracks = Vec::with_capacity(results.len());
    for r in results {
        let get = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let license_url = get("license_ccurl");
        let (license, commercial_ok) = if license_url.is_empty() {
            ("Jamendo — see track".to_string(), false)
        } else {
            cc_label(&license_url)
        };
        let tags = r.get("musicinfo").and_then(|m| m.get("tags")).map(|t| {
            let mut v = Vec::new();
            for key in ["vartags", "genres", "instruments", "vibes"] {
                if let Some(arr) = t.get(key).and_then(|x| x.as_array()) {
                    v.extend(arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())));
                }
            }
            v
        }).unwrap_or_default();
        let pace = r.get("musicinfo").and_then(|m| m.get("speed")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        tracks.push(Track {
            source: Source::JamendoMusic,
            id: get("id"),
            title: get("name"),
            artist: get("artist_name"),
            duration_secs: r.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0),
            license,
            license_url,
            commercial_ok,
            preview_url: get("audio"),
            download_url: if r.get("audiodownload_allowed").and_then(|v| v.as_bool()).unwrap_or(false) { get("audiodownload") } else { String::new() },
            page_url: get("shareurl"),
            image_url: get("image"),
            tags,
            pace,
            original: None,
        });
    }
    Ok(tracks)
}

/// Sanitise a string into a safe file-name fragment.
fn safe_name(s: &str) -> String {
    // Sixty characters, not bytes: String::truncate(60) panicked when byte
    // 60 fell inside a multi-byte letter, and a release build closes on a
    // panic (#822).
    let out: String = s.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).take(60).collect();
    out.trim().replace(' ', "_")
}

/// Download a track's audio into `dest_dir`, and write a small credit
/// file beside it so the licence and attribution travel with the asset.
/// Returns the path to the downloaded audio. Prefers the full download
/// URL, falling back to the preview stream when a download is not offered.
pub fn download_track(track: &Track, dest_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let url = if !track.download_url.is_empty() { &track.download_url } else { &track.preview_url };
    if url.is_empty() {
        return Err(format!("\"{}\" has no downloadable audio", track.title));
    }
    // A Freesound sound's preview is the preview, whatever its rate; a
    // Jamendo track's download is the MP3 the library offers, not a
    // master: "download".
    let quality = if track.source == Source::Freesound || track.download_url.is_empty() { "preview" } else { "download" };
    let resp = ureq::get(url).timeout(std::time::Duration::from_secs(45)).call().map_err(|e| format!("download failed: {e}"))?;
    fetch_into(track, dest_dir, "mp3", resp, quality)
}

/// A Freesound sound's original recording (#1894), with `bearer` the
/// signed-in user's token, kept under its own format's extension. `Err`
/// when the library doesn't say what the original is, or refuses: never
/// the preview in its place.
pub fn download_original(track: &Track, dest_dir: &std::path::Path, bearer: &str) -> Result<std::path::PathBuf, String> {
    let original = track.original.as_ref().ok_or_else(|| format!("Freesound didn't say what \"{}\"'s original is; search for it again", track.title))?;
    if track.id.is_empty() || !track.id.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("\"{}\" has no Freesound id to fetch the original by", track.title));
    }
    let url = format!("https://freesound.org/apiv2/sounds/{}/download/", track.id);
    let resp = match ureq::get(&url).set("Authorization", &format!("Bearer {bearer}")).timeout(std::time::Duration::from_secs(120)).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("Freesound refused the original of \"{}\" ({code}); quality preview takes the preview instead", track.title)),
        Err(e) => return Err(format!("the original of \"{}\" could not be downloaded ({}); quality preview takes the preview instead", track.title, e.kind())),
    };
    fetch_into(track, dest_dir, original.extension(), resp, "original")
}

/// `resp`'s body into `dest_dir` as the track's file with `ext`, capped
/// and moved into place whole, and its credit beside it, with what it is
/// as measured off the file.
fn fetch_into(track: &Track, dest_dir: &std::path::Path, ext: &str, resp: ureq::Response, quality: &str) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("creating {}: {e}", dest_dir.display()))?;
    let base = format!("{}-{}-{}", safe_name(&track.artist), safe_name(&track.title), track.id);
    let audio_path = dest_dir.join(format!("{base}.{ext}"));
    refuse_announced_size(&resp, MAX_TRACK_BYTES)?;
    // Streamed to a file beside it, capped, and moved into place whole:
    // never held in memory, and never a half track under the real name.
    let part = audio_path.with_extension(format!("{ext}.part"));
    let written = std::fs::File::create(&part)
        .map_err(|e| format!("writing {}: {e}", part.display()))
        .and_then(|file| {
            let mut out = std::io::BufWriter::new(file);
            let n = copy_capped(resp.into_reader(), &mut out, MAX_TRACK_BYTES).map_err(|e| format!("reading audio: {e}"))?;
            std::io::Write::flush(&mut out).map_err(|e| format!("writing {}: {e}", part.display()))?;
            Ok(n)
        })
        .and_then(|n| if n < 1024 { Err(format!("\"{}\" downloaded empty", track.title)) } else { Ok(n) });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &audio_path).map_err(|e| format!("writing {}: {e}", audio_path.display()))?;
    // The credit line, kept next to the file for export attribution, and
    // what the file is.
    let _ = std::fs::write(audio_path.with_extension("klipp-credit.txt"), credit_text(track, &Download::measure(&audio_path, quality)));
    Ok(audio_path)
}

/// The credit file's text for `track` downloaded as `download`.
fn credit_text(track: &Track, download: &Download) -> String {
    format!("{}\nLicence: {} ({})\nSource page: {}\nQuality: {}\n", track.attribution(), track.license, track.license_url, track.page_url, download.describe())
}

/// A downloaded track's credit, as [`download_track`] keeps it beside the
/// file: what an export's credits list names (#1582).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credit {
    pub title: String,
    pub author: String,
    /// The licence's label, "CC BY" or "CC0".
    pub licence: String,
    /// The licence deed's URL; empty when the credit names none.
    pub licence_url: String,
    /// The page the track came from, to link back to.
    pub source: String,
    /// What the file is, "preview (MP3 184 kbps, 44.1 kHz stereo)"
    /// (#1894); `None` for a credit written before it was said.
    pub quality: Option<String>,
}

impl Credit {
    /// Whether the licence asks to be credited: every one but a dedication
    /// to the public domain.
    pub fn needs_attribution(&self) -> bool {
        let l = self.licence.to_lowercase();
        !(l.contains("cc0") || l.contains("public domain") || l.contains("publicdomain"))
    }
}

/// The credit [`download_track`] wrote beside `audio`; `None` for a file
/// with none, or one that doesn't read as a credit.
pub fn read_credit(audio: &std::path::Path) -> Option<Credit> {
    parse_credit(&std::fs::read_to_string(audio.with_extension("klipp-credit.txt")).ok()?)
}

/// The credits list's file, beside an export: `film.mp4` gets
/// `film.credits.txt`.
pub fn credits_path(export: &std::path::Path) -> std::path::PathBuf {
    export.with_extension("credits.txt")
}

/// Write the credits of the files a finished export played (`heard`)
/// beside it, and say where and how many (#1582, #1604). With nothing
/// to credit, a list an earlier export left there is removed, so the
/// file beside an export is always that export's.
pub fn write_credits(export: &std::path::Path, heard: &[std::path::PathBuf]) -> std::io::Result<Option<(std::path::PathBuf, usize)>> {
    let mut seen = std::collections::HashSet::new();
    let credits: Vec<Credit> = heard.iter().filter(|p| seen.insert(*p)).filter_map(|p| read_credit(p)).collect();
    let file = credits_path(export);
    if credits.is_empty() {
        match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => return Ok(None),
        }
    }
    std::fs::write(&file, credits_text(&credits))?;
    Ok(Some((file, credits.len())))
}

/// A credit file's text: `"title" by author — licence (page)`, then
/// `Licence: label (deed URL)` and `Source page: page`.
fn parse_credit(text: &str) -> Option<Credit> {
    let first = text.lines().next()?;
    let (head, _) = first.rsplit_once(" — ")?;
    let (title, author) = head.strip_prefix('"')?.rsplit_once("\" by ")?;
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)).map(|v| v.trim().to_owned());
    let licence_line = field("Licence:")?;
    let (licence, licence_url) = match licence_line.rsplit_once(" (") {
        Some((label, url)) => (label.trim().to_owned(), url.trim_end_matches(')').trim().to_owned()),
        None => (licence_line, String::new()),
    };
    Some(Credit { title: title.to_owned(), author: author.trim().to_owned(), licence, licence_url, source: field("Source page:").unwrap_or_default(), quality: field("Quality:") })
}

/// The credits text for `credits`: each track once, grouped by licence,
/// the licences that ask to be credited first, then those that don't. A
/// licence is its label and its deed together: two tracks under "CC BY"
/// at 3.0 and at 4.0 are credited under each their own (#1933). Empty for
/// no credits.
pub fn credits_text(credits: &[Credit]) -> String {
    let mut unique: Vec<&Credit> = Vec::new();
    for c in credits {
        if !unique.iter().any(|u| u.source == c.source && u.title == c.title && u.author == c.author) {
            unique.push(c);
        }
    }
    if unique.is_empty() {
        return String::new();
    }
    let mut licences: Vec<(&str, &str, bool)> = Vec::new();
    for c in &unique {
        if !licences.iter().any(|(l, u, _)| *l == c.licence && *u == c.licence_url) {
            licences.push((c.licence.as_str(), c.licence_url.as_str(), c.needs_attribution()));
        }
    }
    licences.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)).then_with(|| a.1.cmp(b.1)));
    let mut out = String::from("Credits\n");
    let mut heading = None;
    for (licence, url, attribute) in licences {
        if heading != Some(attribute) {
            out.push_str(if attribute { "\nAttribution required\n" } else { "\nNo attribution required\n" });
            heading = Some(attribute);
        }
        out.push_str(&format!("\n{licence}{}\n", if url.is_empty() { String::new() } else { format!(" ({url})") }));
        let mut tracks: Vec<&&Credit> = unique.iter().filter(|c| c.licence == licence && c.licence_url == url).collect();
        tracks.sort_by(|a, b| a.title.cmp(&b.title));
        for c in tracks {
            out.push_str(&format!("  \"{}\" by {}{}{}\n", c.title, c.author, if c.source.is_empty() { String::new() } else { format!(" — {}", c.source) }, c.quality.as_ref().map_or(String::new(), |q| format!(" [{q}]"))));
        }
    }
    out
}

/// Which library `audio` was fetched from, read from the credit file
/// `download_track` wrote beside it: its source page names the library.
/// `None` for a file with no credit beside it, or one naming neither.
pub fn library_source(audio: &std::path::Path) -> Option<Source> {
    let credit = std::fs::read_to_string(audio.with_extension("klipp-credit.txt")).ok()?;
    let page = credit.lines().find_map(|l| l.strip_prefix("Source page:"))?.trim();
    let host = page.split("://").nth(1)?.split('/').next()?;
    match host.trim_start_matches("www.") {
        "jamendo.com" => Some(Source::JamendoMusic),
        "freesound.org" => Some(Source::Freesound),
        _ => None,
    }
}

/// Download a URL's bytes into memory — for a quick preview that plays
/// without touching disk.
pub fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    if url.is_empty() {
        return Err("nothing to preview".into());
    }
    let resp = ureq::get(url).timeout(std::time::Duration::from_secs(30)).call().map_err(|e| format!("preview download failed: {e}"))?;
    refuse_announced_size(&resp, MAX_PREVIEW_BYTES)?;
    let mut bytes = Vec::new();
    copy_capped(resp.into_reader(), &mut bytes, MAX_PREVIEW_BYTES).map_err(|e| format!("reading preview: {e}"))?;
    Ok(bytes)
}

/// The largest track downloaded (#824): an hour at 320 kb/s is about
/// 144 MB. The timeouts bound how long a download takes, not its size.
pub const MAX_TRACK_BYTES: u64 = 256 * 1024 * 1024;
/// The largest preview held in memory: previews are short, low-rate
/// streams of a few megabytes.
pub const MAX_PREVIEW_BYTES: u64 = 32 * 1024 * 1024;

/// Refuses a response that announces more than `limit` bytes, before any
/// of it is read. A response that announces nothing is capped while read.
fn refuse_announced_size(resp: &ureq::Response, limit: u64) -> Result<(), String> {
    match resp.header("Content-Length").and_then(|n| n.trim().parse::<u64>().ok()) {
        Some(n) if n > limit => Err(format!("the server offers {} MiB, more than the {} MiB allowed", n / (1024 * 1024), limit / (1024 * 1024))),
        _ => Ok(()),
    }
}

/// Copies `from` into `to`, failing once more than `limit` bytes arrive.
/// Returns the bytes copied.
fn copy_capped(from: impl std::io::Read, to: &mut impl std::io::Write, limit: u64) -> std::io::Result<u64> {
    let n = std::io::copy(&mut from.take(limit + 1), to)?;
    if n > limit {
        return Err(std::io::Error::other(format!("more than the {} MiB allowed", limit / (1024 * 1024))));
    }
    Ok(n)
}

/// A one-clip audio previewer, held per-thread so its (non-Send) audio
/// output never has to live in a UI struct.
struct Preview {
    _stream: rodio::OutputStream,
    handle: rodio::OutputStreamHandle,
    sink: rodio::Sink,
}

impl Preview {
    fn new() -> Option<Self> {
        let (stream, handle) = rodio::OutputStream::try_default().ok()?;
        let sink = rodio::Sink::try_new(&handle).ok()?;
        Some(Self { _stream: stream, handle, sink })
    }

    fn play(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        // Stop whatever is playing, then start fresh on a new sink.
        self.sink.stop();
        let sink = rodio::Sink::try_new(&self.handle).map_err(|e| format!("audio sink: {e}"))?;
        let decoder = rodio::Decoder::new(std::io::Cursor::new(bytes)).map_err(|e| format!("could not decode the preview: {e}"))?;
        sink.append(decoder);
        sink.play();
        self.sink = sink;
        Ok(())
    }

    fn stop(&self) {
        self.sink.stop();
    }
}

thread_local! {
    static PREVIEW: std::cell::RefCell<Option<Preview>> = const { std::cell::RefCell::new(None) };
}

/// Play preview audio (the bytes of an mp3 or wav file) through the default output,
/// replacing anything already previewing. Best-effort: a machine with no
/// audio output returns an error the caller can show.
pub fn preview_play(bytes: Vec<u8>) -> Result<(), String> {
    PREVIEW.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Preview::new();
        }
        match slot.as_mut() {
            Some(p) => p.play(bytes),
            None => Err("no audio output device for preview".into()),
        }
    })
}

/// Pause or resume the current preview. Returns whether it is now paused.
pub fn preview_toggle_pause() -> bool {
    PREVIEW.with(|cell| {
        if let Some(p) = cell.borrow().as_ref() {
            if p.sink.is_paused() {
                p.sink.play();
                false
            } else {
                p.sink.pause();
                true
            }
        } else {
            false
        }
    })
}

/// Whether a preview is loaded (playing OR paused) — for showing controls.
pub fn preview_active() -> bool {
    PREVIEW.with(|cell| cell.borrow().as_ref().is_some_and(|p| !p.sink.empty()))
}

/// Whether the current preview is paused.
pub fn preview_is_paused() -> bool {
    PREVIEW.with(|cell| cell.borrow().as_ref().is_some_and(|p| p.sink.is_paused()))
}

/// Whether a preview is currently playing (not stopped, not paused).
pub fn preview_is_playing() -> bool {
    PREVIEW.with(|cell| cell.borrow().as_ref().is_some_and(|p| !p.sink.empty() && !p.sink.is_paused()))
}

/// Stop the current preview.
pub fn preview_stop() {
    PREVIEW.with(|cell| {
        if let Some(p) = cell.borrow().as_ref() {
            p.stop();
        }
    });
}

/// The directory downloaded audio lands in (and narration, below it):
/// a kept folder, `klipp_media::made::made_dir("audio")`, not the cache
/// (#730). A cache is safe to delete, and a project's music must not go
/// with it; a project's save copies what it uses into its own folder.
/// Spelled out here rather than called, as this crate does not depend on
/// klipp-media; a klipp-control test holds the two to one place.
pub fn download_dir() -> std::path::PathBuf {
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("klipp")
        .join("media")
        .join("audio")
}

#[cfg(test)]
mod tests {
    /// #1604: the list beside a finished export is the credits of what it
    /// played, each once; a file with no credit adds nothing; and an
    /// export that played nothing credited leaves no list, removing one
    /// an earlier export left.
    #[test]
    fn a_finished_export_gets_the_credits_of_what_it_played() {
        let dir = std::env::temp_dir().join(format!("klipp-write-credits-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str, credited: bool| {
            let audio = dir.join(format!("{name}.mp3"));
            std::fs::write(&audio, b"x").unwrap();
            if credited {
                let page = format!("https://freesound.org/people/a/sounds/{name}/");
                std::fs::write(audio.with_extension("klipp-credit.txt"), format!("\"{name}\" by a — CC BY ({page})\nLicence: CC BY (https://creativecommons.org/licenses/by/4.0/)\nSource page: {page}\n")).unwrap();
            }
            audio
        };
        let film = dir.join("film.mp4");
        assert_eq!(super::credits_path(&film), dir.join("film.credits.txt"));
        let (whoosh, own) = (file("whoosh", true), file("own", false));
        let (path, count) = super::write_credits(&film, &[whoosh.clone(), own.clone(), whoosh]).unwrap().unwrap();
        assert_eq!((path.clone(), count), (dir.join("film.credits.txt"), 1));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("\"whoosh\" by a").count(), 1, "each once: {text}");
        assert!(super::write_credits(&film, &[own]).unwrap().is_none());
        assert!(!path.exists(), "a list left by an earlier export goes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1570: a no-derivatives licence forbids editing the music, as an
    /// editor does; the other Creative Commons licences don't.
    #[test]
    fn no_derivatives_licences_forbid_edits() {
        assert!(super::forbids_edits("CC BY-ND"));
        assert!(super::forbids_edits("CC BY-NC-ND 3.0"));
        assert!(!super::forbids_edits("CC BY-SA"));
        assert!(!super::forbids_edits("CC0"));
        assert!(!super::forbids_edits("CC BY 4.0"));
    }

    /// #1573: a phrase that matches nothing falls back to shorter runs
    /// of its first words, longest first.
    #[test]
    fn a_phrase_falls_back_to_fewer_words() {
        assert_eq!(super::sfx_attempts("reverse cymbal swell"), vec!["reverse cymbal swell", "reverse cymbal", "reverse"]);
        assert_eq!(super::sfx_attempts("  whoosh "), vec!["whoosh"]);
        assert!(super::sfx_attempts("   ").is_empty());
    }

    /// #824: a download stops at its limit instead of reading on, however
    /// much the server sends.
    #[test]
    fn a_download_past_its_limit_is_refused() {
        let mut out = Vec::new();
        assert_eq!(super::copy_capped(&[7u8; 100][..], &mut out, 100).unwrap(), 100);
        assert_eq!(out.len(), 100);
        let mut out = Vec::new();
        let err = super::copy_capped(std::io::repeat(7), &mut out, 4096).unwrap_err();
        assert!(err.to_string().contains("allowed"), "{err}");
        assert!(out.len() <= 4097, "read {} bytes past a 4096-byte limit", out.len());
    }

    /// #822: a long name with a multi-byte letter where the cut falls is
    /// shortened to sixty characters instead of panicking.
    #[test]
    fn a_long_unicode_name_is_shortened_by_characters() {
        let name = format!("{}é and more", "a".repeat(59));
        let safe = safe_name(&name);
        assert_eq!(safe.chars().count(), 60);
        assert!(safe.ends_with('é'), "{safe}");
        assert_eq!(safe_name("Sigur Rós – Hoppípolla"), "Sigur_Rós___Hoppípolla");
    }

    /// #797: the credit beside a download says which library it came
    /// from; a file without one is no library's.
    #[test]
    fn the_credit_names_the_library() {
        let dir = std::env::temp_dir().join(format!("klipp-library-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let credit = |name: &str, page: &str| {
            let audio = dir.join(format!("{name}.mp3"));
            std::fs::write(audio.with_extension("klipp-credit.txt"), format!("\"x\" by y — CC BY ({page})\nLicence: CC BY (https://creativecommons.org/licenses/by/4.0/)\nSource page: {page}\n")).unwrap();
            audio
        };
        assert_eq!(library_source(&credit("song", "https://www.jamendo.com/track/123/song")), Some(Source::JamendoMusic));
        assert_eq!(library_source(&credit("whip", "https://freesound.org/people/a/sounds/734692/")), Some(Source::Freesound));
        assert_eq!(library_source(&credit("other", "https://example.com/a")), None);
        assert_eq!(library_source(&dir.join("bare.mp3")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1582: a credit file reads back as what was credited, and the
    /// credits list names each track once, under its licence, the ones
    /// that ask to be credited first.
    #[test]
    fn credits_are_listed_by_licence_attribution_first() {
        let text = |title: &str, author: &str, licence: &str, deed: &str, page: &str| {
            format!("\"{title}\" by {author} — {licence} ({page})\nLicence: {licence} ({deed})\nSource page: {page}\n")
        };
        let by = "https://creativecommons.org/licenses/by/4.0/";
        let zero = "https://creativecommons.org/publicdomain/zero/1.0/";
        let whoosh = parse_credit(&text("Big \"Air\" Whoosh", "Artninja", "CC BY", by, "https://freesound.org/people/Artninja/sounds/1/")).unwrap();
        assert_eq!(whoosh.title, "Big \"Air\" Whoosh", "a quote inside the title is kept");
        assert_eq!(whoosh.author, "Artninja");
        assert_eq!(whoosh.licence, "CC BY");
        assert_eq!(whoosh.licence_url, by);
        assert_eq!(whoosh.source, "https://freesound.org/people/Artninja/sounds/1/");
        assert!(whoosh.needs_attribution());
        let song = parse_credit(&text("Golden", "Ana", "CC BY-SA", "https://creativecommons.org/licenses/by-sa/3.0/", "https://www.jamendo.com/track/2")).unwrap();
        let hit = parse_credit(&text("Hit", "Papkin", "CC0", zero, "https://freesound.org/people/Papkin/sounds/3/")).unwrap();
        assert!(!hit.needs_attribution());
        assert!(parse_credit("not a credit").is_none());

        let list = credits_text(&[hit.clone(), song, whoosh.clone(), whoosh]);
        assert_eq!(list.matches("by Artninja").count(), 1, "a track used twice is credited once:\n{list}");
        let at = |s: &str| list.find(s).unwrap_or_else(|| panic!("{s} missing:\n{list}"));
        assert!(at("Attribution required") < at("CC BY (") && at("CC BY (") < at("CC BY-SA"), "{list}");
        assert!(at("CC BY-SA") < at("No attribution required") && at("No attribution required") < at("CC0"), "{list}");
        assert!(list.contains("  \"Hit\" by Papkin — https://freesound.org/people/Papkin/sounds/3/"), "{list}");
        assert_eq!(credits_text(&[]), "");
    }

    /// #1894: Freesound's description of an original reads as its format,
    /// rate, depth and channels, and its download keeps its extension.
    #[test]
    fn an_original_is_described_as_freesound_gives_it() {
        let r = serde_json::json!({"type": "wav", "samplerate": 48000.0, "channels": 2, "bitrate": 2304, "bitdepth": 24, "filesize": 2_100_000});
        let o = freesound_original(&r).unwrap();
        assert_eq!(o.describe(), "WAV 48 kHz 24-bit stereo, 2.1 MB");
        assert_eq!(o.extension(), "wav");
        let mp3 = freesound_original(&serde_json::json!({"type": "mp3", "samplerate": 44100, "channels": 1, "bitrate": 320})).unwrap();
        assert_eq!(mp3.describe(), "MP3 44.1 kHz 320 kbps mono");
        assert_eq!(freesound_original(&serde_json::json!({"type": "aif"})).unwrap().extension(), "aiff");
        assert!(freesound_original(&serde_json::json!({"name": "x"})).is_none(), "no type: nothing said");
    }

    /// #1894: a download is described by what it measures, not a fixed
    /// figure; a credit keeps it and the export credits carry it.
    #[test]
    fn a_download_says_what_it_measures_and_the_credit_keeps_it() {
        let d = Download { quality: "preview".into(), codec: "mp3".into(), bitrate_kbps: Some(184), sample_rate: Some(44100), channels: Some(2), bit_depth: None };
        assert_eq!(d.describe(), "preview (MP3 184 kbps, 44.1 kHz stereo)");
        let o = Download { quality: "original".into(), codec: "pcm_s24le".into(), bitrate_kbps: Some(2304), sample_rate: Some(48000), channels: Some(1), bit_depth: Some(24) };
        assert_eq!(o.describe(), "original (PCM_S24LE 24-bit, 48 kHz mono)");
        let track = Track {
            source: Source::Freesound, id: "1".into(), title: "Soft Impact".into(), artist: "dan".into(), duration_secs: 1.0, license: "CC0".into(),
            license_url: "http://creativecommons.org/publicdomain/zero/1.0/".into(), commercial_ok: true, preview_url: String::new(), download_url: String::new(),
            page_url: "https://freesound.org/s/1/".into(), image_url: String::new(), tags: vec![], pace: String::new(), original: None,
        };
        let credit = parse_credit(&credit_text(&track, &d)).expect("reads back");
        assert_eq!(credit.quality.as_deref(), Some("preview (MP3 184 kbps, 44.1 kHz stereo)"));
        assert!(credits_text(&[credit]).contains("[preview (MP3 184 kbps, 44.1 kHz stereo)]"));
        // A credit from before, with no quality line, still reads.
        let old = "\"Old\" by Ana — CC BY (https://x)\nLicence: CC BY (https://deed)\nSource page: https://x\n";
        assert_eq!(parse_credit(old).map(|c| c.quality), Some(None));
        assert_eq!(decode("a%2Fb+c%"), "a/b c%");
    }

    /// #1894: the measure reads a real file's codec, rate, channels and,
    /// for PCM, depth; for a lossy one, its bitrate. Skipped, said, without
    /// ffmpeg.
    #[test]
    fn a_download_is_measured_off_its_file() {
        let dir = std::env::temp_dir().join(format!("klipp-download-measure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, args: &[&str]| -> Option<std::path::PathBuf> {
            let out = dir.join(name);
            let ok = std::process::Command::new("ffmpeg").args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i", "sine=f=440:d=1:r=48000"]).args(args).arg(&out).status().is_ok_and(|s| s.success());
            ok.then_some(out)
        };
        let (Some(wav), Some(mp3)) = (make("o.wav", &["-ac", "2", "-c:a", "pcm_s24le"]), make("p.mp3", &["-ar", "44100", "-b:a", "192k"])) else {
            eprintln!("UNVERIFIED: no ffmpeg, downloads not measured");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let w = Download::measure(&wav, "original");
        assert_eq!((w.codec.as_str(), w.sample_rate, w.channels, w.bit_depth), ("pcm_s24le", Some(48000), Some(2), Some(24)), "{w:?}");
        let m = Download::measure(&mp3, "preview");
        assert_eq!((m.codec.as_str(), m.sample_rate, m.bit_depth), ("mp3", Some(44100), None), "{m:?}");
        assert!(m.bitrate_kbps.is_some_and(|b| (180..=200).contains(&b)), "{m:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1933: two tracks with the same label and different deeds are each
    /// credited under their own, never one's deed given to the other; the
    /// same label and deed still group.
    #[test]
    fn equal_licence_labels_keep_their_own_versions() {
        let credit = |title: &str, deed: &str| Credit {
            title: title.into(),
            author: "Ana".into(),
            licence: "CC BY".into(),
            licence_url: deed.into(),
            source: format!("https://www.jamendo.com/track/{title}"),
            quality: None,
        };
        let (three, four) = ("https://creativecommons.org/licenses/by/3.0/", "https://creativecommons.org/licenses/by/4.0/");
        let list = credits_text(&[credit("Old", three), credit("New", four), credit("Newer", four)]);
        let (h3, h4) = (format!("CC BY ({three})"), format!("CC BY ({four})"));
        assert_eq!(list.matches(&h3).count(), 1, "the 3.0 deed has its heading:\n{list}");
        assert_eq!(list.matches(&h4).count(), 1, "the 4.0 deed has its heading, once for both its tracks:\n{list}");
        let at = |s: &str| list.find(s).unwrap_or_else(|| panic!("{s} missing:\n{list}"));
        assert!(at(&h3) < at("\"Old\"") && at("\"Old\"") < at(&h4), "Old is under 3.0:\n{list}");
        assert!(at(&h4) < at("\"New\"") && at(&h4) < at("\"Newer\""), "New and Newer are under 4.0:\n{list}");
    }

    use super::*;

    fn track(title: &str, tags: &[&str]) -> Track {
        Track {
            source: Source::JamendoMusic, id: title.into(), title: title.into(), artist: String::new(),
            duration_secs: 120.0, license: String::new(), license_url: String::new(), commercial_ok: true,
            preview_url: String::new(), download_url: String::new(), page_url: String::new(),
            image_url: String::new(), tags: tags.iter().map(|t| t.to_string()).collect(), pace: String::new(), original: None,
        }
    }

    /// #739: the most popular answer to "chill acoustic guitar" was a dance
    /// track; ranked by the query's words, the acoustic one comes first and
    /// the energetic one last.
    #[test]
    fn music_is_ranked_by_the_query_not_popularity() {
        let mut t = vec![
            track("Energy", &["happy", "energetic", "dance", "electronic"]),
            track("Morning", &["pop"]),
            track("Porch", &["chill", "acoustic", "guitar"]),
        ];
        let words: Vec<String> = ["chill", "acoustic", "guitar"].iter().map(|w| w.to_string()).collect();
        rank_by_query(&mut t, &words);
        let order: Vec<&str> = t.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(order, vec!["Porch", "Morning", "Energy"]);
    }

    /// #1931: a family is named as results label it, or loosely; one that
    /// isn't is refused with the families there are.
    #[test]
    fn a_licence_family_is_read_by_its_label() {
        assert_eq!(licence_family("CC BY-SA"), Ok("CC BY-SA"));
        assert_eq!(licence_family("by-sa"), Ok("CC BY-SA"));
        assert_eq!(licence_family("cc0"), Ok("CC0"));
        assert_eq!(licence_family("by nc nd"), Ok("CC BY-NC-ND"));
        let err = licence_family("GPL").unwrap_err();
        assert!(LICENCE_FAMILIES.iter().all(|f| err.contains(f)), "{err}");
        // Every family is a label the licence reader gives.
        for url in ["publicdomain/zero/1.0/", "licenses/by/4.0/", "licenses/by-sa/3.0/", "licenses/by-nc/3.0/", "licenses/by-nc-sa/3.0/", "licenses/by-nd/3.0/", "licenses/by-nc-nd/3.0/"] {
            let label = cc_label(&format!("https://creativecommons.org/{url}")).0;
            assert!(LICENCE_FAMILIES.contains(&label.as_str()), "{label}");
        }
    }

    /// #1931: the filters drop what the editor can't use before the
    /// ranking and the limit, and the library is asked to leave it out.
    #[test]
    fn a_music_filter_keeps_only_editable_tracks_of_its_families() {
        let track = |license: &str, commercial_ok: bool| Track {
            source: Source::JamendoMusic, id: license.into(), title: "t".into(), artist: "a".into(), duration_secs: 60.0,
            license: license.into(), license_url: String::new(), commercial_ok, preview_url: "https://p".into(),
            download_url: String::new(), page_url: String::new(), image_url: String::new(), tags: vec![], pace: String::new(), original: None,
        };
        let nd = track("CC BY-ND", true);
        let sa = track("CC BY-SA", true);
        let zero = track("CC0", true);
        let nc = track("CC BY-NC", false);
        let edits = MusicFilter { edits_only: true, ..Default::default() };
        assert!(!edits.keeps(&nd) && edits.keeps(&sa) && edits.keeps(&zero) && edits.keeps(&nc));
        let families = MusicFilter { licences: vec!["CC0", "CC BY-SA"], ..Default::default() };
        assert!(families.keeps(&sa) && families.keeps(&zero) && !families.keeps(&nd) && !families.keeps(&nc));
        let both = MusicFilter { commercial_only: true, edits_only: true, ..Default::default() };
        assert!(!both.keeps(&nc) && !both.keeps(&nd) && both.keeps(&sa));
        assert_eq!(library_filters(&both), "&ccnc=false&ccnd=false");
        assert_eq!(library_filters(&MusicFilter::default()), "");
    }

    #[test]
    fn cc_labels_and_commercial_flag() {
        assert_eq!(cc_label("https://creativecommons.org/licenses/by/4.0/").0, "CC BY");
        assert!(cc_label("https://creativecommons.org/licenses/by/4.0/").1);
        assert_eq!(cc_label("https://creativecommons.org/licenses/by-nc-sa/4.0/").0, "CC BY-NC-SA");
        assert!(!cc_label("https://creativecommons.org/licenses/by-nc-sa/4.0/").1);
        assert_eq!(cc_label("https://creativecommons.org/publicdomain/zero/1.0/").0, "CC0");
    }

    #[test]
    fn encode_keeps_unreserved_and_plus_spaces() {
        assert_eq!(encode("upbeat electronic"), "upbeat+electronic");
        assert_eq!(encode("lo-fi"), "lo-fi");
    }

    /// Hits the live Freesound API — run on demand (`--ignored`), so a
    /// network blip never blocks the workspace test run.
    #[test]
    #[ignore = "hits the live Freesound API"]
    fn live_sfx_search_when_configured() {
        if !sfx_available() {
            eprintln!("skipping: no Freesound token");
            return;
        }
        let tracks = search_sfx("whoosh", 3).expect("search runs");
        assert!(!tracks.is_empty(), "whoosh returns effects");
        let t = &tracks[0];
        assert!(!t.title.is_empty() && !t.preview_url.is_empty(), "effect has a title and a preview: {t:?}");
    }

    /// Hits the live Jamendo API — run on demand (`--ignored`), so a
    /// network blip never blocks the workspace test run.
    #[test]
    #[ignore = "hits the live Jamendo API"]
    fn live_music_search_when_configured() {
        if !music_available() {
            eprintln!("skipping: no Jamendo client id");
            return;
        }
        let tracks = search_music("upbeat", 3).expect("search runs");
        assert!(!tracks.is_empty(), "upbeat returns tracks");
        let t = &tracks[0];
        assert!(!t.title.is_empty() && !t.preview_url.is_empty(), "track has a title and a preview: {t:?}");
    }
}
