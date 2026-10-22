//! OpenTimelineIO (OTIO) export — the inverse of [`crate::otio_import`].
//!
//! OTIO is the interchange format with adapters to everything else, so
//! one `.otio` file reaches AAF, Avid, Resolve and the rest through
//! `otiotool` without Klipp implementing any of them. That is the point
//! of this module: it is the cheapest route out of the application.
//!
//! Plain `.otio` JSON only — `.otiod` directory bundles and `.otioz` zip
//! archives are out of scope, matching the importer.
//!
//! # The one structural difference from Klipp
//!
//! A Klipp clip carries an absolute `timeline_in`. An OTIO track's
//! children are **sequential**: a clip's position is the sum of the
//! durations before it, and a hole has to be an explicit `Gap`. So the
//! exporter walks each track in time order and fills every hole,
//! including one before the first clip. Get this wrong and the file is
//! still valid OTIO — every later clip is simply in the wrong place,
//! which is the failure mode this module's tests exist to catch.
//!
//! # What does not survive
//!
//! OTIO has no representation for a grade, an effect or a title. Those
//! become named gaps of the right duration, so the timing of everything
//! after them is preserved, and what they were is recorded under
//! `metadata.klipp` — which round-trips through Klipp's own importer one
//! day and is ignored by every other reader. Every drop is reported in
//! the returned warnings so the caller can say what was lost rather than
//! letting the far end discover it.

use std::path::Path;

use klipp_core::MarkerColor;
use klipp_media::MediaPool;
use klipp_timeline::{ClipKind, Sequence, TrackKind};
use serde_json::{json, Value};

/// Error type for OTIO export failures.
#[derive(Debug, thiserror::Error)]
pub enum OtioExportError {
    #[error("Failed to write file: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to serialise OTIO JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// What an export produced: the JSON, and what did not fit in it.
pub struct OtioExport {
    pub json: String,
    /// One line per thing the format could not carry. Empty means the
    /// timeline went out whole.
    pub warnings: Vec<String>,
}

/// Export `seq` as an OTIO timeline to `output_path`.
pub fn export_otio(
    seq: &Sequence,
    media_pool: &MediaPool,
    output_path: &Path,
) -> Result<Vec<String>, OtioExportError> {
    let out = generate_otio(seq, media_pool)?;
    std::fs::write(output_path, &out.json)?;
    Ok(out.warnings)
}

/// Build the OTIO JSON for `seq` without touching the filesystem.
pub fn generate_otio(seq: &Sequence, media_pool: &MediaPool) -> Result<OtioExport, OtioExportError> {
    let mut warnings = Vec::new();
    let rate = seq.fps.as_f64();

    let mut tracks = Vec::new();
    for track in &seq.tracks {
        match track.kind {
            TrackKind::Video | TrackKind::Audio => {
                tracks.push(build_track(track, media_pool, rate, &mut warnings));
            }
            other => {
                // Subtitle and text tracks have no OTIO track kind. They
                // are not dropped silently: sidecar subtitle export is
                // the route for those, and the caller is told.
                warnings.push(format!(
                    "Track '{}' is a {other:?} track, which OTIO has no kind for — not exported; \
                     use subtitle export for it",
                    track.name
                ));
            }
        }
    }

    let markers: Vec<Value> = seq
        .markers
        .iter()
        .map(|m| {
            json!({
                "OTIO_SCHEMA": "Marker.2",
                "name": m.label,
                "color": klipp_color_to_otio(m.color),
                "marked_range": time_range(
                    m.position.to_secs_f64(),
                    m.duration.map(|d| d.to_secs_f64()).unwrap_or(0.0),
                    rate,
                ),
                "comment": m.comment,
                "metadata": {},
            })
        })
        .collect();

    let timeline = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": seq.name,
        // Klipp has no sequence start timecode, so a timeline starts at
        // zero rather than the 10:00:00:00 a broadcast delivery uses.
        "global_start_time": Value::Null,
        "metadata": { "klipp": {
            "resolution": { "width": seq.resolution.width, "height": seq.resolution.height },
            "fps": { "num": seq.fps.num, "den": seq.fps.den },
        }},
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": tracks,
            "markers": markers,
            "effects": [],
            "metadata": {},
            "source_range": Value::Null,
            "enabled": true,
        },
    });

    Ok(OtioExport {
        json: serde_json::to_string_pretty(&timeline)?,
        warnings,
    })
}

