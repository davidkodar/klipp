//! #3264: a track bounced to a media file. Its frames drawn alone at full
//! size with their alpha: transparent in the gap between clips, a clip at
//! half opacity half transparent, and over the track below the bounced
//! clip composites as the track did. Its linked sound is in the file,
//! sample for sample. Highlights above white are clipped. Tracks that
//! can't be bounced are refused, saying why.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use klipp_core::{MediaId, RationalTime, WorkingColorspace};
use klipp_engine::bounce::{self, BounceFormat, BounceRequest, BounceResult};
use klipp_render::MediaFile;
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

mod common;

const W: u32 = 320;
const H: u32 = 180;

/// A bounce's job, cancelled and joined when let go, a failing test's
/// included: a test that panicked before its `join` left the thread in
/// ffmpeg while the binary exited, and the process died in `exit()`
/// (#3355).
struct Joined(Arc<klipp_jobs::ThreadJob>);

impl Drop for Joined {
    fn drop(&mut self) {
        use klipp_jobs::BackgroundJob;
        self.0.cancel();
        self.0.join();
    }
}

impl std::ops::Deref for Joined {
    type Target = klipp_jobs::ThreadJob;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// [`bounce::spawn_bounce`] from `files`, its result sent to `tx`, held
/// by a [`Joined`].
fn spawn(req: BounceRequest, files: Arc<HashMap<MediaId, MediaFile>>, tx: std::sync::mpsc::Sender<BounceResult>) -> Joined {
    Joined(bounce::spawn_bounce(req, move || klipp_render::ExportFrameSource::shared(files, 24.0, WorkingColorspace::Rec709Linear), move |r| {
        let _ = tx.send(r);
    }))
}

fn tc(f: i64) -> RationalTime {
    RationalTime::from_frames(f, 24)
}

fn ffmpeg(args: &[&str], out: &std::path::Path) -> bool {
    std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-y"]).args(args).arg(out).status().is_ok_and(|s| s.success())
}

/// A 4:4:4 FFV1 picture and a 440 Hz tone, two seconds each.
///
/// Written once per process: the tests here run at once, and `ffmpeg -y`
/// rewriting the files while another test bounced from them gave that
/// one a short or broken source.
fn sources() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    static SOURCES: std::sync::OnceLock<Option<(std::path::PathBuf, std::path::PathBuf)>> = std::sync::OnceLock::new();
    SOURCES
        .get_or_init(|| {
            let dir = klipp_testing::scratch::scratch_dir("klipp-bounce");
            let (pic, tone) = (dir.join("pic.mkv"), dir.join("tone.wav"));
            let ok = ffmpeg(&["-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=24:d=2"), "-c:v", "ffv1", "-pix_fmt", "yuv444p"], &pic)
                && ffmpeg(&["-f", "lavfi", "-i", "sine=f=440:d=2:r=48000", "-ac", "2", "-c:a", "pcm_s16le"], &tone);
            ok.then_some((pic, tone))
        })
        .clone()
}

fn file(path: &std::path::Path, secs: f64) -> MediaFile {
    MediaFile { path: path.to_path_buf(), log_format: None, source_rotation: 0, duration_secs: Some(secs), fps: Some((24, 1)), resolution: Some((W, H)), scene_transfer: None }
}

/// V1, bounced: clip A over frames 0..12, a gap, clip B at half opacity
/// over 18..30. V2: red under all. A1: a tone linked to A.
struct Timeline {
    seq: Sequence,
    pool: klipp_media::MediaPool,
    files: HashMap<MediaId, MediaFile>,
    v1: klipp_core::TrackId,
}

