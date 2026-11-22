//! The project's Veylvik compositions (see `klipp_timeline::veylvik`):
//! each in `media/veylvik/<id>/composition.veylvik`, the file exactly as
//! Veylvik writes it, so standalone Veylvik opens it too.
//!
//! The project holds them in memory as the bytes of that file; a save
//! writes those its clips name, an open reads them back. Klipp does not
//! read inside one here: that is Veylvik's, when the clip is rendered or
//! opened in the tab.
//!
//! The composition's folder is also its assets folder: the images, fonts
//! and models it names by paths relative to it, as standalone Veylvik
//! reads them beside its file. They stay on disk, in the folder of the
//! project they were opened from; a save somewhere else, an autosave,
//! Collect and a copied clip's own composition take them along.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use klipp_timeline::{CompositionId, Sequence};

/// The compositions, by id: each file's bytes.
pub type Compositions = BTreeMap<CompositionId, Arc<[u8]>>;

/// The file's name in its folder.
pub const FILE_NAME: &str = "composition.veylvik";

/// The largest composition read: Veylvik's own limit for a document.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The folder of composition `id` in the project at `project_dir`: its
/// file, and anything Veylvik keeps beside it.
pub fn composition_dir(project_dir: &Path, id: CompositionId) -> PathBuf {
    // The id is a UUID, written by its own formatter: one plain path
    // component, whatever the project file said.
    project_dir.join("media").join("veylvik").join(id.0.hyphenated().to_string())
}

/// Where composition `id`'s file is.
pub fn composition_file(project_dir: &Path, id: CompositionId) -> PathBuf {
    composition_dir(project_dir, id).join(FILE_NAME)
}

/// Folders deep that a composition's assets are taken along.
const ASSET_DEPTH: usize = 8;

/// Where each composition's assets are read from on a save: the folder of
/// the project they are in now, and the composition each copied clip's
/// own was forked from.
#[derive(Clone, Copy, Default)]
pub struct AssetsFrom<'a> {
    /// The project the compositions were opened from or last saved to:
    /// none for one never saved, whose compositions have no assets yet.
    pub project_dir: Option<&'a Path>,
    /// (from, to): a copied clip's composition `to`, made of `from`.
    pub forks: &'a [(CompositionId, CompositionId)],
}

/// Write into `project_dir` every composition the sequences name, each
/// one whose file already holds the same bytes left as it is, with its
/// assets from where `from` says they are. One named but not held (a clip
/// pasted from another project) is left out and said.
pub fn save_compositions(project_dir: &Path, sequences: &[Sequence], compositions: &Compositions, from: AssetsFrom<'_>) -> Result<Vec<String>, std::io::Error> {
    let mut missing = Vec::new();
    for id in klipp_timeline::compositions_used(sequences) {
        let Some(bytes) = compositions.get(&id) else {
            missing.push(format!("A clip's Veylvik composition ({id}) is not in this project; the clip shows its media"));
            continue;
        };
        let here = composition_dir(project_dir, id);
        if let Some(src) = from.project_dir {
            // A copied clip's composition has the assets of the one it was
            // made of; any other, its own.
            let of = from.forks.iter().find(|(_, to)| *to == id).map_or(id, |(f, _)| *f);
            let there = composition_dir(src, of);
            if there != here && there.is_dir() {
                missing.extend(copy_assets(&there, &here, ASSET_DEPTH)?);
            }
        }
        let file = composition_file(project_dir, id);
        if std::fs::read(&file).is_ok_and(|on_disk| on_disk == bytes.as_ref()) {
            continue;
        }
        std::fs::create_dir_all(&here)?;
        crate::path_util::atomic_write(&file, bytes)?;
    }
    Ok(missing)
}

/// Composition `from`'s assets in `home` into composition `to`'s folder
/// there, for a copy made in the session (a clip's version duplicated):
/// what can't be copied is said, nothing copied when it has none.
pub fn copy_composition_assets(home: &Path, from: CompositionId, to: CompositionId) -> Result<Vec<String>, std::io::Error> {
    let there = composition_dir(home, from);
    if !there.is_dir() {
        return Ok(Vec::new());
    }
    copy_assets(&there, &composition_dir(home, to), ASSET_DEPTH)
}

