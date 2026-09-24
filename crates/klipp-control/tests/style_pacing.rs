use klipp_analysis::{beats::BeatData, beatsheet, motion::Motion, shots::Shots};
use klipp_control::{style_cut_summary, style_pacing_evidence};

fn pacing(boundaries: &[f64], motion: Option<&Motion>, sheet: Option<&beatsheet::BeatSheet>) -> serde_json::Value {
    style_pacing_evidence(&Shots::from_boundaries(100.0, boundaries.to_vec()), motion, sheet)
}

fn measured(values: Vec<f32>) -> Motion { Motion::from_samples(values.len() as f64, values.into_iter().map(Some).collect()) }

#[test]
fn continuous_change_is_not_a_slow_cut_recipe() {
    let motion = measured(vec![3.0; 12]);
    let shots = Shots::from_boundaries(12.0, vec![]);
    let sheet = beatsheet::build(12.0, Some(&shots), &[]);
    let evidence = pacing(&[], Some(&motion), Some(&sheet));
    assert_eq!(evidence["within_shot_sample_seconds"], 12);
    assert!(evidence["guidance"].as_str().unwrap().contains("change continues inside"));
    assert_eq!(evidence["hold_evidence"], "no_settled_intervals_detected");
    assert!(!evidence["guidance"].as_str().unwrap().contains("Cut slow"));
}

#[test]
fn cut_changes_do_not_count_as_within_shot_motion_and_holds_are_reported() {
    let motion = measured(vec![0.1, 0.1, 20.0, 0.1, 0.1, 0.1]);
    let shots = Shots::from_boundaries(6.0, vec![2.0]);
    let sheet = beatsheet::build(6.0, Some(&shots), &[(3.0, 6.0)]);
    let evidence = pacing(&shots.boundaries, Some(&motion), Some(&sheet));
    assert_eq!(evidence["within_shot_motion_class"], "static");
    assert_eq!(evidence["within_shot_sample_seconds"], 5);
    assert_eq!(evidence["hold_evidence"], "detected");
    assert!(!evidence["detected_holds"].as_array().unwrap().is_empty());
    let cuts = style_cut_summary(&[(0.0, 2.0), (2.0, 6.0)], &[2.0], 6.0,
        &BeatData { bpm: 0.0, confidence: 0.0, beat_times: vec![] });
    assert_eq!(cuts["shot_count"], 2);
    assert_eq!(cuts["mean_shot_secs"], 3.0);
}

#[test]
fn absent_analysis_is_not_low_motion_or_a_hold() {
    let evidence = pacing(&[], None, None);
    assert!(evidence["within_shot_mean_ydif"].is_null());
    assert!(evidence["detected_holds"].is_null());
    assert_eq!(evidence["hold_evidence"], "unavailable");
    assert!(evidence["guidance"].as_str().unwrap().contains("evidence is unavailable"));
}

#[test]
fn synthetic_motion_with_few_cuts_do_not_prescribe_long_holds() {
    if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
        eprintln!("skipping synthetic motion fixture: ffmpeg unavailable");
        return;
    }
    // A continuously moving test pattern needs neither fonts nor drawtext.
    let source = "testsrc2=size=160x90:rate=30:duration=6";
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
    assert!(motion.mean.is_some_and(|m| m > 0.0));
    let evidence = pacing(&boundaries, Some(&motion), None);
    let shots = Shots::from_boundaries(6.0, boundaries);
    let beats = BeatData { bpm: 0.0, confidence: 0.0, beat_times: vec![] };
    let cuts = style_cut_summary(&shots.shots.iter().map(|s| (s.start_secs, s.end_secs)).collect::<Vec<_>>(), &shots.boundaries, 6.0, &beats);
    let response = klipp_control::style_profile_response("synthetic", "fixture", cuts, &beats, serde_json::Value::Null, klipp_control::StylePacingInput { shots: &shots, motion: Some(&motion), sheet: None });
    let guidance = response["guidance"].as_str().unwrap();
    for field in ["text", "guidance", "next"] {
        let text = response[field].as_str().unwrap();
        assert!(!text.contains("aim for shots"), "{field}: {text}");
        assert!(!text.contains("Cut slow"), "{field}: {text}");
        assert!(!text.contains("target shot length"), "{field}: {text}");
    }
    assert!(guidance.contains("not target hold lengths or animation timings"));
    assert!(!guidance.contains("aim for shots"));
    assert!(!guidance.contains("Cut slow"));
    assert!(evidence["within_shot_sample_seconds"].as_u64().unwrap() >= 4);
}

#[test]
fn fast_beat_aligned_boundaries_exclude_only_their_own_bin() {
    let motion = measured(vec![9.0; 10]);
    let evidence = pacing(&[2.0, 4.0, 6.0, 8.0], Some(&motion), None);
    assert_eq!(evidence["within_shot_sample_seconds"], 6);
    assert_eq!(evidence["motion_evidence"], "measured");
}

#[test]
fn fast_cuts_are_distinct_from_missing_analysis() {
    let motion = measured(vec![9.0; 2]);
    let evidence = pacing(&[0.5, 1.5], Some(&motion), None);
    assert_eq!(evidence["motion_evidence"], "excluded_by_cuts");
    assert!(evidence["within_shot_mean_ydif"].is_null());
    assert_ne!(evidence, pacing(&[], None, None));
    assert!(evidence["guidance"].as_str().unwrap().contains("every valid one-second sample overlaps a cut"));
}

