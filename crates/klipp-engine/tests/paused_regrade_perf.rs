//! How long a paused 4K HDR frame takes to regrade — what a grade slider
//! costs while parked. A probe, not a pass/fail test: run with
//! `cargo test -p klipp-engine --test paused_regrade_perf -- --ignored --nocapture`.
//!
//! The real `PlaybackEngine` runs headless on one graded 4K PQ clip, is
//! parked on a frame, and has a slider dragged the way the UI drags one:
//! `BeginNodeParamEdit`, a `ParamChange` per step on the engine's queue,
//! `EndNodeParamEdit`. Each step is timed from the change to the new
//! frame, with the node editor closed and then open (asking for a
//! thumbnail of every node); the engine's own perf samples give the
//! breakdown.

mod common;


use klipp_core::AppCommand;

#[test]
#[ignore = "perf probe: run explicitly"]
fn paused_4k_pq_regrade() {
    // The viewer at full 4K ("Max playback resolution" 2160), or at the
    // height given in KLIPP_PROBE_HEIGHT.
    let height = std::env::var("KLIPP_PROBE_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(2160);
    klipp_engine::video_decoder::set_max_playback_height(height);
    let Some(src) = common::source("pq", 3840, 2160, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start(src, 3840, 2160);

    let drag = |label: &str| {
        parked.begin_drag();
        let steps: Vec<f64> = (0..16).filter_map(|i| parked.step(0.5 + i as f32 * 0.05)).collect();
        let release = parked.end_drag();
        parked.settle();
        for s in parked.perf().iter().rev().take(3).rev() {
            eprintln!(
                "  engine: {}x{} total {:6.1} ms | grade {:5.1} fx {:4.1} lut {:4.1} composite {:5.1} | encode {:5.1} scope {:4.1}",
                s.width, s.height, s.total_ms, s.grade_ms, s.builtin_ms, s.lut_ms, s.composite_ms, s.reencode_ms, s.scope_ms,
            );
        }
        let mut sorted = steps[1..].to_vec(); // the first step pays for the drag's setup
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "{label}: slider step to new frame median {:.1} ms (best {:.1}, worst {:.1}); release to full-res frame {release:.1?} ms",
            sorted[sorted.len() / 2], sorted[0], sorted[sorted.len() - 1],
        );
    };
    drag("node editor closed");
    // The node editor open: it asks for a thumbnail of every node but
    // Output, on every paused frame.
    parked.send(AppCommand::SetVisibleNodeThumbs {
        request: 0,
        clip_id: parked.clip_id,
        at: [parked.source_node, parked.exposure_node].into_iter().map(|n| klipp_compose::inspect::grade_node(parked.clip_id, n)).collect(),
    });
    parked.settle();
    drag("node editor open");
}

/// What a slider step costs below a heavy node (#901): the parked 4K PQ
/// frame graded by an Exposure node, with temporal noise reduction on the
/// clip (run at the source, before the grade, reading the frames either
/// side) or an OFX node between the source and the Exposure node, and
/// both. Whether a cache of the picture above the edited node would pay:
/// it would, where a step here is well over a 60 fps frame (16.7 ms).
///
/// The OFX node is the repository's C fixture (`org.klipp.fixture.channels`,
/// an 8-bit channel swap), compiled into a bundle in a scratch folder and
/// found through `OFX_PLUGIN_PATH`: what it measures is the host's round
/// trip — the picture leaving the GPU chain, converted for the plug-in and
/// back — not a real plug-in's own work, which comes on top. Needs a C
/// compiler; without one the OFX cases are skipped and said so.
#[test]
#[ignore = "perf probe: run explicitly"]
fn paused_4k_regrade_below_heavy_nodes() {
    let height = std::env::var("KLIPP_PROBE_HEIGHT").ok().and_then(|v| v.parse().ok()).unwrap_or(2160);
    klipp_engine::video_decoder::set_max_playback_height(height);
    let Some(src) = common::source("pq", 3840, 2160, common::PQ_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let ofx = common::ofx_fixture_bundle();
    match &ofx {
        // Read once, when the engine discovers its plug-ins, which the
        // first `Parked` below does; no other thread reads it meanwhile.
        Some(dir) => std::env::set_var("OFX_PLUGIN_PATH", dir),
        None => eprintln!("no C compiler: the OFX cases are skipped"),
    }
    let tnr = |clip: &mut klipp_timeline::Clip| {
        *clip = klipp_timeline::test_clip::TestClip::of(clip.clone())
            .effect(klipp_effects::BuiltinEffect::TemporalDenoise { strength: 0.8, threshold: 0.05 })
            .build();
    };
    let ofx_node = |clip: &mut klipp_timeline::Clip| {
        use klipp_timeline::GradeAccess;
        let mut g = clip.grade().clone();
        let id = klipp_nodes::NodeId::new_v4();
        let params = klipp_nodes::NodeParams {
            ofx: Some(klipp_nodes::plugin::OfxNodeParams { plugin: "org.klipp.fixture.channels".into(), values: Default::default(), filter: true }),
            ..Default::default()
        };
        g.nodes.insert(1, klipp_nodes::Node { id, kind: klipp_nodes::NodeKind::Ofx, label: "OFX".into(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] });
        // Source → OFX → Exposure, where it was Source → Exposure.
        let (source, exposure) = (g.nodes[0].id, g.nodes[2].id);
        g.edges.retain(|e| !(e.from_node == source && e.to_node == exposure));
        g.edges.push(klipp_nodes::Edge { from_node: source, from_port: 0, to_node: id, to_port: 0 });
        g.edges.push(klipp_nodes::Edge { from_node: id, from_port: 0, to_node: exposure, to_port: 0 });
        g.node_positions.insert(id, klipp_nodes::NodePos { x: 50.0, y: 0.0 });
        clip.set_grade(g);
    };
    type Tweak = Box<dyn Fn(&mut klipp_timeline::Clip)>;
    let mut cases: Vec<(&str, Tweak)> = vec![("plain", Box::new(|_: &mut klipp_timeline::Clip| {})), ("temporal NR", Box::new(tnr))];
    if ofx.is_some() {
        cases.push(("OFX node", Box::new(ofx_node)));
        cases.push(("temporal NR + OFX node", Box::new(move |c: &mut klipp_timeline::Clip| { tnr(c); ofx_node(c) })));
    }
    split_temporal_nr(&src);
    for (label, tweak) in cases {
        let parked = common::Parked::start_with(src.clone(), 3840, 2160, |c| tweak(c));
        parked.begin_drag();
        let steps: Vec<f64> = (0..16).filter_map(|i| parked.step(0.5 + i as f32 * 0.05)).collect();
        let release = parked.end_drag();
        parked.settle();
        if let Some(s) = parked.perf().last() {
            eprintln!(
                "  engine: {}x{} total {:6.1} ms | grade {:5.1} fx {:4.1} lut {:4.1} composite {:5.1} | encode {:5.1} scope {:4.1}",
                s.width, s.height, s.total_ms, s.grade_ms, s.builtin_ms, s.lut_ms, s.composite_ms, s.reencode_ms, s.scope_ms,
            );
        }
        let mut sorted = steps[1..].to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "{label}: slider step to new frame median {:.1} ms (best {:.1}, worst {:.1}); release {release:.1?} ms",
            sorted[sorted.len() / 2], sorted[0], sorted[sorted.len() - 1],
        );
    }
}

/// The temporal NR step taken apart (#1863), as the viewer runs it on
/// each step: a reader opened on the file, the frames either side of the
/// parked one decoded into linear light, then the denoise over 4K. The
/// median of five, each from a fresh reader as a step's is.
fn split_temporal_nr(src: &std::path::Path) {
    let working = klipp_engine::video_decoder::working_colorspace();
    let t = 10.0 / common::FPS as f64;
    let dt = 1.0 / common::FPS as f64;
    let ms = |i: std::time::Instant| i.elapsed().as_secs_f64() * 1000.0;
    let (mut open, mut fetch, mut compute) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..5 {
        let t0 = std::time::Instant::now();
        let Some(mut reader) = klipp_media::frame_reader::SeekableFrameReader::open_linear(src, None, working) else {
            eprintln!("temporal NR split: the file would not open");
            return;
        };
        open.push(ms(t0));
        // In the order a step asks: the frame before, then the one after.
        let t1 = std::time::Instant::now();
        let before = reader.decode_at(t - dt).expect("the frame before").0;
        let after = reader.decode_at(t + dt).expect("the frame after").0;
        fetch.push(ms(t1));
        let current = reader.decode_at(t).expect("the parked frame").0;
        let t2 = std::time::Instant::now();
        let out = klipp_effects::temporal_denoise::denoise_frame(&current, &[&before, &after], 0.05, 0.8, klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::Rec709Linear));
        compute.push(ms(t2));
        assert_eq!(out.len(), current.len());
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    eprintln!(
        "temporal NR split: reader open {:.1} ms, two neighbours decoded {:.1} ms, denoise {:.1} ms",
        median(open), median(fetch), median(compute),
    );
}


/// A slider step on a parked 4K frame with a title over the clip (#2351):
/// the frame takes the GPU stack, which grades the clip and composites
/// the title, against the parked regrade with no title. Each with an SDR
/// and a PQ source.
#[test]
#[ignore = "perf probe: run explicitly"]
fn paused_4k_regrade_under_a_title() {
    klipp_engine::video_decoder::set_max_playback_height(2160);
    let title = || klipp_timeline::TitleData::single_text_fx(
        klipp_timeline::TextBox { text: "Over the grade".into(), font_size: 90.0, color: [240, 220, 120, 255], ..Default::default() },
        klipp_timeline::LayerFx { glows: vec![klipp_timeline::TextGlow { enabled: true, color: [80, 150, 255, 220], radius: 10.0, radius_keyframes: vec![], intensity: 0.8, intensity_keyframes: vec![] }], ..Default::default() },
    );
    for (label, tags) in [("sdr", common::SDR_TAGS), ("pq", common::PQ_TAGS)] {
        let Some(src) = common::source(label, 3840, 2160, tags) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        for titled in [false, true] {
            let parked = if titled { common::Parked::start_under_title(src.clone(), 3840, 2160, title()) } else { common::Parked::start(src.clone(), 3840, 2160) };
            parked.begin_drag_seen(0.25);
            let steps: Vec<f64> = (0..16).filter_map(|i| parked.step(0.5 + i as f32 * 0.05)).collect();
            parked.end_drag();
            parked.settle();
            for p in parked.perf().iter().rev().skip(1).take(2) {
                eprintln!("PROBE   engine: total {:5.1} | decode {:4.1} grade {:5.1} fx {:4.1} lut {:4.1} composite {:5.1} encode {:5.1} scope {:4.1} | phases {:?}", p.total_ms, p.decode_ms, p.grade_ms, p.builtin_ms, p.lut_ms, p.composite_ms, p.reencode_ms, p.scope_ms, p.phases);
            }
            let mut sorted = steps[1..].to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            eprintln!(
                "PROBE {label} {}: slider step median {:.1} ms (best {:.1}, worst {:.1})",
                if titled { "under a title (GPU stack)" } else { "no title (parked regrade)" },
                sorted[sorted.len() / 2], sorted[0], sorted[sorted.len() - 1],
            );
        }
    }
}
