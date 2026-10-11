//! Shared frame routes for the real-project pixel check and generated corpus.
//! Reference updates, file copying and fixture generation belong to the tools.
use crate::snapshot::SequenceSnapshot;
use crate::ExportFrameSource;
use klipp_core::{ExportSettings, Timecode, WorkingColorspace};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Whether an RGBA frame is one colour throughout (alpha aside).
fn is_flat(px: &[u8]) -> bool {
    let first = &px[..3.min(px.len())];
    px.chunks_exact(4).all(|p| &p[..3] == first)
}

/// What a project rendered: frames written and how many were flat, its
/// media offline and other files missing (a LUT, a title image or texture),
/// whether a grade ran on the GPU for its `-gpu` frames, and which way each
/// of its stack and export frames went.
#[derive(Default)]
pub struct Rendered {
    pub frames: usize,
    pub flat: usize,
    pub offline: usize,
    pub missing: usize,
    pub grade_on_gpu: bool,
    /// Frames the GPU stack drew (`-stack`), and those it declined.
    pub stack: usize,
    pub stack_declined: usize,
    /// Clips whose effects ran as nodes of the grade's plan, read from
    /// their lifted picture, and those whose picture could not give them
    /// (the list stood in), over every render of the project.
    pub plan_clips: u32,
    pub plan_fallbacks: u32,
    /// Clip effect runs the CPU kernels drew in the GPU renders (#1274):
    /// zero unless an effect has no GPU kernel or a plan failed.
    pub gpu_cpu_kernels: u32,
    /// Export frames converted on the GPU (`-export-yuv`), and those
    /// rendered and converted on the CPU (`-export`), as the export job
    /// chooses between them.
    pub export_gpu: usize,
    pub export_cpu: usize,
    /// Export frames whose picture didn't fill the output even at the
    /// export size chosen for it, so went the letterbox way (on the CPU).
    pub export_letterboxed: usize,
    /// The nodes that didn't draw what they show, one line per decline
    /// per frame ([`decline_line`]), as the CPU and GPU renders found them.
    pub declines: Vec<String>,
}

/// A frame's decline as `declines.txt` holds it: the frame (`frame`, its
/// sequence and render: `0/12-gpu`), the clip and node, the outcome and the
/// reason in plain words. Never its detail (`why`), which can name the
/// scratch copy's path.
pub fn decline_line(frame: &str, d: &klipp_nodes::Decline) -> String {
    let clip = d.clip.map_or_else(|| "-".to_string(), |c| c.0.to_string());
    format!(
        "{frame} clip {clip} node {}: {} ({})",
        d.node,
        d.plain(),
        d.outcome.label()
    )
}

impl Rendered {
    pub fn covered(&self) -> bool {
        self.offline == 0 && self.missing == 0 && self.flat < self.frames
    }
    /// Add another sequence's frames to this project's counts.
    pub fn merge(&mut self, other: Self) {
        self.frames += other.frames;
        self.flat += other.flat;
        self.offline += other.offline;
        self.missing += other.missing;
        self.grade_on_gpu |= other.grade_on_gpu;
        self.stack += other.stack;
        self.stack_declined += other.stack_declined;
        self.plan_clips += other.plan_clips;
        self.plan_fallbacks += other.plan_fallbacks;
        self.gpu_cpu_kernels += other.gpu_cpu_kernels;
        self.export_gpu += other.export_gpu;
        self.export_cpu += other.export_cpu;
        self.export_letterboxed += other.export_letterboxed;
        self.declines.extend(other.declines);
    }
}

pub fn settings() -> ExportSettings {
    ExportSettings {
        output_path: PathBuf::from("/dev/null"),
        ..ExportSettings::default()
    }
}

fn display(linear: &[f32], ws: WorkingColorspace) -> Vec<u8> {
    klipp_color::display::display_transform_ws_to(
        linear,
        klipp_color::display::ToneMapOp::None,
        ws,
        klipp_core::SdrTransfer::Srgb,
    )
}

/// The renderer as the export sets it up (`export_frames::export_renderer`),
/// on the CPU or with the GPU.
pub fn default_renderer(
    snapshot: &SequenceSnapshot,
    gpu: bool,
) -> Result<klipp_compose::Renderer, String> {
    let mut r = crate::export_frames::export_renderer(snapshot, "pixel check");
    r.set_gpu_enabled(gpu);
    Ok(r)
}

/// A renderer configured for one route; tools can supply isolated plug-in
/// hosts and analysis stores without changing how frames are drawn.
pub type RendererFactory<'a> =
    dyn Fn(&SequenceSnapshot, bool) -> Result<klipp_compose::Renderer, String> + 'a;

