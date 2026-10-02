//! Where media Klipp makes or fetches for a project lives (#730): music
//! and sound effects from the libraries, narration, screen and voice
//! recordings, synthesized sound.
//!
//! It used to go into the user's cache, and a cache is meant to be safe
//! to delete: clearing it, or a system cleaner doing so, took the
//! project's music offline, and the licence and credit beside it. Now it
//! is written to a folder that is kept (`made_dir`), and a project's save
//! copies what it uses into the project's own folder
//! (`klipp_project::adopt_made_media`), so the project carries it.

use std::path::{Path, PathBuf};

/// The kept folder for one kind of made media: `audio` (library music
/// and effects, narration under it), `recordings`, `auto-sfx`.
pub fn made_dir(kind: &str) -> PathBuf {
    made_base().join(kind)
}

/// `~/.local/share/klipp/media`, or the platform's data folder.
pub fn made_base() -> PathBuf {
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("klipp")
        .join("media")
}

/// The folders made media lives in or used to: the kept one, and the
/// cache folders earlier builds wrote to, which a save still adopts from
/// so a project made before this keeps its music when the cache goes.
pub fn made_roots() -> Vec<PathBuf> {
    let cache = crate::disk_cache::cache_base_dir();
    vec![made_base(), cache.join("audio"), cache.join("recordings"), cache.join("auto-sfx")]
}

/// Where `path` sits under the made-media folders, as the path a
/// project keeps it at below its own `media` folder: `audio/x.mp3`,
/// `recordings/take.mkv`. `None` for anything else.
pub fn made_relative(path: &Path) -> Option<PathBuf> {
    let base = made_base();
    if let Ok(rel) = path.strip_prefix(&base) {
        return Some(rel.to_path_buf());
    }
    let cache = crate::disk_cache::cache_base_dir();
    for kind in ["audio", "recordings", "auto-sfx"] {
        if let Ok(rel) = path.strip_prefix(cache.join(kind)) {
            return Some(Path::new(kind).join(rel));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_media_is_not_in_the_cache_and_old_cache_files_are_recognised() {
        let cache = crate::disk_cache::cache_base_dir();
        assert!(!made_dir("audio").starts_with(&cache), "made media must not live in the cache: {}", made_dir("audio").display());
        assert_eq!(made_relative(&made_dir("audio").join("song.mp3")), Some(PathBuf::from("audio/song.mp3")));
        assert_eq!(made_relative(&cache.join("audio").join("narration").join("vo.wav")), Some(PathBuf::from("audio/narration/vo.wav")));
        assert_eq!(made_relative(&cache.join("recordings").join("take.mkv")), Some(PathBuf::from("recordings/take.mkv")));
        assert_eq!(made_relative(Path::new("/home/someone/Videos/shot.mov")), None);
        assert_eq!(made_relative(&cache.join("proxies").join("p.mov")), None, "a proxy is a cache, rightly");
    }
}
