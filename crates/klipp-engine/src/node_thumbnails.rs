//! Bounded node previews, tied to the snapshot and request they answer.

use std::collections::HashMap;

use klipp_core::{Fps, RationalTime};
use klipp_compose::InspectAt as NodeOutput;
use klipp_timeline::SequenceRevision;

pub const WIDTH: usize = 192;
pub const HEIGHT: usize = 108;
pub const BYTES_PER_THUMB: usize = WIDTH * HEIGHT * 4;
/// At most 5.1 MiB of RGBA previews per batch, before texture upload.
pub const MAX_THUMBS: usize = 64;

/// The source of the preview, distinct from the thumbnail's own size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewQuality {
    /// Captured from the viewer's render at the requested frame.
    Viewer,
    /// Rendered from a small filmstrip image near the requested frame.
    Filmstrip,
}

impl PreviewQuality {
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Viewer => None,
            Self::Filmstrip => Some("Low-res preview"),
        }
    }
}

/// The sequence revision and frame this answer rendered. Keeping only a
/// token lets later sequence writes remain in place between renders.
#[derive(Clone, Debug)]
pub struct ThumbnailBatch {
    pub request: u64,
    pub frame: i64,
    pub quality: PreviewQuality,
    pub revision: SequenceRevision,
    pub images: HashMap<NodeOutput, Vec<u8>>,
}

impl ThumbnailBatch {
    /// Consume only requested, correctly sized images, in priority order.
    /// An empty batch is meaningful: the old previews must disappear.
    pub fn new(
        request: u64,
        frame: i64,
        quality: PreviewQuality,
        revision: SequenceRevision,
        wanted: &[NodeOutput],
        mut found: HashMap<NodeOutput, Vec<u8>>,
    ) -> Self {
        let images = wanted.iter().take(MAX_THUMBS).filter_map(|at| {
            let bytes = found.remove(at)?;
            (bytes.len() == BYTES_PER_THUMB).then(|| (at.clone(), bytes))
        }).collect();
        Self { request, frame, quality, revision, images }
    }

    pub fn matches(&self, request: u64, frame: i64, revision: SequenceRevision) -> bool {
        self.request == request && self.frame == frame && self.revision == revision
    }
}