/// Every file in `from` but the composition's own into `to`, as plain
/// files and folders: one already there with the same length and time is
/// left as it is. A link is not followed or copied, and is said: what it
/// points at is not the project's.
fn copy_assets(from: &Path, to: &Path, depth: usize) -> Result<Vec<String>, std::io::Error> {
    let mut said = Vec::new();
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if depth == ASSET_DEPTH && name == FILE_NAME {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(&name));
        let meta = std::fs::symlink_metadata(&src)?;
        if meta.is_dir() {
            if depth == 0 {
                said.push(format!("A Veylvik composition's assets go {ASSET_DEPTH} folders deep at most; {} was left out", src.display()));
                continue;
            }
            std::fs::create_dir_all(&dst)?;
            said.extend(copy_assets(&src, &dst, depth - 1)?);
        } else if meta.is_file() {
            let same = std::fs::symlink_metadata(&dst).is_ok_and(|d| d.is_file() && d.len() == meta.len() && d.modified().ok() == meta.modified().ok());
            if !same {
                std::fs::create_dir_all(to)?;
                std::fs::copy(&src, &dst)?;
                // Its time as the original's, so the next save sees it the same.
                if let Ok(t) = meta.modified() {
                    let _ = std::fs::File::options().write(true).open(&dst).and_then(|f| f.set_modified(t));
                }
            }
        } else {
            said.push(format!("A Veylvik composition's asset {} is a link, not a file, and was left out", src.display()));
        }
    }
    Ok(said)
}

/// Read from `project_dir` every composition the sequences name. One
/// missing, unreadable, not a plain file or larger than [`MAX_BYTES`] is
/// left out and said: its clip shows its media, and the project opens.
pub fn load_compositions(project_dir: &Path, sequences: &[Sequence]) -> (Compositions, Vec<String>) {
    let mut compositions = Compositions::new();
    let mut warnings = Vec::new();
    for id in klipp_timeline::compositions_used(sequences) {
        match read_one(&composition_file(project_dir, id)) {
            Ok(bytes) => {
                compositions.insert(id, bytes);
            }
            Err(why) => warnings.push(format!("A clip's Veylvik composition was not read ({why}); the clip shows its media")),
        }
    }
    (compositions, warnings)
}

fn read_one(file: &Path) -> Result<Arc<[u8]>, String> {
    // Not followed through a link: the folder is the project's own.
    let meta = std::fs::symlink_metadata(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a plain file", file.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("{} is larger than {} MB", file.display(), MAX_BYTES >> 20));
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(file)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{} is larger than {} MB", file.display(), MAX_BYTES >> 20));
    }
    Ok(bytes.into())
}

