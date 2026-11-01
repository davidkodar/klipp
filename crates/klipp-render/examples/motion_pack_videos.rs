//! The built-in composition templates (#2757) played in turn, each at its
//! own length over a shot, a short card naming it before each: one video
//! per format, written by the export job as Klipp exports. An authoring
//! aid for reviewing the set; nothing in Klipp runs it.
//!
//! `cargo run --release -p klipp-render --example motion_pack_videos -- <clips dir> <photo.png> <out dir>`
//!
//! The clips are named below; use only footage that may be shown.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use klipp_core::{EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, Fps, HdrMode, MediaId, OdtChoice, RateControl, RationalTime, Resolution, TemplateValue, Timecode, ToneMapOp, WorkingColorspace};
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::ExportJob;
use klipp_timeline::composition_template::{builtin_templates, instance_for};
use klipp_timeline::title_library::{face, layers, rect, text_at, upper, LABEL};
use klipp_timeline::{Clip, ClipKind, FontStyle, Sequence, TextAlignment, Track, TrackKind};

/// The shot under every template, then the fills for its slots, in turn.
const UNDER: &str = "12-sunset-sea.mp4";
const FILLS: [&str; 4] = ["06-market-fruit.mp4", "07-promenade-morning-run.mp4", "09-cyclists-street.mp4", "02-harbour-town-calm.mp4"];

const FORMATS: [(&str, u32, u32); 3] = [("16x9", 1920, 1080), ("9x16", 1080, 1920), ("1x1", 1080, 1080)];

/// How long the card naming the next template stays up.
const CARD: f64 = 1.0;

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

/// A pool item for `file`, probed for its size, rate and length.
fn media(file: &Path) -> MediaItem {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate,duration", "-of", "csv=p=0"])
        .arg(file)
        .output()
        .expect("ffprobe runs");
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let f: Vec<&str> = line.split(',').collect();
    let (w, h) = (f[0].parse().expect("width"), f[1].parse().expect("height"));
    let (num, den) = f[2].split_once('/').expect("a rate");
    let fps = Fps::new(num.parse().expect("rate"), den.parse().expect("rate"));
    let secs: f64 = f[3].parse().expect("length");
    let id = MediaId::new();
    MediaItem {
        path: file.to_path_buf(),
        kind: MediaItemKind::File,
        resolution: Resolution { width: w, height: h },
        video_codec: "h264".into(),
        filename: file.file_name().unwrap().to_string_lossy().into_owned(),
        ..MediaItem::new_synthetic_title(id, String::new(), RationalTime::from_secs_f64(secs, fps.num), fps)
    }
}

/// A card naming the template next, white on near-black.
fn card(name: &str, category: &str, from: f64) -> Clip {
    let td = layers(vec![
        rect(0.5, 0.5, 1.0, 1.0, [14, 14, 18, 255], 0.0),
        text_at(face(name, LABEL, FontStyle::Bold, 130.0, [255, 255, 255, 255], TextAlignment::Center), 0.5, 0.45, 0.5),
        text_at(upper(face(category, LABEL, FontStyle::Regular, 46.0, [190, 190, 200, 255], TextAlignment::Center), 3.0), 0.5, 0.57, 0.5),
    ]);
    Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(from + CARD)) }
}

fn settings(out: &Path) -> ExportSettings {
    ExportSettings {
        output_path: out.to_path_buf(),
        format: ExportFormat::Mp4,
        video_codec: ExportVideoCodec::H264,
        resolution: None,
        fps: None,
        crf: 25,
        preset: EncoderPreset::Fast,
        audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None,
        video_bitrate_kbps: None,
        rate_control: RateControl::Crf,
        burn_in_subtitles: false,
        embed_subtitles: false,
        target_lufs: None,
        true_peak_dbtp: None,
        pan_law: Default::default(),
        embed_chapters: false,
        captions_608: false,
        hdr10plus: false,
        bit_depth: 0,
        alpha: false,
        hdr_mode: HdrMode::Sdr,
        tone_map_op: ToneMapOp::Hable,
        odt: OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    }
}

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"))).with_target(false).init();
    let args: Vec<String> = std::env::args().collect();
    let (clips, photo, out) = (PathBuf::from(&args[1]), PathBuf::from(&args[2]), PathBuf::from(&args[3]));
    std::fs::create_dir_all(&out).expect("the output folder");
    let mut pool = MediaPool::new();
    let under = pool.add(media(&clips.join(UNDER)));
    let fills: Vec<MediaId> = FILLS.iter().map(|f| pool.add(media(&clips.join(f)))).collect();
    let templates = builtin_templates();
    for (label, w, h) in FORMATS {
        let mut v1 = Track::new("V1", TrackKind::Video);
        let mut v2 = Track::new("V2", TrackKind::Video);
        let mut all: Vec<Sequence> = Vec::new();
        let mut at = 0.0;
        for t in &templates {
            v2.clips.push(card(&t.name, &t.category, at));
            at += CARD;
            let layout = t.composition_for(w, h).expect("a layout").clone();
            let mut instance = instance_for(t);
            for (n, slot) in t.slots.keys().enumerate() {
                instance.media_slots.insert(slot.clone(), fills[n % fills.len()].0.to_string());
            }
            for (name, p) in &t.parameters {
                if matches!(p.default, TemplateValue::Text(_)) && p.label == "Photo" {
                    instance.overrides.insert(name.clone(), TemplateValue::Text(photo.to_string_lossy().into_owned()));
                }
            }
            let secs = layout.total_duration().to_secs_f64();
            let mut placed = Clip::precomposition(layout.id, tc(at), tc(at + secs));
            placed.kind = ClipKind::TemplatePrecomposition { sequence_id: layout.id, template_instance: instance };
            v2.clips.push(placed);
            // The shot covering the frame, whatever its shape.
            let mut shot = Clip::media(under, tc(at), tc(at + secs), tc(0.0), tc(secs));
            shot.push_effect(klipp_effects::BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: w as f32 / h as f32, focus_x: 0.5, focus_y: 0.5 });
            v1.clips.push(shot);
            all.extend(t.sequences.iter().cloned());
            at += secs;
            println!("{label}: {} ({}) {secs:.2} s", t.name, t.category);
        }
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(24, 1);
        seq.resolution = Resolution { width: w, height: h };
        seq.tracks = vec![v2, v1];
        all.push(seq.clone());
        let file = out.join(format!("motion-pack-{label}.mp4"));
        let _ = std::fs::remove_file(&file);
        let settings = settings(&file);
        let snapshot = SequenceSnapshot::from_sequence_with_all(&seq, &all, &pool, &settings, None, false, WorkingColorspace::Rec709Linear).expect("a snapshot");
        let frames = snapshot.duration_frames;
        let t0 = std::time::Instant::now();
        ExportJob {
            id: uuid::Uuid::new_v4(),
            output_path: file.clone(),
            settings,
            snapshot,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU64::new(0)),
            hw_info: Default::default(),
            render_stage: Default::default(),
            loudness_result: Default::default(),
            timings: Default::default(),
            total_frames: frames,
            analyser: None,
        }
        .run()
        .unwrap_or_else(|e| panic!("{label}: {e}"));
        let mb = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
        println!("wrote {} — {frames} frames in {:.0} s, {mb:.1} MB", file.display(), t0.elapsed().as_secs_f64());
    }
}