#[test]
fn complete_response_keeps_cut_statistics_descriptive() {
    let beats = BeatData { bpm: 120.0, confidence: 0.9, beat_times: vec![] };
    for motion in [None, Some(measured(vec![3.0; 12]))] {
        let cuts = style_cut_summary(&[(0.0, 12.0)], &[], 12.0, &beats);
        let response = klipp_control::style_profile_response("fixture", "id", cuts, &beats, serde_json::Value::Null, klipp_control::StylePacingInput { shots: &Shots::from_boundaries(12.0, vec![]), motion: motion.as_ref(), sheet: None });
        for field in ["text", "guidance", "next"] {
            let text = response[field].as_str().unwrap();
            for forbidden in ["aim for shots", "Cut slow", "target shot length"] {
                assert!(!text.contains(forbidden), "{field}: {text}");
            }
        }
        assert!(response["guidance"].as_str().unwrap().contains("not target hold lengths"));
    }
}

#[test]
fn decoded_motion_does_not_become_static_because_of_an_audio_tail() {
    let motion = klipp_analysis::motion::parse_metadata("frame:1 pts_time:0.5\nlavfi.signalstats.YDIF=12\n", 21.0);
    let evidence = pacing(&[], Some(&motion), None);
    assert_eq!(evidence["within_shot_sample_seconds"], 1);
    assert_eq!(evidence["within_shot_motion_class"], "busy");
    let empty = klipp_analysis::motion::parse_metadata("", 21.0);
    assert_eq!(pacing(&[], Some(&empty), None)["motion_evidence"], "no_valid_samples");
}

#[test]
fn complete_response_labels_cut_spikes_and_measured_empty_holds() {
    let motion = measured(vec![0.1, 0.1, 60.0, 0.1, 0.1]);
    let beats = BeatData { bpm: 0.0, confidence: 0.0, beat_times: vec![] };
    let shots = Shots::from_boundaries(5.0, vec![2.7]);
    let sheet = beatsheet::build(5.0, Some(&shots), &[]);
    let response = klipp_control::style_profile_response("fixture", "id", serde_json::json!({}), &beats,
        serde_json::Value::Null, klipp_control::StylePacingInput { shots: &shots, motion: Some(&motion), sheet: Some(&sheet) });
    assert_eq!(response["motion"]["scope"], "whole_file_including_cuts");
    assert_eq!(response["motion"]["class"], "busy");
    assert_eq!(response["pacing_evidence"]["within_shot_motion_class"], "static");
    assert_eq!(response["pacing_evidence"]["detected_holds"], serde_json::json!([]));
    let text = response["text"].as_str().unwrap();
    assert!(text.contains("whole-file luma change (includes cuts) — busy"));
    assert!(text.contains("within-shot luma change — static"));
}

#[test]
fn a_slow_pass_cut_cannot_leave_its_spike_in_the_previous_second() {
    let motion = measured(vec![0.1, 0.1, 60.0, 0.1, 0.1]);
    let evidence = style_pacing_evidence(&Shots::from_passes(5.0, vec![], vec![3.0]), Some(&motion), None);
    assert_eq!(evidence["within_shot_sample_seconds"], 3);
    assert_eq!(evidence["within_shot_motion_class"], "static");
    let fast = pacing(&[2.98], Some(&motion), None);
    assert_eq!(fast["within_shot_sample_seconds"], 4);
}

#[test]
fn beat_aligned_fast_cuts_are_not_misidentified_as_slow_pass_cuts() {
    let motion = measured(vec![3.0; 10]);
    let shots = Shots::from_passes(10.0, (1..10).map(|n| n as f64).collect(), vec![]);
    let evidence = style_pacing_evidence(&shots, Some(&motion), None);
    assert_eq!(evidence["motion_evidence"], "measured");
    assert_eq!(evidence["within_shot_sample_seconds"], 1);
    assert_eq!(evidence["within_shot_motion_class"], "gentle");
    let duplicate = Shots::from_passes(5.0, vec![2.98], vec![3.0]);
    assert!(duplicate.slow_boundaries.is_empty());
    assert_eq!(duplicate.boundaries, vec![2.98]);
}

#[test]
fn response_text_distinguishes_absent_empty_and_cut_excluded_analysis() {
    let beats = BeatData { bpm: 0.0, confidence: 0.0, beat_times: vec![] };
    let empty = Motion::from_samples(2.0, vec![None; 2]);
    let measured = measured(vec![3.0; 2]);
    let shots = Shots::from_passes(2.0, vec![], vec![0.5, 1.5]);
    for (motion, expected, text) in [(None, "unavailable", "unavailable"), (Some(&empty), "no_valid_samples", "no decoded samples"), (Some(&measured), "excluded_by_cuts", "excluded by cuts")] {
        let response = klipp_control::style_profile_response("fixture", "id", serde_json::json!({}), &beats,
            serde_json::Value::Null, klipp_control::StylePacingInput { shots: &shots, motion, sheet: None });
        assert_eq!(response["pacing_evidence"]["motion_evidence"], expected);
        assert!(response["text"].as_str().unwrap().contains(&format!("within-shot luma change — {text}")));
        if matches!(expected, "unavailable" | "no_valid_samples") {
            assert!(response["next"].as_str().unwrap().contains("Re-analyse"));
            assert!(response["guidance"].as_str().unwrap().contains("Re-analyse"));
        }
    }
}

#[test]
fn slow_detection_survives_a_removed_fast_duplicate() {
    let shots = Shots::from_passes(5.0, vec![2.0], vec![2.5,3.0]);
    assert_eq!(shots.boundaries, vec![2.0,3.0]);
    assert_eq!(shots.slow_boundaries, vec![3.0]);
    let motion = measured(vec![1.0,1.0,99.0,99.0,1.0]);
    let evidence = klipp_control::style_pacing_evidence(&shots,Some(&motion),None);
    assert_eq!(evidence["within_shot_mean_ydif"],1.0);
    assert_eq!(evidence["within_shot_sample_seconds"],3);
}
