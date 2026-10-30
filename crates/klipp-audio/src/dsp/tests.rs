//! Comprehensive tests for the audio DSP effects pipeline.
//!
//! These tests verify:
//! - Every effect can be created via the factory
//! - Every effect actually modifies audio (non-passthrough)
//! - Effects handle various buffer sizes correctly
//! - AudioEffectChain wiring works end-to-end
//! - Serialization/deserialization round-trips
//! - Reset clears internal state
//! - Effects handle edge cases (silence, DC, mono, stereo)

use super::*;
use klipp_core::AudioEffectInstance;

const SR: u32 = 48000;

/// Generate a stereo sine wave buffer (interleaved L/R).
fn sine_stereo(freq: f32, duration_secs: f32, sample_rate: u32) -> Vec<f32> {
    let num_frames = (duration_secs * sample_rate as f32) as usize;
    let mut buf = Vec::with_capacity(num_frames * 2);
    for i in 0..num_frames {
        let t = i as f32 / sample_rate as f32;
        let s = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5;
        buf.push(s); // L
        buf.push(s); // R
    }
    buf
}

/// Generate a loud stereo sine wave that will trigger compressors/limiters.
fn loud_sine_stereo(freq: f32, duration_secs: f32, sample_rate: u32) -> Vec<f32> {
    let num_frames = (duration_secs * sample_rate as f32) as usize;
    let mut buf = Vec::with_capacity(num_frames * 2);
    for i in 0..num_frames {
        let t = i as f32 / sample_rate as f32;
        let s = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.95;
        buf.push(s);
        buf.push(s);
    }
    buf
}

/// Compute RMS of a buffer.
fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() { return 0.0; }
    (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt()
}

