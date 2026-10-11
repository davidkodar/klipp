//! Native file fitting must not weaken the node boundary's size contract.
use klipp_compose::{FrameSource, LinearFrameProviderFn, SourceFrame};
use klipp_core::{MediaId, Timecode, WorkingColorspace};
use klipp_nodes::frame_context::{BoundedSources, Missing, Neighbour, SourceProvider};
use klipp_timeline::Clip;

struct WrongSize;
impl FrameSource for WrongSize {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
    fn temporal_frames_linear(
        &mut self,
        _: &Clip,
        _: WorkingColorspace,
    ) -> Option<LinearFrameProviderFn> {
        Some(Box::new(|_| Some((vec![0.2, 0.2, 0.2, 1.0], 1, 1))))
    }
}

#[test]
fn a_provider_with_the_wrong_size_still_fails_the_node_boundary() {
    let tc = |s| Timecode::from_secs_f64(s, 24);
    let clip = Clip::media(MediaId::new(), tc(0.0), tc(1.0), tc(0.0), tc(1.0));
    let mut reader = WrongSize;
    let mut source = klipp_compose::render::ClipSources::new(
        &mut reader,
        &clip,
        WorkingColorspace::Rec709Linear,
    )
    .with_current(0.5, None, 4, 4);
    let mut bounded = BoundedSources::new(&mut source, 4, 4);
    bounded.for_node(klipp_nodes::NodeId::new_v4(), 1);
    assert_eq!(
        bounded.linear(0.5),
        Neighbour::Missing(Missing::WrongSize {
            width: 1,
            height: 1
        })
    );
}

#[test]
fn a_native_reader_is_fitted_with_bars_and_missing_frames_stay_missing() {
    let raw: LinearFrameProviderFn = Box::new(|time| {
        if time < 0.0 {
            None
        } else {
            Some(([0.2, 0.4, 0.6, 1.0].repeat(2), 2, 1))
        }
    });
    let mut reader = klipp_compose::render::fit_linear_reader(raw, 4, 4);
    assert!(reader(-1.0).is_none());
    let (pixels, w, h) = reader(0.0).unwrap();
    assert_eq!((w, h), (4, 4));
    for (i, pixel) in pixels.chunks_exact(4).enumerate() {
        assert_eq!(
            pixel,
            if i / 4 == 0 || i / 4 == 3 {
                &[0.0; 4]
            } else {
                &[0.2, 0.4, 0.6, 1.0]
            }
        );
    }
    let malformed: LinearFrameProviderFn = Box::new(|_| Some((vec![0.5], 2, 1)));
    assert!(klipp_compose::render::fit_linear_reader(malformed, 4, 4)(0.0).is_none());
}
