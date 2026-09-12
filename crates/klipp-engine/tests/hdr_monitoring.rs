//! What the viewer measures of an HDR picture is what the export writes.
//!
//! HDR cannot be judged on an SDR screen, so the viewer's HDR monitoring
//! is a measuring instrument: it renders the picture the export writes
//! (display light, 1.0 = 203 cd/m²) and publishes the light of every part
//! of it in cd/m², plus false colours against the levels HDR is judged
//! at. These tests check the numbers, on a source whose levels are known
//! by construction:
//!
//! * the viewer's light matches the PQ source's own levels,
//! * and matches what `ExportJob` puts in the file at the same frame,
//! * the false colours land on the right side of 26, 203 and 1000 cd/m²,
//! * and with monitoring off the viewer's picture is unchanged.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use klipp_core::*;
use klipp_render::{MediaFile, VideoTimeline};

const W: u32 = 640;
const H: u32 = 360;
/// Four vertical bands of known luminance, in cd/m².
const BAND_NITS: [f64; 4] = [5.0, 26.0, 203.0, 1000.0];
/// A fifth band, saturated: BT.2020 red at this level, green and blue at
/// the bottom. Its brightest channel is the red one — the same thing the
/// encoder measures, once the picture is in BT.2020. In a BT.709 project
/// the working values of this colour are another basis entirely (and its
/// green and blue are negative), so measuring there reads a different
/// number.
const SATURATED_RED_NITS: f64 = 600.0;

fn pq_signal(nits: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let y = (nits / 10000.0f64).powf(m1);
    ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
}

