use crate::stage::{ClipContext, FrameContext, ProcessingStage};
use klipp_color::ColorPipeline;
use klipp_core::ExportSettings;
use klipp_nodes::{GraphExecutor, NodeGraph};
use std::path::PathBuf;

/// Processing stage that applies per-clip color grading and the global LUT.
///
/// Film grain is handled inside the node graph (FilmGrain node).
/// Global LUT is applied AFTER the node graph as a post-graph step.
pub struct ColorGradeStage {
    pipeline: ColorPipeline,
    executor: GraphExecutor,
    /// Cached node graph for the current clip.
    cached_graph: Option<NodeGraph>,
    /// Export-wide frame counter for grain seed.
    export_frame_counter: u64,
}

impl ColorGradeStage {
    pub fn new(global_lut_path: Option<PathBuf>) -> Self {
        let mut pipeline = ColorPipeline::new();

        if let Some(ref lut_path) = global_lut_path {
            if let Err(e) = pipeline.load_global_lut(lut_path) {
                tracing::warn!("ColorGradeStage: failed to load global LUT: {e}");
            }
        }

        Self {
            pipeline,
            executor: GraphExecutor::new(), // Creates its own GPU pipeline for the export thread
            cached_graph: None,
            export_frame_counter: 0,
        }
    }
}

impl ProcessingStage for ColorGradeStage {
    fn name(&self) -> &'static str {
        "Color Grade"
    }

    fn begin_export(&mut self, _settings: &ExportSettings) {
        self.export_frame_counter = 0;
    }

    fn begin_clip(&mut self, ctx: &ClipContext) {
        let graph = ctx.clip.node_graph.clone();
        if let Err(e) = graph.topo_sort() {
            tracing::warn!("Clip {:?} has cycle in node graph: {:?}", ctx.clip.id, e);
        }
        self.executor.prepare_clip(&graph);
        self.cached_graph = Some(graph);
    }

    fn end_clip(&mut self) {
        self.cached_graph = None;
    }

    fn process(
        &mut self,
        frame: Vec<u8>,
        width: u32,
        height: u32,
        ctx: &FrameContext,
    ) -> Vec<u8> {
        // 1. Execute node graph for per-clip color grading
        let mut rgba = if let Some(ref graph) = self.cached_graph {
            let scene = ctx
                .scene_referred
                .as_ref()
                .filter(|s| s.width == width && s.height == height && s.codes.len() == (width * height) as usize);
            if !graph.is_identity() && scene.is_some() {
                // PQ / HLG / camera log: grade scene-linear from the codes,
                // then the display tone map — the viewer's order. The
                // 8-bit `frame` is only the tone-mapped preview.
                let scene = scene.unwrap();
                let linear = klipp_color::hdr::hdr10_codes_to_scene_linear_rgba(
                    &scene.codes,
                    scene.transfer,
                    scene.source_to_working,
                );
                match self.executor.execute_linear(
                    &linear,
                    graph,
                    width,
                    height,
                    ctx.clip_time_secs,
                    self.export_frame_counter,
                    None,
                    None,
                ) {
                    Ok((mut graded, _)) => {
                        klipp_color::hdr::apply_hdr_display_tonemap(&mut graded, scene.transfer);
                        klipp_color::hdr::display_linear_to_srgb8(&graded)
                    }
                    Err(e) => {
                        tracing::error!("Node graph execution failed: {:?}", e);
                        frame
                    }
                }
            } else if !graph.is_identity() {
                match self.executor.execute(
                    &frame,
                    graph,
                    width,
                    height,
                    ctx.clip_time_secs,
                    self.export_frame_counter,
                    None, // No scope tap during export
                    None, // No matte preview during export
                ) {
                    Ok((result, _)) => result,
                    Err(e) => {
                        tracing::error!("Node graph execution failed: {:?}", e);
                        frame
                    }
                }
            } else {
                frame
            }
        } else {
            frame
        };

        // 2. Global LUT — applied AFTER the node graph (not part of per-clip graph)
        if let Some(ref lut_data) = self.pipeline.global_lut {
            let opacity = self.pipeline.global_lut_opacity;
            klipp_color::apply_3d_lut(&mut rgba, lut_data, opacity);
        }

        self.export_frame_counter += 1;
        rgba
    }

    fn end_export(&mut self) {}
}
