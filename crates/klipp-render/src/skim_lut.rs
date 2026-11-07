//! The picture an HDR clip's skim stand-in carries: Klipp's own HDR to SDR
//! conversion, baked into a 3D LUT that ffmpeg applies while it writes the
//! skim. The stand-in was tone-mapped by ffmpeg's own curve, so playing
//! HDR footage from it showed a brighter, paler picture than the export
//! and the parked frame (100-nit grey at 198 where the export has 139).
//!
//! The LUT is not a second copy of the conversion: it is rendered. A
//! lattice of the transfer's code values goes through the renderer as a
//! scene-referred clip's frame, as the export draws an ungraded clip, and
//! comes out encoded as the still path encodes. So it stays what the
//! export draws whatever the working space, and changes with it.

use std::sync::Arc;

use klipp_color::hdr::{HdrTransfer, SceneCodes};
use klipp_compose::{FrameSource, Inspect, RenderOptions, Renderer, SourceFrame, SourcePixels};
use klipp_core::{ColorPrimaries, Fps, MediaId, RationalTime, Resolution, WorkingColorspace};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

/// Points along each axis: 65, as grading LUTs are, fine enough between
/// points on a perceptual (PQ or HLG) input that the interpolation is
/// within an 8-bit skim's step.
pub const SIZE: usize = 65;

/// A baked LUT: `SIZE³` display-encoded R'G'B' values, red fastest, then
/// green, then blue, as a `.cube` file lists them.
pub struct SkimLut {
    pub size: usize,
    pub values: Vec<[f32; 3]>,
}

/// The lattice as a scene-referred frame: one pixel a point, red along x
/// within each block of `size`, green across the blocks, blue down.
struct Lattice {
    size: usize,
    transfer: HdrTransfer,
    to_working: [[f32; 3]; 3],
}

impl Lattice {
    fn code(&self, i: usize) -> u16 {
        ((i as f64 / (self.size - 1) as f64) * 65_535.0).round() as u16
    }
}

impl FrameSource for Lattice {
    fn frame(&mut self, _clip: &Clip, _secs: f64, width: u32, height: u32) -> Option<SourceFrame> {
        let n = self.size;
        if width as usize != n * n || height as usize != n {
            return None;
        }
        let mut codes = Vec::with_capacity(n * n * n);
        for b in 0..n {
            for g in 0..n {
                for r in 0..n {
                    codes.push(klipp_color::hdr::pack_rgb16(self.code(r), self.code(g), self.code(b)));
                }
            }
        }
        let pixels = SourcePixels::SceneReferred { codes: Arc::new(SceneCodes::Deep16(codes)), transfer: self.transfer, source_to_working: self.to_working };
        Some(SourceFrame { pixels, width, height, pts: None })
    }
}

/// The skim LUT for BT.2020 sources in `transfer`, drawn in `working`.
pub fn bake(transfer: HdrTransfer, working: WorkingColorspace) -> Result<SkimLut, String> {
    bake_sized(transfer, working, SIZE)
}

fn bake_sized(transfer: HdrTransfer, working: WorkingColorspace, size: usize) -> Result<SkimLut, String> {
    let (w, h) = ((size * size) as u32, size as u32);
    let fps = Fps::new(24, 1);
    let at = |frames: i64| RationalTime::from_frames(frames, 24);
    let media = MediaId::new();
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips = vec![Clip { kind: ClipKind::Media { media_id: media }, source_in: at(0), source_out: at(1), ..Clip::gap(at(0), at(1)) }];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: w, height: h };
    seq.tracks = vec![track];

    let mut source = Lattice { size, transfer, to_working: klipp_color::camera_log::primaries_to_working_matrix(ColorPrimaries::Bt2020, working) };
    // As the export renderer is set up, without the global LUT: the skim
    // is one clip's picture, and the global LUT is the timeline's.
    let mut renderer = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    renderer.set_working_colorspace(working);
    renderer.set_lut_tetrahedral(true);
    let (linear, found) = renderer.render_inspected(&seq, at(0), w, h, &mut source, RenderOptions::default(), Inspect::default());
    if let Some(why) = crate::export_frames::refused(&found.declines) {
        return Err(why);
    }
    if linear.len() != (w * h * 4) as usize {
        return Err(format!("the lattice rendered {} values, not {}", linear.len(), w * h * 4));
    }
    // Encoded as a still is (`still::still_from_linear`), kept in float.
    let display = klipp_color::display::ExportDisplay::new(klipp_core::ToneMapOp::None, working, klipp_core::OdtChoice::MatchViewer);
    let values = linear
        .chunks_exact(4)
        .map(|p| {
            let e = display.encode([p[0], p[1], p[2]]);
            [e[0].clamp(0.0, 1.0), e[1].clamp(0.0, 1.0), e[2].clamp(0.0, 1.0)]
        })
        .collect();
    Ok(SkimLut { size, values })
}

impl SkimLut {
    /// The LUT as a `.cube` file's text.
    pub fn to_cube(&self) -> String {
        let mut s = format!("TITLE \"Klipp skim\"\nLUT_3D_SIZE {}\n", self.size);
        for v in &self.values {
            s.push_str(&format!("{:.6} {:.6} {:.6}\n", v[0], v[1], v[2]));
        }
        s
    }

    /// A short name for this LUT's content: a skim made with another LUT
    /// (another working space, a changed conversion) is another file.
    pub fn content_key(&self) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&self.to_cube(), &mut h);
        format!("{:08x}", std::hash::Hasher::finish(&h) as u32)
    }

    /// The value at lattice point (`r`, `g`, `b`).
    pub fn at(&self, r: usize, g: usize, b: usize) -> [f32; 3] {
        self.values[r + g * self.size + b * self.size * self.size]
    }
}