/// Check if two buffers differ by more than a threshold.
fn buffers_differ(a: &[f32], b: &[f32], threshold: f32) -> bool {
    assert_eq!(a.len(), b.len());
    let diff_rms = (a.iter().zip(b.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>() / a.len() as f32).sqrt();
    diff_rms > threshold
}

fn make_ctx(sample_rate: u32) -> AudioProcessContext {
    AudioProcessContext {
        sample_rate,
        channels: 2,
        clip_time_secs: 0.0,
    }
}

// ═══════════════════════════════════════════════════════════════════
// Factory & availability tests
// ═══════════════════════════════════════════════════════════════════

#[test]
fn factory_creates_all_effects() {
    let effects = available_effects();
    assert!(effects.len() >= 13, "expected at least 13 effects, got {}", effects.len());
    for (type_id, name) in &effects {
        let fx = create_effect(type_id, SR);
        assert!(fx.is_some(), "factory failed for '{}' ({})", type_id, name);
        let fx = fx.unwrap();
        assert_eq!(fx.type_id(), *type_id);
        assert!(!fx.name().is_empty());
    }
}

/// Each declared audio effect is made by the factory with the params
/// it declares, in their order, and the ops guide names its params: the
/// ops, the browser and the engine all read the one table (#2888).
#[test]
fn every_declared_effect_is_made_with_its_declared_params() {
    let ops_md = include_str!("../../../../assets/craft/ops.md");
    let entry = ops_md.lines().find(|l| l.starts_with("track_audio ")).expect("track_audio in ops.md");
    for spec in klipp_core::audio_effect_registry::AUDIO_EFFECTS {
        let fx = create_effect(spec.type_id, SR).unwrap_or_else(|| panic!("no {} in the factory", spec.type_id));
        assert_eq!(fx.type_id(), spec.type_id);
        let made: Vec<&str> = fx.param_descriptors().iter().map(|d| d.name).collect();
        assert_eq!(spec.params, made.as_slice(), "{}", spec.type_id);
        assert!(entry.contains(&format!("{} ", spec.type_id)), "ops.md lists no params for {}", spec.type_id);
    }
}

/// Each built-in audio preset (#2887) sets settings its effect has, each
/// inside the effect's own range; the voice isolation's Full is all the
/// model can remove.
#[test]
fn every_audio_preset_is_inside_its_effects_ranges() {
    let listing = klipp_core::audio_effect_registry::audio_preset_listing();
    for p in &listing.entries[..listing.built_in] {
        let fx = create_effect(&p.effect, SR).unwrap_or_else(|| panic!("{}: no {}", p.name, p.effect));
        for (k, v) in p.values() {
            let d = fx.param_descriptors().iter().find(|d| d.name == k).unwrap_or_else(|| panic!("{}: no {k}", p.name));
            assert!((d.min..=d.max).contains(&v), "{}: {k} {v} outside {}..{}", p.name, d.min, d.max);
        }
    }
    let full = klipp_core::audio_effect_registry::presets_of("voice_isolation").find(|p| p.name == "Full").unwrap().values();
    assert_eq!(full, [("reduction".to_string(), voice_isolation::FULL_REDUCTION_DB)]);
}

#[test]
fn factory_returns_none_for_unknown() {
    assert!(create_effect("nonexistent", SR).is_none());
    assert!(create_effect("", SR).is_none());
}

#[test]
fn all_effects_have_param_descriptors() {
    for (type_id, _) in available_effects() {
        let fx = create_effect(type_id, SR).unwrap();
        let descs = fx.param_descriptors();
        assert!(!descs.is_empty(), "{} has no param descriptors", type_id);
        for d in descs {
            assert!(!d.name.is_empty(), "{}: param has empty name", type_id);
            assert!(!d.label.is_empty(), "{}: param '{}' has empty label", type_id, d.name);
            assert!(d.min <= d.default && d.default <= d.max,
                "{}: param '{}' default {} not in [{}, {}]", type_id, d.name, d.default, d.min, d.max);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Per-effect: verify each effect actually modifies audio
// ═══════════════════════════════════════════════════════════════════

/// Helper: create effect, set params to non-default values that should
/// produce audible change, process a buffer, verify it differs from input.
fn assert_effect_modifies_audio(type_id: &str, params: &[(&str, f32)]) {
    let mut fx = create_effect(type_id, SR)
        .unwrap_or_else(|| panic!("failed to create '{}'", type_id));
    for &(name, value) in params {
        fx.set_param(name, value);
    }

    let original = loud_sine_stereo(440.0, 0.1, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    fx.process(&mut processed, &ctx);

    assert!(buffers_differ(&original, &processed, 0.001),
        "{} with params {:?} did not modify audio (RMS diff too small)", type_id, params);
}

#[test]
fn eq_modifies_audio() {
    assert_effect_modifies_audio("eq", &[("band3_gain", 12.0), ("band3_frequency", 440.0)]);
}

#[test]
fn compressor_modifies_audio() {
    assert_effect_modifies_audio("compressor", &[("threshold", -20.0), ("ratio", 8.0)]);
}

#[test]
fn limiter_modifies_audio() {
    assert_effect_modifies_audio("limiter", &[("ceiling", -6.0)]);
}

#[test]
fn gate_modifies_audio() {
    // Gate with very high threshold should silence the signal
    let mut fx = create_effect("gate", SR).unwrap();
    fx.set_param("threshold", -1.0);
    fx.set_param("ratio", 20.0);

    // Use a quiet signal so the gate closes
    let mut buf = sine_stereo(440.0, 0.1, SR);
    for s in buf.iter_mut() { *s *= 0.01; }
    let original = buf.clone();
    let ctx = make_ctx(SR);
    fx.process(&mut buf, &ctx);

    assert!(rms(&buf) < rms(&original),
        "gate should reduce level of quiet signal");
}

#[test]
fn reverb_modifies_audio() {
    assert_effect_modifies_audio("reverb", &[("wet", 0.8), ("room_size", 0.8)]);
}

#[test]
fn delay_modifies_audio() {
    assert_effect_modifies_audio("delay", &[("delay_time", 50.0), ("mix", 0.8)]);
}

#[test]
fn deesser_modifies_audio() {
    // De-esser targets sibilance frequencies — use a high-frequency signal
    let mut fx = create_effect("deesser", SR).unwrap();
    fx.set_param("threshold", -30.0);
    fx.set_param("frequency", 6000.0);

    let original = loud_sine_stereo(6000.0, 0.1, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    fx.process(&mut processed, &ctx);

    assert!(buffers_differ(&original, &processed, 0.001),
        "deesser should modify 6kHz signal");
}

#[test]
fn noise_reduction_modifies_audio() {
    assert_effect_modifies_audio("noise_reduction", &[("reduction", 24.0), ("sensitivity", 80.0)]);
}

#[test]
fn normalize_modifies_audio() {
    let mut fx = create_effect("normalize", SR).unwrap();
    fx.set_param("target_lufs", -6.0);
    // Clip mode, its clip measured quiet (#2778).
    fx.set_param("measured_lufs", -30.0);

    // Use a quiet signal — normalize should boost it
    let mut buf = sine_stereo(440.0, 0.2, SR);
    for s in buf.iter_mut() { *s *= 0.1; }
    let original_rms = rms(&buf);
    let ctx = make_ctx(SR);
    fx.process(&mut buf, &ctx);

    assert!(rms(&buf) > original_rms * 1.1,
        "normalize should increase level of quiet signal");
}

#[test]
fn pitch_shift_modifies_audio() {
    assert_effect_modifies_audio("pitch_shift", &[("semitones", 5.0)]);
}

#[test]
fn chorus_modifies_audio() {
    assert_effect_modifies_audio("chorus", &[("depth", 5.0), ("rate", 1.0)]);
}

#[test]
fn multiband_compressor_modifies_audio() {
    assert_effect_modifies_audio("multiband_compressor", &[
        ("threshold_1", -10.0), ("ratio_1", 8.0),
        ("threshold_2", -10.0), ("ratio_2", 8.0),
    ]);
}

#[test]
fn declick_modifies_audio_with_clicks() {
    let mut fx = create_effect("declick", SR).unwrap();
    fx.set_param("threshold", 2.0);

    // Create signal with artificial clicks (sudden spikes)
    let mut buf = sine_stereo(440.0, 0.1, SR);
    // Insert clicks
    for i in (0..buf.len()).step_by(500) {
        buf[i] = 0.9;
        if i + 1 < buf.len() { buf[i + 1] = 0.9; }
    }
    let original = buf.clone();
    let ctx = make_ctx(SR);
    fx.process(&mut buf, &ctx);

    // Declick should smooth out the spikes
    assert!(buffers_differ(&original, &buf, 0.0001),
        "declick should modify signal with clicks");
}

// ═══════════════════════════════════════════════════════════════════
// Buffer size handling
// ═══════════════════════════════════════════════════════════════════

#[test]
fn all_effects_handle_small_buffers() {
    let ctx = make_ctx(SR);
    // 64 stereo frames = 128 floats — smallest realistic buffer
    let small_buf = sine_stereo(440.0, 64.0 / SR as f32, SR);

    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        let mut buf = small_buf.clone();
        fx.process(&mut buf, &ctx); // should not panic
    }
}

#[test]
fn all_effects_handle_large_buffers() {
    let ctx = make_ctx(SR);
    // 8192 stereo frames — large buffer
    let large_buf = sine_stereo(440.0, 8192.0 / SR as f32, SR);

    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        let mut buf = large_buf.clone();
        fx.process(&mut buf, &ctx); // should not panic
    }
}

#[test]
fn all_effects_handle_odd_frame_counts() {
    let ctx = make_ctx(SR);
    // 127 stereo frames = 254 floats — odd number
    let buf_template = sine_stereo(440.0, 127.0 / SR as f32, SR);

    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        let mut buf = buf_template.clone();
        fx.process(&mut buf, &ctx); // should not panic
    }
}

#[test]
fn all_effects_handle_empty_buffer() {
    let ctx = make_ctx(SR);
    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        let mut buf: Vec<f32> = vec![];
        fx.process(&mut buf, &ctx); // should not panic
    }
}

#[test]
fn all_effects_handle_silence() {
    let ctx = make_ctx(SR);
    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        let mut buf = vec![0.0f32; 2048];
        fx.process(&mut buf, &ctx);
        // Output should not contain NaN or Inf
        for (i, s) in buf.iter().enumerate() {
            assert!(s.is_finite(), "{}: sample {} is not finite ({}) after processing silence", type_id, i, s);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// AudioEffectChain wiring
// ═══════════════════════════════════════════════════════════════════

#[test]
fn chain_from_instances_constructs_correctly() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("threshold".into(), -20.0), ("ratio".into(), 8.0)],
        },
        AudioEffectInstance {
            effect_type: "eq".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("gain_1".into(), 6.0)],
        },
    ];

    let chain = AudioEffectChain::from_instances(&instances, SR);
    assert_eq!(chain.slot_count(), 2);
}

#[test]
fn chain_processes_audio() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("threshold".into(), -20.0), ("ratio".into(), 8.0)],
        },
    ];

    let mut chain = AudioEffectChain::from_instances(&instances, SR);
    let original = loud_sine_stereo(440.0, 0.1, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    chain.process(&mut processed, &ctx);

    assert!(buffers_differ(&original, &processed, 0.001),
        "chain with compressor should modify audio");
}

#[test]
fn chain_respects_enabled_flag() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: false, // disabled!
            wet_dry: 1.0,
            params: vec![("threshold".into(), -10.0), ("ratio".into(), 20.0)],
        },
    ];

    let mut chain = AudioEffectChain::from_instances(&instances, SR);
    let original = loud_sine_stereo(440.0, 0.1, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    chain.process(&mut processed, &ctx);

    // Disabled effect should pass through unchanged
    assert!(!buffers_differ(&original, &processed, 0.0001),
        "disabled effect should not modify audio");
}

