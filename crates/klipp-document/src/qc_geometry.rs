//! Detection and title regions compared at the same timeline time.
use crate::ops::Transcripts;
use klipp_media::MediaPool;
use klipp_timeline::{Clip, Sequence, TrackKind};

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub placement: Option<klipp_timeline::surface::Placement>,
    pub opacity: f32,
    /// Only true when the entire fitted rectangle is known to be opaque.
    pub opaque: bool,
}
impl Default for Geometry {
    fn default() -> Self {
        Self {
            placement: None,
            opacity: 1.0,
            opaque: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Detection {
    pub track: usize,
    pub at: f64,
    pub rect: [f32; 4],
}
#[derive(Default)]
pub struct Detections {
    pub samples: Vec<Detection>,
    pub incomplete: bool,
}

pub fn intersection(a: [f32; 4], b: [f32; 4]) -> Option<[f32; 4]> {
    let (x, y) = (a[0].max(b[0]), a[1].max(b[1]));
    let (w, h) = (
        (a[0] + a[2]).min(b[0] + b[2]) - x,
        (a[1] + a[3]).min(b[1] + b[3]) - y,
    );
    (w > 0.0 && h > 0.0).then_some([x, y, w, h])
}
fn subtract(a: [f32; 4], cover: [f32; 4]) -> Vec<[f32; 4]> {
    let Some([x, y, w, h]) = intersection(a, cover) else {
        return vec![a];
    };
    [
        [a[0], a[1], a[2], y - a[1]],
        [a[0], y + h, a[2], a[1] + a[3] - y - h],
        [a[0], y, x - a[0], h],
        [x + w, y, a[0] + a[2] - x - w, h],
    ]
    .into_iter()
    .filter(|r| r[2] > 0.0 && r[3] > 0.0)
    .collect()
}
pub fn placed(rect: [f32; 4], shown: [u32; 2], frame: [u32; 2], g: Geometry) -> Option<[f32; 4]> {
    if g.opacity <= 0.01 {
        return None;
    }
    let [x, y, w, h] = intersection(rect, [0.0, 0.0, 1.0, 1.0])?;
    let q = klipp_timeline::surface::source_quad_in_frame(
        &[[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
        shown,
        frame,
        g.placement,
    );
    let x0 = q.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
    let y0 = q.iter().map(|p| p[1]).fold(f32::INFINITY, f32::min);
    let x1 = q.iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
    let y1 = q.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
    intersection([x0, y0, x1 - x0, y1 - y0], [0.0, 0.0, 1.0, 1.0])
}
fn size(pool: &MediaPool, c: &Clip, frame: [u32; 2]) -> [u32; 2] {
    c.media_id()
        .and_then(|id| pool.get(&id))
        .map_or(frame, |m| {
            let (w, h) = (m.resolution.width, m.resolution.height);
            if m.source_rotation.rem_euclid(180) == 90 {
                [h, w]
            } else {
                [w, h]
            }
        })
}
/// Map each analysis sample through the clip's retime, then its picture
/// geometry. No temporal padding: a detection outside the shown source
/// range must not become evidence against a neighbouring shot.
pub fn detections(
    seq: &Sequence,
    pool: &MediaPool,
    host: &dyn Transcripts,
    from: f64,
    to: f64,
    faces: bool,
) -> Detections {
    let frame = [seq.resolution.width, seq.resolution.height];
    let mut out = Detections::default();
    for (ti, track) in seq
        .tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.visible && t.kind == TrackKind::Video)
    {
        for c in track.clips.iter().filter(|c| c.enabled) {
            let lo = from.max(c.timeline_in.to_secs_f64());
            let hi = to.min(c.timeline_out.to_secs_f64());
            if hi <= lo {
                continue;
            }
            let Some(item) = c.media_id().and_then(|id| pool.get(&id)) else {
                continue;
            };
            let samples = if faces {
                host.faces(item)
            } else {
                host.people(item)
            };
            for (source, rect) in samples {
                let start = c.timeline_in.to_secs_f64();
                let end_source = c.source_secs_at(hi - start, seq.fps.as_f64());
                let start_source = c.source_secs_at(lo - start, seq.fps.as_f64());
                if source < start_source.min(end_source) || source > start_source.max(end_source) {
                    continue;
                }
                let at = start + c.elapsed_for_source_secs(source, seq.fps.as_f64());
                if at < lo || at >= hi {
                    continue;
                }
                let Some(g) = host.picture_geometry(seq, c, at) else {
                    out.incomplete = true;
                    continue;
                };
                let Some(r) = placed(rect, size(pool, c, frame), frame, g) else {
                    continue;
                };
                let mut regions = vec![r];
                for above in seq.tracks[..ti].iter().filter(|t| t.visible) {
                    for oc in above.clips.iter().filter(|oc| {
                        oc.enabled
                            && oc.timeline_in.to_secs_f64() <= at
                            && oc.timeline_out.to_secs_f64() > at
                    }) {
                        let Some(og) = host.picture_geometry(seq, oc, at) else {
                            // An unsupported foreground video might hide this
                            // detection. Do not call it a definite visible face.
                            if above.kind == TrackKind::Video {
                                out.incomplete = true;
                                regions.clear();
                            }
                            continue;
                        };
                        if !og.opaque
                            || og.opacity < 0.999
                            || og.placement.is_some_and(|p| p.4.abs() >= 1e-5)
                        {
                            continue;
                        }
                        if let Some(item) = oc.media_id().and_then(|id| pool.get(&id)) {
                            // Explicitly recognised opaque formats only. Unknown or
                            // alpha-bearing footage cannot prove full occlusion.
                            if !item.pixel_format.as_deref().is_some_and(|pf| {
                                matches!(
                                    pf,
                                    "yuv420p"
                                        | "yuv422p"
                                        | "yuv444p"
                                        | "yuv420p10le"
                                        | "yuv422p10le"
                                        | "yuv444p10le"
                                        | "nv12"
                                        | "rgb24"
                                        | "bgr24"
                                )
                            }) {
                                continue;
                            }
                        }
                        if let Some(cover) =
                            placed([0.0, 0.0, 1.0, 1.0], size(pool, oc, frame), frame, og)
                        {
                            regions = regions
                                .into_iter()
                                .flat_map(|r| subtract(r, cover))
                                .collect();
                        }
                    }
                }
                out.samples
                    .extend(regions.into_iter().map(|rect| Detection {
                        track: ti,
                        at,
                        rect,
                    }));
            }
        }
    }
    out
}

pub fn title_overlap(
    seq: &Sequence,
    host: &dyn Transcripts,
    title: &Clip,
    sample: Detection,
    margin: f32,
) -> Option<bool> {
    // A title underneath the detected picture cannot cover its face.
    if seq
        .tracks
        .iter()
        .position(|t| t.clips.iter().any(|c| c.id == title.id))
        .is_some_and(|i| sample.track < i)
    {
        return Some(false);
    }
    let td = title.title_data()?;
    let g = host.picture_geometry(seq, title, sample.at)?;
    let local = sample.at - title.timeline_in.to_secs_f64();
    let duration = (title.timeline_out - title.timeline_in).to_secs_f64();
    let frame = [seq.resolution.width, seq.resolution.height];
    Some(
        host.title_regions(td, seq.resolution, local, duration)
            .into_iter()
            .filter_map(|r| placed(r, frame, frame, g))
            .any(|r| crate::ops::boxes_overlap(r, sample.rect, margin)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Resolution, Timecode};
    use klipp_timeline::{ClipKind, Track};

    fn tc(s: f64) -> Timecode {
        Timecode::from_secs_f64(s, 1000)
    }
    struct Host {
        geometry: Geometry,
        samples: Vec<(f64, [f32; 4])>,
    }
    impl Transcripts for Host {
        fn words(&self, _: &klipp_media::MediaItem) -> Option<Vec<crate::ops::SpokenWord>> {
            None
        }
        fn faces(&self, _: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
            self.samples.clone()
        }
        fn picture_geometry(&self, _: &Sequence, clip: &Clip, _: f64) -> Option<Geometry> {
            Some(if clip.name.as_deref() == Some("cover") {
                Geometry {
                    opaque: true,
                    ..Default::default()
                }
            } else if clip.title_data().is_some() {
                Geometry::default()
            } else {
                self.geometry
            })
        }
        fn title_regions(
            &self,
            _: &klipp_timeline::TitleData,
            _: Resolution,
            local: f64,
            _: f64,
        ) -> Vec<[f32; 4]> {
            // A moving title occupies the face only at 2 seconds.
            vec![[if local < 2.0 { 0.7 } else { 0.1 }, 0.1, 0.1, 0.1]]
        }
    }
    fn scene() -> (Sequence, MediaPool, Host) {
        let mut pool = MediaPool::new();
        let mut item =
            klipp_media::MediaItem::synthetic_for_test("/tmp/qc-geometry.mov".into(), None);
        item.resolution = Resolution {
            width: 1920,
            height: 1080,
        };
        item.pixel_format = Some("yuv420p".into());
        let id = pool.add(item);
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution {
            width: 1920,
            height: 1080,
        };
        let mut track = Track::new("Video", TrackKind::Video);
        track
            .clips
            .push(Clip::media(id, tc(0.0), tc(4.0), tc(40.0), tc(44.0)));
        seq.tracks = vec![track];
        (
            seq,
            pool,
            Host {
                geometry: Geometry::default(),
                samples: vec![(41.0, [0.1, 0.1, 0.1, 0.1])],
            },
        )
    }
    #[test]
    fn qc_inset_maps_faces_out_of_the_header_and_keeps_real_overlaps() {
        let (seq, pool, mut host) = scene();
        host.geometry.placement = Some((0.0, 0.0, 0.5, 0.5, 0.0, 0.5, 0.5));
        let face = detections(&seq, &pool, &host, 0.0, 4.0, true).samples[0];
        for (got, want) in face.rect.into_iter().zip([0.3, 0.3, 0.05, 0.05]) {
            assert!((got - want).abs() < 1e-5);
        }
        assert!(!crate::ops::boxes_overlap(
            face.rect,
            [0.0, 0.05, 1.0, 0.1],
            0.0
        ));
        assert!(crate::ops::boxes_overlap(
            face.rect,
            [0.3, 0.3, 0.1, 0.1],
            0.0
        ));
    }
    #[test]
    fn qc_frame_edges_clip_detections() {
        let g = Geometry {
            placement: Some((-288.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5)),
            ..Default::default()
        };
        let r = placed([0.1, 0.1, 0.1, 0.1], [1920, 1080], [1920, 1080], g).unwrap();
        assert_eq!(r[0], 0.0);
        assert!((r[2] - 0.05).abs() < 1e-5);
    }
    #[test]
    fn qc_source_samples_respect_trim_and_title_sample_time() {
        let (seq, pool, mut host) = scene();
        host.samples
            .extend([(39.9, [0.1; 4]), (44.0, [0.1; 4]), (42.0, [0.1; 4])]);
        let found = detections(&seq, &pool, &host, 0.0, 4.0, true);
        assert_eq!(
            found.samples.iter().map(|s| s.at).collect::<Vec<_>>(),
            vec![1.0, 2.0]
        );
        let title = Clip {
            kind: ClipKind::Title(klipp_timeline::TitleData::single_text(Default::default())),
            ..Clip::gap(tc(0.0), tc(4.0))
        };
        assert_eq!(
            title_overlap(&seq, &host, &title, found.samples[0], 0.0),
            Some(false)
        );
        assert_eq!(
            title_overlap(&seq, &host, &title, found.samples[1], 0.0),
            Some(true)
        );
        let mut behind = seq.clone();
        let mut track = Track::new("Title underneath", TrackKind::Text);
        track.clips.push(title.clone());
        behind.tracks.push(track);
        assert_eq!(
            title_overlap(&behind, &host, &title, found.samples[1], 0.0),
            Some(false)
        );
    }
    #[test]
    fn qc_only_proven_opaque_foreground_hides_faces() {
        let (mut seq, mut pool, host) = scene();
        let mut cover = seq.tracks[0].clone();
        cover.clips[0].id = klipp_core::ClipId::new();
        cover.clips[0].name = Some("cover".into());
        // No detections on the cover itself.
        cover.kind = TrackKind::Text;
        seq.tracks.insert(0, cover);
        assert!(detections(&seq, &pool, &host, 0.0, 4.0, true)
            .samples
            .is_empty());
        let id = seq.tracks[0].clips[0].media_id().unwrap();
        pool.get_mut(&id).unwrap().pixel_format = Some("rgba".into());
        assert_eq!(
            detections(&seq, &pool, &host, 0.0, 4.0, true).samples.len(),
            1
        );
        let remainder = subtract([0.0, 0.0, 1.0, 1.0], [0.25, 0.25, 0.5, 0.5]);
        assert!((remainder.iter().map(|r| r[2] * r[3]).sum::<f32>() - 0.75).abs() < 1e-6);
        assert!(remainder
            .iter()
            .all(|r| intersection(*r, [0.25, 0.25, 0.5, 0.5]).is_none()));
    }
}