fn timeline(pic: &std::path::Path, tone: &std::path::Path, a_grade: Option<klipp_nodes::NodeGraph>) -> Timeline {
    let (pic_id, tone_id) = (MediaId::new(), MediaId::new());
    let mut pool = klipp_media::MediaPool::default();
    let mut item = klipp_media::MediaItem::synthetic_for_test(pic.to_path_buf(), None);
    (item.id, item.resolution, item.has_audio) = (pic_id, klipp_core::Resolution { width: W, height: H }, false);
    item.duration = tc(48);
    pool.add(item);
    let mut item = klipp_media::MediaItem::synthetic_for_test(tone.to_path_buf(), None);
    (item.id, item.video_codec) = (tone_id, String::new());
    item.duration = tc(48);
    pool.add(item);
    let files: HashMap<MediaId, MediaFile> = [(pic_id, file(pic, 2.0)), (tone_id, file(tone, 2.0))].into();

    let mut a = Clip::media(pic_id, tc(0), tc(12), tc(0), tc(12));
    if let Some(g) = a_grade {
        a.set_grade(g);
    }
    let b = TestClip::of(Clip::media(pic_id, tc(18), tc(30), tc(12), tc(24))).effects([klipp_effects::BuiltinEffect::Opacity { value: 0.5 }]).build();
    let mut sound = Clip::media(tone_id, tc(0), tc(12), tc(0), tc(12));
    (a.linked_clip_id, sound.linked_clip_id) = (Some(sound.id), Some(a.id));
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![a, b];
    let mut v2 = Track::new("V2", TrackKind::Video);
    v2.clips = vec![Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [255, 0, 0, 255] }), ..Clip::gap(tc(0), tc(36)) }];
    let mut a1 = Track::new("A1", TrackKind::Audio);
    a1.clips = vec![sound];
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::film_24();
    seq.resolution = klipp_core::Resolution { width: W, height: H };
    let v1_id = v1.id;
    seq.tracks = vec![v1, v2, a1];
    Timeline { seq, pool, files, v1: v1_id }
}

/// Bounce track 0 of `t` as `format`; the result.
fn bounce(t: &Timeline, format: BounceFormat, name: &str) -> BounceResult {
    let flat = klipp_timeline::flatten::flatten_sequence(&t.seq, &[]);
    assert_eq!(bounce::refusal(&flat, 0, WorkingColorspace::Rec709Linear), None);
    let (first, end) = bounce::span(&flat, 0).expect("something on the track");
    assert_eq!((first, end), (0, 30), "from the first clip to the last");
    let sound = klipp_render::stem::linked_sound_snapshot(&flat, &[], &t.pool, t.v1).unwrap().expect("a linked sound");
    let dir = klipp_testing::scratch::scratch_dir("klipp-bounce-out");
    let path = dir.join(format!("{name}.{}", format.container().extension()));
    let req = BounceRequest { seq: Arc::new(flat), track: 0, first, end, size: (W, H), working: WorkingColorspace::Rec709Linear, format, path, sound: Some(sound) };
    let files = Arc::new(t.files.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let job = spawn(req, files, tx);
    let result = rx.recv_timeout(Duration::from_secs(300)).expect("the bounce finished");
    // Its thread ended, so nothing of it is closing down as the test
    // binary exits.
    job.join();
    result
}

/// The bounced file's frame `f` in linear light.
fn read(path: &std::path::Path, f: i64) -> Vec<f32> {
    let mut reader = klipp_media::frame_reader::SeekableFrameReader::open_linear(path, None, WorkingColorspace::Rec709Linear).expect("opens");
    reader.decode_at(tc(f).to_secs_f64() + 0.001).expect("decodes").0
}

fn alphas(px: &[f32]) -> (f32, f32) {
    let a: Vec<f32> = px.chunks_exact(4).map(|p| p[3]).collect();
    (a.iter().copied().fold(f32::MAX, f32::min), a.iter().copied().fold(0.0, f32::max))
}

/// Whole frame `f` of `seq`, rendered as export renders it.
fn composite(seq: &Sequence, files: &HashMap<MediaId, MediaFile>, f: i64) -> Vec<f32> {
    let mut src = klipp_render::ExportFrameSource::shared(Arc::new(files.clone()), 24.0, WorkingColorspace::Rec709Linear);
    let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let flat = klipp_timeline::flatten::flatten_sequence(seq, &[]);
    r.render(&flat, tc(f), W, H, &mut src, klipp_compose::RenderOptions::default())
}

fn worst_encoded(a: &[f32], b: &[f32]) -> f32 {
    let enc = |v: f32| klipp_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0));
    a.chunks_exact(4).zip(b.chunks_exact(4)).flat_map(|(x, y)| (0..3).map(move |c| (enc(x[c]) - enc(y[c])).abs())).fold(0.0, f32::max) * 255.0
}