#[test]
fn chain_wet_dry_blends() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 0.5, // 50% wet
            params: vec![("threshold".into(), -10.0), ("ratio".into(), 20.0)],
        },
    ];

    let mut chain_half = AudioEffectChain::from_instances(&instances, SR);

    let full_instances = vec![
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0, // 100% wet
            params: vec![("threshold".into(), -10.0), ("ratio".into(), 20.0)],
        },
    ];
    let mut chain_full = AudioEffectChain::from_instances(&full_instances, SR);

    let original = loud_sine_stereo(440.0, 0.1, SR);
    let mut half_wet = original.clone();
    let mut full_wet = original.clone();
    let ctx = make_ctx(SR);

    chain_half.process(&mut half_wet, &ctx);
    chain_full.process(&mut full_wet, &ctx);

    // 50% wet should be between original and full wet
    let orig_rms = rms(&original);
    let half_rms = rms(&half_wet);
    let full_rms = rms(&full_wet);

    // Full compression reduces level more than half
    assert!(half_rms > full_rms || (half_rms - full_rms).abs() < 0.1,
        "50% wet ({:.4}) should be between original ({:.4}) and full ({:.4})",
        half_rms, orig_rms, full_rms);
}

#[test]
fn chain_multiple_effects_compose() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("threshold".into(), -20.0), ("ratio".into(), 4.0)],
        },
        AudioEffectInstance {
            effect_type: "reverb".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("wet".into(), 0.5), ("room_size".into(), 0.7)],
        },
    ];

    let mut chain = AudioEffectChain::from_instances(&instances, SR);
    let original = loud_sine_stereo(440.0, 0.1, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    chain.process(&mut processed, &ctx);

    assert!(buffers_differ(&original, &processed, 0.001),
        "multi-effect chain should modify audio");
}

#[test]
fn chain_empty_is_passthrough() {
    let mut chain = AudioEffectChain::new();
    let original = sine_stereo(440.0, 0.1, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    chain.process(&mut processed, &ctx);

    assert_eq!(original, processed, "empty chain should be passthrough");
}

#[test]
fn chain_skips_unknown_effect_types() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "nonexistent_effect".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![],
        },
    ];

    let chain = AudioEffectChain::from_instances(&instances, SR);
    assert_eq!(chain.slot_count(), 0, "unknown effect should be skipped");
}

// ═══════════════════════════════════════════════════════════════════
// Reset
// ═══════════════════════════════════════════════════════════════════

#[test]
fn reset_clears_effect_state() {
    // Reverb accumulates a tail — after reset, processing silence should produce silence
    let mut fx = create_effect("reverb", SR).unwrap();
    fx.set_param("wet", 1.0);
    fx.set_param("room_size", 0.9);

    let ctx = make_ctx(SR);

    // Process a loud signal to fill the reverb tail
    let mut loud = loud_sine_stereo(440.0, 0.2, SR);
    fx.process(&mut loud, &ctx);

    // Reset
    fx.reset();

    // Process silence — should be silent (no reverb tail leaking through)
    let mut silence = vec![0.0f32; 4096];
    fx.process(&mut silence, &ctx);

    let silence_rms = rms(&silence);
    assert!(silence_rms < 0.001,
        "reverb after reset should not leak tail into silence (rms={:.6})", silence_rms);
}

#[test]
fn chain_reset_clears_all_effects() {
    let instances = vec![
        AudioEffectInstance {
            effect_type: "reverb".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("wet".into(), 1.0), ("room_size".into(), 0.9)],
        },
    ];

    let mut chain = AudioEffectChain::from_instances(&instances, SR);
    let ctx = make_ctx(SR);

    // Fill reverb tail
    let mut loud = loud_sine_stereo(440.0, 0.2, SR);
    chain.process(&mut loud, &ctx);

    chain.reset();

    // Silence after reset should stay silent
    let mut silence = vec![0.0f32; 4096];
    chain.process(&mut silence, &ctx);
    assert!(rms(&silence) < 0.001, "chain reset should clear reverb tail");
}

// ═══════════════════════════════════════════════════════════════════
// Serialization round-trip
// ═══════════════════════════════════════════════════════════════════

#[test]
fn all_effects_serialize_deserialize_roundtrip() {
    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();

        // Set some non-default params
        let midpoints: Vec<(&str, f32)> = fx.param_descriptors()
            .iter()
            .map(|d| (d.name, (d.min + d.max) / 2.0))
            .collect();
        for (name, mid) in &midpoints {
            fx.set_param(name, *mid);
        }

        let serialized = fx.serialize_params();
        assert!(!serialized.is_empty(), "{} serialized to empty", type_id);

        // Create fresh instance and deserialize
        let mut fx2 = create_effect(type_id, SR).unwrap();
        fx2.deserialize_params(&serialized);

        // Verify params match
        for (name, value) in &serialized {
            let restored = fx2.get_param(name);
            assert!(restored.is_some(), "{}: param '{}' not found after deserialize", type_id, name);
            let diff = (restored.unwrap() - value).abs();
            assert!(diff < 0.01, "{}: param '{}' drifted: {} vs {}", type_id, name, restored.unwrap(), value);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Numeric stability
// ═══════════════════════════════════════════════════════════════════

#[test]
fn effects_produce_no_nan_or_inf() {
    let ctx = make_ctx(SR);

    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        // Set aggressive params
        let maxes: Vec<(&str, f32)> = fx.param_descriptors()
            .iter()
            .map(|d| (d.name, d.max))
            .collect();
        for (name, max) in &maxes {
            fx.set_param(name, *max);
        }

        // Process multiple buffers to accumulate state
        for _ in 0..10 {
            let mut buf = loud_sine_stereo(440.0, 0.05, SR);
            fx.process(&mut buf, &ctx);
            for (i, s) in buf.iter().enumerate() {
                assert!(s.is_finite(),
                    "{} with max params: sample {} = {} (not finite)", type_id, i, s);
            }
        }
    }
}

#[test]
fn effects_do_not_explode_on_dc_offset() {
    let ctx = make_ctx(SR);
    // DC signal — tests filter stability
    let dc: Vec<f32> = vec![0.5; 4096];

    for (type_id, _) in available_effects() {
        let mut fx = create_effect(type_id, SR).unwrap();
        let mut buf = dc.clone();
        fx.process(&mut buf, &ctx);
        for (i, s) in buf.iter().enumerate() {
            assert!(s.is_finite(),
                "{}: sample {} = {} on DC input", type_id, i, s);
            assert!(s.abs() < 100.0,
                "{}: sample {} = {} exploded on DC input", type_id, i, s);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Integration: AudioEffectInstance → chain → process
// ═══════════════════════════════════════════════════════════════════

#[test]
fn full_pipeline_instance_to_audio() {
    // Simulate the exact path from clip.audio_effects to processed audio
    let clip_effects = vec![
        AudioEffectInstance {
            effect_type: "eq".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("gain_1".into(), 6.0), ("freq_1".into(), 1000.0)],
        },
        AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("threshold".into(), -15.0), ("ratio".into(), 4.0)],
        },
        AudioEffectInstance {
            effect_type: "limiter".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("ceiling".into(), -1.0)],
        },
    ];

    // This is exactly what the playback/export pipeline does
    let mut chain = AudioEffectChain::from_instances(&clip_effects, SR);
    assert_eq!(chain.slot_count(), 3);

    let original = loud_sine_stereo(1000.0, 0.2, SR);
    let mut processed = original.clone();
    let ctx = make_ctx(SR);
    chain.process(&mut processed, &ctx);

    // EQ boost + compression + limiting should produce different output
    assert!(buffers_differ(&original, &processed, 0.001),
        "full EQ→compressor→limiter chain should modify audio");

    // Limiter should keep output below ceiling (-1dB ≈ 0.891)
    let peak = processed.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(peak <= 1.0, "limiter should prevent clipping, peak={}", peak);
}

