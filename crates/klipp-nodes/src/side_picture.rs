//! Pictures handed to a run beside its source (#1746): read by the
//! renderer before the run, where it can read them, and drawn by a
//! [`NodeKind::SidePicture`](crate::NodeKind::SidePicture) node, which
//! outputs the one its key names.
//!
//! A media source's frame is one. Another picture a plan needs (a key
//! reading the picture its grade reads, apart from that grade) is a new
//! [`SideKey`] variant: the renderer puts the picture in the run's map
//! under it ([`crate::GraphExecutor::set_side_pictures`]) and makes a
//! `SidePicture` node with that key in `NodeParams::side_picture`. On the
//! GPU, [`crate::GraphExecutor::upload_side_picture`] puts it in a kept
//! picture for a node that reads one.

use std::collections::HashMap;
use std::sync::Arc;

/// Which picture a side node outputs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SideKey {
    /// A media item's frame at the clip's time plus an offset: the item's
    /// pool id and the offset's bits, as a media source holds them.
    Media { item: String, offset_bits: u32 },
}

impl SideKey {
    /// The picture `effect` draws, when it draws one handed to the run.
    pub fn of(effect: &klipp_effects::BuiltinEffect) -> Option<Self> {
        match effect {
            klipp_effects::BuiltinEffect::MediaSource { media, offset } => Some(Self::Media { item: media.clone(), offset_bits: offset.to_bits() }),
            _ => None,
        }
    }

    /// The node's name in a decline: the effect's that draws it.
    pub fn name(&self) -> String {
        match self {
            Self::Media { .. } => crate::decline::effect_name(&klipp_effects::BuiltinEffect::MediaSource { media: String::new(), offset: 0.0 }),
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

    /// A side picture isn't made from what its node reads: it declares no
    /// colour domain, as a generator's doesn't (#1290).
    #[test]
    fn a_side_picture_declares_no_domain() {
        assert_eq!(crate::domains::kind_step(NodeKind::SidePicture, &NodeParams::default()), crate::domains::DomainStep::Undeclared);
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
}
