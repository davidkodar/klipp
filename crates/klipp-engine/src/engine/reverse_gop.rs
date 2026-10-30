//! A reversed clip's frames from the forward decoder (#2798).
//!
//! A long-GOP file can only be decoded forward, from a keyframe. Read
//! backward a frame at a time, every frame shown cost a seek and a decode
//! from its keyframe, then a 4K picture made on the CPU and uploaded: 53
//! ms a frame, 12 fps reversed, the same cost at a tenth speed for a frame
//! that hadn't changed. Here the clip's own decoder, the one forward
//! playback uses (frames on the GPU when there is one), fills a window of
//! frames ending at the time wanted, decoding forward from the keyframe
//! before it and keeping only the window's frames. They are handed out
//! back to front, the same frame for as long as it is shown, while the
//! window before is decoded. The frames held are counted against the
//! memory they live in.
//!
//! A window is decoded from its keyframe and keeps every frame from
//! there (#3118): the window before it then ends at that keyframe, and
//! starts at the one before, so each group of pictures is decoded once.
//! Windows that dropped the frames before their own start decoded those
//! again for the window before: read back a frame at a time, a source
//! with a keyframe every 50 frames decoded 338 frames for its 150.

use super::streams::HeldFrame;
use crate::video_decoder::VideoDecoder;

/// A window being decoded: the frames from `from` to before `to`, after
/// the seek `epoch` that started it, given as far as `reached`, the
/// latest frame's time. `from` moves back to the keyframe the decode
/// started from once its frame has come.
#[derive(Debug, Clone, Copy)]
struct Fill {
    from: f64,
    to: f64,
    epoch: u64,
    reached: Option<f64>,
}

pub(crate) struct ReverseGop {
    decoder: VideoDecoder,
    /// The source's frame duration.
    frame_secs: f64,
    /// Frames held, earliest first.
    frames: Vec<HeldFrame>,
    filling: Option<Fill>,
    /// The most frames held at once; a window is half of it, so the one
    /// decoding ahead fits beside the one shown.
    capacity: usize,
    /// The bytes of a frame, while `capacity` is the guess made before
    /// the memory's free size was known: see [`Self::learn_free`].
    guessed_for: Option<f64>,
    /// Where the source starts: nothing is decoded before it.
    start: f64,
    /// The frame last handed out, by its time, and how many different
    /// frames have been handed out: a frame shown again is not new.
    last_given: Option<f64>,
    /// The frame duration read from the frames, once two have come.
    measured: Option<f64>,
    /// The time last asked for, and whether the clip is only coming up,
    /// not on screen yet: then nothing waits for it.
    target: Option<f64>,
    ahead: bool,
}

impl ReverseGop {
    /// Reading `decoder`'s source backward, holding at most `capacity`
    /// frames, nothing before `start`. Its frame duration is read from the
    /// frames it decodes; 30 fps until then.
    pub fn new(decoder: VideoDecoder, capacity: usize, start: f64) -> Self {
        decoder.pause();
        Self { decoder, frame_secs: 1.0 / 30.0, frames: Vec::new(), filling: None, capacity: capacity.max(8), guessed_for: None, start: start.max(0.0), last_given: None, measured: None, target: None, ahead: false }
    }

    /// Reading `decoder`'s source backward, frames of `bytes` each held in
    /// a memory with `free` bytes free ([`Self::capacity_for`]), nothing
    /// before `start`. With `free` not known yet, as on the first read of
    /// the GPU's, the capacity is a guess until [`Self::learn_free`] has it.
    pub fn for_memory(decoder: VideoDecoder, bytes: f64, free: Option<f64>, start: f64) -> Self {
        let mut gop = Self::new(decoder, Self::capacity_for(bytes, free), start);
        gop.guessed_for = free.is_none().then_some(bytes);
        gop
    }

    /// The memory's free bytes, read again: a capacity guessed before
    /// they were known is made from them, once. Not again after: the free
    /// memory read later is less by the frames this one holds.
    pub fn learn_free(&mut self, free: Option<f64>) {
        if let (Some(bytes), Some(free)) = (self.guessed_for, free) {
            self.capacity = Self::capacity_for(bytes, Some(free)).max(8);
            self.guessed_for = None;
        }
    }