#[test]
fn consecutive_process_calls_maintain_state() {
    // Effects with internal state (reverb) should produce different
    // output on the second call even with the same input, because the
    // reverb tail from the first call bleeds into the second
    let mut fx = create_effect("reverb", SR).unwrap();
    fx.set_param("wet", 0.9);
    fx.set_param("room_size", 0.9);

    let ctx = make_ctx(SR);
    // Feed a longer signal so reverb builds up
    let input = loud_sine_stereo(440.0, 0.2, SR);

    let mut buf1 = input.clone();
    fx.process(&mut buf1, &ctx);

    // Second call with silence — reverb tail should produce non-zero output
    let mut silence = vec![0.0f32; 4096];
    fx.process(&mut silence, &ctx);

    assert!(rms(&silence) > 0.001,
        "reverb should produce tail in subsequent silence buffer (rms={:.6})", rms(&silence));
}

// ═══════════════════════════════════════════════════════════════════
// Normalize effect — LUFS mode tests (uses BS.1770-4 LufsMeter)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn normalize_lufs_mode_boosts_quiet_signal() {
    let mut fx = create_effect("normalize", SR).unwrap();
    fx.set_param("target_lufs", -14.0);
    fx.set_param("mode", 1.0); // Clip mode
    // Measured 10 LU under the target: 10 dB up, exactly (#2778).
    fx.set_param("measured_lufs", -24.0);

    let mut buf = sine_stereo(440.0, 1.0, SR);
    for s in buf.iter_mut() { *s *= 0.05; } // very quiet
    let original_rms = rms(&buf);
    let ctx = make_ctx(SR);
    fx.process(&mut buf, &ctx);

    let gain_db = 20.0 * (rms(&buf) / original_rms).log10();
    assert!((gain_db - 10.0).abs() < 0.01, "a clip measured 10 LU under its target goes up {gain_db:.2} dB");
}

#[test]
fn normalize_unmeasured_is_unity() {
    // Clip mode before its clip is measured: no gain at all (#2778).
    let mut fx = create_effect("normalize", SR).unwrap();
    fx.set_param("target_lufs", -6.0);
    let mut buf = sine_stereo(440.0, 0.5, SR);
    for s in buf.iter_mut() { *s *= 0.1; }
    let before = buf.clone();
    fx.process(&mut buf, &make_ctx(SR));
    assert_eq!(buf, before);
}

#[test]
fn normalize_reset_restores_unity_gain() {
    let mut fx = create_effect("normalize", SR).unwrap();
    fx.set_param("target_lufs", -14.0);
    fx.set_param("mode", 1.0);

    // Process some audio to change the internal gain
    let mut buf = sine_stereo(440.0, 1.0, SR);
    let ctx = make_ctx(SR);
    fx.process(&mut buf, &ctx);

    // Reset
    fx.reset();

    // Process a known signal after reset — should start from unity gain
    let mut test_buf = sine_stereo(1000.0, 0.01, SR); // short buffer
    let before = test_buf.clone();
    fx.process(&mut test_buf, &ctx);

    // Right after reset, gain should be near 1.0, so output ≈ input
    let diff_rms = rms(&test_buf.iter().zip(before.iter())
        .map(|(a, b)| a - b)
        .collect::<Vec<f32>>());
    assert!(diff_rms < 0.1,
        "After reset, first few samples should be near-unity: diff_rms={:.4}", diff_rms);
}

#[test]
fn normalize_params_roundtrip() {
    let mut fx = create_effect("normalize", SR).unwrap();
    fx.set_param("target_lufs", -18.0);
    fx.set_param("mode", 0.0);

    assert_eq!(fx.get_param("target_lufs"), Some(-18.0));
    assert_eq!(fx.get_param("mode"), Some(0.0));

    let serialized = fx.serialize_params();
    let mut fx2 = create_effect("normalize", SR).unwrap();
    fx2.deserialize_params(&serialized);

    assert_eq!(fx2.get_param("target_lufs"), Some(-18.0));
    assert_eq!(fx2.get_param("mode"), Some(0.0));
}

// ═══════════════════════════════════════════════════════════════
// Honest-naming regression.
//
// Two effects had user-facing names that didn't match what the code
// actually does:
//
//   * "Noise Reduction" was an 8-band noise gate (no spectral
//     subtraction, no noise-profile capture). Renamed to "Multi-band
//     Gate" — the type_id stays "noise_reduction" so existing .klipp
//     projects keep loading.
//   * "Brick-wall limiter" doc comment promised look-ahead behaviour;
//     the implementation is feed-forward instant-gain-reduction (no
//     look-ahead). Doc comment now acknowledges the absence; the
//     display name "Limiter" stays since it's generic.
//
// These tests pin the honest names so a future "let me restore the
// marketing names" surfaces in CI.
// ═══════════════════════════════════════════════════════════════

#[test]
fn noise_reduction_type_id_is_stable_for_klipp_project_compat() {
    // .klipp projects serialise audio_effects by type_id, so renaming
    // the type_id would break any project saved with the old effect.
    // The user-facing name() got renamed in the C-3 audit but type_id
    // MUST stay.
    let fx = create_effect("noise_reduction", SR).expect("type_id must still resolve");
    assert_eq!(fx.type_id(), "noise_reduction",
        "type_id is the saved-project key — must not change");
}

#[test]
fn noise_reduction_name_advertises_multiband_gate() {
    // The display name now matches what the code actually does.
    let fx = create_effect("noise_reduction", SR).unwrap();
    assert_eq!(fx.name(), "Multi-band Gate",
        "user-facing name must be the honest 'Multi-band Gate', NOT the marketing 'Noise Reduction' (no spectral subtraction)");
}

