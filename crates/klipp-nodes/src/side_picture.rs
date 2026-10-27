//! Pictures handed to a run beside its source (#1746): read by the
//! renderer before the run, where it can read them, and drawn by a
//! [`NodeKind::SidePicture`](crate::NodeKind::SidePicture) node, which
//! outputs the one its key names.
//!
//! A media source's frame is one, and what is below a clip another.
//! Another picture a plan needs (a key reading the picture its grade
//! reads, apart from that grade) is a new [`SideKey`] variant: the
//! renderer puts the picture in the run's map under it
//! ([`crate::GraphExecutor::put_side_picture`], after the map is cleared
//! for the clip) and makes a `SidePicture` node with that key in
//! `NodeParams::side_picture`. On the GPU,
//! [`crate::GraphExecutor::upload_side_picture`] puts it in a kept picture
//! for a node that reads one.

use std::collections::HashMap;
use std::sync::Arc;

/// Which picture a side node outputs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SideKey {
    /// A media item's frame at the clip's time plus an offset: the item's
    /// pool id and the offset's bits, as a media source holds them.
    Media { item: String, offset_bits: u32 },
    /// A title drawn as a picture (#2758): the title as its file stores
    /// it.
    Title { title: String },
    /// What the tracks below the clip's composite to, seen through the
    /// clip's placement.
    Below,
    /// The picture the clip's grade reads, for a key that reads it in a run
    /// apart from that grade (#1265).
    GradeInput,
    /// A frame of the clip's own media picked as a difference key's clean
    /// plate (#2758): the media's pool id and the frame's source seconds'
    /// bits.
    Plate { media: String, secs_bits: u64 },
}

impl SideKey {
    /// The picture `effect` draws, when it draws one handed to the run.
    pub fn of(effect: &klipp_effects::BuiltinEffect) -> Option<Self> {
        match effect {
            klipp_effects::BuiltinEffect::MediaSource { media, offset } => Some(Self::Media { item: media.clone(), offset_bits: offset.to_bits() }),
            klipp_effects::BuiltinEffect::TitleSource { title } => Some(Self::Title { title: title.clone() }),
            klipp_effects::BuiltinEffect::ClipBelow {} => Some(Self::Below),
            _ => None,
        }
    }

    /// The node's name in a decline: the effect's that draws it.
    pub fn name(&self) -> String {
        match self {
            Self::Media { .. } => crate::decline::effect_name(&klipp_effects::BuiltinEffect::MediaSource { media: String::new(), offset: 0.0 }),
            Self::Title { .. } => crate::decline::effect_name(&klipp_effects::BuiltinEffect::TitleSource { title: String::new() }),
            Self::Below => crate::decline::effect_name(&klipp_effects::BuiltinEffect::ClipBelow {}),
            Self::GradeInput => "Grade input".into(),
            Self::Plate { .. } => "Clean plate".into(),
        }
    }
}

/// Why a side picture isn't given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideMissing {
    /// Its file isn't on this machine, or the item isn't in the pool: its
    /// name, or its id when the pool has no such item.
    File(String),
    /// No picture there: past a media's end, a decode that failed, or a
    /// render that reads none.
    Unavailable,
    /// The render can't make it, said why: what is below a clip placed
    /// with a 3D tilt or a pin.
    Unsupported(String),
}