/// One OTIO track: the clips in time order, with every hole filled.
fn build_track(
    track: &klipp_timeline::Track,
    media_pool: &MediaPool,
    rate: f64,
    warnings: &mut Vec<String>,
) -> Value {
    let mut children: Vec<Value> = Vec::new();

    // Time order, because OTIO position is implied by what came before.
    let mut clips: Vec<&klipp_timeline::Clip> = track.clips.iter().collect();
    clips.sort_by(|a, b| {
        a.timeline_in
            .to_secs_f64()
            .partial_cmp(&b.timeline_in.to_secs_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut cursor_secs = 0.0f64;
    for clip in clips {
        let start = clip.timeline_in.to_secs_f64();
        let end = clip.timeline_out.to_secs_f64();
        let dur = (end - start).max(0.0);
        if dur <= 0.0 {
            continue;
        }

        // A hole before this clip is a Gap; without it every clip after
        // the hole lands early. Half a frame of tolerance keeps float
        // noise from emitting gaps nobody asked for.
        let hole = start - cursor_secs;
        if hole > 0.5 / rate {
            children.push(gap(hole, rate, None));
        } else if hole < -0.5 / rate {
            // Overlapping clips on one track cannot be expressed by a
            // sequential track at all. Said out loud rather than
            // quietly shifting the rest of the track later.
            warnings.push(format!(
                "Track '{}': a clip at {start:.3} s overlaps the one before it by {:.3} s — \
                 OTIO tracks are sequential, so it was moved to {cursor_secs:.3} s",
                track.name,
                -hole,
            ));
        }

        children.push(clip_value(clip, media_pool, rate, &track.name, warnings));
        cursor_secs = cursor_secs.max(start) + dur;
        // A transition out of this clip, at the cut after it.
        for t in track.transitions.iter().filter(|t| t.clip_a_id == clip.id) {
            children.push(transition_value(t, rate, &track.name, warnings));
        }
    }

    json!({
        "OTIO_SCHEMA": "Track.1",
        "name": track.name,
        "kind": if track.kind == TrackKind::Video { "Video" } else { "Audio" },
        "children": children,
        "markers": [],
        "effects": [],
        "metadata": {},
        "source_range": Value::Null,
        "enabled": !track.muted,
    })
}

/// A transition at its cut: how far it reaches into the clip before and
/// the clip after, as its alignment puts it; a dissolve as OTIO's, any
/// other kind as a custom transition named for it.
fn transition_value(t: &klipp_timeline::Transition, rate: f64, track_name: &str, warnings: &mut Vec<String>) -> Value {
    let d = t.duration.to_secs_f64();
    let (before, after) = match t.alignment {
        klipp_core::TransitionAlignment::Center => (d / 2.0, d / 2.0),
        klipp_core::TransitionAlignment::EndAtCut => (d, 0.0),
        klipp_core::TransitionAlignment::StartAtCut => (0.0, d),
    };
    let (kind, name) = match t.kind {
        klipp_core::TransitionKind::CrossDissolve => ("SMPTE_Dissolve", "Cross Dissolve".to_string()),
        other => {
            warnings.push(format!("Track '{track_name}': a {other:?} transition exported as a custom transition — OTIO names only the dissolve"));
            ("Custom_Transition", format!("{other:?}"))
        }
    };
    json!({
        "OTIO_SCHEMA": "Transition.1",
        "name": name,
        "transition_type": kind,
        "in_offset": rational_time(before, rate),
        "out_offset": rational_time(after, rate),
        "metadata": {},
    })
}

/// One track child: a Clip for media, a named Gap for everything else.
fn clip_value(
    clip: &klipp_timeline::Clip,
    media_pool: &MediaPool,
    rate: f64,
    track_name: &str,
    warnings: &mut Vec<String>,
) -> Value {
    let dur = (clip.timeline_out.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
    let at = clip.timeline_in.to_secs_f64();

    let media_id = match &clip.kind {
        ClipKind::Media { media_id } => Some(*media_id),
        ClipKind::Gap => return gap(dur, rate, None),
        other => {
            // A title, a generator, an adjustment layer or a compound
            // clip. OTIO can carry none of them, so the duration is
            // held by a named gap and what it was goes in metadata —
            // the timing of everything after it is what matters most.
            let what = describe(other);
            warnings.push(format!(
                "Track '{track_name}': {what} at {at:.3} s became a gap — OTIO has no \
                 representation for it",
            ));
            return gap(dur, rate, Some(&what));
        }
    };

    let item = media_id.and_then(|id| media_pool.get(&id));
    let name = item
        .map(|m| m.filename.clone())
        .unwrap_or_else(|| "Untitled".to_string());

    // An offline item still has a path, and an item Klipp lost entirely
    // has none — MissingReference is OTIO's way of saying so, and the
    // importer already reads it back as an offline pool entry.
    let media_reference = match item {
        Some(m) if !m.offline => json!({
            "OTIO_SCHEMA": "ExternalReference.1",
            "target_url": format!("file://{}", m.path.display()),
            "name": m.filename,
            "available_range": time_range(0.0, m.duration.to_secs_f64(), rate),
            "metadata": {},
        }),
        Some(m) => {
            warnings.push(format!(
                "Track '{track_name}': '{}' is offline — exported as a MissingReference",
                m.filename
            ));
            json!({
                "OTIO_SCHEMA": "MissingReference.1",
                "name": m.filename,
                "available_range": Value::Null,
                "metadata": { "klipp": { "path": m.path.display().to_string() } },
            })
        }
        None => json!({
            "OTIO_SCHEMA": "MissingReference.1",
            "name": name,
            "available_range": Value::Null,
            "metadata": {},
        }),
    };

    let markers: Vec<Value> = clip
        .clip_markers
        .iter()
        .map(|m| {
            json!({
                "OTIO_SCHEMA": "Marker.2",
                "name": m.name,
                "color": klipp_color_to_otio(m.color),
                // Clip markers are relative to source_in, which is the
                // frame source_range is measured from too.
                "marked_range": time_range(clip.source_in.to_secs_f64() + m.time_secs, 0.0, rate),
                "comment": m.comment,
                "metadata": {},
            })
        })
        .collect();

    // A constant speed: OTIO keeps the clip's length on the track and says
    // how fast it plays its source.
    let effects: Vec<Value> = if (clip.speed - 1.0).abs() > 1e-9 {
        vec![json!({ "OTIO_SCHEMA": "LinearTimeWarp.1", "name": "speed", "time_scalar": clip.speed, "metadata": {} })]
    } else {
        Vec::new()
    };

    json!({
        "OTIO_SCHEMA": "Clip.1",
        "name": name,
        "source_range": time_range(clip.source_in.to_secs_f64(), dur, rate),
        "media_reference": media_reference,
        "markers": markers,
        "effects": effects,
        "metadata": {},
        "enabled": clip.enabled,
    })
}

/// A Gap of `dur` seconds, optionally named for what it stands in for.
fn gap(dur: f64, rate: f64, stood_in_for: Option<&str>) -> Value {
    json!({
        "OTIO_SCHEMA": "Gap.1",
        "name": stood_in_for.unwrap_or(""),
        "source_range": time_range(0.0, dur, rate),
        "markers": [],
        "effects": [],
        "metadata": match stood_in_for {
            Some(what) => json!({ "klipp": { "was": what } }),
            None => json!({}),
        },
        "enabled": true,
    })
}

/// An OTIO `TimeRange` from seconds, at the timeline's rate.
///
/// OTIO counts in frames at a stated rate, where Klipp's `RationalTime`
/// counts ticks of the fps numerator — so the conversion goes through
/// seconds, exactly as `otio_import::rt_from_otio` does coming back.
fn time_range(start_secs: f64, dur_secs: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational_time(start_secs, rate),
        "duration": rational_time(dur_secs, rate),
    })
}

fn rational_time(secs: f64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "RationalTime.1",
        "rate": rate,
        // Frames, rounded to the grid: a value a fraction of a frame off
        // reimports as a clip a frame short.
        "value": (secs * rate).round(),
    })
}