#[test]
fn limiter_type_id_and_display_name_are_stable() {
    // The C-5 fix is doc-comment-only — type_id and display name don't
    // change. This test pins them so a future doc-cleanup pass that
    // bundled in a rename would surface in CI.
    let fx = create_effect("limiter", SR).expect("limiter must register");
    assert_eq!(fx.type_id(), "limiter");
    assert_eq!(fx.name(), "Limiter");
}

// ═══════════════════════════════════════════════════════════════════
// AudioProcessContext.clip_time_secs forwarding contract
//
// The chain receives an `AudioProcessContext` per process() call and
// must forward `ctx.clip_time_secs` verbatim to each slot's
// effect.process(). This is the load-bearing field for keyframe
// evaluation in the audio chain: feeders (pcm_reader, audio_decoder)
// and the offline mixdown derive a real time from sample-position /
// PTS / frame-number and hand it to chain.process; effects then sample
// their kf tracks at that time.
//
// Before this contract: all four call sites hardcoded
// `clip_time_secs: 0.0`, so any future "I'll just smuggle the time
// in via a side channel" alternative would silently regress automation.
// The recording-stub effect below is the simplest possible witness:
// every value the chain receives shows up unchanged in the recorder.
// ═══════════════════════════════════════════════════════════════════

/// Test-only effect that captures every `ctx.clip_time_secs` value.
struct RecordingStub {
    times: std::sync::Arc<std::sync::Mutex<Vec<f64>>>,
}

impl RecordingStub {
    fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<f64>>>) {
        let times = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (Self { times: times.clone() }, times)
    }
}

impl AudioEffect for RecordingStub {
    fn type_id(&self) -> &'static str { "recording_stub" }
    fn name(&self) -> &'static str { "Recording Stub" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { &[] }
    fn get_param(&self, _: &str) -> Option<f32> { None }
    fn set_param(&mut self, _: &str, _: f32) {}
    fn process(&mut self, _: &mut [f32], ctx: &AudioProcessContext) {
        self.times.lock().unwrap().push(ctx.clip_time_secs);
    }
    fn reset(&mut self) {}
    fn serialize_params(&self) -> Vec<(String, f32)> { vec![] }
    fn deserialize_params(&mut self, _: &[(String, f32)]) {}
}

#[test]
fn chain_forwards_clip_time_secs_to_slot_effects() {
    let (stub, times) = RecordingStub::new();
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(stub), true, 1.0);

    let mut buf = vec![0.0f32; 1024];
    let mk = |t: f64| AudioProcessContext {
        sample_rate: SR,
        channels: 2,
        clip_time_secs: t,
    };
    chain.process(&mut buf, &mk(0.0));
    chain.process(&mut buf, &mk(0.5));
    chain.process(&mut buf, &mk(1.5));
    chain.process(&mut buf, &mk(3.25));

    let recorded = times.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![0.0, 0.5, 1.5, 3.25],
        "AudioEffectChain.process must forward ctx.clip_time_secs verbatim — \
         feeders thread the chunk-start time through this field, and Phase 3 \
         keyframe evaluation reads it. Hardcoding 0.0 anywhere in the chain \
         silently kills automation."
    );
}

#[test]
fn chain_forwards_time_even_when_wet_dry_lt_1() {
    // Wet/dry mixing path is a separate code branch in chain.process —
    // it builds dry_buffer and only re-routes the buffer through the
    // effect, but the ctx is the same instance. Pin that the time still
    // makes it through for partially-wet slots.
    let (stub, times) = RecordingStub::new();
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(stub), true, 0.5); // 50% wet

    let mut buf = vec![0.1f32; 256];
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 2.75,
    });

    assert_eq!(*times.lock().unwrap(), vec![2.75]);
}

#[test]
fn chain_skips_disabled_slots_and_does_not_record_time() {
    // Disabled slots are skipped entirely — process() on the inner
    // effect never fires. This pins that "enabled: false" really
    // bypasses the slot and isn't just a wet/dry=0 mix.
    let (stub, times) = RecordingStub::new();
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(stub), false, 1.0); // disabled

    let mut buf = vec![0.0f32; 256];
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 5.0,
    });

    assert!(times.lock().unwrap().is_empty(),
        "disabled slot must not have its process() called");
}

// ═══════════════════════════════════════════════════════════════════
// Keyframe evaluation in AudioEffectChain
//
// The chain's job once kfs are attached: divide the buffer into
// sub-chunks, evaluate each kf'd value at the slice's start time,
// and route the values through the effect. The tests below pin the
// observable contract — given a kf track, what does the buffer look
// like after process()? — using a tiny gain effect that just scales
// every sample so the pre-/post-buffer math is obvious.
// ═══════════════════════════════════════════════════════════════════

/// Test-only effect with a single `gain` param it actually applies.
/// Useful for asserting kfs reach the slot and produce the right
/// signal shape.
struct GainStub {
    gain: f32,
    descs: Vec<AudioParamDescriptor>,
}

impl GainStub {
    fn new(initial_gain: f32) -> Self {
        Self {
            gain: initial_gain,
            descs: vec![AudioParamDescriptor {
                name: "gain",
                label: "Gain",
                default: 1.0,
                min: 0.0,
                max: 4.0,
                unit: ParamUnit::None,
            }],
        }
    }
}

impl AudioEffect for GainStub {
    fn type_id(&self) -> &'static str { "gain_stub" }
    fn name(&self) -> &'static str { "Gain Stub" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { &self.descs }
    fn get_param(&self, name: &str) -> Option<f32> {
        if name == "gain" { Some(self.gain) } else { None }
    }
    fn set_param(&mut self, name: &str, value: f32) {
        if name == "gain" { self.gain = value; }
    }
    fn process(&mut self, buffer: &mut [f32], _: &AudioProcessContext) {
        for s in buffer.iter_mut() {
            *s *= self.gain;
        }
    }
    fn reset(&mut self) {}
    fn serialize_params(&self) -> Vec<(String, f32)> {
        vec![("gain".to_string(), self.gain)]
    }
    fn deserialize_params(&mut self, params: &[(String, f32)]) {
        for (k, v) in params {
            if k == "gain" { self.gain = *v; }
        }
    }
}

fn linear_kf(t: f64, v: f32) -> klipp_core::Keyframe {
    klipp_core::Keyframe {
        time_secs: t,
        value: v,
        interpolation: klipp_core::Interpolation::Linear,
        tangent_in: (0.0, 0.0),
        tangent_out: (0.0, 0.0),
        handles: Default::default(),
    }
}

fn step_kf(t: f64, v: f32) -> klipp_core::Keyframe {
    klipp_core::Keyframe {
        time_secs: t,
        value: v,
        interpolation: klipp_core::Interpolation::Step,
        tangent_in: (0.0, 0.0),
        tangent_out: (0.0, 0.0),
        handles: Default::default(),
    }
}