fn pq_nits(signal: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let p = signal.powf(1.0 / m2);
    10000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

/// A lossless 10-bit PQ clip of four neutral bands at [`BAND_NITS`], plus a
/// saturated fifth at [`SATURATED_RED_NITS`].
///
/// Cached between runs under a name that carries the codes it holds: keyed
/// on the frame size alone, a file written for other band levels stayed in
/// /tmp and every later run measured it instead — bands 0 and 1 happened
/// to match, and band 2 read 1004 cd/m² where the test wanted 203.
fn pq_bands() -> Option<PathBuf> {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let y10 = |signal: f64| (64.0 + signal * 876.0).round() as u16;
    let c10 = |v: f64| (512.0 + v * 896.0).round().clamp(4.0, 1019.0) as u16;
    // Four neutral bands, then BT.2020 R'G'B' → Y'CbCr (BT.2020 NCL,
    // limited range) for the saturated one.
    let (kr, kb) = (0.2627f64, 0.0593f64);
    let (r, g, b) = (pq_signal(SATURATED_RED_NITS), pq_signal(1.0), pq_signal(1.0));
    let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
    let saturated = [y10(luma), c10((b - luma) / (2.0 * (1.0 - kb))), c10((r - luma) / (2.0 * (1.0 - kr)))];
    let codes: Vec<[u16; 3]> = BAND_NITS
        .iter()
        .map(|n| [y10(pq_signal(*n)), 512, 512])
        .chain(std::iter::once(saturated))
        .collect();
    // Every code in the name, chroma included: the saturated band differs
    // from a neutral one in chroma alone.
    let name: String = codes.iter().flatten().map(|c| format!("-{c}")).collect();
    let out = std::env::temp_dir().join(format!("klipp-hdr-monitor-bands-{W}x{H}{name}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let raw = std::env::temp_dir().join(format!("klipp-hdr-monitor-bands{name}.yuv"));
    let mut bytes = Vec::new();
    // Two seconds: the harness parks ten frames in.
    for _ in 0..50 {
        for plane in 0..3 {
            let pw = if plane == 0 { W } else { W / 2 };
            let ph = if plane == 0 { H } else { H / 2 };
            for _ in 0..ph {
                for x in 0..pw {
                    let band = (x * W / pw * 5 / W) as usize;
                    bytes.extend_from_slice(&codes[band.min(4)][plane].to_le_bytes());
                }
            }
        }
    }
    std::fs::write(&raw, bytes).ok()?;
    let size = format!("{W}x{H}");
    // Written beside the cache name and moved into place, so a run that
    // dies mid-encode leaves no half file for the next one to measure.
    let partial = out.with_extension("partial.mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-s", &size, "-r", "25"])
        .args(["-i", raw.to_str()?, "-vf", &format!("setparams=range=tv:{}", common::PQ_TAGS)])
        .args(["-c:v", "ffv1"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    let _ = std::fs::remove_file(&raw);
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// The same one clip, exported as 10-bit PQ H.265 through `ExportJob`.
fn export_pq(src: &Path, out: &Path) {
    let id = MediaId::new();
    let tc = |f: i64| RationalTime::from_frames(f, common::FPS);
    let mut clip = klipp_timeline::Clip {
        kind: klipp_timeline::ClipKind::Media { media_id: id },
        ..klipp_timeline::Clip::gap(tc(0), tc(50))
    };
    clip.source_in = tc(0);
    clip.source_out = tc(50);
    let mut video = VideoTimeline::default();
    video.sequence.fps = Fps::new(common::FPS, 1);
    video.sequence.resolution = Resolution { width: W, height: H };
    let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip];
    video.sequence.tracks.push(track);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0 });
    let snapshot = klipp_render::SequenceSnapshot {
        fps: common::FPS as f64, fps_num: common::FPS, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: 12, export_range: None, video, audio_tracks: vec![],
        pan_law: Default::default(), global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: WorkingColorspace::Rec709Linear,
    };
    let settings = ExportSettings {
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H265,
        crf: 1, audio_codec: ExportAudioCodec::Copy, bit_depth: 10, hdr_mode: HdrMode::Pq,
        ..Default::default()
    };
    klipp_render::ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(),
        total_frames: 12,
    }
    .run()
    .unwrap();
}

/// The exported file's level at the centre of each neutral band, from its
/// luma code. Only the neutral ones: for a saturated colour the luma is
/// not the level its brightest channel carries, and the brightest channel
/// is what both the encoder's metadata and the viewer's readout measure.
fn exported_band_nits(path: &Path) -> Vec<f64> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .unwrap();
    let y: Vec<u16> = out.stdout.chunks_exact(2).take((W * H) as usize).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    (0..BAND_NITS.len() as u32)
        .map(|b| {
            let x = b * W / 5 + W / 10;
            let code = y[((H / 2) * W + x) as usize] as f64;
            pq_nits((code - 64.0) / 876.0)
        })
        .collect()
}

