//! An analysis node reads its asset at the frame's source time (M4 step 9,
//! #1467): a trimmed clip's first frame draws as the untrimmed clip's
//! frame at the same source time, and its analysis isn't another frame's.
//! Without its asset, or with a stale one, it draws the frame unchanged.

use std::sync::Arc;

use klipp_analysis::assets::{store, Asset, AssetKey, AssetKind, AssetStore, MemoryStore};
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

const W: u32 = 8;
const H: u32 = 4;
const FPS: u32 = 25;

fn key() -> AssetKey {
    AssetKey { kind: AssetKind::Deflicker, media: "ramp".into(), width: 480, height: 240, fps: (FPS, 1) }
}

/// A flat grey whatever the time; its analysis is the asset's.
struct Grey;

impl FrameSource for Grey {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new([0.4f32, 0.4, 0.4, 1.0].repeat((w * h) as usize))), width: w, height: h, pts: None })
    }
    fn analysis_key(&mut self, _: &Clip, kind: AssetKind) -> Option<AssetKey> {
        (kind == AssetKind::Deflicker).then(key)
    }
}

/// A file whose light pumps hard frame to frame: every frame its own gain.
fn pumping() -> MemoryStore {
    let luma: Vec<f32> = (0..250).map(|i| if i % 2 == 0 { 0.3 } else { 0.6 } + i as f32 * 0.001).collect();
    let mut s = MemoryStore::default();
    store(&mut s, key(), serde_json::to_value(klipp_effects::deflicker::DeflickerData { luma, fps: FPS as f64 }).unwrap()).unwrap();
    s
}

fn tc(f: i64) -> RationalTime {
    RationalTime::from_frames(f, FPS)
}

/// A clip of the file from source frame `from`, 100 frames long,
/// deflickered.
fn sequence(from: i64) -> Sequence {
    let mut c = Clip::media(MediaId::new(), tc(0), tc(100), tc(from), tc(from + 100));
    c.set_effect_list(vec![E::Deflicker { strength: 1.0, window_secs: 0.4, gain: 1.0 }], vec![true]);
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::new(FPS, 1);
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut t = Track::new("V", TrackKind::Video);
    t.clips = vec![c];
    seq.tracks = vec![t];
    seq
}

fn draw(seq: &Sequence, frame: i64, assets: MemoryStore) -> Vec<f32> {
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    r.set_asset_store(Box::new(assets));
    r.render_inspected(seq, tc(frame), W, H, &mut Grey, RenderOptions::default(), Inspect::default()).0
}

#[test]
fn a_trimmed_clip_reads_the_analysis_of_the_frame_it_shows() {
    // Source frame 50 (2 s): the trimmed clip's first frame, the untrimmed
    // clip's fiftieth.
    let trimmed = draw(&sequence(50), 0, pumping());
    let untrimmed = draw(&sequence(0), 50, pumping());
    assert_eq!(trimmed, untrimmed, "the same source frame, the same gain");
    assert_ne!(trimmed, draw(&sequence(0), 0, pumping()), "not the file's first frame's");
}

#[test]
fn without_its_analysis_the_frame_is_unchanged() {
    let plain = draw(&sequence(50), 0, MemoryStore::default());
    assert!(plain.chunks_exact(4).all(|p| (p[0] - 0.4).abs() < 1e-6), "no asset: the frame as it is");
    let mut stale = MemoryStore::default();
    stale.put(Asset { key: key(), algorithm: 99, data: serde_json::json!({ "luma": [0.3, 0.6], "fps": 25.0 }) }).unwrap();
    let drawn = draw(&sequence(50), 0, stale);
    assert!(drawn.chunks_exact(4).all(|p| (p[0] - 0.4).abs() < 1e-6), "a stale asset is never read");
}

/// A store that counts its full reads, its assets shared with the test.
struct Counting {
    inner: Arc<std::sync::Mutex<MemoryStore>>,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl AssetStore for Counting {
    fn get(&self, key: &AssetKey) -> Option<Asset> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.lock().unwrap().get(key)
    }
    fn put(&mut self, asset: Asset) -> Result<(), String> {
        self.inner.lock().unwrap().put(asset)
    }
    fn stamp(&self, key: &AssetKey) -> Option<u64> {
        self.inner.lock().unwrap().stamp(key)
    }
}

/// An asset is read once and kept while its stamp holds: playback costs a
/// look at the store each frame, never a read and a parse of the whole
/// asset. Kept again (a new analysis), it's read again, once.
#[test]
fn an_asset_is_read_once_while_it_holds() {
    let inner = Arc::new(std::sync::Mutex::new(pumping()));
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(false);
    r.set_asset_store(Box::new(Counting { inner: Arc::clone(&inner), reads: Arc::clone(&reads) }));
    let seq = sequence(0);
    for f in 0..12 {
        r.render_inspected(&seq, tc(f), W, H, &mut Grey, RenderOptions::default(), Inspect::default());
    }
    assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 1, "twelve frames, one read");
    let luma: Vec<f32> = vec![0.5; 250];
    store(&mut *inner.lock().unwrap(), key(), serde_json::to_value(klipp_effects::deflicker::DeflickerData { luma, fps: FPS as f64 }).unwrap()).unwrap();
    for f in 12..20 {
        r.render_inspected(&seq, tc(f), W, H, &mut Grey, RenderOptions::default(), Inspect::default());
    }
    assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 2, "kept again: read again, once");
}
