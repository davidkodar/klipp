//! A clip's graded still (#899, `still::clip_still`): its frame at a time
//! of its own, read from its file and drawn with its grade, small. The
//! source is a grey ramp, one level per frame (written with the ffmpeg
//! CLI; the test skips without it): a later time is a brighter frame, and
//! a grade that brightens makes the same frame brighter.

use klipp_core::*;
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_timeline::GradeAccess;

const FPS: u32 = 24;

/// Two seconds at 24 fps of a 64x36 grey, frame n at 20 + 4n.
fn ramp() -> Option<std::path::PathBuf> {
    static RAMP: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    RAMP.get_or_init(|| {
        let path = klipp_testing::scratch::scratch_file("klipp-graded-still", "ramp.mkv");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c=black:s=64x36:r={FPS}:d=2,geq=lum='20+4*N':cb=128:cr=128"))
            .args(["-c:v", "ffv1", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(path)
    })
    .clone()
}

fn mean(still: &klipp_render::still::Still) -> f64 {
    still.rgba.chunks_exact(4).map(|p| p[1] as f64).sum::<f64>() / (still.width * still.height) as f64
}

#[test]
fn a_clips_still_is_its_frame_with_its_grade() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
    item.resolution = Resolution { width: 64, height: 36 };
    item.fps = Fps::new(FPS, 1);
    item.duration = RationalTime::from_frames(48, FPS);
    let id = item.id;
    let mut pool = klipp_media::MediaPool::new();
    pool.add(item);
    let tc = |f| RationalTime::from_frames(f, FPS);
    // A trimmed clip, placed late on the timeline: its own time is read.
    let plain = klipp_timeline::Clip::media(id, tc(100), tc(140), tc(4), tc(44));
    let still = |clip: &klipp_timeline::Clip, at: f64| klipp_render::still::clip_still(clip, &pool, at, 32, WorkingColorspace::Rec709Linear).expect("a still");

    let early = still(&plain, 0.0);
    assert_eq!((early.width, early.height), (32, 18), "the media's aspect, 32 wide");
    let late = still(&plain, 1.5);
    assert!(mean(&late) > mean(&early) + 20.0, "a later frame of the ramp is brighter: {} then {}", mean(&early), mean(&late));

    let mut graded = plain.clone();
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == corrector && n.kind == NodeKind::Corrector).unwrap().params.exposure = Some(1.0);
    graded.set_grade(g);
    assert!(mean(&still(&graded, 0.0)) > mean(&early) + 10.0, "a stop up brightens the still: {} against {}", mean(&still(&graded, 0.0)), mean(&early));

    let title = klipp_timeline::Clip::gap(tc(0), tc(10));
    assert!(klipp_render::still::clip_still(&title, &pool, 0.0, 32, WorkingColorspace::Rec709Linear).is_err(), "only a media clip has a still");
}

/// #1913: a still of a clip whose grade has a Mask wired in half way
/// (Source into the Mask, the Mask into the Corrector) is a whole
/// picture, `width × height` of it, or an error: never a buffer of
/// another size, which the thumbnail code then panicked on.
#[test]
fn a_half_wired_mask_gives_a_whole_still_or_an_error() {
    let Some(path) = ramp() else {
        eprintln!("skipping: ffmpeg could not write the source");
        return;
    };
    let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
    item.resolution = Resolution { width: 64, height: 36 };
    item.fps = Fps::new(FPS, 1);
    item.duration = RationalTime::from_frames(48, FPS);
    let id = item.id;
    let mut pool = klipp_media::MediaPool::new();
    pool.add(item);
    let tc = |f| RationalTime::from_frames(f, FPS);
    let clip = klipp_timeline::Clip::media(id, tc(0), tc(40), tc(0), tc(40));
    // The ways a Mask is met half wired: Source into it, it into the
    // Corrector's picture or its matte, it with nothing in, it straight
    // into the Output.
    for variant in 0..12 {
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("a Source").id;
    let output = g.output_node_id;
    let mask = klipp_nodes::Node {
        id: klipp_nodes::NodeId::new_v4(),
        kind: NodeKind::Mask,
        label: "Mask".into(),
        enabled: true,
        params: Default::default(),
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    };
    let mask_id = mask.id;
    g.nodes.push(mask);
    let edge = |from_node, to_node, to_port| klipp_nodes::Edge { from_node, from_port: 0, to_node, to_port };
    match variant {
        0 => {
            g.edges.push(edge(source, mask_id, 0));
            for e in &mut g.edges {
                if e.to_node == corrector && e.from_node == source {
                    e.from_node = mask_id;
                }
            }
        }
        1 => {
            g.edges.push(edge(source, mask_id, 0));
            g.edges.push(edge(mask_id, corrector, 1));
        }
        2 => {
            for e in &mut g.edges {
                if e.to_node == corrector && e.from_node == source {
                    e.from_node = mask_id;
                }
            }
        }
        3 => {
            g.edges.push(edge(source, mask_id, 0));
            for e in &mut g.edges {
                if e.to_node == output {
                    e.from_node = mask_id;
                }
            }
        }
        4 => {
            g.edges.push(edge(mask_id, corrector, 1));
        }
        // Other matte makers, straight from the Source into the Output's
        // picture, and with the Mask's matte gating them.
        v => {
            let kind = [NodeKind::Qualifier, NodeKind::LumaKey, NodeKind::CombineMattes, NodeKind::DifferenceKey, NodeKind::ApplyMatte, NodeKind::MaskCutout, NodeKind::ExternalMatte][v - 5];
            let id = klipp_nodes::NodeId::new_v4();
            g.nodes.push(klipp_nodes::Node { id, kind, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() });
            g.edges.push(edge(source, id, 0));
            g.edges.push(edge(source, mask_id, 0));
            for e in &mut g.edges {
                if e.to_node == corrector && e.from_node == source {
                    e.from_node = id;
                }
            }
        }
    }
    // Then the variant's graph inside a Compound node of the clip's
    // grade, as a group made in the node editor is: the crash's case.
    for in_group in [false, true] {
    let g = g.clone();
    let g = if in_group {
        let mut outer = NodeGraph::default();
        let source_o = outer.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
        let out_o = outer.output_node_id;
        let compound = klipp_nodes::Node {
            id: klipp_nodes::NodeId::new_v4(),
            kind: NodeKind::Compound,
            label: "Group".into(),
            enabled: true,
            params: klipp_nodes::NodeParams { compound_subgraph: Some(g), ..Default::default() },
            keyframes: Default::default(),
            output_link: None,
            input_links: Vec::new(),
        };
        let cid = compound.id;
        outer.nodes.push(compound);
        outer.edges.retain(|e| e.to_node != out_o);
        outer.edges.push(klipp_nodes::Edge { from_node: source_o, from_port: 0, to_node: cid, to_port: 0 });
        outer.edges.push(klipp_nodes::Edge { from_node: cid, from_port: 0, to_node: out_o, to_port: 0 });
        outer
    } else {
        g
    };
    let mut clip = clip.clone();
    clip.set_grade(g);
    match klipp_render::still::clip_still(&clip, &pool, 0.0, 32, WorkingColorspace::Rec709Linear) {
        Ok(still) => assert_eq!(still.rgba.len(), (still.width * still.height * 4) as usize, "variant {variant}, in a group {in_group}: a whole picture"),
        Err(why) => eprintln!("variant {variant}: no still, as it should say: {why}"),
    }
    }
    }
}