/// A baked LUT on disk, ready for a skim transcode: its `.cube` file and
/// the key a skim made with it is stored under.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub cube: std::path::PathBuf,
    pub key: String,
}

/// The skim LUT for `transfer` in `working`, written into `dir` as a
/// `.cube` file once: baked once a process for each pair, the file
/// named by its content so another conversion never reuses it.
pub fn prepared(transfer: HdrTransfer, working: WorkingColorspace, dir: &std::path::Path) -> Result<Prepared, String> {
    static MADE: std::sync::Mutex<Vec<((HdrTransfer, WorkingColorspace, std::path::PathBuf), Prepared)>> = std::sync::Mutex::new(Vec::new());
    let id = (transfer, working, dir.to_path_buf());
    if let Some((_, p)) = MADE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(k, _)| *k == id) {
        if p.cube.exists() {
            return Ok(p.clone());
        }
    }
    let lut = bake(transfer, working)?;
    let key = lut.content_key();
    let name = match transfer {
        HdrTransfer::Pq => "pq",
        HdrTransfer::Hlg => "hlg",
        HdrTransfer::Log(_) => return Err("a camera log clip's skim is not tone-mapped from HDR".into()),
    };
    let cube = dir.join(format!("klipp-skim-{name}-{key}.cube"));
    if !cube.exists() {
        std::fs::create_dir_all(dir).map_err(|e| format!("the skim folder {dir:?}: {e}"))?;
        // Written whole and moved into place, so a transcode never reads half.
        let partial = dir.join(format!(".klipp-skim-{name}-{key}-{}.cube", std::process::id()));
        std::fs::write(&partial, lut.to_cube()).map_err(|e| format!("writing {partial:?}: {e}"))?;
        std::fs::rename(&partial, &cube).map_err(|e| format!("placing {cube:?}: {e}"))?;
    }
    let p = Prepared { cube, key };
    MADE.lock().unwrap_or_else(|e| e.into_inner()).push((id, p.clone()));
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The baked LUT is what the export draws: a lattice point equals the
    /// same code rendered alone as a clip's frame and encoded as a still,
    /// and 100-nit grey (PQ code 0.508) is the 139 of 255 measured in the
    /// export of a PQ test clip, where ffmpeg's tone map gave the skim 198.
    /// Black stays black and the grey axis only rises.
    #[test]
    fn the_lut_is_the_exports_picture() {
        let lut = bake_sized(HdrTransfer::Pq, WorkingColorspace::Rec709Linear, 17).expect("baked");
        assert_eq!(lut.values.len(), 17 * 17 * 17);
        assert!(lut.at(0, 0, 0).iter().all(|v| *v < 0.01), "black {:?}", lut.at(0, 0, 0));
        // Point (8, 4, 12) is code (0.5, 0.25, 0.75): the same as a render
        // of that code alone, so the lattice is laid out as a .cube reads.
        let alone = render_one(HdrTransfer::Pq, [0.5, 0.25, 0.75]);
        let point = lut.at(8, 4, 12);
        for c in 0..3 {
            assert!((alone[c] - point[c]).abs() <= 1.0 / 255.0, "lattice {point:?} against a render of the same code {alone:?}");
        }
        let grey = render_one(HdrTransfer::Pq, [klipp_color::hdr::pq_encode_nits(100.0); 3]);
        assert!((grey[1] * 255.0 - 139.0).abs() <= 2.0, "100-nit grey at {} of 255", grey[1] * 255.0);
        for i in 1..17 {
            assert!(lut.at(i, i, i)[1] >= lut.at(i - 1, i - 1, i - 1)[1], "grey rises at {i}");
        }
    }

    /// One code rendered alone, through the same path, encoded as a still.
    fn render_one(transfer: HdrTransfer, code: [f32; 3]) -> [f32; 3] {
        bake_sized_with(transfer, WorkingColorspace::Rec709Linear, code)
    }

    fn bake_sized_with(transfer: HdrTransfer, working: WorkingColorspace, code: [f32; 3]) -> [f32; 3] {
        struct One(SceneCodes, HdrTransfer, [[f32; 3]; 3]);
        impl FrameSource for One {
            fn frame(&mut self, _c: &Clip, _s: f64, w: u32, h: u32) -> Option<SourceFrame> {
                (w == 1 && h == 1).then(|| SourceFrame { pixels: SourcePixels::SceneReferred { codes: Arc::new(self.0.clone()), transfer: self.1, source_to_working: self.2 }, width: 1, height: 1, pts: None })
            }
        }
        let q = |v: f32| (v * 65_535.0).round() as u16;
        let mut src = One(SceneCodes::Deep16(vec![klipp_color::hdr::pack_rgb16(q(code[0]), q(code[1]), q(code[2]))]), transfer, klipp_color::camera_log::primaries_to_working_matrix(ColorPrimaries::Bt2020, working));
        let fps = Fps::new(24, 1);
        let at = |f: i64| RationalTime::from_frames(f, 24);
        let mut track = Track::new("V1", TrackKind::Video);
        track.clips = vec![Clip { kind: ClipKind::Media { media_id: MediaId::new() }, source_in: at(0), source_out: at(1), ..Clip::gap(at(0), at(1)) }];
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width: 1, height: 1 };
        seq.tracks = vec![track];
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_working_colorspace(working);
        let (linear, _) = r.render_inspected(&seq, at(0), 1, 1, &mut src, RenderOptions::default(), Inspect::default());
        let still = crate::still::still_from_linear(&linear, 1, 1, working).unwrap();
        [still.rgba[0] as f32 / 255.0, still.rgba[1] as f32 / 255.0, still.rgba[2] as f32 / 255.0]
    }
}