#[test]
fn a_track_bounces_with_its_alpha_opacity_and_sound() {
    let Some((pic, tone)) = sources() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let t = timeline(&pic, &tone, None);
    let started = std::time::Instant::now();
    let r = bounce(&t, BounceFormat::Ffv1, "ffv1");
    eprintln!("MEASURE bounced 30 frames of {W}x{H} as FFV1 in {:.2} s", started.elapsed().as_secs_f64());
    assert_eq!((r.error.as_deref(), r.cancelled, r.frames), (None, false, 30));
    let path = r.path.expect("written");
    assert!(!klipp_render::partial_path_for(&path).exists(), "the partial file renamed into place");

    // The picture: opaque in A, transparent in the gap, half in B.
    let (a, gap, b) = (read(&path, 6), read(&path, 15), read(&path, 24));
    assert_eq!(alphas(&gap), (0.0, 0.0), "the gap transparent");
    let (lo, hi) = alphas(&a);
    assert!(lo > 0.999 && hi <= 1.0, "clip A opaque: {lo}..{hi}");
    let (lo, hi) = alphas(&b);
    assert!((lo - 0.5).abs() < 0.003 && (hi - 0.5).abs() < 0.003, "clip B's opacity baked into its alpha: {lo}..{hi}");

    // Over the red track, the bounced clip composites as the track did.
    let mut bounced = t.seq.clone();
    let bounced_id = MediaId::new();
    let mut files = t.files.clone();
    files.insert(bounced_id, file(&path, 30.0 / 24.0));
    bounced.tracks[0].clips = vec![Clip::media(bounced_id, tc(0), tc(30), tc(0), tc(30))];
    for f in [6, 15, 24] {
        let worst = worst_encoded(&composite(&t.seq, &t.files, f), &composite(&bounced, &files, f));
        eprintln!("MEASURE composite at frame {f}: bounced vs live, worst {worst:.2} of 255");
        assert!(worst <= 2.0, "frame {f}: the bounced clip composites as the track did, worst {worst}");
    }

    // The sound: the linked tone alone, sample for sample (24-bit PCM).
    let flat = klipp_timeline::flatten::flatten_sequence(&t.seq, &[]);
    let snap = klipp_render::stem::linked_sound_snapshot(&flat, &[], &t.pool, t.v1).unwrap().unwrap();
    let mut mix = klipp_render::AudioMixdown::new(&snap).unwrap();
    let want: Vec<f32> = (0..30).flat_map(|f| mix.get_frame_audio(f)).collect();
    let out = std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-i"]).arg(&path).args(["-f", "f32le", "-ac", "2", "-ar", "48000", "-"]).output().unwrap();
    let got: Vec<f32> = out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    assert!(got.len() >= want.len() * 99 / 100, "the sound's length: {} of {}", got.len(), want.len());
    let worst = want.iter().zip(&got).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    let loudest = want.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    eprintln!("MEASURE sound: worst {worst:.2e} against the linked tone alone, loudest {loudest:.3}");
    // ffmpeg's tone is an eighth of full scale, centred at -3 dB.
    assert!(loudest > 0.05 && worst < 1e-4, "the linked sound, sample for sample: worst {worst}, loudest {loudest}");
    // 48 kHz stereo at 24 fps: 4000 samples a frame.
    let silent = got[(18 * 4000)..(24 * 4000)].iter().all(|v| v.abs() < 1e-6);
    assert!(silent, "silent where nothing is linked");

    // ProRes 4444, the default: the same alpha within its 10-bit coding.
    let started = std::time::Instant::now();
    let r = bounce(&t, BounceFormat::ProRes4444, "prores");
    eprintln!("MEASURE bounced 30 frames of {W}x{H} as ProRes 4444 in {:.2} s", started.elapsed().as_secs_f64());
    let path = r.path.expect("written");
    let (gap, b) = (read(&path, 15), read(&path, 24));
    assert!(alphas(&gap).1 < 0.002, "ProRes: the gap transparent: {:?}", alphas(&gap));
    let (lo, hi) = alphas(&b);
    assert!((lo - 0.5).abs() < 0.003 && (hi - 0.5).abs() < 0.003, "ProRes: clip B at half: {lo}..{hi}");
}

/// The file is display-referred: a clip graded past white comes back
/// clipped at white, where the live layer goes over it.
#[test]
fn highlights_above_white_are_clipped() {
    let Some((pic, tone)) = sources() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let t = timeline(&pic, &tone, Some(common::exposure_graph(3.0)));
    let flat = klipp_timeline::flatten::flatten_sequence(&t.seq, &[]);
    let mut src = klipp_render::ExportFrameSource::shared(Arc::new(t.files.clone()), 24.0, WorkingColorspace::Rec709Linear);
    let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    let live = r.render_track_layer(&flat, 0, tc(6), W, H, &mut src).expect("drawn alone");
    let live_max = live.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).fold(0.0f32, f32::max);
    let res = bounce(&t, BounceFormat::Ffv1, "clipped");
    let back = read(res.path.as_ref().expect("written"), 6);
    let back_max = back.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).fold(0.0f32, f32::max);
    eprintln!("MEASURE three stops up: the live layer reaches {live_max:.2}, the bounced file {back_max:.4}");
    assert!(live_max > 1.5, "the live layer goes past white: {live_max}");
    assert!(back_max <= 1.0 + 1e-4, "the bounced file stops at white: {back_max}");
    assert!(bounce::CLIPPING_NOTE.contains("clipped") && bounce::CLIPPING_NOTE.contains("grade before bouncing"));
}

