//! Fractional-rate checkpoint sampling must address the same timeline time on
//! every route as an equivalent integral-rate checkpoint (#1557).
use klipp_core::{Fps, GeneratorData, Resolution, Timecode};
use klipp_render::pixel_check::{default_renderer, differ, render_sequence, settings, Options};
use klipp_render::SequenceSnapshot;
use klipp_timeline::{Clip, GradeAccess, Sequence, Track, TrackKind};

#[test]
fn a_fractional_frame_draws_its_time_on_every_checkpoint_route() {
    let root =
        std::env::temp_dir().join(format!("klipp-pixel-fractional-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let time = |f: i64| Timecode {
        value: f * 1001,
        rate: 30_000,
    };
    let mut sequence = Sequence::new_default();
    sequence.fps = Fps::new(30_000, 1001);
    sequence.resolution = Resolution {
        width: 64,
        height: 36,
    };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips.push(Clip {
        kind: klipp_timeline::ClipKind::Generator(GeneratorData::LivingGradient {
            a: [200, 40, 30, 255],
            b: [20, 60, 220, 255],
            angle_deg: 30.0,
            drift_deg_per_sec: 90.0,
            glow: 0.4,
        }),
        source_in: time(0),
        source_out: time(61),
        ..Clip::gap(time(0), time(61))
    });
    track.clips[0].edit_grade(|g| {
        let corrector = g.ensure_corrector();
        g.nodes
            .iter_mut()
            .find(|n| n.id == corrector)
            .unwrap()
            .params
            .exposure = Some(0.3);
    });
    sequence.tracks = vec![track];
    let fractional = SequenceSnapshot::from_sequence_with_all(
        &sequence,
        std::slice::from_ref(&sequence),
        &klipp_media::MediaPool::default(),
        &settings(),
        None,
        false,
        Default::default(),
    )
    .unwrap();
    // The authored timeline is unchanged. 60 * 1001 / 30000 and
    // 60060 / 30000 address exactly the same time (2.002 seconds).
    let mut integral = fractional.clone();
    integral.fps_den = 1;
    integral.fps = 30_000.0;
    integral.duration_frames *= 1001;
    let draw = |snapshot: &SequenceSnapshot, frame, name: &str| {
        render_sequence(
            snapshot,
            &root.join(name),
            name,
            &root.join(format!("{name}.mp4")),
            &Options {
                width: 64,
                frames: Some(vec![frame]),
            },
            &default_renderer,
        )
        .unwrap()
    };
    let a = draw(&fractional, 60, "fractional");
    let b = draw(&integral, 60_060, "integral");
    assert_eq!(a.frames, b.frames);
    assert_eq!(a.frames, 4, "CPU, GPU, stack and export must be checked");
    assert_eq!(a.stack, 1, "the viewer stack must draw the frame");
    assert_eq!(a.export_gpu, 1, "the export's GPU planes must be checked");
    let mut compared = 0;
    for file in std::fs::read_dir(root.join("fractional")).unwrap() {
        let path = file.unwrap().path();
        if path.extension().is_some_and(|x| x == "png") {
            let name = path.file_name().unwrap().to_str().unwrap();
            let expected = root
                .join("integral")
                .join(name.replacen("60-", "60060-", 1));
            let (off, _) = differ(&path, &expected).unwrap();
            assert!(
                off <= 1,
                "{name}: frame 60 at 30000/1001 differs by {off} from 2.002 seconds"
            );
            compared += 1;
        }
    }
    assert_eq!(compared, a.frames);
    std::fs::remove_dir_all(root).unwrap();
}
