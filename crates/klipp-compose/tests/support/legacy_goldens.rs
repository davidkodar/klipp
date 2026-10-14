//! Frames the legacy effects route drew, stored, for the tests that held
//! the plan route to it (phase 2b M2 step 11d). The legacy route is
//! deleted in 11d; before that, each test recorded what it drew, on that
//! route, into `tests/goldens/legacy-<name>.png` (the frames' bytes end to
//! end as one grey image, as `golden_corpus` stores its frames) with
//! `legacy-<name>.json` naming each frame, the head and route that
//! recorded it, and why.
//!
//! A test then compares the plan route's frame with the stored one where
//! it compared it with a live legacy render. `legacy-manifest.json` holds
//! each file's checksum and a version, read only when it matches
//! [`VERSION`] here, so a stored frame can't change unnoticed. Step 11d
//! deleted the legacy route, so the frames can't be recorded again: the
//! recording half below is what wrote them (`KLIPP_RECORD_LEGACY_GOLDENS`),
//! and now refuses.
//!
//! One test's frames can be recorded again from today's route instead,
//! when a reviewed change moves what that route draws on purpose (#2017):
//! `KLIPP_RECORD_CURRENT_GOLDEN` names the test and
//! `KLIPP_RECORD_CURRENT_REASON` says why. Its file is marked as drawn by
//! the plan route, and only its checksum moves in the manifest.
//!
//! The frames are a GPU's floats, and once the legacy route is deleted
//! they can't be recorded again. So each `legacy-<name>.json` names the
//! adapter and driver that drew them, and a test compares bit for bit
//! only on that adapter and driver ([`exact`]). Anywhere else (another
//! GPU, a driver update, no GPU at all) it compares within one display
//! level per channel and says so with an `UNVERIFIED` line:
//! [`matches_f32`], [`matches_f32_within`], [`matches_bytes`].

#![allow(dead_code)]

use std::collections::BTreeMap;

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens");
/// Whether the legacy route that drew the frames is gone from the tree, so
/// nothing can record them again.
const ROUTE_GONE: bool = true;

/// The stored frames' version: a recording writes `VERSION + 1` into the
/// manifest, and the stored frames are read only when the two agree.
pub const VERSION: u64 = 2;