/// The check's render size and selected timeline frames.
pub struct Options {
    pub width: u32,
    /// None keeps the historical five-frame sampling used by pixel_check.
    pub frames: Option<Vec<u64>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 320,
            frames: None,
        }
    }
}

/// A timeline frame at the snapshot's full rational rate. Treating the
/// numerator alone as fps would sample frame 60 at 29.97 fps at 0.002 s.
fn frame_time(snapshot: &SequenceSnapshot, frame: u64) -> Result<Timecode, String> {
    if snapshot.fps_num == 0 || snapshot.fps_den == 0 {
        return Err("pixel-check frame rate must be positive".into());
    }
    let value = i64::try_from(frame)
        .ok()
        .and_then(|frame| frame.checked_mul(i64::from(snapshot.fps_den)))
        .ok_or("pixel-check frame time exceeds its integer range")?;
    Ok(Timecode {
        value,
        rate: snapshot.fps_num,
    })
}

/// Draw one snapshot through CPU, GPU, viewer stack and the export's frame
/// maker. The project loader and reference storage remain the caller's.
pub fn render_sequence(
    snapshot: &SequenceSnapshot,
    seq_dir: &Path,
    prefix: &str,
    probe: &Path,
    options: &Options,
    factory: &RendererFactory<'_>,
) -> Result<Rendered, String> {
    let width = options.width;
    if !(2..=4096).contains(&width) || !width.is_multiple_of(2) {
        return Err("pixel-check width must be even and between 2 and 4096".into());
    }
    let (first, last) = (snapshot.start_frame(), snapshot.end_frame());
    if last <= first {
        return Ok(Rendered::default());
    }
    let span = last - 1 - first;
    let frames = match &options.frames {
        Some(frames) => {
            if frames.is_empty()
                || frames.iter().any(|&f| f < first || f >= last)
                || frames
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != frames.len()
            {
                return Err(format!(
                    "{prefix}: explicit frames must be unique and within {first}..{last}"
                ));
            }
            frames.clone()
        }
        None => [0, span / 3, span / 2, span * 2 / 3, span]
            .iter()
            .map(|f| first + f)
            .collect(),
    };
    let h =
        ((width as u64 * snapshot.height as u64 / snapshot.width.max(1) as u64) as u32).max(2) & !1;
    std::fs::create_dir_all(seq_dir).map_err(|e| e.to_string())?;
    let mut done = Rendered::default();
    for (gpu, name) in [(false, "cpu"), (true, "gpu")] {
        let mut r = factory(snapshot, gpu)?;
        let mut source = ExportFrameSource::new(
            &snapshot.video.media,
            snapshot.fps,
            snapshot.working_colorspace,
        );
        for &f in &frames {
            let time = frame_time(snapshot, f)?;
            let (linear, found) = r.render_inspected(
                &snapshot.video.sequence,
                time,
                width,
                h,
                &mut source,
                klipp_compose::RenderOptions::default(),
                klipp_compose::Inspect::default(),
            );
            done.plan_clips += found.stats.plan_clips();
            done.plan_fallbacks += found.stats.effects_fallback_lists;
            done.declines.extend(
                found
                    .declines
                    .iter()
                    .map(|d| decline_line(&format!("{prefix}/{f}-{name}"), d)),
            );
            if gpu {
                done.gpu_cpu_kernels += found.stats.effects_on_cpu_kernels;
            }
            // As the export does after each frame: a decoder that
            // failed fails the project rather than being recorded,
            // and the streams the frame did not use are closed.
            source
                .end_frame()
                .map_err(|e| format!("frame {f} ({name}): {e}"))?;
            let px = display(&linear, snapshot.working_colorspace);
            image::save_buffer(
                seq_dir.join(format!("{f}-{name}.png")),
                &px,
                width,
                h,
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| e.to_string())?;
            done.frames += 1;
            done.flat += usize::from(is_flat(&px));
        }
        // A `-gpu` frame drawn without a GPU would make a CPU
        // reference pass for a GPU one. The effects pipeline and
        // the grade's executor open the same shared device; the
        // executor opens it only when there is a grade to run.
        if gpu && r.gpu_pipeline().is_none() {
            return Err("no GPU for the -gpu frames".into());
        }
        done.grade_on_gpu |= gpu && r.executor().has_gpu();
    }
    stack_frames(snapshot, &frames, width, h, seq_dir, &mut done, factory)?;
    export_frames(
        snapshot, &frames, width, h, seq_dir, probe, &mut done, factory,
    )?;
    Ok(done)
}

