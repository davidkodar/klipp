use super::*;
use klipp_color::measure::LogPrior;
use std::sync::{Arc, Mutex, RwLock};

#[test]
fn isolated_measurement_uses_its_own_media_metadata() {
    let root = std::env::temp_dir().join(format!("klipp-log-prior-{}", uuid::Uuid::new_v4()));
    let mut pool = klipp_media::MediaPool::new();
    let overlay = pool.add(klipp_media::MediaItem::synthetic_for_test(
        root.join("overlay.mov"),
        None,
    ));
    let mut item = klipp_media::MediaItem::synthetic_for_test(root.join("aerial.mp4"), None);
    item.color_transfer = Some("bt709".into());
    item.bit_depth = Some(8);
    item.pixel_format = Some("yuv420p".into());
    let aerial = pool.add(item);
    let mut sequence = klipp_timeline::Sequence::new_default();
    sequence.tracks.clear();
    let time = |frames| klipp_core::Timecode::from_frames(frames, 24);
    let mut make_track = |name, media| {
        let mut track = klipp_timeline::Track::new(name, klipp_timeline::TrackKind::Video);
        track.clips.push(klipp_timeline::Clip::media(
            media,
            time(0),
            time(48),
            time(0),
            time(48),
        ));
        sequence.tracks.push(track);
    };
    make_track("Overlay", overlay);
    make_track("Footage", aerial);
    let isolated = sequence.tracks[1].clips[0].id;
    let fb = Arc::new(Mutex::new(klipp_engine::FrameBuffer::new(8, 8)));
    let (commands, receiver) =
        crossbeam_channel::unbounded::<klipp_document::control::ControlEnvelope>();
    let frame_buffer = fb.clone();
    // Deliver the frame synchronously when the real command path wakes its
    // host. No engine, model, socket, sleeps or scheduling assumptions needed.
    let wake_ui = Arc::new(move || {
        while let Ok(envelope) = receiver.try_recv() {
            for command in envelope.commands {
                if let AppCommand::SeekTo(at) = command {
                    let mut frame = frame_buffer.lock().unwrap();
                    frame.write_frame([90, 110, 130, 255].repeat(64), 8, 8);
                    frame.set_presented(at, true);
                }
            }
            if let Some(ack) = envelope.ack {
                ack.send(klipp_document::control::ControlAck {
                    revision: 0,
                    refused: None,
                    refused_op: None,
                    renamed: Vec::new(),
                    ungated: Vec::new(),
                    picture_refused: None,
                })
                .unwrap();
            }
        }
    });
    let ctx = ControlContext {
        commands,
        shared: Arc::new(klipp_core::ControlShared::default()),
        wake_ui,
        sequence: Arc::new(klipp_timeline::SharedSequence::new(sequence)),
        media_pool: Arc::new(RwLock::new(pool)),
        frame_buffer: fb,
        perf_ring: klipp_engine::perf::shared(),
        log_path: root.join("unused.log"), others: Default::default(),
    };
    assert_eq!(
        measure_at(&ctx, 1.0, None, Some(60_000)).unwrap().log_prior,
        LogPrior::Unknown
    );
    let measured = measure_at(&ctx, 1.0, Some(isolated), Some(60_000)).unwrap();
    assert_eq!(
        measured.log_prior,
        LogPrior::DisplayTagged,
        "isolated footage must not inherit the overlay's unknown format"
    );
}

/// #1896 review: only a display-referred SDR file's own frame is read
/// against the graded one; an HDR, BT.2020 or log file is on another
/// scale.
#[test]
fn only_an_sdr_file_is_read_as_its_own_floor() {
    let item = |f: &dyn Fn(&mut klipp_media::MediaItem)| {
        let mut i = klipp_media::MediaItem::synthetic_for_test("/tmp/klipp-1896/a.mov".into(), None);
        f(&mut i);
        display_sdr(&i)
    };
    assert!(item(&|_| {}));
    assert!(item(&|i| i.color_transfer = Some("bt709".into())));
    assert!(!item(&|i| i.color_transfer = Some("smpte2084".into())));
    assert!(!item(&|i| i.color_primaries = Some("bt2020".into())));
    assert!(!item(&|i| i.colour_space = klipp_media::ColourSpace::Hlg));
    assert!(!item(&|i| { i.log_format = klipp_core::LogFormat::SLog3; i.log_format_user_set = true; }));
}
