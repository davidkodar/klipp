//! The sound a clip's Veylvik composition makes of its own (its sound
//! files, music and effects; never the clip's footage, which Klipp plays
//! as it always does), for Edit and the export to hear with the clip.
//!
//! Each is mixed by Veylvik's own mix, the one its editor and export play,
//! from the composition's start to past the end the clip plays, and kept
//! in Klipp's cache folder as a raw stereo f32 file at 48 kHz (what
//! playback's PCM feeder reads) and a WAV of the same (what the export's
//! decoder reads), named by what made it. Made off the interface thread;
//! a composition with no sound of its own gives none.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use klipp_core::ClipId;

/// The rate the files are at: the PCM feeder's.
pub const RATE: u32 = 48_000;

/// A clip whose composition may have sound: its composition's bytes, its
/// assets' folder, and how far into the composition the clip plays.
pub struct Want<'a> {
    pub clip: ClipId,
    pub bytes: &'a Arc<[u8]>,
    pub assets: PathBuf,
    pub until_secs: f64,
}

/// The composition sounds made so far, and the ones being made.
pub struct Sounds {
    folder: PathBuf,
    /// By key: the file, or none for a composition with no sound.
    made: HashMap<u64, Option<PathBuf>>,
    making: HashSet<u64>,
    tx: Sender<(u64, Option<PathBuf>)>,
    rx: Receiver<(u64, Option<PathBuf>)>,
    /// What was last handed out.
    given: Vec<(ClipId, PathBuf)>,
}

impl Sounds {
    /// Sounds kept in `folder`.
    pub fn new(folder: PathBuf) -> Self {
        let (tx, rx) = channel();
        Self { folder, made: HashMap::new(), making: HashSet::new(), tx, rx, given: Vec::new() }
    }

    /// The clips' sounds for `wants`, the ones not made yet set going: the
    /// list when it differs from the one last given, else `None`.
    pub fn update<'a>(&mut self, wants: impl IntoIterator<Item = Want<'a>>) -> Option<Vec<(ClipId, PathBuf)>> {
        while let Ok((key, file)) = self.rx.try_recv() {
            self.making.remove(&key);
            self.made.insert(key, file);
        }
        let mut now = Vec::new();
        for want in wants {
            let key = key_of(want.bytes, &want.assets, want.until_secs);
            match self.made.get(&key) {
                Some(Some(file)) => now.push((want.clip, file.clone())),
                Some(None) => {}
                None => {
                    // Made before, by this session or another: kept.
                    let file = self.folder.join(format!("{key:016x}.pcm"));
                    if file.exists() && file.with_extension("wav").exists() {
                        self.made.insert(key, Some(file.clone()));
                        now.push((want.clip, file));
                    } else if self.making.insert(key) {
                        let (bytes, assets, until, tx) = (Arc::clone(want.bytes), want.assets, want.until_secs, self.tx.clone());
                        std::thread::Builder::new()
                            .name("veylvik composition sound".into())
                            .spawn(move || {
                                let made = match make(&bytes, &assets, until, &file) {
                                    Ok(true) => Some(file),
                                    Ok(false) => None,
                                    Err(e) => {
                                        tracing::warn!("a Veylvik composition's sound could not be made: {e}");
                                        None
                                    }
                                };
                                let _ = tx.send((key, made));
                            })
                            .ok();
                    }
                }
            }
        }
        now.sort_by(|a, b| (a.0.0, &a.1).cmp(&(b.0.0, &b.1)));
        (now != self.given).then(|| {
            self.given = now.clone();
            now
        })
    }

    /// The clips' sounds as last handed out.
    pub fn given(&self) -> &[(ClipId, PathBuf)] {
        &self.given
    }

    /// Whether a sound is being made, so the caller asks again soon.
    pub fn busy(&self) -> bool {
        !self.making.is_empty()
    }
}

/// The file's name: the composition's bytes, where its assets are, and
/// how much of it is mixed.
fn key_of(bytes: &[u8], assets: &Path, until_secs: f64) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ("veylvik composition sound", 1u32, RATE).hash(&mut h);
    bytes.hash(&mut h);
    assets.hash(&mut h);
    (until_secs.max(0.0).ceil() as u64).hash(&mut h);
    h.finish()
}

/// Mix the composition in `bytes` from its start to `until_secs` (and a
/// second over) into `file` and its WAV twin; `false` when it has no sound.
pub fn make(bytes: &[u8], assets: &Path, until_secs: f64, file: &Path) -> Result<bool, String> {
    let doc: veylvik_document::Composition = serde_json::from_slice(bytes).map_err(|e| format!("the composition can't be read: {e}"))?;
    let end = until_secs.max(0.0) + 1.0;
    let frames = (end * f64::from(RATE)).ceil() as usize;
    let mut samples = vec![0.0f32; frames * 2];
    let mut any = false;
    let mut from = 0.0;
    while from < end {
        let to = (from + veylvik_audio::MAX_SECONDS).min(end);
        if let Some(pcm) = veylvik_audio::mix(&doc, assets, [from, to], RATE, || true).map_err(|e| e.to_string())? {
            any = true;
            let first = (pcm.start * f64::from(RATE)).round() as usize;
            for (k, pair) in pcm.samples.as_chunks::<2>().0.iter().enumerate() {
                if let Some(at) = samples.get_mut((first + k) * 2..(first + k) * 2 + 2) {
                    at.copy_from_slice(pair);
                }
            }
        }
        from = to;
    }
    if !any {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let raw: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    write_whole(&file.with_extension("wav"), &[wav_header(frames), raw.clone()].concat())?;
    // The raw file last: its being there says both are.
    write_whole(file, &raw)?;
    Ok(true)
}

/// Written beside, then renamed: whole or not there.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// A WAV header for `frames` of stereo 32-bit float at [`RATE`].
fn wav_header(frames: usize) -> Vec<u8> {
    let data = (frames * 8) as u32;
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(36 + data).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&RATE.to_le_bytes());
    h.extend_from_slice(&(RATE * 8).to_le_bytes());
    h.extend_from_slice(&8u16.to_le_bytes());
    h.extend_from_slice(&32u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data.to_le_bytes());
    h
}
