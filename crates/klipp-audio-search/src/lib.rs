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
}

impl Track {
    /// The attribution line to keep with the asset and put in credits.
    pub fn attribution(&self) -> String {
        format!("\"{}\" by {} — {} ({})", self.title, self.artist, self.license, self.page_url)
    }
}

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

/// Read a key: the environment variable first, then `keys.toml`.
fn read_key(env: &str, toml_key: &str) -> Option<String> {
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
    let token = freesound_token().ok_or_else(|| "no Freesound token set — put freesound_token in ~/.config/klipp/keys.toml or set KLIPP_FREESOUND_TOKEN".to_string())?;
    let limit = limit.clamp(1, 50);
    let q = encode(query.trim());
    let filter = match max_secs.filter(|m| m.is_finite() && *m > 0.0) {
        Some(m) => format!("&filter={}", encode(&format!("duration:[0 TO {m}]"))),
        None => String::new(),
    };
    let url = format!(
        "https://freesound.org/apiv2/search/text/?query={q}&token={token}&page_size={limit}&sort=rating_desc&fields=id,name,username,duration,license,previews,url,tags{filter}"
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
        let str_of = |k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let license_url = str_of("license");
        let (license, commercial_ok) = if license_url.is_empty() { ("Creative Commons".to_string(), false) } else { cc_label(&license_url) };
        let preview = r.get("previews").and_then(|p| p.get("preview-hq-mp3").or_else(|| p.get("preview-lq-mp3"))).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let tags = r.get("tags").and_then(|t| t.as_array()).map(|a| {
            let mut v: Vec<String> = a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
            v.truncate(8);
            v
        }).unwrap_or_default();
        tracks.push(Track {
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
        });
    }
    Ok(tracks)
}

/// What a music search is narrowed to.
#[derive(Debug, Clone, Copy, Default)]
pub struct MusicFilter {
    /// Only tracks whose licence allows commercial use.
    pub commercial_only: bool,
    /// Only instrumental tracks: no voice over a promo's own titles.
    pub instrumental: bool,
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
            let mut got = jamendo_tracks(&extra, page, offset, filter)?;
            let read = got.len();
            got.retain(|t| !t.preview_url.is_empty() || !t.download_url.is_empty());
            if filter.commercial_only {
                got.retain(|t| t.commercial_ok);
            }
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

fn jamendo_tracks(extra: &str, limit: u32, offset: u32, filter: MusicFilter) -> Result<Vec<Track>, String> {
    let cid = jamendo_client_id().ok_or_else(|| "no Jamendo client id set — put jamendo_client_id in ~/.config/klipp/keys.toml or set KLIPP_JAMENDO_CLIENT_ID".to_string())?;
    let vocal = if filter.instrumental { "&vocalinstrumental=instrumental" } else { "" };
    // `ccnc=false`: no non-commercial licences, filtered by the library
    // before it counts the page.
    let commercial = if filter.commercial_only { "&ccnc=false" } else { "" };
    let url = format!(
        "https://api.jamendo.com/v3.0/tracks/?client_id={cid}&format=json&limit={limit}&offset={offset}&order=popularity_total&include=musicinfo+licenses&audioformat=mp32{vocal}{commercial}&{extra}"
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
        });
    }
    Ok(tracks)
}

/// Sanitise a string into a safe file-name fragment.
fn safe_name(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
    out.truncate(60);
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
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("creating {}: {e}", dest_dir.display()))?;
    let base = format!("{}-{}-{}", safe_name(&track.artist), safe_name(&track.title), track.id);
    let audio_path = dest_dir.join(format!("{base}.mp3"));
    let resp = ureq::get(url).timeout(std::time::Duration::from_secs(45)).call().map_err(|e| format!("download failed: {e}"))?;
    let mut reader = resp.into_reader();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes).map_err(|e| format!("reading audio: {e}"))?;
    if bytes.len() < 1024 {
        return Err(format!("\"{}\" downloaded empty", track.title));
    }
    std::fs::write(&audio_path, &bytes).map_err(|e| format!("writing {}: {e}", audio_path.display()))?;
    // The credit line, kept next to the file for export attribution.
    let credit = format!("{}
Licence: {} ({})
Source page: {}
", track.attribution(), track.license, track.license_url, track.page_url);
    let _ = std::fs::write(dest_dir.join(format!("{base}.klipp-credit.txt")), credit);
    Ok(audio_path)
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
    let mut reader = resp.into_reader();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut bytes).map_err(|e| format!("reading preview: {e}"))?;
    Ok(bytes)
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

    use super::*;

    fn track(title: &str, tags: &[&str]) -> Track {
        Track {
            source: Source::JamendoMusic, id: title.into(), title: title.into(), artist: String::new(),
            duration_secs: 120.0, license: String::new(), license_url: String::new(), commercial_ok: true,
            preview_url: String::new(), download_url: String::new(), page_url: String::new(),
            image_url: String::new(), tags: tags.iter().map(|t| t.to_string()).collect(), pace: String::new(),
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
