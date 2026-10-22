//! On-screen text through PP-OCR (PaddleOCR's mobile models) in ONNX:
//! a DBNet detector that marks where text is, and a CRNN recogniser
//! with CTC decoding that reads each line. Both are small (a few MB),
//! Apache-2.0, and read clean screen text well. The models come from
//! the registry like every other; nothing is bundled.
//!
//! Only the session plumbing needs ONNX Runtime; the box extraction
//! and the CTC decode are plain code, tested without it.

/// The recogniser's alphabet: PaddleOCR's `en_dict.txt`, index 1
/// onward (0 is CTC's blank). A class past the end reads as a space,
/// which is where PaddleOCR puts its `use_space_char` extra.
pub const EN_DICT: &str = "0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~!\"#$%&'()*+,-./ ";

/// The detector's longest side. PaddleOCR's default is 960; screen
/// type is small, so the map is finer.
pub const DET_MAX_SIDE: u32 = 1600;
/// The recogniser's input height (PP-OCRv4).
pub const REC_HEIGHT: u32 = 48;
/// The widest crop the recogniser is handed, in pixels.
pub const REC_MAX_WIDTH: u32 = 1280;
/// Lines read in one run of the recogniser.
pub const REC_BATCH: usize = 16;
/// Threads a session gets. Eight read a line fastest on a many-core
/// machine; the runtime's default of one per core is slower and
/// leaves nothing for the analyser running alongside.
pub const SESSION_THREADS: usize = 8;
/// A strip whose pixels differ from the previous frame's at the same
/// place by less than this, on average, is the same line: its read is
/// reused. Screen chrome is most of what a screen recording shows.
pub const SAME_STRIP_MEAN_DIFF: f32 = 2.0;

/// Mean absolute difference between two RGBA8 strips of one size.
pub fn strip_mean_diff(a: &[u8], b: &[u8]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return f32::MAX;
    }
    let sum: u64 = a.iter().zip(b).map(|(x, y)| (*x as i16 - *y as i16).unsigned_abs() as u64).sum();
    sum as f32 / a.len() as f32
}

/// Where a box sits, coarsely: the key under which its last read is kept.
pub fn box_key(b: PxBox) -> (u32, u32, u32, u32) {
    (b.x / 8, b.y / 8, b.w / 8, b.h / 8)
}
/// Probability-map threshold for "this pixel is text".
pub const DET_THRESH: f32 = 0.3;
/// A box whose mean probability is under this is noise.
pub const DET_BOX_THRESH: f32 = 0.5;
/// How far a text box grows past its dense core (DB's unclip ratio).
pub const DET_UNCLIP: f32 = 1.6;

/// One read line of text and where it is, as fractions of the frame.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    pub text: String,
    pub confidence: f32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// An axis-aligned box in pixels: left, top, width, height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PxBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Text boxes from a DB probability map of `w`×`h`: connected
