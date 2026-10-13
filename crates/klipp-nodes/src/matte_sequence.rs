//! A matte sequence (#239): a matte for every frame of a clip's media,
//! made by a segmentation model (the subject matte pass), kept as a
//! folder of 8-bit greyscale PNGs, one per source frame, and a manifest.
//! An `ExternalMatte` node whose path is such a folder reads the frame
//! for the time it renders, so the matte follows the subject through the
//! shot; the viewer and the export read the same files.
//!
//! A frame is named by its time in the media, as the frame readers name
//! it (`SeekableFrameReader`: the stream's pts in seconds), in whole
//! microseconds: not counted from the first frame, so a stream that
//! starts late, or a variable frame rate, is named as the render asks
//! for it. The render takes the frame nearest the time it renders, within
//! half a frame; further than that is another frame standing in.
//!
//! The values are coverage, 0..255 for 0..1: never colour, never
//! decoded as sRGB.
//!
//! Frame-by-frame segmentation boils at the subject's edge. A pass keeps
//! the model's own mattes in a `raw` subfolder and publishes, for the
//! render, each frame steadied ([`steady_frame`]): a weighted mean of
//! itself and the frames two either side in the same shot, a neighbour
//! weighing less the further its value is from this frame's, so a flicker
//! of a few levels averages away while an edge that really moved stays
//! where it went.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The manifest's file name in a matte sequence's folder.
pub const MANIFEST: &str = "matte-sequence.json";
/// The format this build writes and reads: frames named by their time.
pub const FORMAT: u32 = 2;
/// Frames either side a steadied frame is averaged with.
pub const STEADY_RADIUS: usize = 2;
/// How far apart (0..1) two values are before a neighbour hardly counts.
pub const STEADY_SIGMA: f32 = 0.25;

/// What a matte sequence holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// The media's frame rate (its average): what half a frame is.
    pub fps: f64,
    /// The mattes' size (smaller than the media's; upsampled on use), in
    /// the picture's upright orientation.
    pub width: u32,
    pub height: u32,
    /// The turn (degrees) the frames were given before the model saw
    /// them: the media's, as its picture is shown.
    #[serde(default)]
    pub rotation: i32,
    /// The model that made them, and its file's size: another model, or
    /// another download of it, is another sequence.
    pub model: String,
    #[serde(default)]
    pub model_bytes: u64,
    /// Where the model ran ("cpu"): another provider is another sequence,
    /// never frames of two mixed.
    #[serde(default)]
    pub provider: String,
    /// The pass's own version: a change of method is another sequence.
    pub algorithm: u32,
    /// Whether the frames the render reads are steadied.
    #[serde(default)]
    pub steadied: bool,
}

impl Manifest {
    /// Half a frame, in microseconds: how near a frame's time must be to
    /// the time asked for to be that time's frame.
    pub fn half_frame_us(&self) -> i64 {
        (500_000.0 / self.fps.max(1.0)).round() as i64
    }
}

/// A frame's key: its time, in whole microseconds.
pub fn key_of(secs: f64) -> i64 {
    (secs * 1_000_000.0).round() as i64
}

/// A frame's file in a matte sequence's folder.
pub fn frame_path(dir: &Path, key: i64) -> PathBuf {
    dir.join(format!("t{key}.png"))
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
    serde_json::from_str::<Manifest>(&text).ok().filter(|m| m.format == FORMAT && m.width > 0 && m.height > 0 && m.fps > 0.0)
}

/// The matte keyed `key`, `width` × `height` values 0..255, written whole
/// and moved into place: a reader sees all of it or none.
pub fn put(dir: &Path, manifest: &Manifest, key: i64, alpha: &[u8]) -> Result<(), String> {
    if alpha.len() != (manifest.width * manifest.height) as usize {
        return Err(format!("a {}×{} matte, not {} values", manifest.width, manifest.height, alpha.len()));
    }
    let path = frame_path(dir, key);
    let tmp = path.with_extension("png.partial");
    image::save_buffer_with_format(&tmp, alpha, manifest.width, manifest.height, image::ExtendedColorType::L8, image::ImageFormat::Png)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// The matte keyed `key` as written, when it is there and the size the
/// manifest says.
pub fn get(dir: &Path, manifest: &Manifest, key: i64) -> Option<Vec<u8>> {
    let img = image::open(frame_path(dir, key)).ok()?.to_luma8();
    (img.width() == manifest.width && img.height() == manifest.height).then(|| img.into_raw())
}

/// The keys of the mattes in `dir`, in order.
pub fn frames(dir: &Path) -> Vec<i64> {
    let mut out: Vec<i64> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix('t')?.strip_suffix(".png")?.parse().ok()
        })
        .collect();
    out.sort_unstable();
    out
}