/// The inverse of `otio_import::otio_color_to_klipp`.
///
/// That mapping folds CYAN onto Blue and MAGENTA and PINK onto Purple,
/// so a round trip through OTIO settles on the six Klipp has and does
/// not invent the ones it does not.
fn klipp_color_to_otio(c: MarkerColor) -> &'static str {
    match c {
        MarkerColor::Red => "RED",
        MarkerColor::Orange => "ORANGE",
        MarkerColor::Yellow => "YELLOW",
        MarkerColor::Green => "GREEN",
        MarkerColor::Blue => "BLUE",
        MarkerColor::Purple => "PURPLE",
    }
}

/// What a non-media clip is, for the warning and the gap's name.
fn describe(kind: &ClipKind) -> String {
    match kind {
        ClipKind::Title(td) => {
            let text = td.text();
            let text = text.trim();
            if text.is_empty() {
                "a title".to_string()
            } else {
                format!("the title \"{}\"", text.chars().take(40).collect::<String>())
            }
        }
        ClipKind::Generator(_) => "a generator".to_string(),
        ClipKind::AdjustmentLayer => "an adjustment layer".to_string(),
        ClipKind::Sequence { .. } => "a compound clip".to_string(),
        ClipKind::Precomposition { .. } => "a precomposition".to_string(),
        ClipKind::TemplatePrecomposition { .. } => "a template precomposition".to_string(),
        ClipKind::Media { .. } | ClipKind::Gap => "a clip".to_string(),
    }
}