#[test]
fn gain_kf_ramp_quietens_start_and_loud_at_end() {
    // gain kf: t=0 → 0.0 (silent), t=1.0 → 1.0 (full). On a 1-second
    // unit signal the first quarter should be near-silent and the last
    // quarter close to unit RMS. This is the headline contract: kf
    // values reach the slot's set_param mid-stream and shape the output.
    let mut chain = AudioEffectChain::new();
    let mut param_kfs = std::collections::HashMap::new();
    param_kfs.insert("gain".to_string(), vec![linear_kf(0.0, 0.0), linear_kf(1.0, 1.0)]);
    chain.push_test_slot_with_kfs(
        Box::new(GainStub::new(1.0)),
        true,
        1.0,
        param_kfs,
        Vec::new(),
        Vec::new(),
    );

    let frames = SR as usize; // 1 second
    let mut buf = vec![0.5f32; frames * 2]; // unit-ish stereo
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 0.0,
    });

    let q = frames * 2 / 4;
    let rms_first = rms(&buf[..q]);
    let rms_last = rms(&buf[buf.len() - q..]);
    assert!(rms_first < 0.1,
        "first quarter must be near-silent (gain ramps from 0): rms={rms_first}");
    assert!(rms_last > 0.4,
        "last quarter must approach unit (gain near 1): rms={rms_last}");
    assert!(rms_last > rms_first * 4.0,
        "ramp must be visibly increasing: first={rms_first}, last={rms_last}");
}

#[test]
fn bypass_kf_step_skips_effect_during_off_window() {
    // bypass kf (Step): t=0 → 0 (bypassed), t=0.5 → 1 (active). Slot
    // has gain=0 (would silence). First half of buffer must pass
    // through unchanged (effect skipped); second half must go silent.
    let mut chain = AudioEffectChain::new();
    let bypass_kfs = vec![step_kf(0.0, 0.0), step_kf(0.5, 1.0)];
    chain.push_test_slot_with_kfs(
        Box::new(GainStub::new(0.0)), // would silence everything if active
        true,
        1.0,
        std::collections::HashMap::new(),
        bypass_kfs,
        Vec::new(),
    );

    let frames = SR as usize; // 1 second
    let mut buf = vec![0.5f32; frames * 2];
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 0.0,
    });

    // First quarter (t<0.5): bypass=0 → effect skipped → buffer unchanged.
    let q = frames * 2 / 4;
    let rms_first = rms(&buf[..q]);
    assert!(rms_first > 0.45,
        "bypass kf=0 means SKIP the effect, so the unit signal must pass through: rms={rms_first}");
    // Last quarter (t>=0.5): bypass=1 → effect active → gain=0 silences.
    let rms_last = rms(&buf[buf.len() - q..]);
    assert!(rms_last < 0.01,
        "bypass kf=1 with gain=0 means active+silenced: rms={rms_last}");
}

#[test]
fn wet_dry_kf_blends_dry_signal_back_in() {
    // wet/dry kf: t=0 → 1.0 (full wet), t=1.0 → 0.0 (full dry).
    // Effect zeroes the signal. Output should ramp from silence (full
    // wet on a silenced effect) toward the original (full dry).
    let mut chain = AudioEffectChain::new();
    let wet_dry_kfs = vec![linear_kf(0.0, 1.0), linear_kf(1.0, 0.0)];
    chain.push_test_slot_with_kfs(
        Box::new(GainStub::new(0.0)),
        true,
        1.0,
        std::collections::HashMap::new(),
        Vec::new(),
        wet_dry_kfs,
    );

    let frames = SR as usize; // 1 second
    let mut buf = vec![0.5f32; frames * 2];
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 0.0,
    });

    let q = frames * 2 / 4;
    let rms_first = rms(&buf[..q]); // mostly wet → silenced
    let rms_last = rms(&buf[buf.len() - q..]); // mostly dry → original
    assert!(rms_first < 0.1,
        "first quarter is mostly wet (silenced): rms={rms_first}");
    assert!(rms_last > 0.3,
        "last quarter is mostly dry (original signal): rms={rms_last}");
}

#[test]
fn static_chain_skips_sub_chunking_when_no_kfs() {
    // Sanity: when no slot has kfs, the chain falls through to the
    // single-pass static path (no sub-chunking). The recording stub
    // captures exactly one process() call regardless of buffer length.
    let (stub, times) = RecordingStub::new();
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(stub), true, 1.0);

    let mut buf = vec![0.0f32; SR as usize * 2]; // 0.5s mono-equivalent
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 1.5,
    });

    let recorded = times.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1,
        "static path must call effect.process exactly once per chain.process — \
         got {} sub-chunked calls (sub-chunking should only fire with kfs attached)",
        recorded.len());
    assert!((recorded[0] - 1.5).abs() < 1e-9);
}

// ═══════════════════════════════════════════════════════════════════
// Sub-chunk seam + edge cases
//
// The keyframed-path loop divides the buffer into 64-stereo-frame
// slices and re-evaluates kfs at each slice's start. Two failure
// modes the tests below pin against:
//
// 1. The final partial sub-chunk — buffers whose length isn't a
//    multiple of (SUB_CHUNK_FRAMES * channels) must process the tail
//    correctly, not silently truncate. A naive `while offset + chunk
//    <= total` would drop the trailing < 64 frames.
// 2. Cross-slice continuity — for a smooth Linear-interp ramp, the
//    LAST sample of one slice and the FIRST sample of the next must
//    differ by ~one slice-step's worth of param movement, not by a
//    quantised jump. With 64-frame slices at 48kHz the step is ~1.3ms
//    of ramp progress. Visible discontinuities here would manifest
//    as zipper noise in real audio.
// ═══════════════════════════════════════════════════════════════════

#[test]
fn sub_chunk_handles_partial_tail_buffer() {
    // Pick a buffer length that is NOT a clean multiple of the
    // sub-chunk size. 64 stereo frames = 128 floats; 200 floats
    // forces a partial 72-float tail.
    let (stub, times) = RecordingStub::new();
    let mut chain = AudioEffectChain::new();
    let mut param_kfs = std::collections::HashMap::new();
    param_kfs.insert("dummy".to_string(), vec![linear_kf(0.0, 0.0), linear_kf(1.0, 1.0)]);
    chain.push_test_slot_with_kfs(
        Box::new(stub),
        true,
        1.0,
        param_kfs,
        Vec::new(),
        Vec::new(),
    );

    let mut buf = vec![0.5f32; 200];
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 0.0,
    });

    // 200 floats / (64 frames * 2 ch) = 1 full sub-chunk + 1 partial.
    // RecordingStub::process is called once per sub-chunk slice.
    let recorded = times.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2,
        "200-float buffer = one full sub-chunk + one partial tail = 2 process() calls, got {}",
        recorded.len());
    // First slice starts at clip_time_secs 0.0; second at 64/SR ≈ 0.00133.
    assert!((recorded[0] - 0.0).abs() < 1e-9);
    let expected_second = 64.0 / SR as f64;
    assert!((recorded[1] - expected_second).abs() < 1e-9,
        "second slice time must be exactly 64 frames in: expected {expected_second}, got {}", recorded[1]);
}