fn manifest() -> serde_json::Value {
    std::fs::read_to_string(format!("{DIR}/legacy-manifest.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| serde_json::json!({ "version": 0, "files": {} }))
}
/// One row of the stored image: 1024 bytes, so PNG's row filters see some
/// of the frames' own structure.
const ROW: u32 = 1024;

/// FNV-1a over `bytes`: a checksum that is the same on every machine and
/// Rust version, unlike `DefaultHasher`.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// A frame's bytes: f32 frames as their little-endian bits, so a stored
/// frame is exactly the one drawn.
pub fn f32_bytes(frame: &[f32]) -> Vec<u8> {
    frame.iter().flat_map(|v| v.to_le_bytes()).collect()
}

pub fn f32_frame(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// One test's stored legacy frames, or, while recording, the frames it
/// is collecting.
pub struct LegacyGoldens {
    name: String,
    stored: BTreeMap<String, Vec<u8>>,
    recording: Option<(String, Vec<(String, Vec<u8>)>)>,
    /// Whether the recording is today's route's (one named test), not the
    /// deleted legacy route's.
    current: bool,
    /// Whether this run's adapter and driver drew the stored frames.
    exact: bool,
}

/// The adapter and driver this process draws on, as stored in a
/// recording's header; `None` without a GPU.
pub fn running_adapter() -> Option<serde_json::Value> {
    let info = klipp_effects::gpu_context::GpuContext::shared()?.adapter_info()?;
    Some(serde_json::json!({ "name": info.name, "driver": info.driver, "driver_info": info.driver_info, "backend": format!("{:?}", info.backend) }))
}

fn describe(adapter: Option<&serde_json::Value>) -> String {
    match adapter {
        Some(a) => format!("{} ({} {}, {})", a["name"].as_str().unwrap_or("?"), a["driver"].as_str().unwrap_or("?"), a["driver_info"].as_str().unwrap_or("?"), a["backend"].as_str().unwrap_or("?")),
        None => "no GPU, or an unrecorded one".to_owned(),
    }
}

impl LegacyGoldens {
    /// The frames `name` stored, read only when the manifest's version is
    /// [`VERSION`] and the file's checksum is the manifest's; recording
    /// instead when `KLIPP_RECORD_LEGACY_GOLDENS` gives a reason.
    pub fn open(name: &str) -> Self {
        let name = name.to_owned();
        if std::env::var("KLIPP_RECORD_CURRENT_GOLDEN").is_ok_and(|n| n == name) {
            let reason = std::env::var("KLIPP_RECORD_CURRENT_REASON").unwrap_or_default();
            assert!(!reason.trim().is_empty(), "say why {name}'s frames are recorded again from today's route");
            return Self { name, stored: BTreeMap::new(), recording: Some((reason, Vec::new())), exact: true, current: true };
        }
        if let Ok(reason) = std::env::var("KLIPP_RECORD_LEGACY_GOLDENS") {
            assert!(!ROUTE_GONE, "the legacy route is gone (M2 step 11d): its stored frames can't be recorded again");
            assert!(!reason.trim().is_empty(), "say why the legacy frames are recorded again");
            return Self { name, stored: BTreeMap::new(), recording: Some((reason, Vec::new())), exact: true, current: false };
        }
        let m = manifest();
        assert_eq!(
            m["version"].as_u64(),
            Some(VERSION),
            "tests/goldens/legacy-manifest.json is at another version than support/legacy_goldens.rs's VERSION: a re-recording must bump VERSION, reviewed like code"
        );
        let png_path = format!("{DIR}/legacy-{name}.png");
        let png = std::fs::read(&png_path).unwrap_or_else(|e| panic!("{png_path}: {e}"));
        let found = format!("{:#018x}", fnv1a(&png));
        assert_eq!(m["files"][&name].as_str(), Some(found.as_str()), "{png_path} is not the file the manifest pins: stored frames change only with a recording");
        let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{DIR}/legacy-{name}.json")).expect("the index")).unwrap();
        let data = image::load_from_memory(&png).expect("the stored frames").to_luma8().into_raw();
        let stored = doc["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let (o, n) = (f["offset"].as_u64().unwrap() as usize, f["len"].as_u64().unwrap() as usize);
                (f["key"].as_str().unwrap().to_owned(), data[o..o + n].to_vec())
            })
            .collect();
        let (recorded, running) = (doc.get("adapter").filter(|a| !a.is_null()).cloned(), running_adapter());
        let exact = recorded.is_some() && recorded == running;
        if !exact {
            eprintln!(
                "UNVERIFIED: bit-exactness for {name}: its legacy frames were drawn on {}, this run draws on {}; compared within one display level",
                describe(recorded.as_ref()),
                describe(running.as_ref())
            );
        }
        Self { name, stored, recording: None, exact, current: false }
    }

    /// Whether this run records rather than checks.
    pub fn recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Store `bytes` under `key` (recording), or give back what was stored
    /// under it.
    pub fn frame(&mut self, key: &str, bytes: impl FnOnce() -> Vec<u8>) -> Vec<u8> {
        match &mut self.recording {
            Some((_, frames)) => {
                let b = bytes();
                assert!(!frames.iter().any(|(k, _)| k == key), "{}: {key} recorded twice", self.name);
                frames.push((key.to_owned(), b.clone()));
                b
            }
            None => self.stored.get(key).cloned().unwrap_or_else(|| panic!("{}: no stored legacy frame {key} (a new case needs recording, with the legacy route)", self.name)),
        }
    }

    /// Write what was recorded, and print the checksum to pin.
    pub fn finish(self) {
        let current = self.current;
        let Some((reason, frames)) = self.recording else { return };
        let mut data = Vec::new();
        let mut index = Vec::new();
        for (key, b) in &frames {
            index.push(serde_json::json!({ "key": key, "offset": data.len(), "len": b.len() }));
            data.extend(b);
        }
        let rows = (data.len() as u32).div_ceil(ROW).max(1);
        data.resize((ROW * rows) as usize, 0);
        let mut png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Best, image::codecs::png::FilterType::Adaptive);
        image::ImageEncoder::write_image(enc, &data, ROW, rows, image::ExtendedColorType::L8).unwrap();
        std::fs::create_dir_all(DIR).unwrap();
        std::fs::write(format!("{DIR}/legacy-{}.png", self.name), &png).unwrap();
        let commit = std::process::Command::new("git").args(["rev-parse", "--short=8", "HEAD"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
        let lines: Vec<String> = index.iter().map(|f| format!("    {f}")).collect();
        let text = format!(
            "{{\n  \"recorded_on\": {},\n  \"route\": {},\n  \"adapter\": {},\n  \"reason\": {},\n  \"frames\": [\n{}\n  ]\n}}\n",
            serde_json::json!(commit),
            serde_json::json!(if current { "plans (today's route)" } else { "legacy (set_legacy_effects)" }),
            running_adapter().unwrap_or(serde_json::Value::Null),
            serde_json::json!(reason),
            lines.join(",\n")
        );
        std::fs::write(format!("{DIR}/legacy-{}.json", self.name), text).unwrap();
        // The manifest: this file's checksum, and the next version, so the
        // stored-frame tests fail until VERSION is bumped to it. Tests
        // record in parallel: one writes the manifest at a time, merging
        // what the others wrote.
        let _guard = MANIFEST.lock().unwrap_or_else(|e| e.into_inner());
        let mut m = manifest();
        // Today's route moves one test's file only: its checksum, read in
        // review, not every stored frame's version.
        if !current {
            m["version"] = serde_json::json!(VERSION + 1);
        }
        m["files"][&self.name] = serde_json::json!(format!("{:#018x}", fnv1a(&png)));
        std::fs::write(format!("{DIR}/legacy-manifest.json"), serde_json::to_string_pretty(&m).unwrap() + "\n").unwrap();
        if current {
            eprintln!("recorded {} frames for {} from today's route", frames.len(), self.name);
        } else {
            eprintln!("recorded {} legacy frames for {}: bump legacy_goldens::VERSION to {}", frames.len(), self.name, VERSION + 1);
        }
    }
}

/// Tests of one binary record in parallel; the manifest is written by one
/// at a time.
static MANIFEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    /// The running test's stored frames and how many it has asked for: a
    /// test's legacy renders happen in a fixed order, so the n-th one is
    /// stored frame n.
    static CURRENT: std::cell::RefCell<Option<(LegacyGoldens, usize)>> = const { std::cell::RefCell::new(None) };
}

/// The running test's stored legacy frames, until dropped: then, if the
/// test is recording and didn't fail, they are written.
pub struct Scope;

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some((g, _)) = CURRENT.with(|c| c.borrow_mut().take()) {
            if !std::thread::panicking() {
                g.finish();
            }
        }
    }
}