/// Why an `.otio` file could not be converted (#89).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    /// No `otioconvert` on the PATH: OpenTimelineIO is not installed.
    #[error("OpenTimelineIO's otioconvert is not installed")]
    NoConverter,
    /// OpenTimelineIO has no adapter for the output's extension: for AAF,
    /// the `otio-aaf-adapter` plugin is not installed.
    #[error("OpenTimelineIO has no adapter that writes .{0} files")]
    NoAdapter(String),
    /// The converter ran and failed; its last line of output.
    #[error("otioconvert failed: {0}")]
    Failed(String),
}

/// Convert the `.otio` file `otio` to `out`, in the format its extension
/// names, with OpenTimelineIO's own converter: how Klipp reaches AAF
/// (#89) and any other format an installed OTIO adapter writes, without
/// implementing them.
pub fn convert(otio: &Path, out: &Path) -> Result<(), ConvertError> {
    convert_with("otioconvert", otio, out)
}

/// [`convert`] with the converter named `program`.
fn convert_with(program: &str, otio: &Path, out: &Path) -> Result<(), ConvertError> {
    let ran = std::process::Command::new(program).arg("-i").arg(otio).arg("-o").arg(out).output();
    let ran = match ran {
        Ok(ran) => ran,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ConvertError::NoConverter),
        Err(e) => return Err(ConvertError::Failed(e.to_string())),
    };
    if ran.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&ran.stderr);
    if stderr.contains("NoKnownAdapterForExtensionError") {
        let ext = out.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(ConvertError::NoAdapter(ext));
    }
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no message").trim().to_string();
    Err(ConvertError::Failed(last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Fps, MarkerId, MediaId, RationalTime, Timecode};
    use klipp_media::{MediaItem, MediaItemKind};
    use klipp_timeline::{Clip, Marker, Track};

    // Built from the real constructors rather than struct literals, so a
    // new field on Sequence or MediaItem does not break every test here
    // — which is exactly what happened when the sequence gained a
    // camera.
    fn seq_with(fps: Fps, tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.name = "Round Trip".into();
        s.fps = fps;
        s.tracks = tracks;
        s
    }

    fn item(id: MediaId, path: &str, secs: f64, rate: u32) -> MediaItem {
        let fps = Fps { num: rate, den: 1 };
        let mut m = MediaItem::new_synthetic_title(
            id,
            String::new(),
            Timecode::from_secs_f64(secs, rate),
            fps,
        );
        m.kind = MediaItemKind::File;
        m.path = std::path::PathBuf::from(path);
        m.filename = std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        m
    }

    fn media_clip(id: MediaId, tl_in: f64, tl_out: f64, src_in: f64, rate: u32) -> Clip {
        Clip::media(
            id,
            RationalTime::from_secs_f64(tl_in, rate),
            RationalTime::from_secs_f64(tl_out, rate),
            RationalTime::from_secs_f64(src_in, rate),
            RationalTime::from_secs_f64(src_in + (tl_out - tl_in), rate),
        )
    }

    /// Parse the exported JSON back through the importer, from a real
    /// file, because that is the only path a far end would use.
    fn round_trip(seq: &Sequence, pool: &MediaPool) -> crate::otio_import::ImportResult {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);

        let out = generate_otio(seq, pool).expect("export");
        let dir = std::env::temp_dir().join(format!("klipp-otio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // One file per call: cargo runs these on parallel threads in one
        // process, and sharing a path means each test reads whatever
        // another was halfway through writing.
        let path = dir.join(format!("rt-{}.otio", N.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&path, &out.json).unwrap();
        let r = crate::otio_import::load_otio(&path)
            .unwrap_or_else(|e| panic!("import failed: {e}\n{}", out.json));
        let _ = std::fs::remove_file(&path);
        r
    }

    #[test]
    fn a_gap_is_written_so_later_clips_keep_their_place() {
        // The whole structural difference between the two models. A
        // Klipp clip knows where it is; an OTIO track child only knows
        // what came before it. Miss the hole and the second clip lands
        // two seconds early — in a file that is still valid OTIO.
        let fps = Fps::new(25, 1);
        let mut track = Track::new("Video 1", TrackKind::Video);
        let mid = MediaId::new();
        track.clips.push(media_clip(mid, 0.0, 1.0, 0.0, fps.rate()));
        track.clips.push(media_clip(mid, 3.0, 4.0, 0.0, fps.rate())); // 2 s hole
        let seq = seq_with(fps, vec![track]);
        let mut pool = MediaPool::new();
        pool.add(item(mid, "/media/a.mov", 10.0, fps.rate()));

        let back = round_trip(&seq, &pool);
        let clips = &back.sequence.tracks[0].clips;
        assert_eq!(clips.len(), 3, "clip, gap, clip: {:?}", clips.len());
        assert!(clips[1].kind.is_gap(), "the hole should be a Gap");
        let second = clips[2].timeline_in.to_secs_f64();
        assert!((second - 3.0).abs() < 0.02, "second clip came back at {second} s, not 3.0");
    }

    #[test]
    fn a_leading_hole_is_a_gap_too() {
        // A track whose first clip does not start at zero.
        let fps = Fps::new(25, 1);
        let mut track = Track::new("Video 1", TrackKind::Video);
        let mid = MediaId::new();
        track.clips.push(media_clip(mid, 2.0, 3.0, 0.0, fps.rate()));
        let seq = seq_with(fps, vec![track]);
        let mut pool = MediaPool::new();
        pool.add(item(mid, "/media/a.mov", 10.0, fps.rate()));

        let back = round_trip(&seq, &pool);
        let clips = &back.sequence.tracks[0].clips;
        assert!(clips[0].kind.is_gap(), "leading hole should be a Gap");
        let at = clips[1].timeline_in.to_secs_f64();
        assert!((at - 2.0).abs() < 0.02, "clip came back at {at} s, not 2.0");
    }

    #[test]
    fn ntsc_times_survive_the_round_trip() {
        // OTIO counts frames at a stated rate; Klipp counts ticks of the
        // fps numerator. 23.976 is where a conversion that goes through
        // the wrong one drifts.
        let fps = Fps::ntsc_23976();
        let mut track = Track::new("Video 1", TrackKind::Video);
        let mid = MediaId::new();
        track.clips.push(media_clip(mid, 0.0, 10.0, 5.0, fps.rate()));
        let seq = seq_with(fps, vec![track]);
        let mut pool = MediaPool::new();
        pool.add(item(mid, "/media/a.mov", 60.0, fps.rate()));

        let back = round_trip(&seq, &pool);
        assert!((back.sequence.fps.as_f64() - 23.976).abs() < 0.01, "{:?}", back.sequence.fps);
        let c = &back.sequence.tracks[0].clips[0];
        let dur = c.timeline_out.to_secs_f64() - c.timeline_in.to_secs_f64();
        assert!((dur - 10.0).abs() < 0.05, "duration came back {dur} s");
        assert!((c.source_in.to_secs_f64() - 5.0).abs() < 0.05, "source_in {}", c.source_in.to_secs_f64());
    }

    #[test]
    fn what_otio_cannot_carry_is_reported_and_keeps_its_duration() {
        // A title has no OTIO representation. Dropping it silently
        // would move everything after it; a named gap keeps the timing
        // and the warning says what happened.
        let fps = Fps::new(25, 1);
        let mut track = Track::new("Video 1", TrackKind::Video);
        let mid = MediaId::new();
        let mut title = Clip::media(
            mid,
            RationalTime::from_secs_f64(0.0, fps.rate()),
            RationalTime::from_secs_f64(2.0, fps.rate()),
            RationalTime::from_secs_f64(0.0, fps.rate()),
            RationalTime::from_secs_f64(2.0, fps.rate()),
        );
        title.kind = ClipKind::Title(Default::default());
        track.clips.push(title);
        track.clips.push(media_clip(mid, 2.0, 4.0, 0.0, fps.rate()));
        let seq = seq_with(fps, vec![track]);
        let mut pool = MediaPool::new();
        pool.add(item(mid, "/media/a.mov", 10.0, fps.rate()));

        let out = generate_otio(&seq, &pool).expect("export");
        assert!(
            out.warnings.iter().any(|w| w.contains("became a gap")),
            "the drop should be reported: {:?}",
            out.warnings
        );

        let back = round_trip(&seq, &pool);
        let clips = &back.sequence.tracks[0].clips;
        let last = clips.last().expect("a clip");
        assert!(
            (last.timeline_in.to_secs_f64() - 2.0).abs() < 0.02,
            "the clip after the title should still be at 2.0 s, not {}",
            last.timeline_in.to_secs_f64()
        );
    }

    #[test]
    fn markers_and_their_colours_survive() {
        let fps = Fps::new(25, 1);
        let mut seq = seq_with(fps, vec![Track::new("Video 1", TrackKind::Video)]);
        seq.markers.push(Marker { review: None,
            id: MarkerId::new(),
            position: Timecode::from_secs_f64(1.0, fps.rate()),
            label: "Check the grade".into(),
            color: MarkerColor::Orange,
            duration: None,
            comment: String::new(),
            kind: Default::default(),
        });
        let pool = MediaPool::new();

        let back = round_trip(&seq, &pool);
        assert_eq!(back.sequence.markers.len(), 1, "{:?}", back.sequence.markers);
        let m = &back.sequence.markers[0];
        assert_eq!(m.label, "Check the grade");
        assert_eq!(m.color, MarkerColor::Orange);
        assert!((m.position.to_secs_f64() - 1.0).abs() < 0.02);
    }

    #[test]
    fn an_offline_source_becomes_a_missing_reference() {
        let fps = Fps::new(25, 1);
        let mut track = Track::new("Video 1", TrackKind::Video);
        let mid = MediaId::new();
        track.clips.push(media_clip(mid, 0.0, 1.0, 0.0, fps.rate()));
        let seq = seq_with(fps, vec![track]);
        let mut pool = MediaPool::new();
        let mut m = item(mid, "/gone/a.mov", 10.0, fps.rate());
        m.offline = true;
        pool.add(m);

        let out = generate_otio(&seq, &pool).expect("export");
        assert!(out.json.contains("MissingReference"), "{}", out.json);
        assert!(
            out.warnings.iter().any(|w| w.contains("offline")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_subtitle_track_is_reported_rather_than_dropped_quietly() {
        let fps = Fps::new(25, 1);
        let seq = seq_with(fps, vec![Track::new("Subtitles", TrackKind::Subtitle)]);
        let pool = MediaPool::new();

        let out = generate_otio(&seq, &pool).expect("export");
        assert!(
            out.warnings.iter().any(|w| w.contains("Subtitle")),
            "{:?}",
            out.warnings
        );
    }

    /// No converter installed: said as such, not as a failed run (#89).
    #[test]
    fn a_missing_converter_is_named() {
        let dir = std::env::temp_dir();
        let got = super::convert_with("klipp-no-such-otioconvert", &dir.join("in.otio"), &dir.join("out.aaf"));
        assert_eq!(got, Err(super::ConvertError::NoConverter));
    }

    /// With OpenTimelineIO installed: a file it has an adapter for is
    /// written, and AAF is written, or refused as the adapter missing.
    #[test]
    fn otioconvert_writes_what_it_has_an_adapter_for() {
        let dir = std::env::temp_dir().join(format!("klipp-otio-convert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.otio");
        std::fs::write(&input, r#"{"OTIO_SCHEMA":"Timeline.1","name":"t","tracks":{"OTIO_SCHEMA":"Stack.1","children":[]}}"#).unwrap();
        match super::convert(&input, &dir.join("out.otio")) {
            Err(super::ConvertError::NoConverter) => {
                eprintln!("skipping: no otioconvert");
                return;
            }
            got => assert_eq!(got, Ok(()), "OTIO to OTIO"),
        }
        assert!(dir.join("out.otio").exists());
        match super::convert(&input, &dir.join("out.aaf")) {
            Ok(()) => assert!(dir.join("out.aaf").exists()),
            Err(e) => assert_eq!(e, super::ConvertError::NoAdapter("aaf".into())),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dissolve, a clip at double speed, marker notes and the picture
    /// size go out and come back as they were.
    #[test]
    fn transitions_speed_notes_and_size_survive() {
        let id = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(item(id, "/tmp/rt.mov", 60.0, 24));
        let a = media_clip(id, 0.0, 2.0, 0.0, 24);
        let mut b = media_clip(id, 2.0, 4.0, 10.0, 24);
        b.speed = 2.0;
        b.source_out = RationalTime::from_secs_f64(14.0, 24);
        b.clip_markers.push(klipp_timeline::ClipMarker { time_secs: 1.0, name: "flare".into(), color: klipp_core::MarkerColor::Red, comment: "fix it".into() });
        let mut v = Track::new("V1", TrackKind::Video);
        v.transitions.push(klipp_timeline::Transition {
            id: klipp_core::TransitionId::new(),
            kind: klipp_core::TransitionKind::CrossDissolve,
            duration: RationalTime::from_secs_f64(0.5, 24),
            clip_a_id: a.id,
            clip_b_id: b.id,
            user_params: Default::default(),
            alignment: klipp_core::TransitionAlignment::Center,
        });
        v.clips = vec![a, b];
        let mut seq = seq_with(Fps { num: 24, den: 1 }, vec![v]);
        seq.resolution = klipp_core::Resolution { width: 1280, height: 720 };
        seq.markers.push(Marker { review: None, id: MarkerId::new(), position: RationalTime::from_secs_f64(1.0, 24), label: "beat".into(), color: klipp_core::MarkerColor::Blue, duration: None, comment: "music".into(), kind: klipp_core::MarkerKind::None });

        let back = round_trip(&seq, &pool);
        let track = &back.sequence.tracks[0];
        let t = &track.transitions[0];
        assert_eq!((t.clip_a_id, t.clip_b_id, t.alignment), (track.clips[0].id, track.clips[1].id, klipp_core::TransitionAlignment::Center));
        assert!((t.duration.to_secs_f64() - 0.5).abs() < 1e-9);
        let b = &track.clips[1];
        assert_eq!(b.speed, 2.0);
        assert!(((b.source_out - b.source_in).to_secs_f64() - 4.0).abs() < 1e-9, "two seconds on the track, four of the source");
        assert_eq!(b.clip_markers[0].comment, "fix it");
        assert_eq!(back.sequence.markers[0].comment, "music");
        assert_eq!(back.sequence.resolution, klipp_core::Resolution { width: 1280, height: 720 });
    }
}
