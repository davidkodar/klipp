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

use super::streams::HeldFrame;
use crate::video_decoder::VideoDecoder;

/// A window being decoded: the frames from `from` to before `to`, after
/// the seek `epoch` that started it.
#[derive(Debug, Clone, Copy)]
struct Fill {
    from: f64,
    to: f64,
    epoch: u64,
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
        Self { decoder, frame_secs: 1.0 / 30.0, frames: Vec::new(), filling: None, capacity: capacity.max(8), start: start.max(0.0), last_given: None, measured: None, target: None, ahead: false }
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

    /// Whether a frame for `secs` is held: one within half a frame, or
    /// `secs` lies between two held frames.
    fn covers(&self, secs: f64) -> bool {
        let (Some(first), Some(last)) = (self.frames.first(), self.frames.last()) else { return false };
        let (a, b) = (first.pts.unwrap_or(0.0), last.pts.unwrap_or(0.0));
        secs >= a - self.half() && secs <= b + self.half()
    }

    fn fill(&mut self, from: f64, to: f64) {
        let from = from.max(self.start);
        if to <= from {
            return;
        }
        let epoch = self.decoder.seek(from);
        self.decoder.resume();
        self.filling = Some(Fill { from, to, epoch });
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
            // A jump: what is held is no use there.
            self.frames.clear();
            self.fill(secs - self.window(), secs + self.half() + 1e-6);
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
                // Decoded from the keyframe before the window: only the
                // window's frames are kept.
                if pts >= fill.from - self.half() {
                    taken.push(HeldFrame::from_frame(f));
                }
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
        for f in taken {
            let pts = f.pts.unwrap_or(0.0);
            match self.frames.binary_search_by(|h| h.pts.unwrap_or(0.0).total_cmp(&pts)) {
                Ok(i) => self.frames[i] = f,
                Err(i) => self.frames.insert(i, f),
            }
        }
        // Over the limit: the latest go first, already shown.
        while self.frames.len() > self.capacity {
            self.frames.pop();
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