    /// Frames held at once for a picture of `bytes` each, given `free`
    /// bytes of the memory they live in: a quarter of it, 8 to 120.
    pub fn capacity_for(bytes: f64, free: Option<f64>) -> usize {
        match free {
            Some(free) if bytes > 0.0 => ((free * 0.25 / bytes) as usize).clamp(8, 120),
            _ => 32,
        }
    }

    fn half(&self) -> f64 {
        self.frame_secs * 0.5
    }

    fn window(&self) -> f64 {
        (self.capacity / 2).max(4) as f64 * self.frame_secs
    }

    /// Frames after the one shown kept when the memory is short: a step
    /// forward again, and the frame a blend draws toward.
    const KEEP_AFTER_FRAMES: f64 = 4.0;

    /// Whether a frame for `secs` is held: one within half a frame, or
    /// `secs` lies between two held frames. Not in a window still being
    /// decoded, past the frames it has given: the window before the one
    /// shown fills from its start, and until it is done there is a gap
    /// between its frames and those after it, where the latest frame held
    /// is one from before the time, not the one at it. A jump there from
    /// later in the clip showed a frame half a second early, as complete.
    fn covers(&self, secs: f64) -> bool {
        let (Some(first), Some(last)) = (self.frames.first(), self.frames.last()) else { return false };
        let (a, b) = (first.pts.unwrap_or(0.0), last.pts.unwrap_or(0.0));
        if secs < a - self.half() || secs > b + self.half() {
            return false;
        }
        // Frames come in time order: once one at or past `secs` has come,
        // the frame for it is the one there or the one before.
        match self.filling {
            Some(fill) if secs >= fill.from - self.half() && secs < fill.to => fill.reached.is_some_and(|r| r >= secs - self.half()),
            _ => true,
        }
    }

    fn fill(&mut self, from: f64, to: f64) {
        let from = from.max(self.start);
        if to <= from {
            return;
        }
        // From the keyframe at or before `from`, every frame handed over.
        let epoch = self.decoder.seek_from_keyframe(from);
        self.decoder.resume();
        self.filling = Some(Fill { from, to, epoch, reached: None });
    }

    /// Asked for the frame at `secs` (the playhead's source time): the
    /// window ending there starts decoding when it isn't held or being
    /// decoded, and the window before it once `secs` is in the first half
    /// of what is held.
    pub fn want(&mut self, secs: f64, ahead: bool) {
        self.target = Some(secs);
        self.ahead = ahead;
        self.pump();
        let filling_here = self.filling.is_some_and(|f| secs >= f.from - self.half() && secs < f.to);
        if !self.covers(secs) && !filling_here {
            // A jump: what is held is no use there. Decoded from the
            // keyframe before `secs`, the least that gives its frame, and
            // all of it kept: the next window ends at that keyframe.
            self.frames.clear();
            self.fill(secs - self.half(), secs + self.half() + 1e-6);
            return;
        }
        // Played on (back): the frames after the shown one are done with,
        // but a few are kept for a step forward again.
        let keep_after = secs + self.window();
        self.frames.retain(|f| f.pts.unwrap_or(0.0) <= keep_after);
        if self.filling.is_none() {
            if let Some(first) = self.frames.first().and_then(|f| f.pts) {
                if first > self.start + self.half() && secs - first < self.window() * 0.5 {
                    self.fill(first - self.window(), first - self.half());
                }
            }
        }
    }

