//! Importing a file reads what the camera recorded about the shot: the
//! start timecode and reel from the container's tags, the exposure and
//! gamma from the sidecar a camera writes beside a clip, and a still's
//! EXIF. Skips without ffmpeg.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_engine::perf::PerfRing;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::MediaPool;
use klipp_timeline::Sequence;

/// Import `path` through a headless engine and return what it reported.
fn import(path: &Path) -> MediaLoadedPayload {
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(64, 64)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(64, 64))),
        Arc::new(RwLock::new(Sequence::new_default())), Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    let media_id = MediaId::new();
    cmd.send(AppCommand::LoadMediaFile { path: path.to_path_buf(), media_id }).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let payload = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match ev_rx.recv_timeout(left) {
            Ok(AppEvent::MediaLoaded(p)) if p.media_id == media_id => break *p,
            Ok(_) => continue,
            Err(_) => panic!("no MediaLoaded for {} within 20 s", path.display()),
        }
    };
    cmd.send(AppCommand::Quit).unwrap();
    let _ = thread.join();
    payload
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-shot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A one-second clip with the tags a camera writes, `None` without ffmpeg.
fn clip(name: &str, args: &[&str]) -> Option<PathBuf> {
    let out = scratch(name);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=64x64:r=25:d=1", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .args(args)
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

const SONY_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<NonRealTimeMeta xmlns="urn:schemas-professionalDisc:nonRealTimeMeta:ver.2.20">
  <Device manufacturer="Sony" modelName="ILCE-7M4"/>
  <AcquisitionRecord>
    <Group name="CameraUnitMetadataSet">
      <Item name="CaptureGammaEquation" value="s-log3-cine"/>
      <Item name="CaptureColorPrimaries" value="s-gamut3-cine"/>
      <Item name="ISOSensitivity" value="800"/>
      <Item name="ShutterSpeed_Time" value="1/50"/>
      <Item name="FNumber" value="F2.8"/>
      <Item name="LensAttributes" value="FE 24-70mm F2.8 GM II"/>
    </Group>
  </AcquisitionRecord>
</NonRealTimeMeta>"#;

#[test]
fn a_clips_timecode_camera_and_sidecar_exposure_are_read_at_import() {
    let Some(mov) = clip("C0001.MOV", &["-timecode", "01:02:03:04", "-metadata", "make=Sony", "-metadata", "model=ILCE-7M4"]) else { return };
    std::fs::write(mov.with_file_name("C0001M01.XML"), SONY_XML).unwrap();
    let p = import(&mov);
    assert_eq!(p.camera_make.as_deref(), Some("Sony"));
    assert_eq!(p.camera_model.as_deref(), Some("ILCE-7M4"));
    let shot = &p.shot;
    assert_eq!(shot.start_timecode.as_deref(), Some("01:02:03:04"), "the timecode track's start");
    assert_eq!(shot.iso, Some(800));
    assert_eq!(shot.shutter.as_deref(), Some("1/50"));
    assert_eq!(shot.aperture, Some(2.8));
    assert_eq!(shot.lens.as_deref(), Some("FE 24-70mm F2.8 GM II"));
    assert_eq!(shot.gamma.as_deref(), Some("s-log3-cine"));
    assert_eq!(shot.colour_gamut.as_deref(), Some("s-gamut3-cine"));
    assert_eq!(shot.exposure_source.as_deref(), Some("Sony XML"));
    // Every tag is kept, by where it came from.
    assert!(shot.tags.iter().any(|(k, v)| k == "format:make" && v == "Sony"), "{:?}", shot.tags);
    assert!(shot.tags.iter().any(|(k, v)| k == "video:timecode" && v == "01:02:03:04"), "{:?}", shot.tags);
    assert!(shot.tags.iter().any(|(k, _)| k == "sidecar:file"), "{:?}", shot.tags);
}

#[test]
fn a_reel_name_is_read_whatever_case_the_container_writes_it_in() {
    let Some(mkv) = clip("reel.mkv", &["-metadata", "reel_name=A001", "-metadata", "timecode=02:03:04:05"]) else { return };
    let p = import(&mkv);
    assert_eq!(p.shot.reel_name.as_deref(), Some("A001"));
    assert_eq!(p.shot.start_timecode.as_deref(), Some("02:03:04:05"));
    assert!(p.shot.exposure_source.is_none(), "nothing beside the clip");
}

#[test]
fn a_still_reads_its_exif_for_camera_lens_exposure_and_date() {
    use exif::{experimental::Writer, Field, In, Rational, Tag, Value};
    let base = scratch("base.jpg");
    let ok = std::process::Command::new("magick").args(["-size", "64x64", "xc:gray"]).arg(&base).status().map(|s| s.success()).unwrap_or(false)
        || std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=gray:s=64x64", "-frames:v", "1"]).arg(&base).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return;
    }
    let fields = [
        Field { tag: Tag::Make, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon".to_vec()]) },
        Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"Canon EOS R5".to_vec()]) },
        Field { tag: Tag::LensModel, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"RF50mm F1.2 L USM".to_vec()]) },
        Field { tag: Tag::PhotographicSensitivity, ifd_num: In::PRIMARY, value: Value::Short(vec![400]) },
        Field { tag: Tag::ExposureTime, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 1, denom: 125 }]) },
        Field { tag: Tag::FNumber, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 12, denom: 10 }]) },
        Field { tag: Tag::FocalLength, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 50, denom: 1 }]) },
        Field { tag: Tag::DateTimeOriginal, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"2024:03:15 10:30:00".to_vec()]) },
    ];
    let mut writer = Writer::new();
    for f in &fields {
        writer.push_field(f);
    }
    let mut blob = std::io::Cursor::new(Vec::new());
    writer.write(&mut blob, false).unwrap();
    let blob = blob.into_inner();
    // The EXIF goes in an APP1 segment straight after the SOI marker.
    let jpeg = std::fs::read(&base).unwrap();
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
    let len = (2 + 6 + blob.len()) as u16;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(&blob);
    out.extend_from_slice(&jpeg[2..]);
    let still = scratch("IMG_0001.jpg");
    std::fs::write(&still, out).unwrap();

    let p = import(&still);
    assert!(p.is_still, "a JPEG is a still");
    assert_eq!(p.camera_make.as_deref(), Some("Canon"));
    assert_eq!(p.camera_model.as_deref(), Some("Canon EOS R5"));
    assert_eq!(p.media_created_at, Some(1_710_498_600), "taken when the camera's clock said");
    let shot = &p.shot;
    assert_eq!(shot.lens.as_deref(), Some("RF50mm F1.2 L USM"));
    assert_eq!(shot.exposure_summary().as_deref(), Some("ISO 400 \u{00B7} 1/125 \u{00B7} f/1.2 \u{00B7} 50 mm"));
    assert_eq!(shot.exposure_source.as_deref(), Some("EXIF"));
    assert!(shot.tags.iter().any(|(k, v)| k == "exif:FNumber" && v.contains("1.2")), "{:?}", shot.tags);
}