/// Open the stored legacy frames of the test `name` for this thread.
pub fn scope(name: &str) -> Scope {
    CURRENT.with(|c| *c.borrow_mut() = Some((LegacyGoldens::open(name), 0)));
    Scope
}

/// The next legacy frame of the running test: drawn by `draw` and stored
/// when recording, else the stored one. Keys are positions: a legacy
/// render added in the middle of a test shifts every later one, so it
/// needs a recording, which only the legacy route can make.
pub fn legacy_bytes(draw: impl FnOnce() -> Vec<u8>) -> Vec<u8> {
    CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        let (g, n) = c.as_mut().expect("legacy_goldens::scope first");
        let key = format!("{n}");
        *n += 1;
        g.frame(&key, draw)
    })
}

/// [`legacy_bytes`] for a linear f32 frame, stored bit for bit.
pub fn legacy_f32(draw: impl FnOnce() -> Vec<f32>) -> Vec<f32> {
    f32_frame(&legacy_bytes(|| f32_bytes(&draw())))
}

/// Whether the running test compares bit for bit: it is recording, or
/// this run's adapter and driver drew its stored frames.
pub fn exact() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_none_or(|(g, _)| g.exact))
}

/// `v` as a display level: the sRGB curve, extended to negative and
/// over-range values by its sign and slope, times 255.
fn level(v: f32) -> f32 {
    let a = v.abs();
    let e = if a <= 0.003_130_8 { 12.92 * a } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v) * 255.0
}

/// The worst difference between two frames in display levels.
pub fn worst_levels(new: &[f32], legacy: &[f32]) -> f32 {
    new.iter().zip(legacy).map(|(a, b)| (level(*a) - level(*b)).abs()).fold(0.0, f32::max)
}

/// A plan-route frame against a stored legacy one: bit for bit when
/// [`exact`], else within one display level per channel.
pub fn matches_f32(new: &[f32], legacy: &[f32]) -> bool {
    if exact() {
        return new.len() == legacy.len() && new.iter().zip(legacy).all(|(a, b)| a.to_bits() == b.to_bits());
    }
    new.len() == legacy.len() && worst_levels(new, legacy) <= 1.0
}

/// [`matches_f32`] for a test that already allows `tol` between the
/// routes on one adapter: within `tol` when [`exact`], else within one
/// display level per channel.
pub fn matches_f32_within(new: &[f32], legacy: &[f32], tol: f32) -> bool {
    if exact() {
        return new.len() == legacy.len() && new.iter().zip(legacy).all(|(a, b)| (a - b).abs() < tol);
    }
    new.len() == legacy.len() && worst_levels(new, legacy) <= 1.0
}

/// [`matches_f32`] for display bytes: equal when [`exact`], else within
/// one level.
pub fn matches_bytes(new: &[u8], legacy: &[u8]) -> bool {
    if exact() {
        return new == legacy;
    }
    new.len() == legacy.len() && new.iter().zip(legacy).all(|(a, b)| a.abs_diff(*b) <= 1)
}

/// The CPU kernels' frame against a stored GPU one, for the cases whose
/// effects the CPU kernels draw (a plan that couldn't apply them): within
/// the bar the effects hold between CPU and GPU everywhere
/// (`cpu_gpu_effect_parity`), one 8-bit sRGB level per colour channel.
pub fn within_cpu_gpu_bar(cpu: &[f32], gpu: &[f32]) -> bool {
    let (a, b) = (klipp_effects::linear_f32_rgba_to_srgb_u8(cpu), klipp_effects::linear_f32_rgba_to_srgb_u8(gpu));
    let worst = a.iter().zip(&b).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (x, y))| x.abs_diff(*y)).max().unwrap_or(0);
    eprintln!("CPU kernels vs the stored GPU frame: worst {worst} levels");
    a.len() == b.len() && worst <= 1
}