#[test]
fn sub_chunk_seam_is_continuous_for_linear_ramp() {
    // A Linear gain ramp 0 → 1 over 1s, processed in one buffer of
    // 1s. Sample the buffer at the boundary between two sub-chunks
    // (frame 64 = sample index 128 stereo) and verify the
    // monotonic, no-jump shape. Without this pin a future "smooth
    // each sub-chunk" change that broke the boundary continuity
    // would surface here.
    let mut chain = AudioEffectChain::new();
    let mut param_kfs = std::collections::HashMap::new();
    param_kfs.insert("gain".to_string(), vec![linear_kf(0.0, 0.0), linear_kf(1.0, 1.0)]);
    chain.push_test_slot_with_kfs(
        Box::new(GainStub::new(1.0)),
        true,
        1.0,
        param_kfs,
        Vec::new(),
        Vec::new(),
    );

    let frames = SR as usize; // 1 second
    let mut buf = vec![1.0f32; frames * 2];
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 0.0,
    });

    // At sub-chunk boundaries the gain should advance by ~1.33ms of
    // ramp progress (≈ 0.0013 in value). Compare last sample of
    // slice 1 to last sample of slice 2 — they MUST differ in the
    // expected direction (rising) and not by a wild quantum.
    let frames_per_chunk = 64;
    // End of slice 1 = sample index (frames_per_chunk * 2 - 1).
    let end_slice_1 = (frames_per_chunk * 2 - 1) as usize;
    // End of slice 2 = sample index (frames_per_chunk * 2 * 2 - 1).
    let end_slice_2 = (frames_per_chunk * 2 * 2 - 1) as usize;
    let v1 = buf[end_slice_1];
    let v2 = buf[end_slice_2];
    assert!(v2 > v1,
        "ramp must be monotonic across sub-chunks: v1={v1}, v2={v2}");
    let step = v2 - v1;
    // One sub-chunk is 64 samples = 64/48000 ≈ 1.33ms of ramp.
    // Expected step ≈ 0.00133 * gain_max - the gain is held constant
    // within a sub-chunk, so v1 = first_sub_chunk_gain and v2 =
    // second_sub_chunk_gain. Within a sub-chunk values are flat;
    // across boundaries they step up by exactly one sub-chunk's
    // worth of ramp progress.
    let expected_step = 64.0 / SR as f32; // ≈ 0.00133
    assert!(
        (step - expected_step).abs() < 0.001,
        "sub-chunk step must equal one sub-chunk of ramp ({:.6}), got {:.6}",
        expected_step, step,
    );
}

#[test]
fn sub_chunk_buffer_smaller_than_one_chunk() {
    // Edge case: buffer is shorter than a single sub-chunk.
    // The loop must still process it as one (final + only) slice.
    let (stub, times) = RecordingStub::new();
    let mut chain = AudioEffectChain::new();
    let mut param_kfs = std::collections::HashMap::new();
    param_kfs.insert("dummy".to_string(), vec![linear_kf(0.0, 0.5)]);
    chain.push_test_slot_with_kfs(
        Box::new(stub),
        true,
        1.0,
        param_kfs,
        Vec::new(),
        Vec::new(),
    );

    let mut buf = vec![0.0f32; 16]; // 8 stereo frames — well below 64
    chain.process(&mut buf, &AudioProcessContext {
        sample_rate: SR, channels: 2, clip_time_secs: 0.0,
    });
    assert_eq!(times.lock().unwrap().len(), 1);
}

// ─── Chain total-latency-samples tests ─────────────────────────────────────
//
// Pin the contract `AudioMixer` consumes via
// `track_latency_samples`: chain reports the SUM of every enabled
// slot's reported latency, capped against per-effect and total
// pathological values so a buggy plugin can't mis-size delay lines.

/// Test-only AudioEffect that returns a fixed latency_samples()
/// value. Process is a no-op so we can compose chains without
/// needing real signal.
struct LatencyStub(u32);
impl AudioEffect for LatencyStub {
    fn type_id(&self) -> &'static str { "test_latency_stub" }
    fn name(&self) -> &'static str { "Latency Stub" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { &[] }
    fn get_param(&self, _: &str) -> Option<f32> { None }
    fn set_param(&mut self, _: &str, _: f32) {}
    fn process(&mut self, _: &mut [f32], _: &AudioProcessContext) {}
    fn reset(&mut self) {}
    fn latency_samples(&self) -> u32 { self.0 }
    fn serialize_params(&self) -> Vec<(String, f32)> { vec![] }
    fn deserialize_params(&mut self, _: &[(String, f32)]) {}
}

#[test]
fn chain_total_latency_zero_on_empty_chain() {
    let chain = AudioEffectChain::new();
    assert_eq!(chain.total_latency_samples(SR), 0);
}

#[test]
fn chain_total_latency_sums_across_enabled_slots() {
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(LatencyStub(64)), true, 1.0);
    chain.push_test_slot(Box::new(LatencyStub(128)), true, 1.0);
    chain.push_test_slot(Box::new(LatencyStub(256)), true, 1.0);
    assert_eq!(chain.total_latency_samples(SR), 448);
}

#[test]
fn chain_total_latency_skips_disabled_slots() {
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(LatencyStub(64)), true, 1.0);
    chain.push_test_slot(Box::new(LatencyStub(1024)), false, 1.0); // bypassed
    chain.push_test_slot(Box::new(LatencyStub(256)), true, 1.0);
    assert_eq!(chain.total_latency_samples(SR), 320, "disabled slot's 1024 samples don't count");
}

#[test]
fn chain_total_latency_caps_per_effect_at_one_second() {
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(LatencyStub(u32::MAX)), true, 1.0);
    // Per-effect cap = sample_rate (1 second). A single buggy plugin
    // returning u32::MAX should not crater the transport.
    assert_eq!(chain.total_latency_samples(SR), SR);
}

#[test]
fn chain_total_latency_total_caps_at_one_second() {
    let mut chain = AudioEffectChain::new();
    // Three plugins each at half a second; sum would be 1.5 seconds.
    // Total cap clamps it to 1 second.
    let half = SR / 2;
    chain.push_test_slot(Box::new(LatencyStub(half)), true, 1.0);
    chain.push_test_slot(Box::new(LatencyStub(half)), true, 1.0);
    chain.push_test_slot(Box::new(LatencyStub(half)), true, 1.0);
    assert_eq!(chain.total_latency_samples(SR), SR);
}

