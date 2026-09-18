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
    let token = freesound_token().ok_or_else(|| "no Freesound token set — put freesound_token in ~/.config/klipp/keys.toml or set KLIPP_FREESOUND_TOKEN".to_string())?;
    let limit = limit.clamp(1, 50);
    let q = encode(query.trim());
    let url = format!(
        "https://freesound.org/apiv2/search/text/?query={q}&token={token}&page_size={limit}&sort=rating_desc&fields=id,name,username,duration,license,previews,url,tags"
    );
    let text = ureq::get(&url)
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
        });
    }
    Ok(tracks)
}

/// Search Jamendo for music. `query` is matched as fuzzy tags (moods,
/// genres), so "upbeat electronic" finds upbeat electronic tracks.
pub fn search_music(query: &str, limit: u32) -> Result<Vec<Track>, String> {
    let cid = jamendo_client_id().ok_or_else(|| "no Jamendo client id set — put jamendo_client_id in ~/.config/klipp/keys.toml or set KLIPP_JAMENDO_CLIENT_ID".to_string())?;
    let limit = limit.clamp(1, 50);
    let q = encode(query.trim());
    let url = format!(
        "https://api.jamendo.com/v3.0/tracks/?client_id={cid}&format=json&limit={limit}&order=popularity_total&include=musicinfo+licenses&audioformat=mp32&fuzzytags={q}"
    );
    let text = ureq::get(&url)
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
            v.truncate(8);
            v
        }).unwrap_or_default();
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
    let resp = ureq::get(url).call().map_err(|e| format!("download failed: {e}"))?;
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

/// The directory downloaded audio lands in: the user's cache.
pub fn download_dir() -> std::path::PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("klipp").join("audio")
}

#[cfg(test)]
mod tests {
    use super::*;

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