/// A run's side pictures by key: linear RGBA f32 in the working space at
/// the render size, or why there is none.
pub type SidePictures = Arc<HashMap<SideKey, Result<Arc<Vec<f32>>, SideMissing>>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Edge, GraphExecutor, Node, NodeGraph, NodeId, NodeKind, NodeParams};

    const W: u32 = 8;
    const H: u32 = 4;

    fn node(id: u128, kind: NodeKind, params: NodeParams) -> Node {
        Node { id: NodeId::from_u128(id), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] }
    }

    /// Source, then a side picture node holding `effect`, then Output.
    fn graph(effect: klipp_effects::BuiltinEffect) -> NodeGraph {
        let nodes = vec![node(1, NodeKind::Source, NodeParams::default()), node(2, NodeKind::SidePicture, NodeParams { effects: vec![effect], ..Default::default() }), node(3, NodeKind::Output, NodeParams::default())];
        let edges = vec![Edge { from_node: nodes[0].id, from_port: 0, to_node: nodes[1].id, to_port: 0 }, Edge { from_node: nodes[1].id, from_port: 0, to_node: nodes[2].id, to_port: 0 }];
        NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] }
    }

    fn media(item: &str) -> klipp_effects::BuiltinEffect {
        klipp_effects::BuiltinEffect::MediaSource { media: item.into(), offset: 0.5 }
    }

    fn key(item: &str) -> SideKey {
        SideKey::Media { item: item.into(), offset_bits: 0.5f32.to_bits() }
    }

    fn picture() -> Vec<f32> {
        (0..W * H * 4).map(|i| (i % 7) as f32 / 7.0).collect()
    }

    #[test]
    fn a_side_picture_node_draws_the_picture_its_key_names() {
        let mut ex = GraphExecutor::new_cpu_only();
        let source = vec![0.5; (W * H * 4) as usize];
        ex.set_side_pictures(Arc::new(HashMap::from([(key("a"), Ok(Arc::new(picture()))), (key("gone"), Err(SideMissing::File("gone.mov".into())))])));
        let (out, _) = ex.execute_linear(&source, &graph(media("a")), W, H, 0.0, 0, None, None).unwrap();
        assert_eq!(out, picture());
        assert!(ex.last_run_declines().is_empty());
        // A missing file: clear, declined as missing, named.
        let (out, _) = ex.execute_linear(&source, &graph(media("gone")), W, H, 0.0, 0, None, None).unwrap();
        assert!(out.iter().all(|v| *v == 0.0));
        let d = &ex.last_run_declines()[0];
        assert_eq!((d.outcome, d.why.as_str()), (crate::DeclineOutcome::FileMissing, "gone.mov"));
        // None handed to the run: clear, a frame missing.
        let (out, _) = ex.execute_linear(&source, &graph(media("other")), W, H, 0.0, 0, None, None).unwrap();
        assert!(out.iter().all(|v| *v == 0.0));
        assert_eq!(ex.last_run_declines()[0].outcome, crate::DeclineOutcome::NeighbourMissing);
    }

    /// A media source's frame is display-referred, an HDR or log item's
    /// given its display step as it is read (#1857), and so is what is
    /// below a clip, the composite (#1867), each by its effect or its key.
    /// A side picture naming none, and the grade's input, declare none.
    #[test]
    fn a_side_pictures_domain_is_its_pictures() {
        use crate::domains::{kind_step, DomainStep, PictureDomain};
        assert_eq!(kind_step(NodeKind::SidePicture, &NodeParams::default()), DomainStep::Undeclared);
        let grade_input = NodeParams { side_picture: Some(SideKey::GradeInput), ..Default::default() };
        assert_eq!(kind_step(NodeKind::SidePicture, &grade_input), DomainStep::Undeclared);
        let media_by_effect = NodeParams { effects: vec![media("a")], ..Default::default() };
        let media_by_key = NodeParams { side_picture: Some(key("a")), ..Default::default() };
        let by_effect = NodeParams { effects: vec![klipp_effects::BuiltinEffect::ClipBelow {}], ..Default::default() };
        let by_key = NodeParams { side_picture: Some(SideKey::Below), ..Default::default() };
        for p in [media_by_effect, media_by_key, by_effect, by_key] {
            assert_eq!(kind_step(NodeKind::SidePicture, &p), DomainStep::Gives(PictureDomain::Display));
        }
    }

    #[test]
    fn a_side_picture_goes_up_to_the_gpu_as_it_is() {
        let mut ex = GraphExecutor::new();
        if ex.gpu_pipeline_or_init().is_none() {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
            eprintln!("UNVERIFIED: no GPU");
            return;
        }
        ex.set_side_pictures(Arc::new(HashMap::from([(key("a"), Ok(Arc::new(picture())))])));
        // An effect at the size first, as a render's, which sizes the
        // pipeline's buffers.
        ex.gpu_pipeline_or_init().unwrap().apply_effects(&[klipp_effects::BuiltinEffect::Invert { mix: 1.0 }], &vec![0.5; (W * H * 4) as usize], W, H);
        assert!(ex.upload_side_picture(&key("b"), W, H).is_none(), "no picture under another key");
        assert!(ex.upload_side_picture(&key("a"), W + 1, H).is_none(), "not at another size");
        let kept = ex.upload_side_picture(&key("a"), W, H).expect("kept");
        let back = ex.gpu_pipeline_or_init().unwrap().read_picture(&kept, (W * H) as usize).expect("read back");
        assert_eq!(back, picture());
    }

    /// #1265: a key reading the picture the grade reads, handed to a run
    /// apart from that grade (`SideKey::GradeInput`), masks an effect the
    /// same on the GPU as on the CPU: Source -> blur, the blur masked by a
    /// luma key of the side picture. On the GPU the key is drawn there, from
    /// the side picture uploaded once. Put and cleared with the run's other
    /// pictures.
    #[test]
    fn a_key_of_the_grades_input_masks_an_effect_alike_on_the_cpu_and_the_gpu() {
        let (w, h) = (64u32, 32u32);
        let blur = node(2, NodeKind::Effect, NodeParams {
            effects: vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 }],
            effect_blend: Some(crate::params::EffectBlend { mix: 1.0, channel: klipp_effects::mask_mix::MaskChannel::Luminance }),
            ..Default::default()
        });
        let side = node(4, NodeKind::SidePicture, NodeParams { side_picture: Some(SideKey::GradeInput), ..Default::default() });
        let key = node(5, NodeKind::LumaKey, NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.4, high: 1.0, soft: 0.1, invert: false }), ..Default::default() });
        let nodes = vec![node(1, NodeKind::Source, NodeParams::default()), blur, node(3, NodeKind::Output, NodeParams::default()), side, key];
        let e = |a: u128, b: u128, port: u8| Edge { from_node: NodeId::from_u128(a), from_port: 0, to_node: NodeId::from_u128(b), to_port: port };
        let g = NodeGraph { output_node_id: NodeId::from_u128(3), nodes, edges: vec![e(1, 2, 0), e(2, 3, 0), e(1, 4, 0), e(4, 5, 0), e(5, 2, 1)], node_positions: Default::default(), working_space_override: None, frames: vec![], sticky_notes: vec![] };
        // A checker to blur, and a grade input bright on the left half only.
        let source: Vec<f32> = (0..w * h).flat_map(|i| { let v = if (i % w / 4 + i / w / 4) % 2 == 0 { 0.8 } else { 0.1 }; [v, v, v, 1.0] }).collect();
        let input: Vec<f32> = (0..w * h).flat_map(|i| { let v = if i % w < w / 2 { 0.9 } else { 0.05 }; [v, v, v, 1.0] }).collect();
        let run = |ex: &mut GraphExecutor| {
            ex.clear_side_pictures();
            ex.put_side_picture(SideKey::GradeInput, Ok(Arc::new(input.clone())));
            ex.execute_linear(&source, &g, w, h, 0.0, 0, None, None).unwrap().0
        };
        let mut cpu = GraphExecutor::new_cpu_only();
        let want = run(&mut cpu);
        // Control: the key reads the side picture, not the source: the left
        // half is blurred, the right half left as it was.
        let at = |v: &[f32], x: u32, y: u32| v[((y * w + x) * 4) as usize];
        assert!((at(&want, w - 3, 5) - at(&source, w - 3, 5)).abs() < 1e-6, "outside the key, the source");
        assert!((0..w / 2 - 4).any(|x| (at(&want, x, 5) - at(&source, x, 5)).abs() > 0.05), "inside, the blur");
        let mut gpu = GraphExecutor::new();
        if gpu.gpu_pipeline_or_init().is_none() {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required, but there is no GPU");
            eprintln!("UNVERIFIED: no GPU");
            return;
        }
        let got = run(&mut gpu);
        let worst = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 2e-3, "GPU against CPU: {worst}");
        // Drawn on the GPU (a chain dispatch), not keyed on the CPU: only the
        // Source and the side node ran there.
        let stats = gpu.last_run_chain_stats();
        assert!(stats.extends >= 1 && stats.cpu_nodes == 2, "{stats:?}");
        // Control: with no picture handed to the run the key can't be drawn
        // there; its reader keys it on the CPU (from the side node's clear
        // picture, declined).
        gpu.clear_side_pictures();
        let _ = gpu.execute_linear(&source, &g, w, h, 0.0, 0, None, None).unwrap();
        assert_eq!(gpu.last_run_chain_stats().cpu_nodes, 3, "{:?}", gpu.last_run_chain_stats());
        assert!(gpu.last_run_declines().iter().any(|d| d.kind == NodeKind::SidePicture), "{:?}", gpu.last_run_declines());
    }
}
