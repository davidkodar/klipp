//! A matte sequence (#239): a matte for every frame of a clip's media,
//! made by a segmentation model (the subject matte pass), kept as a
//! folder of 8-bit greyscale PNGs, one per source frame, and a manifest.
//! An `ExternalMatte` node whose path is such a folder reads the frame
//! for the time it renders, so the matte follows the subject through the
//! shot; the viewer and the export read the same files.
//!
//! Frame-by-frame segmentation boils at the subject's edge. [`steady`]
//! evens that out after the pass: each frame becomes a weighted mean of
//! itself and its neighbours two either side, a neighbour weighing less
//! the further its value is from this frame's, so a flicker of a few
//! levels averages away while an edge that really moved stays where it
//! went.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The manifest's file name in a matte sequence's folder.
pub const MANIFEST: &str = "matte-sequence.json";
/// The format this build writes and reads.
pub const FORMAT: u32 = 1;
/// Frames either side a steadied frame is averaged with.
pub const STEADY_RADIUS: i64 = 2;
/// How far apart (0..1) two values are before a neighbour hardly counts.
pub const STEADY_SIGMA: f32 = 0.25;

/// What a matte sequence holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// The media's frame rate: source frame `n` is at `n / fps` seconds.
    pub fps: f64,
    /// The mattes' size (smaller than the media's; upsampled on use).
    pub width: u32,
    pub height: u32,
    /// The model that made them, and its file's size: another model, or
    /// another download of it, is another sequence.
    pub model: String,
    #[serde(default)]
    pub model_bytes: u64,
    /// The pass's own version: a change of method is another sequence.
    pub algorithm: u32,
    /// Whether [`steady`] has run over the frames there.
    #[serde(default)]
    pub steadied: bool,
}

/// A frame's file in a matte sequence's folder.
pub fn frame_path(dir: &Path, frame: i64) -> PathBuf {
    dir.join(format!("f{frame:07}.png"))
}

/// Whether `dir` is a matte sequence's folder.
pub fn is_sequence(dir: &Path) -> bool {
    dir.join(MANIFEST).is_file()
}

/// A matte sequence's folder, made (with its manifest) if it isn't there.
pub fn create(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    write_manifest(dir, manifest)
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let json = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{MANIFEST}.partial"));
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(MANIFEST)).map_err(|e| e.to_string())
}

/// The manifest of the sequence in `dir`, when it is one this build reads.
pub fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join(MANIFEST)).ok()?;
    serde_json::from_str::<Manifest>(&text).ok().filter(|m| m.format <= FORMAT && m.width > 0 && m.height > 0 && m.fps > 0.0)
}

