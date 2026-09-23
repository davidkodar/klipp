use klipp_analysis::{beats::BeatData, beatsheet, motion::Motion, shots::Shots};
use klipp_control::{style_cut_summary, style_pacing_evidence};

#[test]
fn continuous_change_is_not_a_slow_cut_recipe() {
    let motion = Motion { duration_secs: 12.0, per_second: vec![3.0; 12], mean: 3.0, p95: 3.0 };
    let shots = Shots::from_boundaries(12.0, vec![]);
    let sheet = beatsheet::build(12.0, Some(&shots), &[]);
    let evidence = style_pacing_evidence(&[], Some(&motion), Some(&sheet));
    assert_eq!(evidence["within_shot_sample_seconds"], 12);
    assert!(evidence["guidance"].as_str().unwrap().contains("change continues inside"));
    assert_eq!(evidence["hold_evidence"], "no_settled_intervals_detected");
    assert!(!evidence["guidance"].as_str().unwrap().contains("Cut slow"));
}

#[test]
fn cut_changes_do_not_count_as_within_shot_motion_and_holds_are_reported() {
    let motion = Motion { duration_secs: 6.0, per_second: vec![0.1, 20.0, 20.0, 0.1, 0.1, 0.1], mean: 6.7, p95: 20.0 };
    let shots = Shots::from_boundaries(6.0, vec![2.0]);
    let sheet = beatsheet::build(6.0, Some(&shots), &[(3.0, 6.0)]);
    let evidence = style_pacing_evidence(&shots.boundaries, Some(&motion), Some(&sheet));
    assert_eq!(evidence["within_shot_motion_class"], "static");
    assert_eq!(evidence["within_shot_sample_seconds"], 4);
    assert_eq!(evidence["hold_evidence"], "detected");
    assert!(!evidence["detected_holds"].as_array().unwrap().is_empty());
    let cuts = style_cut_summary(&[(0.0, 2.0), (2.0, 6.0)], &[2.0], 6.0,
        &BeatData { bpm: 0.0, confidence: 0.0, beat_times: vec![] });
    assert_eq!(cuts["shot_count"], 2);
    assert_eq!(cuts["mean_shot_secs"], 3.0);
}

#[test]
fn absent_analysis_is_not_low_motion_or_a_hold() {
    let evidence = style_pacing_evidence(&[], None, None);
    assert!(evidence["within_shot_mean_ydif"].is_null());
    assert!(evidence["detected_holds"].is_null());
    assert_eq!(evidence["hold_evidence"], "unavailable");
    assert!(evidence["guidance"].as_str().unwrap().contains("evidence is unavailable"));
}

#[test]
fn synthetic_reveals_with_few_cuts_do_not_prescribe_long_holds() {
    // Original procedural typography: a travelling entrance, then two
    // reveals within one continuous scene. No reference-media dependency.
    let source = "color=c=black:s=320x180:r=30:d=6,drawtext=text='REVEAL':x='w-t*60':y=35:fontsize=28:fontcolor=white,drawtext=text='BUILD':x=20:y=95:fontsize=28:fontcolor=white:enable='gte(t,2)',drawtext=text='LAUNCH':x=170:y=135:fontsize=24:fontcolor=orange:enable='gte(t,4)'";
    let read = |filter: &str| {
        let output = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-f", "lavfi", "-i", source,
                "-vf", filter, "-f", "null", "-"])
            .output().expect("ffmpeg is a build/test prerequisite");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
    };
    let motion = klipp_analysis::motion::parse_metadata(&read("signalstats,metadata=print:file=-"), 6.0);
    let boundaries = klipp_analysis::shots::parse_showinfo(&read("select='gt(scene,0.3)',showinfo"));
    assert!(boundaries.len() <= 1, "continuous scene: {boundaries:?}");
    assert!(motion.mean > 0.0);
    let evidence = style_pacing_evidence(&boundaries, Some(&motion), None);
    let guidance = evidence["guidance"].as_str().unwrap();
    assert!(guidance.contains("not target hold lengths or animation timings"));
    assert!(!guidance.contains("aim for shots"));
    assert!(!guidance.contains("Cut slow"));
    assert!(evidence["within_shot_sample_seconds"].as_u64().unwrap() >= 4);
}
