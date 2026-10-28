//! The Piper engine, downloaded on request (#2978): Piper's official
//! Linux release, pinned by its checksum in the model manifest like every
//! other download, unpacked into Klipp's data folder and run from there.
//!
//! A `piper` the person installed (`KLIPP_PIPER`, or on the path) comes
//! first; this is for everyone else. It is never fetched unasked.
//!
//! The archive is read as untrusted, checksum and all: it is checked
//! against the manifest before a byte is unpacked, every name in it must
//! be a plain path under `piper/` (no `..`, nothing absolute), a link may
//! only name a file beside it, nothing but files, folders and such links
//! is unpacked, and there are caps on what it unpacks to. It is unpacked
//! into a fresh folder beside the final one and moved into place whole,
//! so a refused or broken archive leaves nothing behind.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

/// The manifest entry of the engine for this machine.
pub const ENGINE: &str = "piper_engine_linux_x86_64";

/// What an archive may unpack to.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Bytes, all files together.
    pub bytes: u64,
    /// Files, folders and links.
    pub entries: usize,
}

/// The release unpacks to 52.6 MB in 406 entries; these leave room for a
/// later one and none for a bomb.
pub const CAPS: Caps = Caps { bytes: 128 * 1024 * 1024, entries: 2_000 };

/// Whether `id` is the engine's manifest entry, not a voice.
pub fn is_engine(id: &str) -> bool {
    id == ENGINE
}

/// The folder the engine is unpacked into, beside the model cache.
pub fn folder(cache: &klipp_ml::cache::ModelCache) -> PathBuf {
    cache.root().with_file_name("piper-engine")
}

/// The program in an unpacked engine, if it is there.
pub fn installed(cache: &klipp_ml::cache::ModelCache) -> Option<PathBuf> {
    Some(folder(cache).join("piper").join("piper")).filter(|p| p.is_file())
}

/// Download the engine through the verified cache and unpack it: the
/// program to run. Blocks for the download.
pub fn install(progress: &mut dyn klipp_ml::download::ProgressSink, cancel: &std::sync::atomic::AtomicBool) -> Result<PathBuf, String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let entry = manifest.get(ENGINE).ok_or_else(|| format!("no {ENGINE} in the model manifest"))?;
    let archive = cache.path_for(entry);
    if !archive.is_file() {
        klipp_ml::download::download_to_cache(&cache, entry, progress, cancel).map_err(|e| format!("download of the Piper engine: {e:?}"))?;
    }
    unpack(&archive, &entry.sha256, &folder(&cache), CAPS)
}

