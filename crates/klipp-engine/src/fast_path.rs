//! The graded playback fast path: a single clip's grade run from the
//! decoder's GPU buffer to display bytes without the frame crossing the
//! bus as floats.
//!
//! The viewer takes it only when the shared renderer would render that
//! one clip and nothing else (`PlaybackEngine::direct_present_blockers`).
//! It must then give the renderer's picture, which
//! `tests/fast_paths_match_renderer.rs` checks against
//! `klipp_compose::Renderer` itself — this function, not a copy of it.

use klipp_color::hdr::HdrTransfer;
use klipp_core::{TrackPoint, WorkingColorspace};
use klipp_nodes::{GraphExecutor, NodeGraph, NodeId};

/// Where the decoder left the frame.
pub enum GpuSeed<'a> {
    /// Display-referred sRGB u8 RGBA (SDR decode).
    Srgb8(&'a wgpu::Buffer),
    /// Packed 10-bit codes of a scene-referred source (PQ, HLG, camera log).
    SceneReferred {
        buffer: &'a wgpu::Buffer,
        transfer: HdrTransfer,
        source_to_working: [[f32; 3]; 3],
    },
    /// The same codes in memory — a parked frame. Uploaded once and kept
    /// by the executor while the same frame is regraded.
    SceneCodes {
        codes: &'a std::sync::Arc<Vec<u32>>,
        transfer: HdrTransfer,
        source_to_working: [[f32; 3]; 3],
    },
    /// Display-referred sRGB u8 RGBA in memory, likewise.
    Srgb8Bytes(&'a std::sync::Arc<Vec<u8>>),
}

/// How the clip's graph runs for this frame — as `Renderer::grade` runs
/// it: at `klipp_compose::render::grade_time`, in the working space, with
/// the grade panel's solo when the clip is the one it inspects.
pub struct GradeAt<'a> {
    pub graph: &'a NodeGraph,
    pub clip_time: f64,
    pub frame_number: u64,
    pub track_points: &'a [TrackPoint],
    pub working: WorkingColorspace,
    /// The user's 3D-LUT interpolation preference, so a clip with a LUT
    /// node looks the same here as through the renderer.
    pub lut_tetrahedral: bool,
    pub solo: Option<NodeId>,
}

/// Grade `seed` and encode it for display on the GPU. `display_encode`
/// finishes a result that came back to the CPU instead (a solo does that)
/// the way the viewer's display encode does. `None` when the frame cannot
/// take this path — the seed refused, a node wanting CPU pixels — having
/// shown nothing; the caller then renders it the ordinary way.
pub fn grade_to_display_on_gpu(
    exec: &mut GraphExecutor,
    seed: GpuSeed<'_>,
    width: u32,
    height: u32,
    at: &GradeAt<'_>,
    display_encode: &dyn Fn(&[f32]) -> Vec<u8>,
) -> Option<Vec<u8>> {
    let transfer = match seed {
        GpuSeed::Srgb8(buffer) => {
            if !exec.prime_chain_from_gpu_u8_buffer(buffer, width, height) {
                return None;
            }
            None
        }
        GpuSeed::SceneReferred { buffer, transfer, source_to_working } => {
            let seeded = exec.prime_chain_from_gpu_hdr10_buffer(
                buffer,
                width,
                height,
                transfer.transfer_id(),
                transfer.scene_scale(),
                klipp_effects::color_wgsl::pack_primaries_matrix(source_to_working),
            );
            if !seeded {
                return None;
            }
            Some(transfer)
        }
        GpuSeed::SceneCodes { codes, transfer, source_to_working } => {
            let seeded = exec.prime_chain_from_hdr10_codes(
                codes,
                width,
                height,
                transfer.transfer_id(),
                transfer.scene_scale(),
                klipp_effects::color_wgsl::pack_primaries_matrix(source_to_working),
            );
            if !seeded {
                return None;
            }
            Some(transfer)
        }
        GpuSeed::Srgb8Bytes(rgba) => {
            if !exec.prime_chain_from_srgb8(rgba, width, height) {
                return None;
            }
            None
        }
    };
    exec.prepare_clip(at.graph);
    exec.set_working_colorspace(at.working);
    exec.set_lut_tetrahedral(at.lut_tetrahedral);
    exec.set_current_clip_track_points(at.track_points);
    exec.set_solo_node(at.solo);
    // Which display transforms the GPU has. `SHADER_LINEAR_TO_DISPLAY_U8`
    // is an sRGB encode and nothing else — no basis change, no gamut
    // mapping — so it is only right for a project already in BT.709.
    // `SHADER_ACES_REC709_U8` is the whole ACES output transform, and
    // `display_transform_ws` on an AcesCG project is exactly that
    // (`acescg_to_rec709_sdr` per pixel, same quantisation), so the two
    // agree. `SHADER_BT2020_DISPLAY_U8` is the BT.2020 one: the basis
    // change, then the BT.2407 compression into BT.709 reading the same
    // reach tables and cusps the CPU bisects, then the sRGB encode.
    //
    // All three are held to `display_transform_ws` frame by frame in
    // tests/fast_paths_match_renderer.rs, and the two wide-gamut
    // shaders pixel by pixel in tests/aces_gpu_matches_cpu.rs and
    // tests/bt2020_gamut_gpu_matches_cpu.rs.
    let gpu_display = matches!(
        at.working,
        WorkingColorspace::Rec709Linear
            | WorkingColorspace::AcesCG
            | WorkingColorspace::Bt2020Linear
    );
    if gpu_display {
        exec.request_output_on_gpu();
    }
    let result = exec.execute_linear_with_thumbs(
        &[], at.graph, width, height, at.clip_time, at.frame_number, None, None, None,
    );
    exec.set_solo_node(None);
    // Typically `CpuSourceRequired`: a node needs CPU pixels.
    let (cpu_result, _, _) = result.ok()?;
    let bytes = if exec.output_on_gpu().is_some() {
        match at.working {
            // The ACES path has its own tone curve and ignores the
            // viewer's tone-map operator, exactly as the CPU transform
            // does (`display_transform_ws` marks it unused there).
            WorkingColorspace::AcesCG => exec.encode_output_to_aces_rec709_u8(width, height)?,
            // BT.2020's transform does honour the tone-map operator, and
            // this shader has none — so `fast_path_blockers` refuses the
            // path unless the operator is None, and there is nothing to
            // apply here.
            WorkingColorspace::Bt2020Linear => {
                exec.encode_output_to_bt2020_display_u8(width, height)?
            }
            WorkingColorspace::Rec709Linear => exec.encode_output_to_display_u8(
                width,
                height,
                transfer.map(|t| t.display_exposure()),
            )?,
        }
    } else {
        if cpu_result.len() != (width as usize) * (height as usize) * 4 {
            return None;
        }
        let mut linear = cpu_result;
        if let Some(t) = transfer {
            klipp_color::hdr::apply_hdr_display_tonemap(&mut linear, t);
        }
        display_encode(&linear)
    };
    (bytes.len() == (width as usize) * (height as usize) * 4).then_some(bytes)
}