/// Canonical frame identity, including fractional sequence rates. A UI
/// seek in seconds and an engine tick on the frame grid name one frame.
pub fn frame_index(time: RationalTime, fps: Fps) -> i64 {
    (time.to_secs_f64() * fps.as_f64()).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Sequence, SharedSequence};

    fn at(group: u128, node: u128) -> NodeOutput {
        let mut at = klipp_compose::inspect::grade_node(
            klipp_core::ClipId(uuid::Uuid::from_u128(1)), uuid::Uuid::from_u128(node));
        at.path.nodes.insert(1, uuid::Uuid::from_u128(group));
        at
    }

    fn frame() -> i64 { 12 }

    fn revision() -> SequenceRevision { SharedSequence::new(Sequence::new_default()).revision() }

    #[test]
    fn viewer_and_socket_times_match_at_integer_and_ntsc_frame_rates() {
        for fps in [Fps::film_24(), Fps::ntsc_23976(), Fps::ntsc_29970()] {
            for index in [0, 1, 48, 180_001] {
                let engine_time = RationalTime::from_frames(index * i64::from(fps.den), fps.num);
                let socket_time = RationalTime::from_secs_f64(engine_time.to_secs_f64(), 1_000_000);
                let revision = revision();
                let batch = ThumbnailBatch::new(1, frame_index(engine_time, fps), PreviewQuality::Viewer,
                    revision, &[], HashMap::new());
                assert!(batch.matches(1, frame_index(socket_time, fps), revision));
                assert!(!batch.matches(1, frame_index(engine_time, fps) + 1, revision));
            }
        }
        assert_eq!(frame_index(RationalTime::from_frames(48, 24), Fps::film_24()),
            frame_index(RationalTime::from_frames(2, 1), Fps::film_24()));
    }

    #[test]
    fn keeping_a_batch_does_not_make_the_next_edit_copy_the_sequence() {
        let shared = SharedSequence::new(Sequence::new_default());
        let pointer = std::sync::Arc::as_ptr(&shared.snapshot());
        let batch = ThumbnailBatch::new(1, frame(), PreviewQuality::Viewer,
            shared.revision(), &[], HashMap::new());
        shared.write().unwrap().name = "live slider edit".into();
        assert_eq!(std::sync::Arc::as_ptr(&shared.snapshot()), pointer);
        assert!(!batch.matches(1, frame(), shared.revision()));
    }

    #[test]
    fn a_stale_thumbnail_is_dropped_after_an_edit() {
        let sequence = SharedSequence::new(Sequence::new_default());
        let batch = ThumbnailBatch::new(1, frame(), PreviewQuality::Viewer,
            sequence.revision(), &[], HashMap::new());
        assert!(batch.matches(1, frame(), sequence.revision()));
        sequence.write().unwrap().name = "edited while rendering".into();
        assert!(!batch.matches(1, frame(), sequence.revision()));
        assert!(!batch.matches(2, frame(), batch.revision));
        assert!(!batch.matches(1, 13, batch.revision));
    }

    #[test]
    fn thumbnails_respect_the_memory_cap_and_full_paths() {
        let wanted: Vec<_> = (0..MAX_THUMBS + 10).map(|g| at(g as u128 + 2, 99)).collect();
        let found = wanted.iter().enumerate().map(|(i, at)|
            (at.clone(), vec![i as u8; BYTES_PER_THUMB])).collect();
        let batch = ThumbnailBatch::new(1, frame(), PreviewQuality::Viewer,
            revision(), &wanted, found);
        assert_eq!(batch.images.len(), MAX_THUMBS);
        assert_eq!(batch.images.values().map(Vec::len).sum::<usize>(), MAX_THUMBS * BYTES_PER_THUMB);
        assert_eq!(batch.images[&wanted[0]][0], 0);
        assert_eq!(batch.images[&wanted[1]][0], 1, "the same local id in another group is distinct");
        assert!(!batch.images.contains_key(&wanted[MAX_THUMBS]));
    }

    #[test]
    fn a_low_res_preview_is_marked() {
        assert_eq!(PreviewQuality::Filmstrip.label(), Some("Low-res preview"));
        assert_eq!(PreviewQuality::Viewer.label(), None);
    }

    #[test]
    fn unavailable_and_malformed_results_leave_no_old_image() {
        let wanted = [at(2, 3)];
        let batch = ThumbnailBatch::new(1, frame(), PreviewQuality::Viewer,
            revision(), &wanted,
            [(wanted[0].clone(), vec![0; 4])].into());
        assert!(batch.images.is_empty());
    }

    #[test]
    fn publication_clears_missing_results_and_keeps_the_better_preview() {
        let revision = revision();
        let wanted = [at(2, 3)];
        let batch = |request, quality, found| ThumbnailBatch::new(
            request, frame(), quality, revision, &wanted, found);
        let mut fb = crate::texture::FrameBuffer::new(1, 1);
        fb.publish_node_thumbnails(batch(1, PreviewQuality::Viewer,
            [(wanted[0].clone(), vec![11; BYTES_PER_THUMB])].into()));
        let generation = fb.node_thumbs_gen;
        fb.publish_node_thumbnails(batch(1, PreviewQuality::Filmstrip,
            [(wanted[0].clone(), vec![22; BYTES_PER_THUMB])].into()));
        assert_eq!(fb.node_thumbs_gen, generation);
        assert_eq!(fb.node_thumbnail_batch.as_ref().unwrap().images[&wanted[0]][0], 11);
        fb.publish_node_thumbnails(batch(2, PreviewQuality::Viewer, HashMap::new()));
        assert!(fb.node_thumbnail_batch.as_ref().unwrap().images.is_empty());
        assert!(fb.node_thumbs_gen > generation);
    }
}
