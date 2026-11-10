//! Whether a file a project names is a picture, a sound or a still, read
//! as someone else's project may name it: a shared project can name any
//! file on the machine, and a Collect Project of it would carry that file
//! off in the folder handed on.

use std::path::Path;

/// The demuxers that read text as a picture or a track: ffmpeg opens a
/// plain text file as an ANSI-art video (`tty`) or as subtitles.
const TEXT_DEMUXERS: &[&str] = &[
    "tty", "srt", "ass", "webvtt", "subviewer", "subviewer1", "lrc", "microdvd", "mpl2", "mpsub", "pjs", "realtext", "sami",
    "stl", "vplayer", "aqtitle", "jacosub", "scc", "dfxp", "ffmetadata",
];

/// `Ok` when `path` is a file Klipp reads as media: one with a picture,
/// sound or still extension, or one that opens as a picture or sound
/// stream. Never one in a hidden folder (`~/.ssh`, `~/.config`) unless
/// under one of `trusted` (the project's own folder, Klipp's caches),
/// and never a dotfile. `Err` says why, for the person.
pub fn media_file_check(path: &Path, trusted: &[&Path]) -> Result<(), String> {
    let real = plain_file(path, trusted)?;
    let path = real.as_path();
    if klipp_core::media_extensions::path_is_media(path) {
        return Ok(());
    }
    // Any other name: read for what it is.
    let input = crate::open_input(path).map_err(|_| "it is not a picture or sound file".to_string())?;
    let format = input.format().name().to_string();
    if format.split(',').any(|f| TEXT_DEMUXERS.contains(&f)) {
        return Err("it is not a picture or sound file".into());
    }
    let media = input.streams().any(|s| {
        let p = s.parameters();
        matches!(p.medium(), ffmpeg_next::media::Type::Video | ffmpeg_next::media::Type::Audio) && p.id() != ffmpeg_next::codec::Id::ANSI
    });
    if media {
        Ok(())
    } else {
        Err("it is not a picture or sound file".into())
    }
}

/// Where `path` really is, `..` and links followed so neither a path
/// through a trusted folder nor a link into a hidden one gets by; refused
/// when that is not a file, or is in a hidden folder (or is a dotfile)
/// outside `trusted`.
fn plain_file(path: &Path, trusted: &[&Path]) -> Result<std::path::PathBuf, String> {
    let real = std::fs::canonicalize(path).map_err(|_| "it is not there".to_string())?;
    // Below a trusted folder only what is under it counts: that folder
    // may sit in a hidden one (~/.cache/klipp), a hidden one in it may not.
    let rest = trusted.iter().filter_map(|t| std::fs::canonicalize(t).ok()).filter_map(|t| real.strip_prefix(&t).ok().map(Path::to_path_buf)).min_by_key(|r| r.components().count()).unwrap_or_else(|| real.clone());
    if rest.components().any(|c| matches!(c, std::path::Component::Normal(n) if n.to_string_lossy().starts_with('.'))) {
        return Err("it is in a hidden folder, where private files are kept, not media".into());
    }
    if !real.is_file() {
        return Err("it is not a file".into());
    }
    Ok(real)
}

/// The names a colour LUT file has.
pub const LUT_EXTENSIONS: &[&str] = &["cube", "3dl", "csp", "spi1d", "spi3d", "lut", "look", "cdl", "cc", "ccc", "clf", "ctf"];

/// `Ok` when `path` is a LUT file: named as one, and not in a hidden
/// folder outside `trusted`. A LUT is text, so its name is what tells it
/// from a private text file.
pub fn lut_file_check(path: &Path, trusted: &[&Path]) -> Result<(), String> {
    let real = plain_file(path, trusted)?;
    let ext = real.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if LUT_EXTENSIONS.contains(&ext.as_str()) {
        Ok(())
    } else {
        Err("it is not a LUT file".into())
    }
}

/// What a person is told of an entry kept offline for `why`, and what to
/// do about it.
pub fn refused_hint(why: &str) -> String {
    format!("Kept offline: {why}. If it is your media, move the file to a folder that isn't hidden, then Relink it.")
}

/// Where media may sit in a hidden folder above it: Klipp's own folders,
/// the temporary folder and the project's own folder, so a project kept
/// in a hidden folder keeps its media. Open, Relink and Collect all ask
/// this, so a file one of them takes the others take too.
pub fn trusted_roots(project_dir: Option<&Path>) -> Vec<std::path::PathBuf> {
    let mut out = klipp_folders();
    out.extend(project_dir.map(Path::to_path_buf));
    out
}

/// The folders Klipp keeps its own files in (template media, caches,
/// brand kits) and the temporary folder: media there may sit in a hidden
/// folder above them, as `~/.cache/klipp` does.
pub fn klipp_folders() -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = [dirs::cache_dir(), dirs::data_dir(), dirs::config_dir()].into_iter().flatten().map(|d| d.join("klipp")).collect();
    out.push(std::env::temp_dir());
    out.extend(flatpak_root(dirs::home_dir().as_deref(), klipp_core::flatpak::app_id().as_deref()));
    out
}