    /// Take the frames decoded so far into the window being filled.
    pub fn pump(&mut self) {
        let Some(fill) = self.filling else { return };
        let mut done = false;
        let mut taken = Vec::new();
        {
            let mut q = self.decoder.frame_queue().lock().unwrap_or_else(|e| e.into_inner());
            while let Some(f) = q.front() {
                if f.seek_epoch < fill.epoch {
                    q.pop();
                    continue;
                }
                if f.is_eof {
                    q.pop();
                    done = true;
                    break;
                }
                let pts = f.pts_secs();
                if pts >= fill.to {
                    done = true;
                    break;
                }
                let f = q.pop().expect("the front frame");
                // Decoded from the keyframe before the window: all kept.
                taken.push(HeldFrame::from_frame(f));
            }
        }
        // The source's frame duration, from frames decoded one after
        // another: the smallest step between them.
        for pair in taken.windows(2) {
            let step = pair[1].pts.unwrap_or(0.0) - pair[0].pts.unwrap_or(0.0);
            if step > 1e-4 && (self.measured.is_none() || step < self.frame_secs) {
                self.frame_secs = step;
                self.measured = Some(step);
            }
        }
        if let (Some(fill), Some(earliest), Some(latest)) = (self.filling.as_mut(), taken.first().and_then(|f| f.pts), taken.last().and_then(|f| f.pts)) {
            fill.reached = Some(fill.reached.map_or(latest, |r| r.max(latest)));
            fill.from = fill.from.min(earliest);
        }
        for f in taken {
            let pts = f.pts.unwrap_or(0.0);
            match self.frames.binary_search_by(|h| h.pts.unwrap_or(0.0).total_cmp(&pts)) {
                Ok(i) => self.frames[i] = f,
                Err(i) => self.frames.insert(i, f),
            }
        }
        // Over the limit: the latest go first, already shown, down to a
        // few after the one shown; then the earliest, the furthest from
        // being shown.
        let keep_to = self.target.map_or(f64::NEG_INFINITY, |t| t + Self::KEEP_AFTER_FRAMES * self.frame_secs);
        while self.frames.len() > self.capacity {
            if self.frames.last().is_some_and(|f| f.pts.unwrap_or(0.0) > keep_to) {
                self.frames.pop();
            } else {
                self.frames.remove(0);
            }
        }
        if done {
            self.decoder.pause();
            self.filling = None;
        }
    }