#[test]
fn the_viewer_measures_what_the_export_writes() {
    let Some(src) = pq_bands() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start_ungraded(src.clone(), W, H);

    // HDR monitoring on: the light of each band, as the viewer has it.
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Preview));
    std::thread::sleep(Duration::from_millis(700));
    let measured: Vec<f32> = (0..5)
        .map(|b| {
            let nx = (b as f32 + 0.5) / 5.0;
            parked.nits_at(nx, 0.5).expect("HDR monitoring publishes the light")
        })
        .collect();
    for (b, want) in BAND_NITS.iter().enumerate() {
        let got = measured[b] as f64;
        eprintln!("band {b}: source {want} cd/m², viewer {got:.1}");
        assert!((got - want).abs() <= want * 0.02 + 0.5, "band {b}: the viewer reads {got:.1}, source {want}");
    }

    // The saturated band reads its own brightest channel — 600 cd/m² of
    // BT.2020 red — wherever the project's working space happens to put
    // that colour. Measured in the working basis instead, a BT.709
    // project reads well over it.
    let saturated = measured[4] as f64;
    eprintln!("saturated band: source {SATURATED_RED_NITS} cd/m² (BT.2020 red), viewer {saturated:.1}");
    assert!(
        (saturated - SATURATED_RED_NITS).abs() <= SATURATED_RED_NITS * 0.03,
        "the saturated band reads {saturated:.1} cd/m², source {SATURATED_RED_NITS}",
    );

    // And the file the export writes carries those same levels.
    let out = std::env::temp_dir().join("klipp-hdr-monitor-export.mp4");
    export_pq(&src, &out);
    let exported = exported_band_nits(&out);
    for (b, want) in BAND_NITS.iter().enumerate() {
        eprintln!("band {b}: export {:.1} cd/m², viewer {:.1}", exported[b], measured[b]);
        assert!(
            (exported[b] - measured[b] as f64).abs() <= want * 0.03 + 1.0,
            "band {b}: the file has {:.1} cd/m², the viewer read {:.1}",
            exported[b], measured[b],
        );
    }

    // False colour puts each band on the right side of the levels.
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::FalseColour));
    std::thread::sleep(Duration::from_millis(700));
    let painted = parked.frame();
    let at = |b: u32| {
        let (x, y) = (b * W / 5 + W / 10, H / 2);
        let i = ((y * W + x) * 4) as usize;
        [painted[i], painted[i + 1], painted[i + 2]]
    };
    // Painted by the light the viewer measured there — the two tools must
    // agree with each other. That the levels themselves fall where
    // BT.2408 puts them is pinned in `klipp_color::hdr_view`'s own tests,
    // exactly; a band placed on a boundary here lands either side of it
    // by a hair of rounding.
    let band_of = klipp_color::hdr_view::false_colour_of_nits;
    for (b, nominal) in BAND_NITS.iter().enumerate() {
        let (got, expect) = (at(b as u32), band_of(measured[b]));
        eprintln!("band {b} ({nominal} cd/m², measured {:.1}): painted {got:?}, expected {expect:?}", measured[b]);
        assert_eq!(got, expect, "band {b} is painted the wrong level");
    }
    // And the bands are told apart: four levels, four colours.
    let painted: Vec<[u8; 3]> = (0..4).map(at).collect();
    for b in 1..4 {
        assert_ne!(painted[b], painted[b - 1], "bands {} and {b} are painted the same", b - 1);
    }

    // A BT.2020 project holds the same colours in another basis, and must
    // measure the same light: the readout is of the picture, not of
    // whichever numbers the working space keeps it in.
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Off));
    std::thread::sleep(Duration::from_millis(300));
    let sdr_picture = parked.frame();
    drop(parked);
    klipp_engine::video_decoder::set_working_colorspace(WorkingColorspace::Bt2020Linear);
    let wide = common::Parked::start_ungraded(src.clone(), W, H);
    wide.send(AppCommand::SetHdrMonitor(HdrMonitor::Preview));
    std::thread::sleep(Duration::from_millis(700));
    for b in 0..5 {
        let nx = (b as f32 + 0.5) / 5.0;
        let got = wide.nits_at(nx, 0.5).expect("HDR monitoring publishes the light") as f64;
        let want = measured[b] as f64;
        eprintln!("band {b}: BT.709 project {want:.1} cd/m², BT.2020 project {got:.1}");
        assert!(
            (got - want).abs() <= want * 0.03 + 0.5,
            "band {b}: a BT.2020 project reads {got:.1} cd/m² where a BT.709 one reads {want:.1}",
        );
    }
    drop(wide);
    klipp_engine::video_decoder::set_working_colorspace(WorkingColorspace::Rec709Linear);
    let parked = common::Parked::start_ungraded(src, W, H);

    // Off again: the picture the viewer has always shown.
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Off));
    std::thread::sleep(Duration::from_millis(700));
    assert!(parked.nits_at(0.5, 0.5).is_none(), "the light map outlives monitoring");
    let worst = sdr_picture
        .iter()
        .zip(&parked.frame())
        .map(|(a, b)| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap();
    eprintln!("monitoring off: the SDR picture differs by {worst} levels");
    assert!(worst <= 1, "turning monitoring off left the picture {worst} levels off");
}
