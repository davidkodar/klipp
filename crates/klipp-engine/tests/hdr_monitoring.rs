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

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use klipp_core::*;
use klipp_render::{MediaFile, VideoTimeline};


/// The same one clip, exported as 10-bit PQ common::BANDS_H.265 through `ExportJob`.
fn export_pq(src: &Path, out: &Path) -> Result<(), String> {
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
    video.sequence.resolution = Resolution { width: common::BANDS_W, height: common::BANDS_H };
    let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    track.clips = vec![clip];
    video.sequence.tracks.push(track);
    video.media.insert(id, MediaFile { path: src.to_path_buf(), log_format: None, source_rotation: 0, ..Default::default() });
    let snapshot = klipp_render::SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: common::FPS as f64, fps_num: common::FPS, fps_den: 1, width: common::BANDS_W, height: common::BANDS_H, sample_rate: 48000,
        duration_frames: 12, export_range: None, video, audio_tracks: vec![],
        pan_law: Default::default(), global_lut_path: None,
        subtitle_tracks: vec![], chapters: vec![], working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H265,
        crf: 1, audio_codec: ExportAudioCodec::Copy, bit_depth: 10, hdr_mode: HdrMode::Pq,
        ..Default::default()
    };
    klipp_render::ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
        total_frames: 12,
    }
    .run()
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

/// Whether this ffmpeg's libx265 encodes 10-bit at all. Arch Linux ARM
/// builds x265 8-bit only, so no 10-bit HEVC export can be made there;
/// Arch's x86_64 x265 carries 8, 10 and 12-bit. Asked of the encoder's
/// pixel formats, not by encoding a frame: the ffmpeg program turns a
/// 10-bit request into 8-bit where the encoder has no 10-bit, and
/// succeeds.
fn libx265_encodes_10_bit() -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-h", "encoder=libx265"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.contains("pixel formats") && l.contains("yuv420p10le")))
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
    let y: Vec<u16> = out.stdout.chunks_exact(2).take((common::BANDS_W * common::BANDS_H) as usize).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
    (0..common::BAND_NITS.len() as u32)
        .map(|b| {
            let x = b * common::BANDS_W / 5 + common::BANDS_W / 10;
            let code = y[((common::BANDS_H / 2) * common::BANDS_W + x) as usize] as f64;
            common::pq_nits((code - 64.0) / 876.0)
        })
        .collect()
}

#[test]
fn the_viewer_measures_what_the_export_writes() {
    let Some(src) = common::pq_bands() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let parked = common::Parked::start_ungraded(src.clone(), common::BANDS_W, common::BANDS_H);

    // HDR monitoring on: the light of each band, as the viewer has it.
    let g = parked.generation();
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Preview));
    parked.settle_after(g, Duration::from_secs(30)).expect("monitoring on showed no frame");
    let measured: Vec<f32> = (0..5)
        .map(|b| {
            let nx = (b as f32 + 0.5) / 5.0;
            parked.wait_nits_at(nx, 0.5, Duration::from_secs(30)).expect("HDR monitoring publishes the light")
        })
        .collect();
    for (b, want) in common::BAND_NITS.iter().enumerate() {
        let got = measured[b] as f64;
        eprintln!("band {b}: source {want} cd/m², viewer {got:.1}");
        assert!((got - want).abs() <= want * 0.02 + 0.5, "band {b}: the viewer reads {got:.1}, source {want}");
    }

    // The saturated band reads its own brightest channel — 600 cd/m² of
    // BT.2020 red — wherever the project's working space happens to put
    // that colour. Measured in the working basis instead, a BT.709
    // project reads well over it.
    let saturated = measured[4] as f64;
    let want_sat = common::SATURATED_RED_NITS;
    eprintln!("saturated band: source {want_sat} cd/m² (BT.2020 red), viewer {saturated:.1}");
    assert!(
        (saturated - common::SATURATED_RED_NITS).abs() <= common::SATURATED_RED_NITS * 0.03,
        "the saturated band reads {saturated:.1} cd/m², source {want_sat}",
    );

    // And the file the export writes carries those same levels.
    let out = std::env::temp_dir().join("klipp-hdr-monitor-export.mp4");
    if let Err(e) = export_pq(&src, &out) {
        // Only a machine that cannot make the file is passed over: an
        // export failing where libx265 does 10-bit is the export's fault.
        if e.contains("open video encoder") && !libx265_encodes_10_bit() {
            eprintln!("skipping the export half: no 10-bit HEVC encoder here ({e})");
            return;
        }
        panic!("the HDR export failed: {e}");
    }
    let exported = exported_band_nits(&out);
    for (b, want) in common::BAND_NITS.iter().enumerate() {
        eprintln!("band {b}: export {:.1} cd/m², viewer {:.1}", exported[b], measured[b]);
        assert!(
            (exported[b] - measured[b] as f64).abs() <= want * 0.03 + 1.0,
            "band {b}: the file has {:.1} cd/m², the viewer read {:.1}",
            exported[b], measured[b],
        );
    }

    // False colour puts each band on the right side of the levels.
    let g = parked.generation();
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::FalseColour));
    parked.settle_after(g, Duration::from_secs(30)).expect("false colour showed no frame");
    let painted = parked.frame();
    let at = |b: u32| {
        let (x, y) = (b * common::BANDS_W / 5 + common::BANDS_W / 10, common::BANDS_H / 2);
        let i = ((y * common::BANDS_W + x) * 4) as usize;
        [painted[i], painted[i + 1], painted[i + 2]]
    };
    // Painted by the light the viewer measured there — the two tools must
    // agree with each other. That the levels themselves fall where
    // BT.2408 puts them is pinned in `klipp_color::hdr_view`'s own tests,
    // exactly; a band placed on a boundary here lands either side of it
    // by a hair of rounding.
    let band_of = klipp_color::hdr_view::false_colour_of_nits;
    for (b, nominal) in common::BAND_NITS.iter().enumerate() {
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
    let g = parked.generation();
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Off));
    parked.settle_after(g, Duration::from_secs(30)).expect("monitoring off showed no frame");
    let sdr_picture = parked.frame();
    drop(parked);
    klipp_engine::video_decoder::set_working_colorspace(WorkingColorspace::Bt2020Linear);
    let wide = common::Parked::start_ungraded(src.clone(), common::BANDS_W, common::BANDS_H);
    let g = wide.generation();
    wide.send(AppCommand::SetHdrMonitor(HdrMonitor::Preview));
    wide.settle_after(g, Duration::from_secs(30)).expect("monitoring on showed no frame");
    for b in 0..5 {
        let nx = (b as f32 + 0.5) / 5.0;
        let got = wide.wait_nits_at(nx, 0.5, Duration::from_secs(30)).expect("HDR monitoring publishes the light") as f64;
        let want = measured[b] as f64;
        eprintln!("band {b}: BT.709 project {want:.1} cd/m², BT.2020 project {got:.1}");
        assert!(
            (got - want).abs() <= want * 0.03 + 0.5,
            "band {b}: a BT.2020 project reads {got:.1} cd/m² where a BT.709 one reads {want:.1}",
        );
    }
    drop(wide);
    klipp_engine::video_decoder::set_working_colorspace(WorkingColorspace::Rec709Linear);
    let parked = common::Parked::start_ungraded(src, common::BANDS_W, common::BANDS_H);

    // Off again: the picture the viewer has always shown.
    parked.send(AppCommand::SetHdrMonitor(HdrMonitor::Off));
    parked.settle();
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
