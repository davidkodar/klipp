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
        Arc::new(klipp_timeline::SharedSequence::new(Sequence::new_default())), Arc::new(RwLock::new(MediaPool::default())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(PerfRing::default())),
    );
    let thread = engine.spawn();
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
    // A decode thread still in ffmpeg's teardown at exit crashes the
    // process (CUDA's context destroyed under it).
    klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
    payload
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp-shot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A one-second clip with the tags a camera writes, `None` without ffmpeg.
fn clip(name: &str, args: &[&str]) -> Option<PathBuf> {
    clip_of(name, 1, args)
}

/// [`clip`] lasting `secs`.
fn clip_of(name: &str, secs: u32, args: &[&str]) -> Option<PathBuf> {
    let out = scratch(name);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s=64x64:r=25:d={secs}"), "-c:v", "libx264", "-pix_fmt", "yuv420p"])
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
    assert_eq!(p.truncated_from_secs, None, "a whole file");
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

#[test]
fn a_copy_cut_short_is_as_long_as_it_holds_and_says_what_its_header_claimed() {
    // The header up front, as a phone writes it, so cutting the file's
    // tail keeps the claim and loses the pictures.
    let Some(whole) = clip_of("whole.mov", 10, &["-g", "15", "-movflags", "+faststart"]) else { return };
    let bytes = std::fs::read(&whole).unwrap();
    let short = scratch("short.mov");
    std::fs::write(&short, &bytes[..bytes.len() * 35 / 100]).unwrap();
    let p = import(&short);
    let held = p.duration.to_secs_f64();
    let claimed = p.truncated_from_secs.expect("the header's claim");
    assert!((claimed - 10.0).abs() < 0.2, "header claims {claimed}");
    assert!(held > 0.5 && held < 6.0, "holds {held} s");
    // Whole, the same file is not cut short.
    let p = import(&whole);
    assert_eq!(p.truncated_from_secs, None);
    assert!((p.duration.to_secs_f64() - 10.0).abs() < 0.2);
}

/// `mp4` (its `moov` last, as ffmpeg writes it) with a Dolby Vision
/// configuration record of `profile` added to its first `hvc1` sample
/// entry, as a Dolby Vision camera or encoder writes it, and each box
/// around it grown to hold it. With `moov` after the media data, no
/// chunk offset moves.
fn with_dolby_vision_record(mp4: &[u8], profile: u8) -> Vec<u8> {
    fn find(b: &[u8], from: usize, to: usize, kind: &[u8; 4]) -> usize {
        let mut i = from;
        while i + 8 <= to {
            let size = u32::from_be_bytes(b[i..i + 4].try_into().unwrap()) as usize;
            if &b[i + 4..i + 8] == kind {
                return i;
            }
            assert!(size >= 8, "a box of {size} bytes");
            i += size;
        }
        panic!("no {} box", String::from_utf8_lossy(kind));
    }
    let size = |b: &[u8], at: usize| u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
    // Each box down to the sample entry, and where its children start:
    // after `stsd`'s version and count, after `hvc1`'s 78 bytes of fields.
    let mut path = Vec::new();
    let (mut from, mut to) = (0, mp4.len());
    for (kind, header) in [(b"moov", 8), (b"trak", 8), (b"mdia", 8), (b"minf", 8), (b"stbl", 8), (b"stsd", 16), (b"hvc1", 86)] {
        let at = find(mp4, from, to, kind);
        path.push(at);
        (from, to) = (at + header, at + size(mp4, at));
    }
    // Version 1.0, then profile (7 bits), level 6 (6 bits), RPU, no
    // enhancement layer, base layer; compatibility 0; reserved.
    let flags: u16 = (u16::from(profile) << 9) | (6 << 3) | 0b101;
    let mut record = vec![1, 0];
    record.extend_from_slice(&flags.to_be_bytes());
    record.extend_from_slice(&[0; 20]);
    let mut dvcc = ((8 + record.len()) as u32).to_be_bytes().to_vec();
    dvcc.extend_from_slice(b"dvcC");
    dvcc.extend_from_slice(&record);

    let mut out = mp4.to_vec();
    let entry_end = path[path.len() - 1] + size(mp4, path[path.len() - 1]);
    out.splice(entry_end..entry_end, dvcc.iter().copied());
    for at in path {
        let grown = (size(&out, at) + dvcc.len()) as u32;
        out[at..at + 4].copy_from_slice(&grown.to_be_bytes());
    }
    out
}

/// Part of #1224: a file whose picture is Dolby Vision profile 5 is
/// imported with its profile, so its info and the media listing can say
/// its colours are wrong; profile 8 is read too, and a plain HEVC file
/// has none.
#[test]
fn a_dolby_vision_profile_is_read_at_import() {
    let plain = scratch("plain_hevc.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=64x64:r=25:d=1", "-c:v", "libx265", "-x265-params", "log-level=none", "-tag:v", "hvc1", "-pix_fmt", "yuv420p10le"])
        .arg(&plain)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        return;
    }
    let bytes = std::fs::read(&plain).unwrap();
    for profile in [5, 8] {
        let path = scratch(&format!("dolby_vision_{profile}.mp4"));
        std::fs::write(&path, with_dolby_vision_record(&bytes, profile)).unwrap();
        let p = import(&path);
        assert_eq!(p.dolby_vision_profile, Some(profile), "{}", path.display());
        assert_eq!((p.resolution.width, p.resolution.height), (64, 64), "the picture still reads");
    }
    assert_eq!(import(&plain).dolby_vision_profile, None);
}

/// #3230: a file whose sound runs on past its picture (60 frames at 29.97,
/// 2.002 s, under 2.5 s of sound) says where each ends; the container is
/// as long as the sound, and a clip of the whole file stops at the picture.
#[test]
fn where_the_picture_and_the_sound_end_is_read_at_import() {
    let out = scratch("longer-sound.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=64x64:r=30000/1001:d=2.002", "-f", "lavfi", "-i", "sine=f=440:d=2.5"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !made {
        return;
    }
    let p = import(&out);
    let container = p.duration.to_secs_f64();
    let picture = p.stream_ends.picture.expect("the picture's end");
    let sound = p.stream_ends.sound.expect("the sound's end");
    eprintln!("MEASURE container {container:.6} s, picture {picture:.6} s, sound {sound:.6} s");
    assert!((picture - 60.0 * 1001.0 / 30000.0).abs() < 0.002, "60 frames at 29.97: {picture}");
    assert!(sound > 2.45 && sound <= container + 1e-6, "the sound runs on: {sound} of {container}");
    assert!(container - picture > 0.4, "the container is the sound's length: {container}");
}
