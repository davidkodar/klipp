//! How far ahead of its cut a clip's stream is opened (#2108): early
//! enough for its own seek. A seek lands on the keyframe at or before the
//! time and decodes every frame from there, so a shot that starts four
//! seconds into a 60p file with keyframes four seconds apart decodes 240
//! frames before its first one shows. The keyframe is read from the file's
//! index, off the engine's thread, once per file and time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Open this long ahead while a file's keyframe is not known yet, and at
/// least this long for any.
pub(crate) const DEFAULT_LEAD_SECS: f64 = 2.0;
/// Open no further ahead than this, however long the seek: a stream held
/// open that long ties up a decoder.
pub(crate) const MAX_LEAD_SECS: f64 = 8.0;
/// Source frames a second a seek decodes on its way to its time, taken
/// low: several 4K streams seek at once on one decoder engine.
const SEEK_DECODE_FPS: f64 = 90.0;
/// Time for the open itself and the first frame's upload.
const MARGIN_SECS: f64 = 0.5;

/// The lead for a seek that decodes from `keyframe` to `secs` in a file
/// of `fps` frames a second.
pub(crate) fn lead_for(secs: f64, keyframe: f64, fps: f64) -> f64 {
    let frames = (secs - keyframe).max(0.0) * fps.max(1.0);
    (MARGIN_SECS + frames / SEEK_DECODE_FPS).clamp(DEFAULT_LEAD_SECS, MAX_LEAD_SECS)
}

/// The time of the video keyframe at or before `secs` in `path`, from its
/// index: a seek no later than `secs`, and the first video packet after it.
pub(crate) fn keyframe_before(path: &Path, secs: f64) -> Option<f64> {
    let mut input = klipp_media::open_input(path).ok()?;
    let (index, time_base) = {
        let s = input.streams().best(ffmpeg_next::media::Type::Video)?;
        (s.index(), s.time_base())
    };
    let ts = (secs.max(0.0) * f64::from(ffmpeg_next::ffi::AV_TIME_BASE)) as i64;
    input.seek(ts, ..ts).ok()?;
    for (stream, packet) in input.packets() {
        if stream.index() == index {
            return Some(packet.pts().or(packet.dts())? as f64 * f64::from(time_base));
        }
    }
    None
}

/// The keyframe before each clip's in point, as far as it is known, and
/// the reads still on their way.
pub(crate) struct SeekLeads {
    known: HashMap<(PathBuf, i64), f64>,
    asked: std::collections::HashSet<(PathBuf, i64)>,
    tx: Sender<((PathBuf, i64), Option<f64>)>,
    rx: Receiver<((PathBuf, i64), Option<f64>)>,
}

impl Default for SeekLeads {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { known: HashMap::new(), asked: Default::default(), tx, rx }
    }
}

impl SeekLeads {
    /// How long before its cut to open a stream at `secs` into `path` (of
    /// `fps` frames a second): from its keyframe when that is known, else
    /// [`DEFAULT_LEAD_SECS`] while the keyframe is read on a thread of its
    /// own.
    pub(crate) fn lead(&mut self, path: &Path, secs: f64, fps: f64) -> f64 {
        while let Ok((key, keyframe)) = self.rx.try_recv() {
            // A file whose index can't be read seeks from where it is.
            self.known.insert(key, keyframe.unwrap_or(secs));
        }
        let key = (path.to_path_buf(), (secs * 1000.0).round() as i64);
        if let Some(keyframe) = self.known.get(&key) {
            return lead_for(secs, *keyframe, fps);
        }
        if self.asked.insert(key.clone()) {
            let tx = self.tx.clone();
            let _ = std::thread::Builder::new().name("seek lead".into()).spawn(move || {
                let keyframe = keyframe_before(&key.0, secs);
                let _ = tx.send((key, keyframe));
            });
        }
        DEFAULT_LEAD_SECS
    }

    /// Known keyframes, for a test.
    #[cfg(test)]
    pub(crate) fn settle(&mut self, path: &Path, secs: f64, fps: f64) -> f64 {
        let start = std::time::Instant::now();
        loop {
            let lead = self.lead(path, secs, fps);
            if self.known.contains_key(&(path.to_path_buf(), (secs * 1000.0).round() as i64)) || start.elapsed().as_secs() > 10 {
                return lead;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seek's lead grows with the frames it decodes from its keyframe,
    /// never under the default nor over the most.
    #[test]
    fn the_lead_follows_the_frames_a_seek_decodes() {
        assert_eq!(lead_for(1.0, 1.0, 30.0), DEFAULT_LEAD_SECS, "on a keyframe");
        // Four seconds at 60p: 240 frames, 2.67 s, and the margin.
        assert!((lead_for(4.0, 0.0, 60.0) - (0.5 + 240.0 / 90.0)).abs() < 1e-9);
        assert_eq!(lead_for(30.0, 0.0, 60.0), MAX_LEAD_SECS, "bounded");
    }

    /// The keyframe before a time, read from a file with keyframes every
    /// two seconds; the lead is the default until it is known.
    #[test]
    fn the_keyframe_before_a_time_is_read_from_the_index() {
        let path = std::env::temp_dir().join(format!("klipp-seek-lead-{}.mp4", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=320x180:r=30:d=7", "-c:v", "libx264", "-g", "60", "-keyint_min", "60", "-sc_threshold", "0", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let k = keyframe_before(&path, 5.5).expect("a keyframe");
        assert!((k - 4.0).abs() < 0.05, "the keyframe before 5.5 s is at 4 s: {k}");
        let mut leads = SeekLeads::default();
        assert_eq!(leads.lead(&path, 5.5, 30.0), DEFAULT_LEAD_SECS, "not known yet");
        let lead = leads.settle(&path, 5.5, 30.0);
        assert!((lead - lead_for(5.5, 4.0, 30.0)).abs() < 0.01, "{lead}");
        let _ = std::fs::remove_file(&path);
    }
}