/// In a Flatpak (`app_id`), the app's own folder in `home`'s hidden
/// `.var/app`, where its sandbox keeps its data, cache and settings: Klipp's
/// own. Another app's folder there is not, and stays refused as hidden.
pub fn flatpak_root(home: Option<&Path>, app_id: Option<&str>) -> Option<std::path::PathBuf> {
    // An id is one name: never a path out of `.var/app`.
    let id = app_id.filter(|id| !id.is_empty() && !id.contains('/') && *id != "." && *id != "..")?;
    Some(home?.join(".var").join("app").join(id))
}

#[cfg(test)]
mod tests {
    use super::media_file_check;

    fn dir(label: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("klipp_media_check_{label}_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// In a Flatpak, the app's own folder under the home's hidden
    /// `.var/app` (`<id>`, where its sandbox keeps its data, cache and
    /// settings) is Klipp's: media there is taken. Another app's folder
    /// there stays hidden and refused, and outside a Flatpak so is Klipp's.
    #[test]
    fn a_flatpaks_own_folder_is_klipps() {
        let home = dir("flatpak_home");
        let id = "io.github.davidkodar.Klipp";
        let own = home.join(".var/app").join(id).join("data/klipp/intro.mp4");
        let other = home.join(".var/app/org.other.App/data/clip.mp4");
        for f in [&own, &other] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, [0u8; 64]).unwrap();
        }
        let roots: Vec<std::path::PathBuf> = super::flatpak_root(Some(&home), Some(id)).into_iter().collect();
        let trusted: Vec<&std::path::Path> = roots.iter().map(|p| p.as_path()).collect();
        assert_eq!(roots, vec![home.join(".var/app").join(id)]);
        assert!(media_file_check(&own, &trusted).is_ok(), "the Flatpak's own folder");
        assert!(media_file_check(&other, &trusted).is_err(), "another app's");
        assert!(super::flatpak_root(Some(&home), None).is_none(), "outside a Flatpak, nothing");
        // An id is one name, never a way out of `.var/app` to the home.
        for bad in ["..", "../..", "a/../..", ""] {
            assert!(super::flatpak_root(Some(&home), Some(bad)).is_none(), "{bad:?}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A person is told why an entry is kept offline, and the way back:
    /// move it out of the hidden folder, then Relink it.
    #[test]
    fn the_hint_says_why_and_what_to_do() {
        let hint = super::refused_hint("it is in a hidden folder, where private files are kept, not media");
        assert!(hint.contains("hidden folder") && hint.contains("move the file") && hint.contains("Relink"), "{hint}");
    }

    /// A hostile project's media entry naming a text file, a key in
    /// a hidden folder or a dotfile is refused; a file with a media name is
    /// taken, and one in a trusted folder even where that folder is hidden.
    #[test]
    fn only_media_is_taken() {
        // As Klipp passes it: the temporary folder is trusted, a hidden one
        // in it is not.
        let t = std::env::temp_dir();
        let tmp: [&std::path::Path; 1] = [&t];
        let d = dir("only");
        let text = d.join("hostname");
        std::fs::write(&text, "laptop\n").unwrap();
        let notes = d.join("notes.txt");
        std::fs::write(&notes, "a private note\nwith lines\n").unwrap();
        let ssh = d.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        let key = ssh.join("id_rsa.mp4");
        std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        let dotfile = d.join(".netrc");
        std::fs::write(&dotfile, "machine x login y password z\n").unwrap();
        let clip = d.join("shot.mov");
        std::fs::write(&clip, [0u8; 64]).unwrap();
        for refused in [&text, &notes, &key, &dotfile] {
            assert!(media_file_check(refused, &tmp).is_err(), "{} was taken", refused.display());
        }
        assert!(media_file_check(&clip, &tmp).is_ok());
        assert!(media_file_check(&d.join("gone.mov"), &tmp).is_err(), "no file");
        let cache = d.join(".cache").join("klipp");
        std::fs::create_dir_all(&cache).unwrap();
        let template_media = cache.join("intro.mp4");
        std::fs::write(&template_media, [0u8; 64]).unwrap();
        assert!(media_file_check(&template_media, &[&t, &cache]).is_ok(), "Klipp's own hidden folder is trusted");
        assert!(media_file_check(&template_media, &tmp).is_err(), "and only when said so");
        // A LUT by its name, never a text file passed off as one.
        let lut = d.join("film.cube");
        std::fs::write(&lut, "LUT_3D_SIZE 2\n").unwrap();
        assert!(super::lut_file_check(&lut, &tmp).is_ok());
        assert!(super::lut_file_check(&text, &tmp).is_err() && super::lut_file_check(&notes, &tmp).is_err());
        // Out of a trusted folder by `..`, or by a link: where it leads.
        let project = d.join("Trip.klipp");
        std::fs::create_dir_all(&project).unwrap();
        assert!(media_file_check(&project.join("..").join(".ssh").join("id_rsa.mp4"), &[&t, &project]).is_err(), "out by ..");
        #[cfg(unix)]
        {
            let link = project.join("holiday.mp4");
            std::os::unix::fs::symlink(&key, &link).unwrap();
            assert!(media_file_check(&link, &[&t, &project]).is_err(), "a link to a hidden folder's file");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