/// Unpack the engine archive at `archive`, whose SHA-256 must be `sha256`,
/// into `into`, replacing what was there: the program to run.
pub fn unpack(archive: &Path, sha256: &str, into: &Path, caps: Caps) -> Result<PathBuf, String> {
    let got = klipp_ml::cache::sha256_of_file(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    if !got.eq_ignore_ascii_case(sha256) {
        return Err(format!("the Piper engine archive is not the one Klipp knows (sha256 {got}); it was not unpacked"));
    }
    let parent = into.parent().ok_or("the engine folder has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let staging = into.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    if let Err(e) = unpack_into(archive, &staging, caps) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    let program = staging.join("piper").join("piper");
    if !program.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("the Piper engine archive has no piper/piper program".into());
    }
    let _ = std::fs::remove_dir_all(into);
    std::fs::rename(&staging, into).map_err(|e| format!("{}: {e}", into.display()))?;
    Ok(into.join("piper").join("piper"))
}

/// An entry's name as plain parts under `piper/`, or why not.
fn plain_parts(name: &Path) -> Result<Vec<&str>, String> {
    let mut parts = Vec::new();
    for c in name.components() {
        match c {
            Component::Normal(p) => parts.push(p.to_str().ok_or_else(|| format!("{name:?} is not a UTF-8 name"))?),
            Component::CurDir if parts.is_empty() => {}
            _ => return Err(format!("{name:?} is not a plain path inside the archive")),
        }
    }
    if parts.first() != Some(&"piper") {
        return Err(format!("{name:?} is not under piper/"));
    }
    Ok(parts)
}

fn unpack_into(archive: &Path, root: &Path, caps: Caps) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let (mut bytes, mut count) = (0u64, 0usize);
    for entry in tar.entries().map_err(|e| format!("the Piper engine archive: {e}"))? {
        let mut entry = entry.map_err(|e| format!("the Piper engine archive: {e}"))?;
        count += 1;
        if count > caps.entries {
            return Err(format!("the Piper engine archive holds more than {} entries; it was not unpacked", caps.entries));
        }
        let name = entry.path().map_err(|e| format!("an entry's name: {e}"))?.into_owned();
        let parts = plain_parts(&name)?;
        let mut path = root.to_path_buf();
        path.extend(&parts);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        } else if kind.is_file() {
            let size = entry.header().size().map_err(|e| format!("{name:?}: {e}"))?;
            bytes = bytes.saturating_add(size);
            if bytes > caps.bytes {
                return Err(format!("the Piper engine archive unpacks to more than {} MB; it was not unpacked", caps.bytes / (1024 * 1024)));
            }
            let executable = entry.header().mode().is_ok_and(|m| m & 0o111 != 0);
            write_file(&path, &mut (&mut entry).take(size), executable)?;
        } else if kind.is_symlink() {
            // A library's short name for its versioned file, beside it.
            let target = entry.link_name().map_err(|e| format!("{name:?}: {e}"))?.ok_or_else(|| format!("{name:?} links to nothing"))?;
            let mut c = target.components();
            match (c.next(), c.next()) {
                (Some(Component::Normal(_)), None) => {}
                _ => return Err(format!("{name:?} links to {target:?}, outside its folder; it was not unpacked")),
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::os::unix::fs::symlink(&target, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        } else {
            return Err(format!("{name:?} is a {kind:?} entry, which an engine archive does not hold; it was not unpacked"));
        }
    }
    Ok(())
}

/// A new file at `path`, never through one that is there already (a link
/// unpacked before it).
fn write_file(path: &Path, from: &mut dyn std::io::Read, executable: bool) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if executable { 0o755 } else { 0o644 })
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    std::io::copy(from, &mut out).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder of the test's own under the temp dir.
    fn scratch(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("klipp-piper-engine-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// One entry of a fixture archive.
    enum Item<'a> {
        Dir(&'a str),
        File(&'a str, &'a [u8], u32),
        Link(&'a str, &'a str),
        HardLink(&'a str, &'a str),
    }

    /// A gzipped tar of `items`, its names written as given (`..` and
    /// absolute ones included, which the tar crate's own setters refuse),
    /// and its SHA-256.
    fn archive(at: &Path, items: &[Item]) -> String {
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(at).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        for item in items {
            let mut h = tar::Header::new_old();
            let (name, kind, data, mode, link): (&str, tar::EntryType, &[u8], u32, Option<&str>) = match item {
                Item::Dir(n) => (n, tar::EntryType::Directory, b"", 0o755, None),
                Item::File(n, d, m) => (n, tar::EntryType::Regular, d, *m, None),
                Item::Link(n, t) => (n, tar::EntryType::Symlink, b"", 0o777, Some(t)),
                Item::HardLink(n, t) => (n, tar::EntryType::Link, b"", 0o644, Some(t)),
            };
            let raw = h.as_old_mut();
            raw.name[..name.len()].copy_from_slice(name.as_bytes());
            if let Some(t) = link {
                raw.linkname[..t.len()].copy_from_slice(t.as_bytes());
            }
            h.set_entry_type(kind);
            h.set_mode(mode);
            h.set_size(data.len() as u64);
            h.set_cksum();
            b.append(&h, data).unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        klipp_ml::cache::sha256_of_file(at).unwrap()
    }

    /// The release's shape in small: the program, a library and its
    /// short-name link, and the phoneme data.
    fn engine_items<'a>() -> Vec<Item<'a>> {
        vec![
            Item::Dir("piper/"),
            Item::File("piper/piper", b"#!/bin/sh\necho piper\n", 0o755),
            Item::File("piper/libespeak-ng.so.1.52.0.1", b"lib", 0o644),
            Item::Link("piper/libespeak-ng.so.1", "libespeak-ng.so.1.52.0.1"),
            Item::Dir("piper/espeak-ng-data/"),
            Item::File("piper/espeak-ng-data/phontab", b"phonemes", 0o644),
        ]
    }

    /// From a local archive, no network: the engine is unpacked into its
    /// folder, the program runnable and the library's link kept.
    #[test]
    fn the_engine_is_unpacked_from_its_archive() {
        let root = scratch("ok");
        let at = root.join("piper.tar.gz");
        let sha = archive(&at, &engine_items());
        let into = root.join("data").join("piper-engine");
        let program = unpack(&at, &sha, &into, CAPS).expect("unpacked");
        assert_eq!(program, into.join("piper").join("piper"));
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&program).unwrap().permissions().mode() & 0o777, 0o755, "the program runs");
        let link = into.join("piper").join("libespeak-ng.so.1");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("libespeak-ng.so.1.52.0.1"));
        assert_eq!(std::fs::read(&link).unwrap(), b"lib", "the link reaches the library beside it");
        assert_eq!(std::fs::read(into.join("piper/espeak-ng-data/phontab")).unwrap(), b"phonemes");
        assert!(!into.with_extension("partial").exists(), "nothing left over");
        // Unpacked again (a newer release), it replaces the old one.
        assert!(unpack(&at, &sha, &into, CAPS).is_ok());
    }

    /// Refused, and nothing left behind, for: an archive that is not the
    /// one pinned, a link leaving the folder (up, or absolute), a name
    /// climbing out or absolute, a hard link, one unpacking past the
    /// caps, and one without the program.
    #[test]
    fn a_hostile_archive_is_refused_and_leaves_nothing() {
        let root = scratch("hostile");
        let into = root.join("data").join("piper-engine");
        let small = Caps { bytes: 1024, entries: 50 };
        static BIG: [u8; 2048] = [7; 2048];
        let with = |extra: Item<'static>| {
            let mut items = engine_items();
            items.push(extra);
            items
        };
        let cases: Vec<(&str, Vec<Item>, Caps, &str)> = vec![
            ("link up", with(Item::Link("piper/libx.so", "../../../../etc/passwd")), CAPS, "outside its folder"),
            ("link absolute", with(Item::Link("piper/libx.so", "/etc/passwd")), CAPS, "outside its folder"),
            ("link into a folder", with(Item::Link("piper/libx.so", "espeak-ng-data/phontab")), CAPS, "outside its folder"),
            ("name up", with(Item::File("piper/../../evil", b"x", 0o644)), CAPS, "not a plain path"),
            ("name absolute", with(Item::File("/tmp/evil", b"x", 0o644)), CAPS, "not a plain path"),
            ("outside piper/", with(Item::File("other/evil", b"x", 0o644)), CAPS, "not under piper/"),
            ("hard link", with(Item::HardLink("piper/h", "/etc/passwd")), CAPS, "does not hold"),
            ("too big", with(Item::File("piper/big.ort", &BIG, 0o644)), small, "more than"),
            ("too many", (0..60).map(|_| Item::Dir("piper/")).collect(), small, "more than 50 entries"),
            ("no program", vec![Item::Dir("piper/"), Item::File("piper/readme", b"x", 0o644)], CAPS, "no piper/piper"),
        ];
        for (what, items, caps, why) in cases {
            let at = root.join("bad.tar.gz");
            let sha = archive(&at, &items);
            let err = unpack(&at, &sha, &into, caps).expect_err(what);
            assert!(err.contains(why), "{what}: {err}");
            assert!(!into.exists() && !into.with_extension("partial").exists(), "{what}: nothing left behind");
            assert!(!root.join("evil").exists() && !Path::new("/tmp/evil").exists(), "{what}: nothing written outside");
        }
        let at = root.join("good.tar.gz");
        archive(&at, &engine_items());
        let err = unpack(&at, &"0".repeat(64), &into, CAPS).expect_err("a checksum not pinned");
        assert!(err.contains("not the one Klipp knows"), "{err}");
        assert!(!into.exists(), "nothing unpacked from it");
    }
}
