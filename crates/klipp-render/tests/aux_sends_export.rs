//! Aux buses and sends in the export (#2861): the render mixes them as
//! playback does, a bus's effects that look ahead landing with the dry
//! sound, and a project without them renders as it did, bit for bit.
use klipp_audio::{AudioMixer, SendLane};
use klipp_core::AudioEffectInstance;
use klipp_render::snapshot::{AudioClipShape, AudioClipSnapshot, AudioTrackSnapshot, SequenceSnapshot};
use klipp_render::AudioMixdown;
use klipp_timeline::{AuxBus, AuxSend};
use std::path::PathBuf;

const RATE: u32 = 48000;
const FRAMES: u64 = 75;

/// Three seconds of a tone with soft swells, stereo, written once; `None`
/// without ffmpeg.
fn tone() -> Option<PathBuf> {
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_writer = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let out = klipp_testing::scratch::scratch_file("klipp-aux-sends", "tone.wav");
    if out.exists() {
        return Some(out);
    }
    let part = out.with_extension("part.wav");
    let left = "0.2*sin(2*PI*220*t)*(0.6+0.4*sin(2*PI*1.5*t))";
    let right = "0.2*sin(2*PI*330*t)*(0.6+0.4*cos(2*PI*1.1*t))";
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("aevalsrc={left}|{right}:s=48000:d=3"))
        .args(["-c:a", "pcm_f32le"])
        .arg(&part)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return None;
    }
    std::fs::rename(&part, &out).ok()?;
    Some(out)
}

fn track(path: &std::path::Path, volume: f32, pan: f32, sends: Vec<AuxSend>) -> AudioTrackSnapshot {
    let clip = AudioClipSnapshot {
        timeline_start_frame: 0,
        timeline_end_frame: FRAMES,
        source_start_secs: 0.0,
        path: path.to_path_buf(),
        audio_effects: vec![],
        audio_effect_keyframes: Default::default(),
        source_range: (0.0, 0.0),
        shape: AudioClipShape { gain: 1.0, clip_duration_secs: FRAMES as f64 / 25.0, ..Default::default() },
        varispeed: None,
        exact_secs: None,
    };
    AudioTrackSnapshot { volume, pan, muted: false, solo: false, clips: vec![clip], automation: Default::default(), effects: vec![], sends }
}

fn snapshot(audio_tracks: Vec<AudioTrackSnapshot>, audio_buses: Vec<AuxBus>) -> SequenceSnapshot {
    SequenceSnapshot {
        fps: 25.0,
        fps_num: 25,
        fps_den: 1,
        width: 64,
        height: 64,
        sample_rate: RATE,
        duration_frames: FRAMES,
        export_range: None,
        video: Default::default(),
        audio_tracks,
        audio_buses,
        pan_law: Default::default(),
        global_lut_path: None,
        subtitle_tracks: vec![],
        chapters: vec![],
        working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
        grade_encoding: klipp_core::GradeEncoding::Bt1886,
    }
}

fn mixdown(snap: &SequenceSnapshot) -> Vec<f32> {
    let mut m = AudioMixdown::new(snap).unwrap();
    (0..FRAMES).flat_map(|f| m.get_frame_audio_unclamped(f)).collect()
}

/// Without buses the render is what it was: each track's samples times
/// its volume and its pan law's gain, summed in track order, bit for bit.
#[test]
fn a_render_without_routing_is_unchanged_bit_for_bit() {
    let Some(path) = tone() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    // Linear at centre is unity: the clip's samples as they are.
    let raw = mixdown(&snapshot(vec![track(&path, 1.0, 0.0, vec![])], vec![]));
    let out = mixdown(&snapshot(vec![track(&path, 0.7, -0.3, vec![]), track(&path, 0.4, 0.6, vec![])], vec![]));
    let law = klipp_core::PanLaw::default();
    let gains = |v: f32, p: f32| {
        let (l, r) = law.gains(p);
        (l * v, r * v)
    };
    let (a, b) = (gains(0.7, -0.3), gains(0.4, 0.6));
    let want: Vec<f32> = raw.iter().enumerate().map(|(i, x)| {
        let (ga, gb) = if i % 2 == 0 { (a.0, b.0) } else { (a.1, b.1) };
        (0.0 + x * ga) + x * gb
    }).collect();
    assert_eq!(out.len(), want.len());
    assert!(out.iter().zip(&want).all(|(o, w)| o.to_bits() == w.to_bits()), "bit for bit");
}

/// The render through a bus whose effect looks ahead is playback's: a
/// track at -3 dB sending at -6 dB after its fader, and pre-fader, to a
/// declick bus, against the playback mixer fed the same samples, which
/// is late by the latency it holds the picture back by.
#[test]
fn a_render_through_a_latent_bus_is_playbacks() {
    let Some(path) = tone() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let raw = mixdown(&snapshot(vec![track(&path, 1.0, 0.0, vec![])], vec![]));
    let declick = vec![AudioEffectInstance { effect_type: "declick".into(), enabled: true, wet_dry: 1.0, params: vec![] }];
    let (fader, level) = (10f32.powf(-3.0 / 20.0), 10f32.powf(-6.0 / 20.0));
    for pre_fader in [false, true] {
        let mut bus = AuxBus::new("Verb");
        bus.effects = declick.clone();
        let send = AuxSend { level, pre_fader, ..AuxSend::new(bus.id) };
        let rendered = mixdown(&snapshot(vec![track(&path, fader, 0.0, vec![send]), track(&path, 1.0, 0.0, vec![])], vec![bus]));

        let mut m = AudioMixer::new(2);
        m.set_sample_rate(RATE);
        m.set_bus_count(1);
        m.set_bus_inserts(0, &declick);
        m.tracks[0].volume = fader;
        m.tracks[0].sends.push(SendLane::new(0, level, pre_fader, false));
        for t in &mut m.tracks {
            t.ramp_from = Some((t.volume, t.pan));
            for s in &mut t.sends {
                s.ramp_from = Some(s.gain);
            }
        }
        m.refresh_pdc();
        let lag = m.pdc_global_max_latency() as usize * 2;
        assert!(lag > 0, "declick looks ahead");
        let mut played = Vec::new();
        for block in raw.chunks(1024) {
            let (mut a, mut b) = (block.to_vec(), block.to_vec());
            m.apply_per_track_pdc_delay(0, &mut a);
            m.apply_per_track_pdc_delay(1, &mut b);
            played.extend(m.mix(&[(0, &a), (1, &b)]));
        }
        let worst = (4096..rendered.len() - lag).map(|i| (rendered[i] - played[i + lag]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "pre_fader {pre_fader}: the render is playback's, off by {worst}");
        let dry_only = mixdown(&snapshot(vec![track(&path, fader, 0.0, vec![]), track(&path, 1.0, 0.0, vec![])], vec![]));
        let wet: f32 = (4096..rendered.len()).map(|i| (rendered[i] - dry_only[i]).abs()).fold(0.0, f32::max);
        let source: f32 = raw[4096..].iter().fold(0.0f32, |a, s| a.max(s.abs()));
        let want_db = if pre_fader { -6.0 } else { -9.0 };
        assert!((20.0 * (wet / source).log10() - want_db).abs() < 0.2, "pre_fader {pre_fader}: the return at {:.2} dB", 20.0 * (wet / source).log10());
    }
}
