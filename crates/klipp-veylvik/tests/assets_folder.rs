//! A composition reads the files it names from its own folder in the
//! project, beside its file, as standalone Veylvik reads them; one whose
//! project moved (a save somewhere else) reads them there.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use klipp_compose::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::{Clip, CompositionId, Sequence, Track, TrackKind};
use klipp_veylvik::{Composition, Compositions, Time};
use veylvik_document::{nodes, Operation};

/// The project's compositions and folders are the process's: one test at a time.
static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

const W: u32 = 16;
const H: u32 = 16;

struct Black;
impl FrameSource for Black {
    fn frame(&mut self, _: &Clip, _: f64, w: u32, h: u32) -> Option<SourceFrame> {
        Some(SourceFrame { pixels: SourcePixels::Linear(Arc::new(vec![0.0; (w * h * 4) as usize])), width: w, height: h, pts: None })
    }
    fn media_fps(&mut self, _: &Clip) -> Option<f64> {
        Some(24.0)
    }
}

/// A flat sRGB PNG of `rgb` at `path`.
fn png(path: &Path, rgb: [u8; 3]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut e = png::Encoder::new(std::fs::File::create(path).unwrap(), W, H);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    let data: Vec<u8> = (0..W * H).flat_map(|_| rgb).collect();
    e.write_header().unwrap().write_image_data(&data).unwrap();
}

#[test]
fn a_composition_reads_its_assets_from_its_folder_in_the_project() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let id = CompositionId(uuid::Uuid::from_u128(1));
    // A picture of the file beside it, and nothing else.
    let mut c = veylvik_document::blank(W, H, Time { ticks: 24, rate: 1 }).unwrap();
    c.frame_count = 24;
    c.nodes.get_mut(&c.output).unwrap().operation = Operation::Image(nodes::Image { asset: "logo.png".into() });
    let _: &Composition = &c;
    let root = std::env::temp_dir().join(format!("klipp_veylvik_assets_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (first, moved) = (root.join("first/media/veylvik/one"), root.join("moved/media/veylvik/one"));
    png(&first.join("logo.png"), [255, 0, 0]);
    png(&moved.join("logo.png"), [0, 0, 255]);

    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), at(0.0), at(1.0), at(0.0), at(1.0));
    clip.composition = Some(id);
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let middle = ((H / 2 * W + W / 2) * 4) as usize;

    klipp_veylvik::set_project_assets(HashMap::from([(id, first.clone())]));
    klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
    let red = r.render(&seq, at(0.5), W, H, &mut Black, RenderOptions::default());
    assert!(red[middle] > 0.9 && red[middle + 2] < 0.05, "the folder's red logo: {:?}", &red[middle..middle + 4]);

    // Saved somewhere else: the folder there, read there.
    klipp_veylvik::set_project_assets(HashMap::from([(id, moved.clone())]));
    let blue = r.render(&seq, at(0.5), W, H, &mut Black, RenderOptions::default());
    assert!(blue[middle + 2] > 0.9 && blue[middle] < 0.05, "the moved folder's blue logo: {:?}", &blue[middle..middle + 4]);

    klipp_veylvik::set_project_compositions(Compositions::new());
    let _ = std::fs::remove_dir_all(&root);
}

/// A composition from a shared project naming a file outside its folder
/// (an absolute path, a way up, a link leading out) reads none of it: the
/// clip shows its media.
#[test]
fn a_composition_reads_nothing_outside_its_folder() {
    if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
        eprintln!("UNVERIFIED: no GPU");
        return;
    }
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let _ = klipp_veylvik::install();
    let root = std::env::temp_dir().join(format!("klipp_veylvik_outside_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let folder = root.join("project/media/veylvik/one");
    std::fs::create_dir_all(&folder).unwrap();
    // The user's own file, outside the project, red.
    let secret = root.join("secret.png");
    png(&secret, [255, 0, 0]);
    std::os::unix::fs::symlink(&secret, folder.join("link.png")).unwrap();
    let at = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let middle = ((H / 2 * W + W / 2) * 4) as usize;
    for (n, name) in [secret.display().to_string(), "../../../../secret.png".to_string(), "link.png".to_string()].into_iter().enumerate() {
        let id = CompositionId(uuid::Uuid::from_u128(100 + n as u128));
        let mut c = veylvik_document::blank(W, H, Time { ticks: 24, rate: 1 }).unwrap();
        c.frame_count = 24;
        c.nodes.get_mut(&c.output).unwrap().operation = Operation::Image(nodes::Image { asset: name.clone() });
        let mut clip = Clip::media(MediaId(uuid::Uuid::from_u128(9)), at(0.0), at(1.0), at(0.0), at(1.0));
        clip.composition = Some(id);
        let mut seq = Sequence::new_default();
        seq.resolution = klipp_core::Resolution { width: W, height: H };
        let mut track = Track::new("V", TrackKind::Video);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        klipp_veylvik::set_project_assets(HashMap::from([(id, folder.clone())]));
        klipp_veylvik::set_project_compositions([(id, Arc::<[u8]>::from(serde_json::to_vec(&c).unwrap()))].into());
        let shown = r.render(&seq, at(0.5), W, H, &mut Black, RenderOptions::default());
        assert!(shown[middle] < 0.05, "{name}: the file outside was read: {:?}", &shown[middle..middle + 4]);
    }
    klipp_veylvik::set_project_compositions(Compositions::new());
    let _ = std::fs::remove_dir_all(&root);
}