/// Frame `frame`'s matte, `width` × `height` values 0..255, written whole
/// and moved into place.
pub fn put(dir: &Path, manifest: &Manifest, frame: i64, alpha: &[u8]) -> Result<(), String> {
    if alpha.len() != (manifest.width * manifest.height) as usize {
        return Err(format!("a {}×{} matte, not {} values", manifest.width, manifest.height, alpha.len()));
    }
    let path = frame_path(dir, frame);
    let tmp = path.with_extension("png.partial");
    image::save_buffer_with_format(&tmp, alpha, manifest.width, manifest.height, image::ExtendedColorType::L8, image::ImageFormat::Png)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Frame `frame`'s matte as written, when it is there and the size the
/// manifest says.
pub fn get(dir: &Path, manifest: &Manifest, frame: i64) -> Option<Vec<u8>> {
    let img = image::open(frame_path(dir, frame)).ok()?.to_luma8();
    (img.width() == manifest.width && img.height() == manifest.height).then(|| img.into_raw())
}

/// The frames from `from` to `to` (inclusive) that have no matte yet.
pub fn missing(dir: &Path, from: i64, to: i64) -> Vec<i64> {
    (from..=to).filter(|f| !frame_path(dir, *f).is_file()).collect()
}

/// The frames with a matte, in order.
pub fn frames(dir: &Path) -> Vec<i64> {
    let mut out: Vec<i64> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix('f')?.strip_suffix(".png")?.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// One steadied frame: `centre` averaged with its `neighbours` (each with
/// its distance in frames), weighted by distance and by how near each
/// value is to the centre's.
pub fn steady_frame(centre: &[u8], neighbours: &[(i64, &[u8])]) -> Vec<u8> {
    let inv = 1.0 / (STEADY_SIGMA * STEADY_SIGMA);
    (0..centre.len())
        .map(|i| {
            let c = centre[i] as f32 / 255.0;
            let (mut sum, mut weight) = (c, 1.0f32);
            for (distance, n) in neighbours {
                let v = n[i] as f32 / 255.0;
                let d = v - c;
                let w = (1.0 / (1.0 + *distance as f32)) * (-d * d * inv).exp();
                sum += v * w;
                weight += w;
            }
            ((sum / weight) * 255.0).round().clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// Steady every frame of the sequence in `dir` from its raw neighbours
/// (read all first, so a frame is never averaged with one already
/// steadied), and say so in its manifest. A sequence already steadied is
/// left as it is.
pub fn steady(dir: &Path) -> Result<(), String> {
    let mut manifest = read_manifest(dir).ok_or_else(|| format!("{} is not a matte sequence", dir.display()))?;
    if manifest.steadied {
        return Ok(());
    }
    let have = frames(dir);
    let raw: std::collections::BTreeMap<i64, Vec<u8>> = have.iter().filter_map(|f| get(dir, &manifest, *f).map(|m| (*f, m))).collect();
    for (&f, centre) in &raw {
        let neighbours: Vec<(i64, &[u8])> = (-STEADY_RADIUS..=STEADY_RADIUS)
            .filter(|k| *k != 0)
            .filter_map(|k| raw.get(&(f + k)).map(|n| (k.abs(), n.as_slice())))
            .collect();
        put(dir, &manifest, f, &steady_frame(centre, &neighbours))?;
    }
    manifest.steadied = true;
    write_manifest(dir, &manifest)
}

/// The matte for a source time, as the render reads it.
pub struct MatteAt {
    /// Values 0..1, `width` × `height`.
    pub values: Vec<f32>,
    pub width: u32,
    pub height: u32,
    /// The frame asked for, and the frame read: another when the one
    /// asked for has no matte yet (the nearest there is).
    pub asked: i64,
    pub read: i64,
}

/// The matte for `source_secs` in the sequence in `dir`: its frame, or
/// the nearest frame with a matte. `None` when there is none at all.
pub fn matte_at(dir: &Path, manifest: &Manifest, source_secs: f64) -> Option<MatteAt> {
    // A thousandth of a frame's grace: a time on a frame's start, a hair
    // short of it in floating point, is that frame.
    let asked = (source_secs * manifest.fps + 1e-3).floor() as i64;
    let read = if frame_path(dir, asked).is_file() {
        asked
    } else {
        frames(dir).into_iter().min_by_key(|f| ((f - asked).abs(), *f))?
    };
    let alpha = get(dir, manifest, read)?;
    Some(MatteAt { values: alpha.iter().map(|v| *v as f32 / 255.0).collect(), width: manifest.width, height: manifest.height, asked, read })
}

/// `src` (`sw` × `sh`) resized to `w` × `h` bilinearly, pixel centres
/// aligned: a soft matte stays soft.
pub fn upsample(src: &[f32], sw: u32, sh: u32, w: u32, h: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity((w * h) as usize);
    let (kx, ky) = (sw as f32 / w as f32, sh as f32 / h as f32);
    for y in 0..h {
        let fy = ((y as f32 + 0.5) * ky - 0.5).clamp(0.0, (sh - 1) as f32);
        let (y0, ty) = (fy.floor() as u32, fy - fy.floor());
        let y1 = (y0 + 1).min(sh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * kx - 0.5).clamp(0.0, (sw - 1) as f32);
            let (x0, tx) = (fx.floor() as u32, fx - fx.floor());
            let x1 = (x0 + 1).min(sw - 1);
            let at = |xx: u32, yy: u32| src[(yy * sw + xx) as usize];
            let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
            let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
            out.push(top + (bot - top) * ty);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-matte-seq-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn manifest(w: u32, h: u32) -> Manifest {
        Manifest { format: FORMAT, fps: 25.0, width: w, height: h, model: "test".into(), model_bytes: 0, algorithm: 1, steadied: false }
    }

    /// A disc of radius `r` at `cx` across a `w` × `h` matte, edge `noise`
    /// levels jittered by `seed`.
    fn disc(w: u32, h: u32, cx: f32, r: f32, noise: u8, seed: u32) -> Vec<u8> {
        (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                let d = ((x - cx).powi(2) + (y - h as f32 / 2.0).powi(2)).sqrt();
                let base = (((r - d) * 64.0).clamp(0.0, 255.0)) as i32;
                let j = ((i.wrapping_mul(2654435761) ^ seed.wrapping_mul(40503)) % (2 * noise as u32 + 1)) as i32 - noise as i32;
                if base > 0 && base < 255 || (d - r).abs() < 2.0 { (base + j).clamp(0, 255) as u8 } else { base as u8 }
            })
            .collect()
    }

    /// Written and read back by frame; the missing frames listed; the
    /// manifest round-trips; a wrong size is refused.
    #[test]
    fn frames_are_kept_and_found() {
        let dir = temp();
        let m = manifest(8, 4);
        create(&dir, &m).unwrap();
        assert!(is_sequence(&dir));
        assert_eq!(read_manifest(&dir), Some(m.clone()));
        put(&dir, &m, 3, &[200; 32]).unwrap();
        put(&dir, &m, 5, &[10; 32]).unwrap();
        assert!(put(&dir, &m, 6, &[1; 5]).is_err());
        assert_eq!(get(&dir, &m, 3), Some(vec![200; 32]));
        assert_eq!(frames(&dir), vec![3, 5]);
        assert_eq!(missing(&dir, 2, 6), vec![2, 4, 6]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The frame for a time is read; with none there, the nearest (and
    /// the earlier of two as near) and the frame asked for are both said.
    #[test]
    fn a_time_reads_its_frame_or_says_it_took_another() {
        let dir = temp();
        let m = manifest(4, 2);
        create(&dir, &m).unwrap();
        for (f, v) in [(10, 50u8), (12, 150), (20, 250)] {
            put(&dir, &m, f, &[v; 8]).unwrap();
        }
        let at = |secs: f64| matte_at(&dir, &m, secs).map(|a| (a.asked, a.read, (a.values[0] * 255.0).round() as u8));
        assert_eq!(at(10.0 / 25.0), Some((10, 10, 50)));
        assert_eq!(at(12.5 / 25.0), Some((12, 12, 150)), "within the frame");
        assert_eq!(at(11.0 / 25.0), Some((11, 10, 50)), "between two as near: the earlier");
        assert_eq!(at(17.0 / 25.0), Some((17, 20, 250)));
        assert_eq!(at(0.0), Some((0, 10, 50)));
        let empty = temp();
        create(&empty, &m).unwrap();
        assert!(matte_at(&empty, &m, 0.0).is_none());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(empty);
    }

    /// Steadying takes the boil out of an edge and keeps a moving edge
    /// where it went: a still disc whose edge jitters by up to 12 levels
    /// a frame jitters far less after; a disc moving 3 pixels a frame
    /// keeps its centre at each frame. Run once only.
    #[test]
    fn steadying_takes_out_the_boil_and_keeps_the_motion() {
        let (w, h) = (48u32, 24u32);
        // Still, boiling.
        let dir = temp();
        let m = manifest(w, h);
        create(&dir, &m).unwrap();
        let raw: Vec<Vec<u8>> = (0..9).map(|f| disc(w, h, 24.0, 8.0, 12, f)).collect();
        for (f, a) in raw.iter().enumerate() {
            put(&dir, &m, f as i64, a).unwrap();
        }
        let boil = |frames: &[Vec<u8>]| -> f32 {
            let mut total = 0.0;
            for pair in frames.windows(2) {
                total += pair[0].iter().zip(&pair[1]).map(|(a, b)| (*a as f32 - *b as f32).abs()).sum::<f32>();
            }
            total / (frames.len() - 1) as f32
        };
        steady(&dir).unwrap();
        let steadied: Vec<Vec<u8>> = (0..9).map(|f| get(&dir, &m, f).unwrap()).collect();
        let (before, after) = (boil(&raw[2..7]), boil(&steadied[2..7]));
        assert!(after < before * 0.5, "the edge boils less: {before} before, {after} after");
        assert!(read_manifest(&dir).unwrap().steadied);
        // Run again: left as it is.
        steady(&dir).unwrap();
        assert_eq!((0..9).map(|f| get(&dir, &m, f).unwrap()).collect::<Vec<_>>(), steadied);

        // Moving, clean: each frame's mass stays centred where the disc is.
        let moving = temp();
        create(&moving, &m).unwrap();
        for f in 0..7 {
            put(&moving, &m, f, &disc(w, h, 12.0 + 3.0 * f as f32, 6.0, 0, 0)).unwrap();
        }
        steady(&moving).unwrap();
        for f in 1..6 {
            let a = get(&moving, &m, f).unwrap();
            let (mut sx, mut s) = (0.0f32, 0.0f32);
            for (i, v) in a.iter().enumerate() {
                let v = *v as f32;
                sx += v * ((i as u32 % w) as f32 + 0.5);
                s += v;
            }
            let centre = sx / s;
            assert!((centre - (12.0 + 3.0 * f as f32)).abs() < 0.75, "frame {f}: centred at {centre}");
            // And still solid inside: no ghost of the frames either side
            // dims it.
            let mid = ((h / 2) * w + (12 + 3 * f as u32)) as usize;
            assert!(a[mid] > 240, "frame {f}: {}", a[mid]);
        }
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(moving);
    }

    /// Upsampling keeps a constant constant and a ramp a ramp, the
    /// corners where they were.
    #[test]
    fn upsampling_is_bilinear_and_centred() {
        let src = [0.0, 1.0, 0.0, 1.0];
        let up = upsample(&src, 2, 2, 8, 2);
        assert_eq!(up[0], 0.0);
        assert_eq!(up[7], 1.0);
        assert!(up.windows(2).take(7).all(|w| w[1] >= w[0]));
        assert!(upsample(&[0.4; 6], 3, 2, 17, 9).iter().all(|v| (v - 0.4).abs() < 1e-6));
    }
}