#[test]
fn a_track_that_cant_be_bounced_is_refused_saying_why() {
    let tc = |f: i64| RationalTime::from_frames(f, 24);
    let mut seq = Sequence::new_default();
    seq.fps = klipp_core::Fps::film_24();
    let clip = || Clip::media(MediaId::new(), tc(0), tc(12), tc(0), tc(12));
    let mut v = Track::new("V1", TrackKind::Video);
    v.clips = vec![clip()];
    seq.tracks = vec![v];
    assert_eq!(bounce::refusal(&seq, 0, WorkingColorspace::Rec709Linear), None);
    let says = |s: &Sequence, ws| bounce::refusal(s, 0, ws).unwrap_or_default();
    assert!(says(&seq, WorkingColorspace::Bt2020Linear).contains("SDR"), "a wide-gamut project");
    assert!(says(&seq, WorkingColorspace::AcesCG).contains("SDR"), "ACES");
    let mut hidden = seq.clone();
    hidden.tracks[0].visible = false;
    assert!(says(&hidden, WorkingColorspace::Rec709Linear).contains("hidden"));
    let mut empty = seq.clone();
    empty.tracks[0].clips.clear();
    assert!(says(&empty, WorkingColorspace::Rec709Linear).contains("nothing"));
    let mut blends = seq.clone();
    let mut screen = clip();
    screen.blend_mode = klipp_core::BlendMode::Screen;
    blends.tracks[0].clips.push(Clip { timeline_in: tc(20), timeline_out: tc(30), ..screen.clone() });
    assert!(says(&blends, WorkingColorspace::Rec709Linear).contains("blend in different ways"), "clips blending apart");
    blends.tracks[0].clips[0].blend_mode = klipp_core::BlendMode::Screen;
    assert_eq!(bounce::refusal(&blends, 0, WorkingColorspace::Rec709Linear), None, "all Screen: the bounced clip takes it");
    assert_eq!(bounce::blend_mode(&blends, 0), Some(klipp_core::BlendMode::Screen));
    let mut sound = seq.clone();
    sound.tracks[0].kind = TrackKind::Audio;
    assert!(says(&sound, WorkingColorspace::Rec709Linear).contains("picture"));
    // A caption track draws against the others: it can't be drawn alone.
    let mut captions = seq.clone();
    captions.tracks[0].kind = TrackKind::Subtitle;
    assert!(!says(&captions, WorkingColorspace::Rec709Linear).is_empty(), "captions refused");
    // Linked sound through a plug-in from outside Klipp.
    let mut a1 = Track::new("A1", TrackKind::Audio);
    let mut s = clip();
    s.linked_clip_id = Some(seq.tracks[0].clips[0].id);
    s.audio_effects.push(klipp_core::AudioEffectInstance { effect_type: "lv2:http://example.org/reverb".into(), enabled: true, wet_dry: 1.0, params: vec![] });
    a1.clips = vec![s];
    let mut lv2 = seq.clone();
    lv2.tracks.push(a1);
    let v1 = lv2.tracks[0].id;
    assert!(klipp_render::stem::linked_sound_refusal(&lv2, v1).is_some_and(|r| r.contains("outside Klipp")));
    assert_eq!(klipp_render::stem::linked_sound_refusal(&seq, v1), None);
}

#[test]
fn a_prores_4444_bounce_is_about_its_estimate() {
    assert_eq!(bounce::estimate_bytes(BounceFormat::Ffv1, (1920, 1080), 24), None, "FFV1 follows the picture");
    let minute = bounce::estimate_bytes(BounceFormat::ProRes4444, (1920, 1080), 24 * 60).unwrap() as f64 / 1e9;
    assert!((1.5..3.0).contains(&minute), "about 2 GB a minute at 1080p24: {minute:.2}");
}