/// Test-only effect that really delays interleaved audio by its reported
/// latency, like a lookahead effect.
struct DelayStub {
    frames: usize,
    line: std::collections::VecDeque<f32>,
}
impl DelayStub {
    fn new(frames: usize) -> Self {
        Self { frames, line: std::collections::VecDeque::new() }
    }
}
impl AudioEffect for DelayStub {
    fn type_id(&self) -> &'static str { "test_delay_stub" }
    fn name(&self) -> &'static str { "Delay Stub" }
    fn param_descriptors(&self) -> &[AudioParamDescriptor] { &[] }
    fn get_param(&self, _: &str) -> Option<f32> { None }
    fn set_param(&mut self, _: &str, _: f32) {}
    fn process(&mut self, buffer: &mut [f32], ctx: &AudioProcessContext) {
        let delay = self.frames * ctx.channels as usize;
        while self.line.len() < delay {
            self.line.push_back(0.0);
        }
        for s in buffer.iter_mut() {
            self.line.push_back(*s);
            *s = self.line.pop_front().unwrap_or(0.0);
        }
    }
    fn reset(&mut self) { self.line.clear(); }
    fn latency_samples(&self) -> u32 { self.frames as u32 }
    fn serialize_params(&self) -> Vec<(String, f32)> { vec![] }
    fn deserialize_params(&mut self, _: &[(String, f32)]) {}
}

#[test]
fn process_aligned_removes_the_chain_latency_from_a_stream() {
    let mut chain = AudioEffectChain::new();
    chain.push_test_slot(Box::new(DelayStub::new(1920)), true, 1.0);
    let ctx = AudioProcessContext { sample_rate: SR, channels: 2, clip_time_secs: 0.0 };
    let input: Vec<f32> = (0..20_000 * 2).map(|i| i as f32).collect();

    let run = |chain: &mut AudioEffectChain, from: usize| {
        let mut out = Vec::new();
        let mut pos = from;
        // Uneven blocks, smaller and larger than the latency.
        for block in [700usize, 4096, 1500, 8192].iter().cycle() {
            if pos >= input.len() { break; }
            let end = (pos + block * 2).min(input.len());
            let mut buf = input[pos..end].to_vec();
            chain.process_aligned(&mut buf, &ctx);
            out.extend(buf);
            pos = end;
        }
        out
    };
    let out = run(&mut chain, 0);
    assert_eq!(out.len(), input.len() - 1920 * 2, "the latency is dropped once, not per block");
    assert_eq!(&out[..], &input[..out.len()], "output lines up with the input position");

    // A seek resets the chain and the alignment starts over.
    chain.reset();
    let from = 6_000 * 2;
    let out = run(&mut chain, from);
    assert_eq!(&out[..], &input[from..from + out.len()]);
}

#[test]
fn apply_settings_changes_a_running_chain_in_place() {
    let inst = |threshold: f32, wet_dry: f32| AudioEffectInstance {
        effect_type: "compressor".into(),
        enabled: true,
        wet_dry,
        params: vec![("threshold".into(), threshold)],
    };
    let mut chain = AudioEffectChain::from_instances(&[inst(-10.0, 1.0)], SR);
    assert!(chain.apply_settings(&[inst(-30.0, 0.5)]));
    assert_eq!(chain.slots[0].effect.get_param("threshold"), Some(-30.0));
    assert_eq!(chain.slots[0].wet_dry, 0.5);
    // Another shape needs a rebuild: refused, nothing changed.
    let two = [inst(-5.0, 1.0), inst(-5.0, 1.0)];
    assert!(!chain.apply_settings(&two));
    assert_eq!(chain.slots[0].effect.get_param("threshold"), Some(-30.0));
}

// ═══════════════════════════════════════════════════════════════════
// Tails (#2780)
// ═══════════════════════════════════════════════════════════════════

fn instance(effect_type: &str, params: &[(&str, f32)]) -> AudioEffectInstance {
    AudioEffectInstance { effect_type: effect_type.into(), enabled: true, wet_dry: 1.0, params: params.iter().map(|(k, v)| (k.to_string(), *v)).collect() }
}

/// A reverb's and a delay's tail rings for about as long as it says:
/// sound just after the input stops, below −80 dB once it has run.
#[test]
fn a_tail_lasts_as_long_as_the_effect_says() {
    for (kind, params) in [("reverb", vec![]), ("reverb", vec![("room_size", 0.9), ("pre_delay", 40.0)]), ("delay", vec![]), ("delay", vec![("feedback", 0.7), ("delay_time", 120.0)])] {
        let mut chain = AudioEffectChain::from_instances(&[instance(kind, &params)], SR);
        let tail = chain.tail_frames(SR);
        assert!(tail > SR as usize / 2 && tail <= SR as usize * 10, "{kind} {params:?}: {tail}");
        let mut buf = sine_stereo(440.0, 0.5, SR);
        let burst = buf.len();
        buf.resize(burst + (tail + SR as usize) * 2, 0.0);
        chain.process(&mut buf, &make_ctx(SR));
        let after = &buf[burst..];
        assert!(rms(&after[..SR as usize / 5 * 2]) > 1e-3, "{kind} {params:?}: it rings on");
        let past = after[tail * 2..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(past < 1e-4, "{kind} {params:?}: {past} past its tail of {tail} frames");
        // Most of the tail is heard: it is not twice as long as it needs.
        let late = after[tail..tail * 2].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(late > 1e-6, "{kind} {params:?}: silent from half its tail ({late})");
    }
}

/// A chain's tail is its effects' one after another: none when dry,
/// bypassed or without one, at most ten seconds.
#[test]
fn a_chain_tail_sums_its_effects() {
    let one = |i: AudioEffectInstance| AudioEffectChain::from_instances(&[i], SR).tail_frames(SR);
    let reverb = one(instance("reverb", &[]));
    let delay = one(instance("delay", &[]));
    let both = AudioEffectChain::from_instances(&[instance("reverb", &[]), instance("eq", &[]), instance("delay", &[])], SR).tail_frames(SR);
    assert!(both.abs_diff(reverb + delay) <= 1, "{both} against {reverb} + {delay}");
    assert_eq!(one(instance("eq", &[])), 0);
    assert_eq!(one(instance("reverb", &[("wet", 0.0)])), 0);
    assert_eq!(one(AudioEffectInstance { enabled: false, ..instance("reverb", &[]) }), 0);
    assert_eq!(one(AudioEffectInstance { wet_dry: 0.0, ..instance("delay", &[]) }), 0);
    assert_eq!(one(instance("delay", &[("feedback", 0.95), ("delay_time", 2000.0)])), SR as usize * 10);
}