/// The viewer's GPU stack: each frame's display bytes, as the viewer draws
/// them while playing, or the stack declining the frame (`None`), which is
/// counted and leaves no `-stack` file.
fn stack_frames(
    snapshot: &SequenceSnapshot,
    frames: &[u64],
    width: u32,
    h: u32,
    seq_dir: &Path,
    done: &mut Rendered,
    factory: &RendererFactory<'_>,
) -> Result<(), String> {
    let mut r = factory(snapshot, true)?;
    let mut source = ExportFrameSource::new(
        &snapshot.video.media,
        snapshot.fps,
        snapshot.working_colorspace,
    );
    for &f in frames {
        let time = frame_time(snapshot, f)?;
        let stack = r.render_display_u8_on_gpu(
            &snapshot.video.sequence,
            time,
            width,
            h,
            &mut source,
            klipp_compose::RenderOptions::default(),
        );
        source
            .end_frame()
            .map_err(|e| format!("frame {f} (stack): {e}"))?;
        match stack {
            Some((px, stats)) => {
                done.plan_clips += stats.plan_clips();
                done.plan_fallbacks += stats.effects_fallback_lists;
                done.gpu_cpu_kernels += stats.effects_on_cpu_kernels;
                image::save_buffer(
                    seq_dir.join(format!("{f}-stack.png")),
                    &px,
                    width,
                    h,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
                done.frames += 1;
                done.flat += usize::from(is_flat(&px));
                done.stack += 1;
            }
            None => done.stack_declined += 1,
        }
    }
    Ok(())
}

/// The export's frames, made by the export's own decision
/// (`crate::export_frames`, which `ExportJob` calls too, #1156):
/// the encoder's planes when a frame is converted on the GPU
/// (`-export-yuv`, grey, the planes one after another), else the picture
/// through the encoder's display transform (`-export`). The encoder opens
/// at the check's size on `probe`, which is removed again.
fn export_frames(
    snapshot: &SequenceSnapshot,
    frames: &[u64],
    width: u32,
    h: u32,
    seq_dir: &Path,
    probe: &Path,
    done: &mut Rendered,
    factory: &RendererFactory<'_>,
) -> Result<(), String> {
    use crate::export_frames::{fit_in, ExportFrame, ExportFrameMaker};
    // The export's size: about the check's, with exactly the sequence's
    // aspect, so the picture fills it as it fills a full-size export (not
    // letterboxed, which the export pads on the CPU, nor a clip a pixel
    // short of the frame, which the GPU conversion declines) and an odd
    // aspect takes the path a full-size export of it takes. Failing that,
    // the size the export's own fit settles on.
    let (mut ew, mut eh) =
        exact_aspect_size(snapshot.width, snapshot.height, width).unwrap_or((width, h));
    for _ in 0..4 {
        let (fw, fh, _, _) = fit_in(snapshot.width, snapshot.height, ew, eh);
        if (fw, fh) == (ew, eh) {
            break;
        }
        (ew, eh) = (fw, fh);
    }
    let mut out = settings();
    out.resolution = Some(klipp_core::Resolution {
        width: ew,
        height: eh,
    });
    // The encoder's planes' layout, written beside each `-export-yuv`
    // frame, so a compare can read it against a frame the export drew
    // through its display transform (a route that changed).
    let mut layout = None;
    let opened = crate::Encoder::open(probe, &out, snapshot).map(|encoder| {
        layout = encoder.gpu_yuv420_params();
        ExportFrameMaker::new(&out, snapshot, &encoder, "pixel check")
    });
    let _ = std::fs::remove_file(probe);
    let mut maker = opened.map_err(|e| format!("opening the export's encoder: {e}"))?;
    if maker.letterboxed() {
        done.export_letterboxed += frames.len();
    }
    let display = crate::encoder::export_display(&out, snapshot.working_colorspace);
    let mut r = factory(snapshot, true)?;
    let mut source = ExportFrameSource::new(
        &snapshot.video.media,
        snapshot.fps,
        snapshot.working_colorspace,
    );
    for &f in frames {
        let time = frame_time(snapshot, f)?;
        let made = maker
            .frame(&mut r, &snapshot.video.sequence, time, &mut source)
            .map_err(|e| format!("frame {f} (export): {e}"))?;
        match made {
            ExportFrame::Planes { planes, .. } => {
                if !planes.len().is_multiple_of(ew as usize) {
                    return Err(format!(
                        "frame {f} (export): {} bytes of planes don't fill rows of {ew}",
                        planes.len()
                    ));
                }
                let rows = (planes.len() / ew as usize) as u32;
                image::save_buffer(
                    seq_dir.join(format!("{f}-export-yuv.png")),
                    &planes,
                    ew,
                    rows,
                    image::ExtendedColorType::L8,
                )
                .map_err(|e| e.to_string())?;
                if let Some(t) = &layout {
                    let side = YuvLayout {
                        width: ew,
                        height: eh,
                        bits: t.bits,
                        semi_planar: t.semi_planar,
                        sample_shift: t.sample_shift,
                        luma: t.luma_coefs,
                    };
                    std::fs::write(
                        seq_dir.join(format!("{f}-export-yuv.json")),
                        side.to_json().to_string(),
                    )
                    .map_err(|e| e.to_string())?;
                }
                // Flat when its luma plane is one value; a sample is two
                // bytes when the planes are deeper than eight bits.
                let luma_len = (ew * eh) as usize;
                let sample = (planes.len() * 2 / 3 / luma_len).max(1);
                let luma = &planes[..luma_len * sample];
                done.frames += 1;
                done.flat += usize::from(luma.chunks_exact(sample).all(|v| v == &luma[..sample]));
                done.export_gpu += 1;
            }
            ExportFrame::Linear(linear) => {
                let px = display.frame(&linear, |v, n| {
                    klipp_color::display::quantise(v, n, 255.0) as u8
                });
                image::save_buffer(
                    seq_dir.join(format!("{f}-export.png")),
                    &px,
                    ew,
                    eh,
                    image::ExtendedColorType::Rgba8,
                )
                .map_err(|e| e.to_string())?;
                done.frames += 1;
                done.flat += usize::from(is_flat(&px));
                done.export_cpu += 1;
            }
        }
    }
    Ok(())
}

/// The size nearest the check's width with exactly `w`:`h`'s aspect and
/// both sides even: a whole multiple of the reduced ratio. `None` when the
/// smallest such size is more than four times the check's width (an aspect
/// with no small exact size).
fn exact_aspect_size(w: u32, h: u32, width: u32) -> Option<(u32, u32)> {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    if w == 0 || h == 0 {
        return None;
    }
    let g = gcd(w, h);
    let (aw, ah) = (w / g, h / g);
    let even = |k: u32| (aw * k).is_multiple_of(2) && (ah * k).is_multiple_of(2);
    (1..)
        .take_while(|k| aw * k <= width * 4)
        .filter(|&k| even(k))
        .map(|k| (aw * k, ah * k))
        .min_by_key(|&(sw, _)| sw.abs_diff(width))
}

/// How an `-export-yuv` frame's planes are laid out: 4:2:0, luma then
/// chroma (planar, or interleaved), `bits` deep in one or two bytes a
/// sample, limited range, by the delivery's luma coefficients.
pub struct YuvLayout {
    pub width: u32,
    pub height: u32,
    pub bits: u32,
    pub semi_planar: bool,
    pub sample_shift: u32,
    pub luma: [f32; 3],
}

impl YuvLayout {
    fn to_json(&self) -> Value {
        json!({ "width": self.width, "height": self.height, "bits": self.bits, "semi_planar": self.semi_planar, "sample_shift": self.sample_shift, "luma": self.luma, "range": "limited" })
    }

    /// The layout a sidecar names; without one (a frame recorded before
    /// they were written), the check's export: 8-bit planar BT.709.
    pub fn read(yuv_png: &Path, width: u32, height: u32) -> Result<Self, String> {
        let side = yuv_png.with_extension("json");
        let Ok(text) = std::fs::read_to_string(&side) else {
            return Ok(Self {
                width,
                height,
                bits: 8,
                semi_planar: false,
                sample_shift: 0,
                luma: [0.2126, 0.7152, 0.0722],
            });
        };
        let v: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", side.display()))?;
        let n = |k: &str| {
            v[k].as_u64()
                .map(|x| x as u32)
                .ok_or_else(|| format!("{}: no {k}", side.display()))
        };
        let luma: Vec<f32> = v["luma"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_f64)
            .map(|x| x as f32)
            .collect();
        if v["range"].as_str() != Some("limited") || luma.len() != 3 {
            return Err(format!(
                "{}: a layout this check doesn't read",
                side.display()
            ));
        }
        Ok(Self {
            width: n("width")?,
            height: n("height")?,
            bits: n("bits")?,
            semi_planar: v["semi_planar"].as_bool().unwrap_or(false),
            sample_shift: n("sample_shift")?,
            luma: [luma[0], luma[1], luma[2]],
        })
    }
}

/// Compare two frames in 8-bit display levels, ignoring alpha as the original
/// real-project check does. Returns the maximum difference and samples over one.
pub fn differ(a: &Path, b: &Path) -> Result<(i32, usize), String> {
    let (x, y) = (
        image::open(a).map_err(|e| e.to_string())?.to_rgba8(),
        image::open(b).map_err(|e| e.to_string())?.to_rgba8(),
    );
    if x.dimensions() != y.dimensions() {
        return Err("sizes differ".into());
    }
    Ok(sample_difference(
        x.as_raw()
            .iter()
            .zip(y.as_raw())
            .enumerate()
            .filter(|(i, _)| i % 4 != 3)
            .map(|(_, (&p, &q))| (p, q)),
    ))
}

/// Maximum sample difference and how many exceed one 8-bit level. Callers
/// choose their channels and verify dimensions before pairing samples.
pub fn sample_difference(samples: impl IntoIterator<Item = (u8, u8)>) -> (i32, usize) {
    let (mut worst, mut over) = (0, 0);
    for (p, q) in samples {
        let d = (p as i32 - q as i32).abs();
        worst = worst.max(d);
        over += usize::from(d > 1);
    }
    (worst, over)
}

/// An export frame drawn through the display transform (`-export.png`)
/// against one converted on the GPU (`-export-yuv.png`): the picture taken
/// to the planes' own layout (the delivery's matrix, limited range, each
/// 2x2 block's chroma averaged) and compared plane by plane, in 8-bit
/// levels. The worst difference, and how many samples are over a level.
pub fn differ_route(rgb_png: &Path, yuv_png: &Path) -> Result<(i32, usize), String> {
    let rgb = image::open(rgb_png).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = rgb.dimensions();
    let l = YuvLayout::read(yuv_png, w, h)?;
    if (l.width, l.height) != (w, h) {
        return Err("sizes differ".into());
    }
    let raw = image::open(yuv_png)
        .map_err(|e| e.to_string())?
        .to_luma8()
        .into_raw();
    let (cw, ch) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
    let size = if l.bits > 8 { 2 } else { 1 };
    // A stored sample as an 8-bit level.
    let sample = |i: usize| -> Option<f32> {
        let v = if size == 2 {
            u16::from_le_bytes([*raw.get(2 * i)?, *raw.get(2 * i + 1)?]) as u32
        } else {
            *raw.get(i)? as u32
        };
        Some((v >> l.sample_shift) as f32 * 255.0 / ((1u32 << l.bits) - 1) as f32)
    };
    let luma_len = (w * h) as usize;
    let chroma = |plane: usize, i: usize| {
        if l.semi_planar {
            sample(luma_len + 2 * i + plane)
        } else {
            sample(luma_len + plane * cw * ch + i)
        }
    };
    let [kr, kg, kb] = l.luma;
    let ycc = |x: u32, y: u32| {
        let p = rgb.get_pixel(x.min(w - 1), y.min(h - 1));
        let (r, g, b) = (
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
        );
        let luma = kr * r + kg * g + kb * b;
        (
            luma,
            (b - luma) / (2.0 * (1.0 - kb)),
            (r - luma) / (2.0 * (1.0 - kr)),
        )
    };
    let (mut worst, mut over) = (0i32, 0usize);
    let mut count = |want: f32, got: f32| {
        let d = (want - got).abs().round() as i32;
        worst = worst.max(d);
        over += usize::from(d > 1);
    };
    for y in 0..h {
        for x in 0..w {
            let got = sample((y * w + x) as usize).ok_or("the planes are short")?;
            count(16.0 + 219.0 * ycc(x, y).0, got);
        }
    }
    for cy in 0..ch as u32 {
        for cx in 0..cw as u32 {
            let block = [
                ycc(2 * cx, 2 * cy),
                ycc(2 * cx + 1, 2 * cy),
                ycc(2 * cx, 2 * cy + 1),
                ycc(2 * cx + 1, 2 * cy + 1),
            ];
            let (cb, cr) = block
                .iter()
                .fold((0.0, 0.0), |(b, r), c| (b + c.1 / 4.0, r + c.2 / 4.0));
            let i = cy as usize * cw + cx as usize;
            count(
                128.0 + 224.0 * cb,
                chroma(0, i).ok_or("the planes are short")?,
            );
            count(
                128.0 + 224.0 * cr,
                chroma(1, i).ok_or("the planes are short")?,
            );
        }
    }
    Ok((worst, over))
}