/// Measured, not a check: bouncing five seconds of one clip at 1080p and
/// at 4K as ProRes 4444, the time and the file against the estimate. Run
/// alone: `--ignored measure_bounce`.
#[test]
#[ignore]
fn measure_bounce_at_1080p_and_4k() {
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        let dir = klipp_testing::scratch::scratch_dir("klipp-bounce-measure");
        let src = dir.join(format!("src-{w}.mp4"));
        if !ffmpeg(&["-f", "lavfi", "-i", &format!("testsrc2=s={w}x{h}:r=24:d=5"), "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"], &src) {
            eprintln!("skipping: no ffmpeg");
            return;
        }
        let id = MediaId::new();
        let files: HashMap<MediaId, MediaFile> = [(id, MediaFile { path: src.clone(), log_format: None, source_rotation: 0, duration_secs: Some(5.0), fps: Some((24, 1)), resolution: Some((w, h)), scene_transfer: None })].into();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![Clip::media(id, tc(0), tc(120), tc(0), tc(120))];
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::film_24();
        seq.resolution = klipp_core::Resolution { width: w, height: h };
        seq.tracks = vec![v1];
        let flat = klipp_timeline::flatten::flatten_sequence(&seq, &[]);
        let path = dir.join(format!("bounce-{w}.mov"));
        let req = BounceRequest { seq: Arc::new(flat), track: 0, first: 0, end: 120, size: (w, h), working: WorkingColorspace::Rec709Linear, format: BounceFormat::ProRes4444, path: path.clone(), sound: None };
        let files = Arc::new(files);
        let (tx, rx) = std::sync::mpsc::channel();
        let started = std::time::Instant::now();
        let job = spawn(req, files, tx);
        let r = rx.recv_timeout(Duration::from_secs(900)).expect("finished");
        job.join();
        let secs = started.elapsed().as_secs_f64();
        assert!(r.error.is_none(), "{:?}", r.error);
        let bytes = std::fs::metadata(&path).unwrap().len();
        let estimate = bounce::estimate_bytes(BounceFormat::ProRes4444, (w, h), 120).unwrap();
        eprintln!(
            "MEASURE {w}x{h}: 120 frames bounced in {secs:.1} s ({:.1} fps, a minute in {:.0} s); {:.0} MB against an estimate of {:.0} MB ({:.2} GB a minute)",
            120.0 / secs, secs * 12.0, bytes as f64 / 1e6, estimate as f64 / 1e6, bytes as f64 * 12.0 / 1e9,
        );
    }
}

/// A test that fails while its bounce runs leaves no thread behind: the
/// job is cancelled and joined as the failing test unwinds, before the
/// binary can exit under it.
#[test]
fn a_bounce_let_go_by_a_failing_test_has_ended() {
    use klipp_jobs::BackgroundJob;
    let Some((pic, tone)) = sources() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let t = timeline(&pic, &tone, None);
    let flat = klipp_timeline::flatten::flatten_sequence(&t.seq, &[]);
    let path = klipp_testing::scratch::scratch_dir("klipp-bounce-failing").join("failing.mov");
    let req = BounceRequest { seq: Arc::new(flat), track: 0, first: 0, end: 30, size: (W, H), working: WorkingColorspace::Rec709Linear, format: BounceFormat::ProRes4444, path, sound: None };
    let (tx, _rx) = std::sync::mpsc::channel();
    let job = spawn(req, Arc::new(t.files.clone()), tx);
    let thread = Arc::clone(&job.0);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _job = job;
        panic!("a test failing while its bounce runs");
    }));
    assert!(failed.is_err());
    assert!(thread.is_done(), "the bounce's thread ended as the failing test unwound");
}

/// Cancelled, a bounce leaves no file, partial or whole, says so, and its
/// thread ends with everything it opened let go.
#[test]
fn a_cancelled_bounce_leaves_no_file_and_its_thread_ends() {
    use klipp_jobs::BackgroundJob;
    let Some((pic, tone)) = sources() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let t = timeline(&pic, &tone, None);
    let flat = klipp_timeline::flatten::flatten_sequence(&t.seq, &[]);
    let dir = klipp_testing::scratch::scratch_dir("klipp-bounce-cancel");
    let path = dir.join("cancelled.mov");
    let req = BounceRequest { seq: Arc::new(flat), track: 0, first: 0, end: 30, size: (W, H), working: WorkingColorspace::Rec709Linear, format: BounceFormat::ProRes4444, path: path.clone(), sound: None };
    let files = Arc::new(t.files.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let job = spawn(req, files, tx);
    job.cancel();
    let r = rx.recv_timeout(Duration::from_secs(60)).expect("it ends");
    job.join();
    assert!(r.cancelled && r.path.is_none() && r.error.is_none(), "{r:?}");
    assert!(!path.exists() && !klipp_render::partial_path_for(&path).exists(), "no file left");
    assert!(job.is_done(), "the thread ended");
}