/// One steadied frame: `centre` averaged with its `neighbours` (each with
/// its distance in frames), weighted by distance and by how near each
/// value is to the centre's.
pub fn steady_frame(centre: &[u8], neighbours: &[(usize, &[u8])]) -> Vec<u8> {
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

/// Where a pass keeps the model's own mattes, before steadying: the
/// folder's `raw` subfolder. The render reads the top only.
pub fn raw_dir(dir: &Path) -> PathBuf {
    dir.join("raw")
}

/// The model's own matte keyed `key`, kept apart for steadying.
pub fn put_raw(dir: &Path, manifest: &Manifest, key: i64, alpha: &[u8]) -> Result<(), String> {
    let raw = raw_dir(dir);
    std::fs::create_dir_all(&raw).map_err(|e| format!("{}: {e}", raw.display()))?;
    put(&raw, manifest, key, alpha)
}

/// Publish `keys` for the render: each steadied from the raw mattes of
/// the frames up to two either side (by order, so a variable frame rate
/// counts frames, not time) that `same_shot` says are in its shot. A key
/// with no raw matte is left as it is. The manifest says it is steadied.
pub fn publish(dir: &Path, keys: &[i64], same_shot: &dyn Fn(i64, i64) -> bool) -> Result<(), String> {
    let mut manifest = read_manifest(dir).ok_or_else(|| format!("{} is not a matte sequence", dir.display()))?;
    let raw = raw_dir(dir);
    let all = frames(&raw);
    let mut cache: std::collections::HashMap<i64, Option<Vec<u8>>> = std::collections::HashMap::new();
    let mut out = Vec::new();
    for &k in keys {
        let Ok(at) = all.binary_search(&k) else { continue };
        let mut read = |key: i64| cache.entry(key).or_insert_with(|| get(&raw, &manifest, key)).clone();
        let Some(centre) = read(k) else { continue };
        let lo = at.saturating_sub(STEADY_RADIUS);
        let hi = (at + STEADY_RADIUS).min(all.len() - 1);
        let around: Vec<(usize, Vec<u8>)> = (lo..=hi)
            .filter(|i| *i != at && same_shot(k, all[*i]))
            .filter_map(|i| read(all[i]).map(|n| (i.abs_diff(at), n)))
            .collect();
        let neighbours: Vec<(usize, &[u8])> = around.iter().map(|(d, n)| (*d, n.as_slice())).collect();
        out.push((k, steady_frame(&centre, &neighbours)));
    }
    for (k, m) in out {
        put(dir, &manifest, k, &m)?;
    }
    if !manifest.steadied {
        manifest.steadied = true;
        write_manifest(dir, &manifest)?;
    }
    Ok(())
}

/// The matte for a source time, as the render reads it.
pub struct MatteAt {
    /// Values 0..1, `width` × `height`.
    pub values: Vec<f32>,
    pub width: u32,
    pub height: u32,
    /// The frame read, by key, and whether it is the frame for the time
    /// asked (within half a frame) or another standing in, the nearest.
    pub read: i64,
    pub exact: bool,
}

/// The matte for `source_secs` from the sequence in `dir` whose frames are
/// `keys` (in order): the frame nearest it, and whether that is within
/// half a frame. `None` with no frame at all.
pub fn matte_at(dir: &Path, manifest: &Manifest, keys: &[i64], source_secs: f64) -> Option<MatteAt> {
    let want = key_of(source_secs);
    let at = keys.partition_point(|k| *k < want);
    let near = [at.checked_sub(1), (at < keys.len()).then_some(at)].into_iter().flatten().map(|i| keys[i]).min_by_key(|k| ((k - want).abs(), *k))?;
    let alpha = get(dir, manifest, near)?;
    Some(MatteAt { values: alpha.iter().map(|v| *v as f32 / 255.0).collect(), width: manifest.width, height: manifest.height, read: near, exact: (near - want).abs() <= manifest.half_frame_us() })
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
        Manifest { format: FORMAT, fps: 25.0, width: w, height: h, rotation: 0, model: "test".into(), model_bytes: 0, provider: "cpu".into(), algorithm: 1, steadied: false }
    }

    /// Frame `f` at 25 fps, as a key.
    fn k(f: i64) -> i64 {
        f * 40_000
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

    /// Written and read back by key; the manifest round-trips; a wrong
    /// size is refused; an older format isn't read.
    #[test]
    fn frames_are_kept_and_found() {
        let dir = temp();
        let m = manifest(8, 4);
        create(&dir, &m).unwrap();
        assert!(is_sequence(&dir));
        assert_eq!(read_manifest(&dir), Some(m.clone()));
        put(&dir, &m, k(3), &[200; 32]).unwrap();
        put(&dir, &m, -5, &[10; 32]).unwrap();
        assert!(put(&dir, &m, 6, &[1; 5]).is_err());
        assert_eq!(get(&dir, &m, k(3)), Some(vec![200; 32]));
        assert_eq!(frames(&dir), vec![-5, k(3)]);
        std::fs::write(dir.join(MANIFEST), serde_json::to_string(&Manifest { format: 1, ..m }).unwrap()).unwrap();
        assert!(read_manifest(&dir).is_none(), "frames counted from zero are not read as timed ones");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The frame nearest a time is read; within half a frame it is that
    /// time's frame, further it stands in (and says so); with uneven
    /// frame times (a variable frame rate) the nearest is by time.
    #[test]
    fn a_time_reads_its_frame_or_says_another_stands_in() {
        let dir = temp();
        let m = manifest(4, 2);
        create(&dir, &m).unwrap();
        let keys = vec![400_000, 433_000, 480_000, 800_000];
        for (key, v) in keys.iter().zip([50u8, 100, 150, 250]) {
            put(&dir, &m, *key, &[v; 8]).unwrap();
        }
        let at = |secs: f64| matte_at(&dir, &m, &keys, secs).map(|a| (a.read, a.exact, (a.values[0] * 255.0).round() as u8));
        assert_eq!(at(0.4), Some((400_000, true, 50)));
        assert_eq!(at(0.44), Some((433_000, true, 100)), "the nearer of two uneven frames");
        assert_eq!(at(0.47), Some((480_000, true, 150)));
        assert_eq!(at(0.6), Some((480_000, false, 150)), "no frame within half a frame: another stands in");
        assert_eq!(at(0.0), Some((400_000, false, 50)));
        assert!(matte_at(&dir, &m, &[], 0.0).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Steadying takes the boil out of an edge and keeps a moving edge
    /// where it went: a still disc whose edge jitters by up to 12 levels
    /// a frame jitters far less after; a disc moving 3 pixels a frame
    /// keeps its centre at each frame; a neighbour in another shot is
    /// left out.
    #[test]
    fn steadying_takes_out_the_boil_and_keeps_the_motion() {
        let (w, h) = (48u32, 24u32);
        let m = manifest(w, h);
        let everything = |_: i64, _: i64| true;
        // Still, boiling.
        let dir = temp();
        create(&dir, &m).unwrap();
        let raw: Vec<Vec<u8>> = (0..9).map(|f| disc(w, h, 24.0, 8.0, 12, f)).collect();
        for (f, a) in raw.iter().enumerate() {
            put_raw(&dir, &m, k(f as i64), a).unwrap();
        }
        let boil = |frames: &[Vec<u8>]| -> f32 {
            let mut total = 0.0;
            for pair in frames.windows(2) {
                total += pair[0].iter().zip(&pair[1]).map(|(a, b)| (*a as f32 - *b as f32).abs()).sum::<f32>();
            }
            total / (frames.len() - 1) as f32
        };
        publish(&dir, &(0..9).map(k).collect::<Vec<_>>(), &everything).unwrap();
        assert_eq!(frames(&dir), (0..9).map(k).collect::<Vec<_>>(), "the render's frames, not the raw folder's");
        let steadied: Vec<Vec<u8>> = (0..9).map(|f| get(&dir, &m, k(f)).unwrap()).collect();
        let (before, after) = (boil(&raw[2..7]), boil(&steadied[2..7]));
        assert!(after < before * 0.5, "the edge boils less: {before} before, {after} after");
        assert!(read_manifest(&dir).unwrap().steadied);

        // Moving, clean: each frame's mass stays centred where the disc is.
        let moving = temp();
        create(&moving, &m).unwrap();
        for f in 0..7 {
            put_raw(&moving, &m, k(f), &disc(w, h, 12.0 + 3.0 * f as f32, 6.0, 0, 0)).unwrap();
        }
        publish(&moving, &(0..7).map(k).collect::<Vec<_>>(), &everything).unwrap();
        for f in 1..6 {
            let a = get(&moving, &m, k(f)).unwrap();
            let (mut sx, mut s) = (0.0f32, 0.0f32);
            for (i, v) in a.iter().enumerate() {
                let v = *v as f32;
                sx += v * ((i as u32 % w) as f32 + 0.5);
                s += v;
            }
            let centre = sx / s;
            assert!((centre - (12.0 + 3.0 * f as f32)).abs() < 0.75, "frame {f}: centred at {centre}");
            let mid = ((h / 2) * w + (12 + 3 * f as u32)) as usize;
            assert!(a[mid] > 240, "frame {f}: {}", a[mid]);
        }

        // A cut between frames 3 and 4: frame 3 is steadied from its own
        // shot only, so a matte across the cut doesn't bleed into it.
        let cut = temp();
        create(&cut, &m).unwrap();
        for f in 0..8 {
            put_raw(&cut, &m, k(f), &if f < 4 { vec![0; (w * h) as usize] } else { vec![200; (w * h) as usize] }).unwrap();
        }
        let shot = |a: i64, b: i64| (a < k(4)) == (b < k(4));
        publish(&cut, &(0..8).map(k).collect::<Vec<_>>(), &shot).unwrap();
        assert!(get(&cut, &m, k(3)).unwrap().iter().all(|v| *v == 0) && get(&cut, &m, k(4)).unwrap().iter().all(|v| *v == 200));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(moving);
        let _ = std::fs::remove_dir_all(cut);
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