    /// The frame shown at `secs`: the latest held at or before it (within
    /// half a frame), the same frame for as long as it is shown.
    pub fn frame(&mut self, secs: f64) -> Option<HeldFrame> {
        self.pump();
        if !self.covers(secs) {
            return None;
        }
        let i = self.frames.partition_point(|f| f.pts.unwrap_or(0.0) <= secs + self.half());
        let f = self.frames.get(i.saturating_sub(1)).or(self.frames.first())?.clone();
        if self.last_given != f.pts {
            self.last_given = f.pts;
            super::REVERSED_FRAMES_GIVEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Some(f)
    }

    /// The held frame after the one [`Self::frame`] shows at `secs`: the
    /// one a frame blend draws toward (#2772). Played back, it was shown
    /// just before, and is kept for a step forward.
    pub fn frame_after(&mut self, secs: f64) -> Option<HeldFrame> {
        self.pump();
        let i = self.frames.partition_point(|f| f.pts.unwrap_or(0.0) <= secs + self.half());
        self.frames.get(i).cloned()
    }

    /// Whether the frame for the time last asked for is held, or nothing
    /// waits for it: the clip is only coming up.
    pub fn ready(&self) -> bool {
        self.ahead || self.target.is_none_or(|t| self.covers(t))
    }
}

impl Drop for ReverseGop {
    fn drop(&mut self) {
        self.decoder.stop_detached();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Six seconds at 25 fps, large enough that a window takes a moment to
    /// decode. `None` without ffmpeg.
    fn source() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-reverse-gop-720p.mkv");
        if out.exists() {
            return Some(out);
        }
        let partial = out.with_extension(format!("{}.mkv", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1280x720:r=25:d=6", "-c:v", "ffv1"])
            .arg(&partial)
            .status()
            .ok()?
            .success();
        (ok && std::fs::rename(&partial, &out).is_ok()).then_some(out)
    }

    /// Six seconds at 25 fps of long-GOP H.264, a keyframe every 50
    /// frames (0, 2 and 4 s). `None` without ffmpeg or its H.264 encoder.
    fn long_gop_source() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("klipp-reverse-gop-720p-gop50.mp4");
        if out.exists() {
            return Some(out);
        }
        let partial = out.with_extension(format!("{}.mp4", std::process::id()));
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=1280x720:r=25:d=6"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "50", "-keyint_min", "50", "-sc_threshold", "0", "-bf", "0", "-pix_fmt", "yuv420p"])
            .arg(&partial)
            .status()
            .ok()?
            .success();
        if !ok {
            let _ = std::fs::remove_file(&partial);
            return None;
        }
        std::fs::rename(&partial, &out).ok()?;
        Some(out)
    }

    /// Asked for `secs` until its frame is held.
    fn settle(gop: &mut ReverseGop, secs: f64) {
        let t0 = Instant::now();
        loop {
            gop.want(secs, false);
            if gop.ready() {
                return;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "no frame for {secs} s");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A jump back into the window being decoded before the one shown is
    /// waited for until its own frame is there: never the latest frame
    /// decoded so far, half a second or more before it, given as the
    /// frame there and the reader ready. With a wide window, as when the
    /// GPU's free memory has been read, the viewer showed a reversed clip's
    /// last frame from 0.5 s too early.
    #[test]
    fn a_time_in_a_window_still_decoding_waits_for_its_own_frame() {
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let lease = crate::decode_budget::admit(0.0, crate::decode_budget::Priority::Overflow);
        let decoder = VideoDecoder::open_with_queue(&path, 0, lease, 8).expect("the source opens");
        let mut gop = ReverseGop::new(decoder, 120, 0.0);
        settle(&mut gop, 4.16);
        // In the first half of what is held: the window before it, from
        // the source's start, starts decoding.
        settle(&mut gop, 2.56);
        assert!(gop.filling.is_some(), "the window before is decoding");
        let t0 = Instant::now();
        loop {
            gop.want(1.0, false);
            let ready = gop.ready();
            if let Some(f) = gop.frame(1.0) {
                let pts = f.pts.expect("a time");
                assert!((pts - 1.0).abs() <= gop.half() + 1e-6, "the frame given at 1.00 s is the one at {pts:.2} s (ready {ready})");
            } else {
                assert!(!ready, "ready with no frame at 1.00 s");
            }
            if ready && gop.filling.is_none() {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "the window before never finished");
            std::thread::sleep(Duration::from_micros(500));
        }
        drop(gop);
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }

    /// Read back from its last frame to its first a frame at a time, a
    /// long-GOP source is decoded about once (#3118): at most 160 frames
    /// decoded for its 150, each step given its own frame. Windows of 60
    /// frames (120 held) that kept only their own frames decoded from the
    /// keyframe before each one, the frames before it again for every
    /// window.
    #[test]
    fn read_backward_each_group_of_pictures_is_decoded_once() {
        let Some(path) = long_gop_source() else {
            eprintln!("skipping: no ffmpeg with libx264");
            return;
        };
        let lease = crate::decode_budget::admit(0.0, crate::decode_budget::Priority::Overflow);
        let decoder = VideoDecoder::open_with_queue(&path, 0, lease, 8).expect("the source opens");
        let mut gop = ReverseGop::new(decoder, 120, 0.0);
        for i in (0..150).rev() {
            let secs = i as f64 / 25.0;
            settle(&mut gop, secs);
            let pts = gop.frame(secs).and_then(|f| f.pts).expect("a frame");
            assert!((pts - secs).abs() < 0.02, "the frame given at {secs:.2} s is the one at {pts:.2} s");
        }
        let decoded = gop.decoder.frames_decoded();
        eprintln!("read backward: {decoded} frames decoded for 150");
        assert!(decoded <= 160, "{decoded} frames decoded to read 150 backward");
        drop(gop);
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }

    /// A capacity guessed before the GPU's free memory was read (#3118):
    /// 32 frames, held for good, kept a 4K source read backward under 29
    /// fps from a cold start. It is made from the free memory once that is
    /// read, and only then: a later, smaller reading leaves it.
    #[test]
    fn a_capacity_guessed_without_the_free_memory_is_made_once_it_is_read() {
        let Some(path) = source() else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let lease = crate::decode_budget::admit(0.0, crate::decode_budget::Priority::Overflow);
        let decoder = VideoDecoder::open_with_queue(&path, 0, lease, 8).expect("the source opens");
        let mut gop = ReverseGop::for_memory(decoder, 1000.0, None, 0.0);
        assert_eq!(gop.capacity, 32, "the guess");
        gop.learn_free(None);
        assert_eq!(gop.capacity, 32, "still not known");
        gop.learn_free(Some(400_000.0));
        assert_eq!(gop.capacity, 100, "a quarter of 400 kB in frames of 1 kB");
        gop.learn_free(Some(40_000.0));
        assert_eq!(gop.capacity, 100, "made once");
        let lease = crate::decode_budget::admit(0.0, crate::decode_budget::Priority::Overflow);
        let decoder = VideoDecoder::open_with_queue(&path, 0, lease, 8).expect("the source opens");
        let mut known = ReverseGop::for_memory(decoder, 1000.0, Some(200_000.0), 0.0);
        known.learn_free(Some(400_000.0));
        assert_eq!(known.capacity, 50, "known from the start: kept");
        drop((gop, known));
        crate::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }
}