/// Give each clip sharing a composition with another one of its own, a
/// copy of the one it shared, and say how many were given. A save does
/// this first, so no two clips in a file name one composition.
/// Its forks, (from, to), for the save to take each one's assets along.
pub fn fork_shared(sequences: &mut [Sequence], compositions: &mut Compositions) -> Vec<(CompositionId, CompositionId)> {
    let forks = klipp_timeline::fork_shared_compositions(sequences);
    for (from, to) in &forks {
        if let Some(bytes) = compositions.get(from).cloned() {
            compositions.insert(*to, bytes);
        }
    }
    forks
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;
    use klipp_timeline::{Track, TrackKind};

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp_compositions_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sequence_with(compositions: &[Option<CompositionId>]) -> Sequence {
        let mut seq = Sequence::new_default();
        let mut track = Track::new("V9", TrackKind::Video);
        for (i, c) in compositions.iter().enumerate() {
            let at = |secs: f64| RationalTime::from_secs_f64(secs, 25);
            let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(i as f64), at(i as f64 + 1.0), at(0.0), at(1.0));
            clip.composition = *c;
            track.clips.push(clip);
        }
        seq.tracks.push(track);
        seq
    }

    fn bytes(text: &str) -> Arc<[u8]> {
        text.as_bytes().into()
    }

    /// A save writes each composition its clips name where Veylvik would
    /// look, an open reads the same bytes back, and one no clip names is
    /// neither written nor read.
    #[test]
    fn compositions_go_with_the_project_and_come_back_the_same() {
        let dir = scratch("round_trip");
        let (a, unused) = (CompositionId::new(), CompositionId::new());
        let seqs = vec![sequence_with(&[Some(a), None])];
        let held: Compositions = [(a, bytes("{\"a\":1}\n")), (unused, bytes("{}"))].into();
        assert!(save_compositions(&dir, &seqs, &held, AssetsFrom::default()).unwrap().is_empty());
        assert_eq!(std::fs::read(dir.join("media/veylvik").join(a.to_string()).join("composition.veylvik")).unwrap(), b"{\"a\":1}\n");
        assert!(!composition_dir(&dir, unused).exists());
        let (back, warnings) = load_compositions(&dir, &seqs);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back.len(), 1);
        assert_eq!(back[&a], held[&a]);
    }

    /// A file already holding the same bytes is not written again: its
    /// time stays, as a copy's does.
    #[test]
    fn an_unchanged_composition_is_not_rewritten() {
        let dir = scratch("unchanged");
        let a = CompositionId::new();
        let seqs = vec![sequence_with(&[Some(a)])];
        let held: Compositions = [(a, bytes("same"))].into();
        save_compositions(&dir, &seqs, &held, AssetsFrom::default()).unwrap();
        let file = composition_file(&dir, a);
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(old).unwrap();
        save_compositions(&dir, &seqs, &held, AssetsFrom::default()).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), old);
        let changed: Compositions = [(a, bytes("changed"))].into();
        save_compositions(&dir, &seqs, &changed, AssetsFrom::default()).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"changed");
    }

    /// A project whose composition is missing, too large or a link opens
    /// with the clip showing its media, and says why.
    #[test]
    fn a_composition_that_cannot_be_read_is_said_and_the_project_opens() {
        let dir = scratch("hostile");
        let (missing, big, linked, fine) = (CompositionId::new(), CompositionId::new(), CompositionId::new(), CompositionId::new());
        let seqs = vec![sequence_with(&[Some(missing), Some(big), Some(linked), Some(fine)])];
        std::fs::create_dir_all(composition_dir(&dir, big)).unwrap();
        std::fs::File::create(composition_file(&dir, big)).unwrap().set_len(MAX_BYTES + 1).unwrap();
        let outside = dir.join("outside.veylvik");
        std::fs::write(&outside, b"not the project's").unwrap();
        std::fs::create_dir_all(composition_dir(&dir, linked)).unwrap();
        std::os::unix::fs::symlink(&outside, composition_file(&dir, linked)).unwrap();
        std::fs::create_dir_all(composition_dir(&dir, fine)).unwrap();
        std::fs::write(composition_file(&dir, fine), b"ok").unwrap();
        let (back, warnings) = load_compositions(&dir, &seqs);
        assert_eq!(back.keys().copied().collect::<Vec<_>>(), vec![fine]);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("larger than 16 MB")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("not a plain file")), "{warnings:?}");
    }

    /// A copied clip, in the same sequence or a duplicated one, is given a
    /// composition of its own with the same contents; the first keeps its
    /// id, and a second fork changes nothing.
    #[test]
    fn a_copied_clip_gets_a_composition_of_its_own() {
        let a = CompositionId::new();
        let mut seqs = vec![sequence_with(&[Some(a), Some(a), None]), sequence_with(&[Some(a)])];
        let mut held: Compositions = [(a, bytes("comp"))].into();
        assert_eq!(fork_shared(&mut seqs, &mut held).len(), 2);
        let ids: Vec<_> = seqs.iter().flat_map(|s| s.tracks.last().unwrap().clips.iter()).filter_map(|c| c.composition).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], a);
        assert_eq!(ids.iter().collect::<std::collections::HashSet<_>>().len(), 3);
        assert!(ids.iter().all(|id| held[id].as_ref() == b"comp"));
        assert!(fork_shared(&mut seqs, &mut held).is_empty());
    }

    /// A clip naming a composition the project does not hold saves with
    /// a warning and writes nothing for it.
    #[test]
    fn a_composition_not_held_is_said_on_save() {
        let dir = scratch("not_held");
        let a = CompositionId::new();
        let warnings = save_compositions(&dir, &[sequence_with(&[Some(a)])], &Compositions::new(), AssetsFrom::default()).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(!composition_dir(&dir, a).exists());
    }

    /// The clip's link is saved in the project file and read back; a
    /// clip without one writes no key, so a project without compositions
    /// saves as before.
    #[test]
    fn the_clips_link_goes_through_the_project_file() {
        let dir = scratch("project_file");
        let a = CompositionId::new();
        let seqs = vec![sequence_with(&[Some(a), None])];
        crate::save_project(&dir, &seqs, &klipp_media::MediaPool::new()).unwrap();
        let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
        assert_eq!(text.matches("\"composition\"").count(), 1, "one clip names one");
        let back = crate::load_project(&dir).unwrap().0;
        let clips: Vec<_> = back.iter().flat_map(|s| &s.tracks).flat_map(|t| &t.clips).filter(|c| c.media_id().is_some()).map(|c| c.composition).collect();
        assert_eq!(clips, vec![Some(a), None]);
    }

    /// Collect writes the compositions the project holds now, saved or
    /// not, into the copy, and an open of the copy reads them.
    #[test]
    fn collect_carries_the_compositions() {
        let source = scratch("collect_src");
        let dest = std::env::temp_dir().join(format!("klipp_compositions_collect_dest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let a = CompositionId::new();
        let seqs = vec![sequence_with(&[Some(a)])];
        let held: Compositions = [(a, bytes("not saved yet"))].into();
        let options = crate::consolidate::ConsolidateOptions { dest_dir: dest.clone(), include_proxies: false, include_skims: false, mode: Default::default(), looks: Vec::new(), fonts: Vec::new(), compositions: held.clone() };
        crate::consolidate::consolidate_project(&source, &klipp_media::MediaPool::new(), &seqs, &crate::ColorSettings::default(), options, None).unwrap();
        let (back, warnings) = load_compositions(&dest, &crate::load_project(&dest).unwrap().0);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(back, held);
        let _ = std::fs::remove_dir_all(&dest);
    }

    fn asset(dir: &Path, id: CompositionId, rel: &str, text: &str) {
        let file = composition_dir(dir, id).join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    /// A save to another folder takes each composition's assets along, its
    /// subfolders too; a copied clip's composition gets the assets of the
    /// one it was made of; a link in the folder is left out and said.
    #[test]
    fn a_compositions_assets_go_where_it_is_saved() {
        let (old, new) = (scratch("assets_old"), scratch("assets_new"));
        let a = CompositionId::new();
        let mut seqs = vec![sequence_with(&[Some(a), Some(a)])];
        let mut held: Compositions = [(a, bytes("comp"))].into();
        save_compositions(&old, &seqs, &held, AssetsFrom::default()).unwrap();
        asset(&old, a, "logo.png", "png");
        asset(&old, a, "fonts/title.ttf", "ttf");
        let outside = old.join("secret.txt");
        std::fs::write(&outside, "not the composition's").unwrap();
        std::os::unix::fs::symlink(&outside, composition_dir(&old, a).join("link.png")).unwrap();
        let forks = fork_shared(&mut seqs, &mut held);
        assert_eq!(forks.len(), 1);
        let copy = forks[0].1;
        let said = save_compositions(&new, &seqs, &held, AssetsFrom { project_dir: Some(&old), forks: &forks }).unwrap();
        for id in [a, copy] {
            assert_eq!(std::fs::read_to_string(composition_dir(&new, id).join("logo.png")).unwrap(), "png", "{id}");
            assert_eq!(std::fs::read_to_string(composition_dir(&new, id).join("fonts/title.ttf")).unwrap(), "ttf", "{id}");
            assert_eq!(std::fs::read(composition_file(&new, id)).unwrap(), b"comp");
            assert!(std::fs::symlink_metadata(composition_dir(&new, id).join("link.png")).is_err(), "no link copied");
        }
        assert_eq!(said.iter().filter(|w| w.contains("is a link")).count(), 2, "{said:?}");
        // Saved in place: nothing copied onto itself, the asset as it was.
        let t = std::fs::metadata(composition_dir(&old, a).join("logo.png")).unwrap().modified().unwrap();
        save_compositions(&old, &seqs[..], &held, AssetsFrom { project_dir: Some(&old), forks: &[] }).unwrap();
        assert_eq!(std::fs::metadata(composition_dir(&old, a).join("logo.png")).unwrap().modified().unwrap(), t);
    }
}
