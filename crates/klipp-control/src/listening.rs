//! Listening for a request: music and sound effect previews heard, their
//! level and their energy over time (#826).

#[allow(unused_imports)]
use super::*;

/// What one listen to a track's preview found.
pub(crate) struct PreviewListen {
    pub(crate) bpm: f32,
    pub(crate) beat_confidence: f32,
    pub(crate) loudness_db: f32,
    /// The level over eight equal parts of the track, dB.
    pub(crate) energy_db: Vec<f32>,
    pub(crate) shape: &'static str,
}

impl PreviewListen {
    pub(crate) fn describe(&self) -> String {
        let beat = if self.beat_confidence < 0.3 { "no clear beat".to_string() } else { format!("{:.0} BPM", self.bpm) };
        let curve: Vec<String> = self.energy_db.iter().map(|d| format!("{d:.0}")).collect();
        format!("{beat}, {:.0} dB, {} (dB by eighths: {})", self.loudness_db, self.shape, curve.join(" "))
    }
}

/// Hear a preview URL straight off the library through ffmpeg: nothing
/// is imported or kept.
pub(crate) fn listen_to_preview(url: &str) -> Result<PreviewListen, String> {
    const SR: u32 = 22_050;
    // Only a library's own https stream goes to ffmpeg, never a file, a
    // pipe or another protocol a result could name (#767 review).
    if !url.starts_with("https://") {
        return Err("the preview is not an https address".into());
    }
    let pcm = klipp_analysis::audio_sync::decode_mono_pcm(std::path::Path::new("ffmpeg"), std::path::Path::new(url), SR, 0.0, 420.0)
        .map_err(|_| "the preview could not be read".to_string())?;
    if pcm.len() < SR as usize * 2 {
        return Err("the preview is too short to judge".into());
    }
    let beats = klipp_analysis::beats::detect_from_pcm(&pcm, SR);
    let (energy_db, shape) = energy_shape(&pcm);
    Ok(PreviewListen { bpm: beats.bpm, beat_confidence: beats.confidence, loudness_db: rms_db(&pcm), energy_db, shape })
}

/// How long the previews heard together may take.
const LISTEN_SECS: u64 = 60;

/// Listen to the previews given, each on a thread of its own, all of
/// them together within a minute (a long ambience's preview streams for
/// most of one, #2359): a stalled stream is reported as
/// not heard rather than holding the reply (#767 review), and a listen
/// still running is left to end on ffmpeg's read timeout. `None` where
/// no preview was asked for.
pub(crate) fn hear_previews<T: Send + 'static>(urls: Vec<Option<String>>, hear: fn(&str) -> Result<T, String>) -> Vec<Option<Result<T, String>>> {
    let mut asked = 0;
    let receivers: Vec<_> = urls
        .into_iter()
        .map(|url| {
            // At most five heard, in the order the results came.
            let url = url.filter(|u| !u.is_empty() && asked < 5)?;
            asked += 1;
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(hear(&url));
            });
            Some(rx)
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(LISTEN_SECS);
    receivers
        .into_iter()
        .map(|r| {
            r.map(|r| match r.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
                Ok(heard) => heard,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!("the preview did not come within {LISTEN_SECS} s; a long one may still be added with add_music, which waits for it")),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the listen failed".into()),
            })
        })
        .collect()
}

/// A sound effect's preview heard: how long it is, when it is loudest,
/// and how loud (#795).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SfxListen {
    pub(crate) secs: f32,
    /// When the loudest tenth of a second is, from the start.
    pub(crate) peak_secs: f32,
    pub(crate) loudness_db: f32,
    pub(crate) peak_db: f32,
}

impl SfxListen {
    pub(crate) fn from_pcm(pcm: &[f32], sr: u32) -> SfxListen {
        let window = (sr as usize / 10).max(1);
        let (peak_at, _) = pcm.chunks(window).enumerate().map(|(i, w)| (i, rms_db(w))).fold((0, f32::MIN), |b, c| if c.1 > b.1 { c } else { b });
        let peak = pcm.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        SfxListen {
            secs: pcm.len() as f32 / sr as f32,
            peak_secs: (peak_at * window + window / 2).min(pcm.len()) as f32 / sr as f32,
            loudness_db: rms_db(pcm),
            peak_db: 20.0 * peak.max(1e-6).log10(),
        }
    }

    pub(crate) fn describe(&self) -> String {
        format!("{:.2} s, loudest at {:.2} s, {:.0} dB (peak {:.0} dB)", self.secs, self.peak_secs, self.loudness_db, self.peak_db)
    }
}

pub(crate) fn listen_to_sfx(url: &str) -> Result<SfxListen, String> {
    const SR: u32 = 22_050;
    // Only a library's own https stream goes to ffmpeg (#767 review).
    if !url.starts_with("https://") {
        return Err("the preview is not an https address".into());
    }
    let pcm = klipp_analysis::audio_sync::decode_mono_pcm(std::path::Path::new("ffmpeg"), std::path::Path::new(url), SR, 0.0, 120.0)
        .map_err(|_| "the preview could not be read".to_string())?;
    if pcm.is_empty() {
        return Err("the preview has no sound".into());
    }
    Ok(SfxListen::from_pcm(&pcm, SR))
}

pub(crate) fn rms_db(pcm: &[f32]) -> f32 {
    let ms = pcm.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / pcm.len().max(1) as f64;
    (10.0 * ms.max(1e-12).log10()) as f32
}

/// The level over eight equal parts of a track and a word for its course:
/// whether it builds, falls away, swells in the middle or holds steady.
pub(crate) fn energy_shape(pcm: &[f32]) -> (Vec<f32>, &'static str) {
    let n = 8;
    let part = (pcm.len() / n).max(1);
    let db: Vec<f32> = pcm.chunks(part).take(n).map(rms_db).collect();
    if db.len() < n {
        return (db, "steady");
    }
    let mean = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
    let (start, middle, end) = (mean(&db[..2]), mean(&db[3..5]), mean(&db[6..]));
    let spread = db.iter().cloned().fold(f32::MIN, f32::max) - db.iter().cloned().fold(f32::MAX, f32::min);
    let shape = if spread < 4.0 {
        "steady"
    } else if middle > start + 3.0 && middle > end + 3.0 {
        "swells in the middle"
    } else if end > start + 3.0 {
        "builds"
    } else if start > end + 3.0 {
        "falls away"
    } else {
        "varies"
    };
    (db, shape)
}
