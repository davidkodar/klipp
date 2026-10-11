//! Saved-project corpus regression: native-size temporal neighbours must be
//! fitted before a reduced-resolution render, including both media edges.
use klipp_core::{Fps, MediaId, Resolution, Timecode};
use klipp_effects::BuiltinEffect;
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_render::pixel_check::{
    default_renderer, differ, differ_route, render_sequence, settings, Options,
};
use klipp_render::SequenceSnapshot;
use klipp_timeline::{Clip, EffectEdit, Sequence};

#[test]
fn saved_temporal_clip_exports_at_another_size_and_both_media_edges() {
    let root = std::env::temp_dir().join(format!("klipp-temporal-size-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let media = root.join("noise.mkv");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i",
            "nullsrc=s=640x360:r=24,geq=lum='80+8*mod(N,3)+8*mod(X+Y+N,2)':cb=128:cr=128,setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709",
            "-frames:v", "12", "-c:v", "ffv1", "-pix_fmt", "yuv420p"])
        .arg(&media).output().expect("FFmpeg is required for the saved-project regression");
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let tc = |f| Timecode::from_frames(f, 24);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        path: media,
        kind: MediaItemKind::File,
        resolution: Resolution {
            width: 640,
            height: 360,
        },
        video_codec: "ffv1".into(),
        ..MediaItem::new_synthetic_title(id, "noise".into(), tc(12), Fps::film_24())
    });
    let mut sequence = Sequence::new_default();
    sequence.resolution = Resolution {
        width: 640,
        height: 360,
    };
    let mut clip = Clip::media(id, tc(0), tc(12), tc(0), tc(12));
    clip.apply_effect_edits(&[EffectEdit::Add(&BuiltinEffect::TemporalDenoise {
        strength: 1.0,
        threshold: 0.15,
    })])
    .unwrap()
    .unwrap();
    sequence.tracks[0].clips.push(clip);
    let project = root.join("temporal.klipp");
    klipp_project::save_project(&project, &[sequence], &pool).unwrap();
    let (sequences, pool, colour, ..) = klipp_project::load_project_strict(&project).unwrap();
    let snapshot = SequenceSnapshot::from_sequence_with_all(
        &sequences[0],
        &sequences,
        &pool,
        &settings(),
        None,
        false,
        colour.working_colorspace,
    )
    .unwrap();
    let options = Options {
        width: 256,
        frames: Some(vec![0, 1, 6, 11]),
    };
    let active = render_sequence(
        &snapshot,
        &root.join("active"),
        "temporal",
        &root.join("probe.mp4"),
        &options,
        &default_renderer,
    )
    .unwrap_or_else(|e| panic!("{e}; corpus retained at {}", root.display()));
    assert_eq!(
        active.export_cpu + active.export_gpu,
        4,
        "every sampled frame must export"
    );
    assert_eq!(active.plan_fallbacks, 0);
    assert!(
        !active
            .declines
            .iter()
            .any(|d| d.contains("neighbour frame missing")),
        "{:?}",
        active.declines
    );
    for frame in [0, 1, 6, 11] {
        let cpu = root.join("active").join(format!("{frame}-cpu.png"));
        let export = root.join("active").join(format!("{frame}-export.png"));
        let (off, _) = if export.is_file() {
            differ(&cpu, &export)
        } else {
            differ_route(
                &cpu,
                &root.join("active").join(format!("{frame}-export-yuv.png")),
            )
        }
        .unwrap();
        assert!(
            off <= 1,
            "frame {frame}: export differs from the denoised picture by {off} levels"
        );
    }
    let mut bypassed = snapshot.clone();
    bypassed.video.sequence.tracks[0].clips[0]
        .apply_effect_edits(&[EffectEdit::SetEnabled(0, false)])
        .unwrap()
        .unwrap();
    render_sequence(
        &bypassed,
        &root.join("bypassed"),
        "control",
        &root.join("probe.mp4"),
        &options,
        &default_renderer,
    )
    .unwrap();
    for frame in [0, 6, 11] {
        let name = format!("{frame}-cpu.png");
        let (off, _) = differ(
            &root.join("active").join(&name),
            &root.join("bypassed").join(&name),
        )
        .unwrap();
        assert!(
            off > 1,
            "frame {frame}: denoise must use its available neighbours, not silently bypass ({off})"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