/// components over the threshold, each grown by the unclip ratio,
/// scored by the mean probability inside, and merged along rows so a
/// line of words is read as one line.
pub fn boxes_from_map(prob: &[f32], w: usize, h: usize) -> Vec<PxBox> {
    let mut seen = vec![false; w * h];
    let mut boxes: Vec<(PxBox, f32)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if seen[start] || prob[start] < DET_THRESH {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
        let (mut sum, mut n) = (0.0f32, 0usize);
        stack.push(start);
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            sum += prob[i];
            n += 1;
            let mut push = |j: usize| {
                if !seen[j] && prob[j] >= DET_THRESH {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 { push(i - 1); }
            if x + 1 < w { push(i + 1); }
            if y > 0 { push(i - w); }
            if y + 1 < h { push(i + w); }
        }
        let (bw, bh) = (x1 + 1 - x0, y1 + 1 - y0);
        let score = sum / n.max(1) as f32;
        if bw < 3 || bh < 3 || score < DET_BOX_THRESH {
            continue;
        }
        // DB's unclip: grow by area * ratio / perimeter on every side.
        let area = (bw * bh) as f32;
        let perimeter = 2.0 * (bw + bh) as f32;
        let d = (area * DET_UNCLIP / perimeter).round() as usize;
        let gx0 = x0.saturating_sub(d);
        let gy0 = y0.saturating_sub(d);
        let gx1 = (x1 + d).min(w - 1);
        let gy1 = (y1 + d).min(h - 1);
        boxes.push((PxBox { x: gx0 as u32, y: gy0 as u32, w: (gx1 + 1 - gx0) as u32, h: (gy1 + 1 - gy0) as u32 }, score));
    }
    merge_rows(boxes.into_iter().map(|(b, _)| b).collect())
}

/// Boxes on one row, close together, become one: the recogniser reads
/// a whole line better than its words apart. Sorted top to bottom,
/// left to right.
pub fn merge_rows(mut boxes: Vec<PxBox>) -> Vec<PxBox> {
    boxes.sort_by_key(|b| (b.y + b.h / 2, b.x));
    let mut out: Vec<PxBox> = Vec::new();
    'next: for b in boxes {
        for o in out.iter_mut() {
            let overlap = (o.y + o.h).min(b.y + b.h) as i64 - o.y.max(b.y) as i64;
            let min_h = o.h.min(b.h).max(1) as i64;
            let same_row = overlap * 10 >= min_h * 6;
            let gap = if b.x > o.x + o.w { (b.x - (o.x + o.w)) as i64 } else if o.x > b.x + b.w { (o.x - (b.x + b.w)) as i64 } else { 0 };
            if same_row && gap <= (min_h as f32 * 1.2) as i64 {
                let x0 = o.x.min(b.x);
                let y0 = o.y.min(b.y);
                let x1 = (o.x + o.w).max(b.x + b.w);
                let y1 = (o.y + o.h).max(b.y + b.h);
                *o = PxBox { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
                continue 'next;
            }
        }
        out.push(b);
    }
    out.sort_by_key(|b| (b.y + b.h / 2, b.x));
    out
}

/// CTC greedy decode of a `(steps × classes)` probability grid: the
/// best class each step, repeats collapsed, blanks (class 0) dropped.
/// Confidence is the mean of the kept steps' best probabilities.
pub fn ctc_decode(probs: &[f32], steps: usize, classes: usize, dict: &str) -> (String, f32) {
    let chars: Vec<char> = dict.chars().collect();
    let mut text = String::new();
    let (mut conf_sum, mut conf_n) = (0.0f32, 0usize);
    let mut last = 0usize;
    for t in 0..steps {
        let row = &probs[t * classes..(t + 1) * classes];
        let (best, p) = row.iter().enumerate().fold((0usize, f32::MIN), |(bi, bp), (i, &v)| if v > bp { (i, v) } else { (bi, bp) });
        if best != 0 && best != last {
            let ch = chars.get(best - 1).copied().unwrap_or(' ');
            text.push(ch);
            conf_sum += p;
            conf_n += 1;
        }
        last = best;
    }
    (text.trim().to_string(), if conf_n == 0 { 0.0 } else { conf_sum / conf_n as f32 })
}

/// Bilinear resample of an RGBA8 image.
pub fn resize_rgba8(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return out;
    }
    let (sx, sy) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
    for y in 0..dh {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = (fy as u32).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let ty = fy - y0 as f32;
        for x in 0..dw {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = (fx as u32).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let tx = fx - x0 as f32;
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                let p = |xx: u32, yy: u32| src[((yy * sw + xx) * 4) as usize + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bot = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                out[o + c] = (top * (1.0 - ty) + bot * ty).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// A crop of an RGBA8 image, clipped to it.
pub fn crop_rgba8(src: &[u8], sw: u32, sh: u32, b: PxBox) -> (Vec<u8>, u32, u32) {
    let x0 = b.x.min(sw.saturating_sub(1));
    let y0 = b.y.min(sh.saturating_sub(1));
    let w = b.w.min(sw - x0).max(1);
    let h = b.h.min(sh - y0).max(1);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y0 + h {
        let row = ((y * sw + x0) * 4) as usize;
        out.extend_from_slice(&src[row..row + (w * 4) as usize]);
    }
    (out, w, h)
}

/// The detector's input size for a frame: the longest side at most
/// `DET_MAX_SIDE`, both sides multiples of 32.
pub fn det_input_size(w: u32, h: u32) -> (u32, u32) {
    let scale = (DET_MAX_SIDE as f32 / w.max(h).max(1) as f32).min(1.0);
    let fit = |v: u32| (((v as f32 * scale) / 32.0).round().max(1.0) as u32) * 32;
    (fit(w), fit(h))
}

#[cfg(feature = "ort")]
pub use infer::TextReader;

#[cfg(feature = "ort")]
mod infer {
    use super::*;

    /// Both PP-OCR sessions, opened once and read many times.
    pub struct TextReader {
        det: ort::session::Session,
        rec: ort::session::Session,
        /// The previous frame's strips and what they read, by where they
        /// were: a line that has not changed is not read again.
        previous: std::collections::HashMap<(u32, u32, u32, u32), (u32, Vec<u8>, String, f32)>,
        /// Time spent in the detector and the recogniser so far, for
        /// whoever wants to say where a pass went.
        pub det_secs: f64,
        pub rec_secs: f64,
        pub lines_read: usize,
        /// Lines whose read was reused from the frame before.
        pub lines_reused: usize,
    }

    fn session(weights: &std::path::Path) -> Result<ort::session::Session, String> {
        use ort::session::builder::GraphOptimizationLevel;
        crate::runtime_resolve::ensure_runtime_loadable()?;
        #[allow(unused_mut)]
        let mut builder = ort::session::Session::builder()
            .map_err(|e| format!("session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| format!("set optimisation: {e}"))?
            .with_intra_threads(SESSION_THREADS)
            .map_err(|e| format!("set threads: {e}"))?;
        // On the GPU where it loads, what it got recorded (#2250).
        let model = weights.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        builder = crate::provider::on_device(builder, klipp_ml::DevicePreference::GpuPreferCpu, &model)?;
        builder.commit_from_file(weights).map_err(|e| format!("load {}: {e}", weights.display()))
    }

    fn first_output(outputs: &ort::session::SessionOutputs<'_>) -> Result<(Vec<i64>, Vec<f32>), String> {
        let (_, v) = outputs.iter().next().ok_or("the model gave no output")?;
        let (shape, data) = v.try_extract_tensor::<f32>().map_err(|e| format!("read output: {e}"))?;
        Ok((shape.iter().copied().collect(), data.to_vec()))
    }

    impl TextReader {
        pub fn open(det: &std::path::Path, rec: &std::path::Path) -> Result<Self, String> {
            Ok(Self { det: session(det)?, rec: session(rec)?, previous: std::collections::HashMap::new(), det_secs: 0.0, rec_secs: 0.0, lines_read: 0, lines_reused: 0 })
        }

        /// Every line of text in an RGBA8 frame, top to bottom.
        pub fn read(&mut self, rgba: &[u8], w: u32, h: u32) -> Result<Vec<TextLine>, String> {
            if rgba.len() < (w * h * 4) as usize || w == 0 || h == 0 {
                return Err("frame is smaller than its size says".into());
            }
            // ── Detect ── PaddleOCR feeds the picture as OpenCV reads it,
            // blue first, normalised by ImageNet's mean and deviation.
            let started = std::time::Instant::now();
            let (dw, dh) = det_input_size(w, h);
            let small = resize_rgba8(rgba, w, h, dw, dh);
            let n = (dw * dh) as usize;
            let mean = [0.485f32, 0.456, 0.406];
            let std = [0.229f32, 0.224, 0.225];
            let mut chw = vec![0f32; 3 * n];
            for i in 0..n {
                for (c, src_c) in [(0usize, 2usize), (1, 1), (2, 0)] {
                    chw[c * n + i] = (small[i * 4 + src_c] as f32 / 255.0 - mean[c]) / std[c];
                }
            }
            let input = ort::value::Value::from_array(([1i64, 3, dh as i64, dw as i64], chw)).map_err(|e| format!("det input: {e}"))?;
            let outputs = self.det.run(ort::inputs![input]).map_err(|e| format!("det run: {e}"))?;
            let (shape, prob) = first_output(&outputs)?;
            let (mh, mw) = match shape.as_slice() {
                [_, _, mh, mw] | [_, mh, mw] => (*mh as usize, *mw as usize),
                other => return Err(format!("det output shape {other:?} is not a map")),
            };
            if prob.len() < mh * mw {
                return Err("det output is shorter than its shape".into());
            }
            let boxes = boxes_from_map(&prob[..mh * mw], mw, mh);
            drop(outputs);
            self.det_secs += started.elapsed().as_secs_f64();
            let started = std::time::Instant::now();
            // Map coordinates back onto the frame, and cut each line to a
            // strip 48 high with its width by aspect, values in -1..1.
            let (fx, fy) = (w as f32 / mw as f32, h as f32 / mh as f32);
            let mut strips: Vec<(PxBox, u32, Vec<u8>)> = Vec::new();
            for b in boxes {
                let px = PxBox { x: (b.x as f32 * fx) as u32, y: (b.y as f32 * fy) as u32, w: ((b.w as f32 * fx).ceil() as u32).max(1), h: ((b.h as f32 * fy).ceil() as u32).max(1) };
                let (crop, cw, ch) = crop_rgba8(rgba, w, h, px);
                let rw = ((cw as f32 * REC_HEIGHT as f32 / ch.max(1) as f32).round() as u32).clamp(8, REC_MAX_WIDTH);
                strips.push((px, rw, resize_rgba8(&crop, cw, ch, rw, REC_HEIGHT)));
            }
            // A strip that looks as it did in the previous frame at the
            // same place reads as it did then.
            let mut lines = Vec::new();
            let mut next: std::collections::HashMap<(u32, u32, u32, u32), (u32, Vec<u8>, String, f32)> = std::collections::HashMap::new();
            let mut unread: Vec<(PxBox, u32, Vec<u8>)> = Vec::new();
            for (px, rw, strip) in strips {
                let key = box_key(px);
                if let Some((prw, pstrip, text, conf)) = self.previous.get(&key) {
                    if *prw == rw && strip_mean_diff(pstrip, &strip) < SAME_STRIP_MEAN_DIFF {
                        self.lines_reused += 1;
                        if !text.is_empty() {
                            lines.push(TextLine { text: text.clone(), confidence: *conf, x: px.x as f32 / w as f32, y: px.y as f32 / h as f32, w: px.w as f32 / w as f32, h: px.h as f32 / h as f32 });
                        }
                        next.insert(key, (rw, strip, text.clone(), *conf));
                        continue;
                    }
                }
                unread.push((px, rw, strip));
            }
            // ── Recognise ── in batches of strips of a similar width, each
            // padded to the widest: one run reads a whole frame's worth of
            // lines, where a run per line cost the frame's whole budget.
            unread.sort_by_key(|(_, rw, _)| *rw);
            for batch in unread.chunks(REC_BATCH) {
                let bw = batch.iter().map(|(_, rw, _)| *rw).max().unwrap_or(8).div_ceil(8) * 8;
                let m = (bw * REC_HEIGHT) as usize;
                let mut chw = vec![0f32; batch.len() * 3 * m];
                for (bi, (_, rw, strip)) in batch.iter().enumerate() {
                    for y in 0..REC_HEIGHT as usize {
                        for x in 0..*rw as usize {
                            let s = (y * *rw as usize + x) * 4;
                            let d = y * bw as usize + x;
                            for (c, src_c) in [(0usize, 2usize), (1, 1), (2, 0)] {
                                chw[bi * 3 * m + c * m + d] = (strip[s + src_c] as f32 / 255.0 - 0.5) / 0.5;
                            }
                        }
                    }
                }
                let input = ort::value::Value::from_array(([batch.len() as i64, 3, REC_HEIGHT as i64, bw as i64], chw)).map_err(|e| format!("rec input: {e}"))?;
                let outputs = self.rec.run(ort::inputs![input]).map_err(|e| format!("rec run: {e}"))?;
                let (shape, probs) = first_output(&outputs)?;
                let (n, steps, classes) = match shape.as_slice() {
                    [n, t, c] => (*n as usize, *t as usize, *c as usize),
                    other => return Err(format!("rec output shape {other:?} is not batch by steps by classes")),
                };
                for (bi, (px, rw, strip)) in batch.iter().enumerate().take(n) {
                    let from = bi * steps * classes;
                    let to = (from + steps * classes).min(probs.len());
                    if to <= from {
                        break;
                    }
                    let (text, confidence) = ctc_decode(&probs[from..to], steps, classes, EN_DICT);
                    let kept = if text.is_empty() || confidence < 0.5 { (String::new(), 0.0) } else { (text.clone(), confidence) };
                    next.insert(box_key(*px), (*rw, strip.clone(), kept.0, kept.1));
                    if text.is_empty() || confidence < 0.5 {
                        continue;
                    }
                    lines.push(TextLine { text, confidence, x: px.x as f32 / w as f32, y: px.y as f32 / h as f32, w: px.w as f32 / w as f32, h: px.h as f32 / h as f32 });
                }
            }
            self.previous = next;
            lines.sort_by(|a, b| (a.y, a.x).partial_cmp(&(b.y, b.x)).unwrap_or(std::cmp::Ordering::Equal));
            self.rec_secs += started.elapsed().as_secs_f64();
            self.lines_read += lines.len();
            Ok(lines)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probability_map_yields_grown_row_merged_boxes() {
        // Two blobs on one row, close together, and one below on its own.
        let (w, h) = (40usize, 20usize);
        let mut map = vec![0.0f32; w * h];
        let paint = |m: &mut Vec<f32>, x0: usize, y0: usize, x1: usize, y1: usize| {
            for y in y0..y1 { for x in x0..x1 { m[y * w + x] = 0.9; } }
        };
        paint(&mut map, 2, 2, 10, 6);
        paint(&mut map, 12, 2, 20, 6);
        paint(&mut map, 2, 12, 12, 16);
        let boxes = boxes_from_map(&map, w, h);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        assert!(boxes[0].y < boxes[1].y);
        assert!(boxes[0].x <= 2 && boxes[0].x + boxes[0].w >= 20, "the row is one box: {:?}", boxes[0]);
        // Faint noise is not text.
        let mut faint = vec![0.0f32; w * h];
        for i in 0..40 { faint[i] = 0.35; }
        assert!(boxes_from_map(&faint, w, h).is_empty());
    }

    #[test]
    fn ctc_collapses_repeats_and_drops_blanks() {
        // classes: 0 blank, 1 '0', ...; 'A' is dict index 17, class 18.
        let classes = 100;
        let mut grid = vec![0.0f32; 6 * classes];
        let set = |g: &mut Vec<f32>, t: usize, c: usize, p: f32| g[t * classes + c] = p;
        set(&mut grid, 0, 18, 0.9); // A
        set(&mut grid, 1, 18, 0.8); // A again: collapsed
        set(&mut grid, 2, 0, 0.9);  // blank
        set(&mut grid, 3, 18, 0.7); // A after a blank: a new A
        set(&mut grid, 4, 2, 0.6);  // '1'
        set(&mut grid, 5, 99, 0.9); // past the dict: a space, trimmed
        let (text, conf) = ctc_decode(&grid, 6, classes, EN_DICT);
        assert_eq!(text, "AA1");
        assert!((conf - (0.9 + 0.7 + 0.6 + 0.9) / 4.0).abs() < 1e-6);
    }

    #[test]
    fn the_detector_input_is_bounded_and_a_multiple_of_32() {
        assert_eq!(det_input_size(3840, 2160), (1600, 896));
        assert_eq!(det_input_size(640, 480), (640, 480));
        assert_eq!(det_input_size(10, 10), (32, 32));
    }

    #[test]
    fn a_strip_that_barely_changed_is_the_same_line() {
        let a: Vec<u8> = (0..48 * 40 * 4).map(|i| (i % 251) as u8).collect();
        let mut b = a.clone();
        for v in b.iter_mut().step_by(7) { *v = v.saturating_add(3); }
        assert!(strip_mean_diff(&a, &b) < SAME_STRIP_MEAN_DIFF);
        let c: Vec<u8> = a.iter().map(|v| v.wrapping_add(40)).collect();
        assert!(strip_mean_diff(&a, &c) > SAME_STRIP_MEAN_DIFF);
        assert_eq!(strip_mean_diff(&a, &a[..8]), f32::MAX);
        assert_eq!(box_key(PxBox { x: 17, y: 9, w: 300, h: 40 }), (2, 1, 37, 5));
    }

    #[test]
    fn resize_and_crop_keep_the_picture() {
        let src: Vec<u8> = (0..4 * 4).flat_map(|i| [i as u8 * 16, 0, 0, 255]).collect();
        let big = resize_rgba8(&src, 4, 4, 8, 8);
        assert_eq!(big.len(), 8 * 8 * 4);
        assert!(big[0] <= big[7 * 4], "brightness rises along the row");
        let (crop, w, h) = crop_rgba8(&src, 4, 4, PxBox { x: 2, y: 2, w: 10, h: 10 });
        assert_eq!((w, h), (2, 2));
        assert_eq!(crop[0], 10 * 16);
    }
}
