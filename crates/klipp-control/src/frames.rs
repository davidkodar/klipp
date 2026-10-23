//! Frames and scopes read for a request: the shown or a solo'd frame, its
//! colour faults, a picture decoded offline, frame statistics, PNGs (#826).

#[allow(unused_imports)]
use super::*;

/// Seek to `secs`, wait for the frame there, read its RGBA bytes, and
/// put the user's playhead back — the picture as the viewer shows it,
/// for measuring. Shares the one-grab lock and the wait with frame_at.
/// How long a frame read waits by default. A window that is on screen
/// renders a 4K frame in about a tenth of a second; one on a workspace
/// that is not being shown renders the same frame in one to three
/// seconds, and the old three-second default failed most reads there.
pub(crate) const FRAME_READ_TIMEOUT_MS: u64 = 10_000;

/// Whether a frame buffer holds a picture at all. A rendered frame is
/// opaque; a buffer that is all zero, alpha included, is one nothing
/// has been written to yet, and measuring it reported a black frame
/// that was never on the timeline.
pub(crate) fn has_picture(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|p| p[3] != 0)
}

pub(crate) fn grab_frame_rgba(ctx: &ControlContext, secs: f64, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    grab_frame_rgba_of(ctx, secs, None, timeout_ms)
}

/// [`grab_frame_rgba`], with `only` the frame is that clip alone —
/// graded and placed, with nothing over or under it (#719): what a colour
/// tool measures a clip by, so a title, a matte or a colour card over it
/// is not taken for its footage. The viewer is back to the whole picture
/// before this returns, whatever happened.
pub(crate) fn grab_frame_rgba_of(ctx: &ControlContext, secs: f64, only: Option<klipp_core::ClipId>, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    static ONE_GRAB: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE_GRAB.lock().unwrap_or_else(|e| e.into_inner());
    let held = hold_transport(ctx)?;
    let fps = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?.fps;
    let target = klipp_core::Timecode::from_secs_f64(secs.max(0.0), fps.rate());
    let before = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?.latest_generation();
    let mut seek = vec![AppCommand::Pause];
    if only.is_some() {
        seek.push(AppCommand::SetMeasureOnlyClip(only));
    }
    seek.push(AppCommand::SeekTo(target));
    send_envelope(ctx, seek, false)?;
    // Whole again before the transport is put back, so the frame the
    // viewer is left on is the composite — on every way out, an early
    // return among them, as the guard clears it when dropped.
    let mut solo = SoloRead { ctx, active: only.is_some(), held: Some(held), read_at: target };
    let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(FRAME_READ_TIMEOUT_MS));
    let deadline = std::time::Instant::now() + wait;
    loop {
        let (g, at, complete) = {
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            (fb.latest_generation(), fb.latest_presented(), fb.latest_complete())
        };
        if g != before && at.is_some_and(|a| a.cmp(&target) == std::cmp::Ordering::Equal) && complete {
            break;
        }
        if std::time::Instant::now() >= deadline {
            solo.end();
            return Err(format!("no frame at {secs:.3} s within {} ms; give a longer timeout_ms", wait.as_millis()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // The bytes: the stack presents zero-copy and the frame buffer's copy
    // can be a beat behind the texture, so a short wait for them.
    let (data, width, height) = loop {
        let (data, width, height) = {
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            let (d, w, h, _) = fb.latest_frame();
            (d.to_vec(), w, h)
        };
        let expected = (width as usize) * (height as usize) * 4;
        if width > 0 && height > 0 && data.len() >= expected && has_picture(&data[..expected]) {
            break (data, width, height);
        }
        if std::time::Instant::now() >= deadline {
            solo.end();
            return Err(format!(
                "the viewer has no picture of {secs:.3} s to measure yet (its copy of the frame is empty); \
                 a window that is not on screen renders slowly, so give a longer timeout_ms"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    solo.end();
    let expected = (width as usize) * (height as usize) * 4;
    Ok((data[..expected].to_vec(), width, height))
}

/// A frame read in progress: ending it, or dropping it on an early
/// return, puts the viewer back as it was — the engine rendering the
/// whole picture again after a clip-alone read (#719), then the playhead
/// where it was and playback running if it ran; with no playhead to go
/// back to, the frame it read redrawn whole (#749, #753 reviews).
pub(crate) struct SoloRead<'a> {
    pub(crate) ctx: &'a ControlContext,
    pub(crate) active: bool,
    pub(crate) held: Option<HeldTransport>,
    pub(crate) read_at: klipp_core::Timecode,
}

impl SoloRead<'_> {
    pub(crate) fn end(&mut self) {
        if std::mem::take(&mut self.active) {
            let _ = send_envelope(self.ctx, vec![AppCommand::SetMeasureOnlyClip(None)], false);
        }
        if let Some(held) = self.held.take() {
            let repaint = held.at.is_none();
            restore_transport(self.ctx, held);
            if repaint {
                let _ = send_envelope(self.ctx, vec![AppCommand::SeekTo(self.read_at)], false);
            }
        }
    }
}

impl Drop for SoloRead<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

/// A GPU-drawn scope image (`mode`: 0 waveform, 1 parade, 2 vectorscope,
/// 3 histogram) of the frame at `secs`, as RGBA plus its size. Grabs the
/// frame, has the engine draw the scope into its snapshot store — apart
/// from the viewer's live scopes — and reads it back.
pub(crate) fn grab_scope(ctx: &ControlContext, mode: u8, secs: f64, timeout_ms: Option<u64>) -> Result<(Vec<u8>, u32, u32), String> {
    let (rgba, w, h) = grab_frame_rgba(ctx, secs, timeout_ms)?;
    let before = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?
        .snapshot_scope_textures.get(&mode).map(|e| e.generation);
    send_envelope(ctx, vec![AppCommand::RenderScopeSnapshot { modes: vec![mode], rgba, width: w, height: h }], false)?;
    let wait = std::time::Duration::from_millis(timeout_ms.unwrap_or(FRAME_READ_TIMEOUT_MS));
    let deadline = std::time::Instant::now() + wait;
    loop {
        {
            let fb = ctx.frame_buffer.lock().map_err(|_| "frame buffer lock poisoned")?;
            if let Some(e) = fb.snapshot_scope_textures.get(&mode) {
                if !e.data.is_empty() && Some(e.generation) != before {
                    return Ok((e.data.clone(), e.w, e.h));
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("the scope was not drawn within {} ms; give a longer timeout_ms, or the frame had nothing to measure", wait.as_millis()));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The scope readings of a display-referred sRGB RGBA frame: what a
/// colourist reads before touching a wheel. Luma is Rec.709 on
/// gamma-encoded values, in IRE (0 black, 100 white), which is how
/// waveforms are read. Everything is measured on a grid of samples, not
/// every pixel — enough to be exact to a fraction and cheap on a 4K
/// frame.
/// The colour faults in a frame's [`frame_stats`] reading, each as
/// (secs, what, hint): what a colourist would flag before delivery.
pub fn color_faults(stats: &serde_json::Value, secs: f64) -> Vec<(f64, String, String)> {
    let mut out = Vec::new();
    let f = |v: &serde_json::Value, a: &str, b: &str| v.get(a).and_then(|x| x.get(b)).and_then(|x| x.as_f64());
    let g = |v: &serde_json::Value, a: &str| v.get(a).and_then(|x| x.as_f64());
    if let Some(hi) = f(stats, "clipping_pct", "high") {
        if hi > 2.0 {
            out.push((secs, format!("{hi:.0}% of the frame is clipped white"), "Bring the highlights down (grade highlights or gain negative) so detail comes back".into()));
        }
    }
    if let Some(lo) = f(stats, "clipping_pct", "low") {
        if lo > 2.0 {
            out.push((secs, format!("{lo:.0}% of the frame is crushed to black"), "Lift the shadows (grade shadows or lift positive) so detail comes back".into()));
        }
    }
    if let Some(white) = f(stats, "luma_ire", "white") {
        if white < 70.0 {
            out.push((secs, format!("the brightest part reaches only {white:.0} IRE — the picture is dull"), "Raise the white point (grade gain or contrast) toward 100".into()));
        }
    }
    if let Some(sat) = g(stats, "saturation_pct") {
        if sat > 60.0 {
            out.push((secs, format!("saturation is high at {sat:.0}% — colours may be over-cooked"), "Bring saturation down, or use vibrance which protects skin".into()));
        }
    }
    // Skin off the line is a white-balance fault only where there is
    // skin: a reading of skin-coloured pixels with no face behind it is
    // stone, sand, wood or leaves as often as people (#1897).
    // Without a face the reading is said for what it is, colour only, and
    // only when much of the frame is skin-coloured: a detector misses
    // small, turned and back-lit faces, and a cast on them must still
    // show (#1897 review).
    if let Some(pct) = g(stats, "skin_on_line_pct").filter(|p| *p < 60.0) {
        if stats.get("skin_from").and_then(|s| s.as_str()) == Some("faces") {
            out.push((secs, format!("only {pct:.0}% of skin tones sit on the skin-tone line"), "Nudge the white balance (temperature, tint) until faces land on the line".into()));
        } else if g(stats, "skin_coloured_pct").is_some_and(|s| s >= SKIN_COLOURED_NOTABLE_PCT) {
            out.push((secs, format!("colour only, no face found: {pct:.0}% of the skin-coloured pixels sit on the skin-tone line"), "Check any skin in the shot by eye; stone, sand, wood and leaves are not skin and need nothing".into()));
        }
    }
    out
}

/// When nothing at `tc` can be drawn because every picture there is a
/// file that is missing: the first of them, said as "<clip> is offline:
/// <path> is not there" (#657). One missing layer under another that is
/// there still gives a frame — the engine draws what opens — and is not
/// a reason to refuse one.
pub(crate) fn offline_picture_at(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, tc: klipp_core::Timecode) -> Option<String> {
    let clips: Vec<&klipp_timeline::Clip> = seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .filter_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c))
        .filter(|c| c.enabled && !matches!(c.kind, klipp_timeline::ClipKind::Gap))
        .collect();
    // A clip's file that is missing, with no ready proxy to stand in.
    let missing = |c: &klipp_timeline::Clip| -> Option<String> {
        let item = pool.get(&c.media_id()?)?;
        let proxy = item.proxy_ready && item.proxy_path.as_ref().is_some_and(|p| p.exists());
        ((item.offline || !item.path.exists()) && !proxy).then(|| format!("{} is offline: {} is not there", item.filename, item.path.display()))
    };
    // Anything there that draws — a file that is there, a title, a
    // generator — and the frame is not refused.
    if clips.is_empty() || clips.iter().any(|c| missing(c).is_none()) {
        return None;
    }
    clips.iter().find_map(|c| missing(c))
}

/// The picture files under `tc` that ffmpeg cannot decode a frame of,
/// when none under it can: why a frame never came, said with the file
/// and the error rather than as where the viewer stopped (#2474).
pub(crate) fn undecodable_picture_at(seq: &klipp_timeline::Sequence, pool: &klipp_media::MediaPool, tc: klipp_core::Timecode) -> Option<String> {
    let fps = seq.fps.as_f64();
    let mut failed = Vec::new();
    for clip in seq
        .tracks
        .iter()
        .filter(|t| t.kind == klipp_timeline::TrackKind::Video && t.visible)
        .filter_map(|t| t.clip_at_timecode(tc).map(|(_, c)| c))
        .filter(|c| c.enabled && !matches!(c.kind, klipp_timeline::ClipKind::Gap))
    {
        // A title or generator draws without a file: the frame is not
        // held up by decoding.
        let item = clip.media_id().and_then(|id| pool.get(&id)).filter(|m| !m.is_synthetic())?;
        let secs = clip.source_time_for_sequence_time(tc, fps).map(|t| t.to_secs_f64()).unwrap_or(0.0);
        let secs = if item.is_still { 0.0 } else { secs };
        match decode_frame_rgba_sized(&item.path, secs, 16, 16) {
            Ok(_) => return None,
            Err(e) => failed.push(format!("{} could not be decoded ({e}): {}", item.filename, item.path.display())),
        }
    }
    (!failed.is_empty()).then(|| failed.join("; "))
}

/// Decode one frame of a media file at `secs` to 320x180 RGBA through
/// ffmpeg, for measuring a reference clip's look off the file (not the
/// timeline). Scaled small: the scope stats read colour and luma, which a
/// downscale preserves.
pub(crate) fn decode_frame_rgba(path: &std::path::Path, secs: f64) -> Result<(Vec<u8>, u32, u32), String> {
    decode_frame_rgba_sized(path, secs, 320, 180)
}

/// [`decode_frame_rgba`] at `w`x`h`: a file's own frame read at the size
/// it is shown, so a reading of it compares with the timeline's.
pub(crate) fn decode_frame_rgba_sized(path: &std::path::Path, secs: f64, w: u32, h: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-nostats", "-ss", &format!("{secs}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let need = (w * h * 4) as usize;
    if out.stdout.len() < need {
        return Err(format!("no frame decoded at {secs:.2} s"));
    }
    Ok((out.stdout[..need].to_vec(), w, h))
}

/// Public for tests and tools that measure a frame without the socket.
pub fn frame_stats(data: &[u8], width: u32, height: u32, secs: f64) -> serde_json::Value {
    // The picture, not its letterbox or pillarbox bars (#647): counted,
    // a portrait clip in a wide sequence read "mid 0, 76.9% clipped at
    // black", and match_grade lifted it to match those numbers.
    let area = klipp_color::measure::picture_area(data, width, height);
    if area != (0, 0, width, height) {
        let mut v = frame_stats(&klipp_color::measure::crop_rgba(data, width, area), area.2, area.3, secs);
        let note = format!(
            "\nmeasured inside the {} bars: the picture is {}x{} of the {width}x{height} frame",
            if area.2 < width { "pillarbox" } else { "letterbox" },
            area.2,
            area.3
        );
        if let Some(t) = v["text"].as_str().map(|t| format!("{t}{note}")) {
            v["text"] = json!(t);
        }
        v["picture"] = json!({ "x": area.0, "y": area.1, "width": area.2, "height": area.3 });
        return v;
    }
    let (w, h) = (width as usize, height as usize);
    let step = ((w * h) / 200_000).max(1);
    let mut lumas: Vec<f32> = Vec::new();
    let (mut r_sum, mut g_sum, mut b_sum) = (0f64, 0f64, 0f64);
    let (mut sat_sum, mut n) = (0f64, 0usize);
    let (mut clip_hi, mut clip_lo) = (0usize, 0usize);
    let mut i = 0;
    while i < w * h {
        let p = i * 4;
        let (r, g, b) = (data[p] as f32, data[p + 1] as f32, data[p + 2] as f32);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        lumas.push(luma / 255.0 * 100.0);
        r_sum += r as f64;
        g_sum += g as f64;
        b_sum += b as f64;
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
        sat_sum += sat as f64;
        // At white and black: the luma on the rail, as the measure reads
        // it, an area and not a grain's lone pixel (#459, #518).
        match klipp_color::measure::at_rail(data, w, h, i % w, i / w) {
            Some(klipp_color::measure::Rail::White) => clip_hi += 1,
            Some(klipp_color::measure::Rail::Black) => clip_lo += 1,
            None => {}
        }
        n += 1;
        i += step;
    }
    lumas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pct = |q: f32| lumas.get(((lumas.len() as f32 * q) as usize).min(lumas.len().saturating_sub(1))).copied().unwrap_or(0.0);
    let nn = n.max(1) as f64;
    let (rb, gb, bb) = (r_sum / nn / 255.0, g_sum / nn / 255.0, b_sum / nn / 255.0);
    // The cast, if any: which channel leads on the frame's neutral
    // surfaces and by how much, in percent of their grey level — what
    // "warm" or "cool" means as a number. Read off the neutrals, as the
    // Lab reading and Auto WB are: the whole frame's average is the
    // sky and the brick, and on one frame it called a blue cast while
    // the neutrals leaned warm (#327). `rb`/`gb`/`bb` stay the frame's
    // average colour, which the scene matchers compare.
    let balance_note = match klipp_color::measure::neutral_reference(data, width, height) {
        None => "no neutral surface to read a cast from".to_string(),
        Some(lin) => {
            let [nr, ng, nb] = lin.map(|v| klipp_color::transfer::linear_to_srgb(v) as f64);
            let grey = (nr + ng + nb) / 3.0;
            let cast = |name: &'static str, v: f64| (name, ((v - grey) / grey.max(1e-6) * 100.0) as f32);
            let mut casts = [cast("red", nr), cast("green", ng), cast("blue", nb)];
            casts.sort_by(|a, b| b.1.total_cmp(&a.1));
            if casts[0].1 - casts[2].1 < 4.0 {
                "neutral".to_string()
            } else {
                format!("{} cast, {:.0}% over {}", casts[0].0, casts[0].1 - casts[2].1, casts[2].0)
            }
        }
    };
    let clip_hi_pct = clip_hi as f64 / nn * 100.0;
    let clip_lo_pct = clip_lo as f64 / nn * 100.0;
    // Colour alone, as nothing here looks for a face: rock, sand, wood
    // and leaves pass the same test as skin, so the share is said as a
    // share of skin-coloured pixels, never as a reading of skin. The
    // frame_stats request says when a face backs it (#1897).
    let skin = skin_line_share(data, width, height, &[[0.0, 0.0, 1.0, 1.0]]);
    let skin_on_line_pct = skin.map(|s| s.0);
    let skin_note = match skin_on_line_pct {
        None => "no clear skin tones".to_string(),
        Some(pct) => skin_colour_note(pct, 0),
    };
    let text = format!(
        "Frame at {secs:.2} s, scope readings (luma in IRE, 0 black to 100 white):\n\
         black point {:.0}, mid {:.0}, white point {:.0}\n\
         RGB balance on the neutral surfaces: {balance_note}\n\
         average saturation {:.0}% of full\n\
         clipping: {clip_hi_pct:.1}% at white, {clip_lo_pct:.1}% at black\n\
         skin: {skin_note}",
        pct(0.02), pct(0.5), pct(0.98), sat_sum / nn * 100.0,
    );
    json!({
        "text": text,
        "secs": secs,
        "luma_ire": { "black": pct(0.02), "mid": pct(0.5), "white": pct(0.98) },
        "rgb_balance": { "red": rb, "green": gb, "blue": bb, "note": balance_note },
        "saturation_pct": sat_sum / nn * 100.0,
        "clipping_pct": { "high": clip_hi_pct, "low": clip_lo_pct },
        "skin": skin_note,
        "skin_on_line_pct": skin_on_line_pct,
        "skin_coloured_pct": skin.map(|s| s.1),
        "skin_from": "colour",
    })
}

/// Inside `regions` (fractions of the frame), the skin-coloured pixels —
/// bright and warm enough to be skin — as (the share of them on the
/// skin-tone line, the share of the regions they are), both per cent;
/// `None` when there are too few to say. The line is the vectorscope's
/// I-line (~123°), where colourists land skin; hue from the usual HSV
/// formula. Read over the faces, it is a reading of skin; over the whole
/// frame, of colour only (#1897).
pub fn skin_line_share(data: &[u8], width: u32, height: u32, regions: &[[f32; 4]]) -> Option<(f64, f64)> {
    let (w, h) = (width as usize, height as usize);
    let step = ((w * h) / 200_000).max(1);
    let inside = |x: usize, y: usize| {
        let (fx, fy) = (x as f32 / w.max(1) as f32, y as f32 / h.max(1) as f32);
        regions.iter().any(|r| fx >= r[0] && fx < r[0] + r[2] && fy >= r[1] && fy < r[1] + r[3])
    };
    let (mut on, mut total, mut n) = (0usize, 0usize, 0usize);
    let mut i = 0;
    while i < w * h && data.len() >= (i + 1) * 4 {
        if inside(i % w, i / w) {
            let p = i * 4;
            let (r, g, b) = (data[p] as f32, data[p + 1] as f32, data[p + 2] as f32);
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
            let sat = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
            if sat > 0.15 && luma > 60.0 && r > g && g >= b {
                let d = mx - mn;
                let hp = if mx == r { ((g - b) / d).rem_euclid(6.0) } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
                total += 1;
                if (20.0..=50.0).contains(&(hp * 60.0)) {
                    on += 1;
                }
            }
            n += 1;
        }
        i += step;
    }
    (total >= (n / 200).max(20)).then(|| (on as f64 / total as f64 * 100.0, total as f64 / n.max(1) as f64 * 100.0))
}

/// How much of the frame skin-coloured pixels off the line must be before
/// a colour-only reading is said as worth a look: a tenth.
const SKIN_COLOURED_NOTABLE_PCT: f64 = 10.0;

/// The skin line of the scope readings for a share `pct` of skin-coloured
/// pixels on the skin-tone line, with `faces` faces read in the frame:
/// with none it is a colour reading and says so (#1897).
pub(crate) fn skin_colour_note(pct: f64, faces: usize) -> String {
    match faces {
        0 => format!("{pct:.0}% of skin-coloured pixels sit on the skin-tone line (colour only, no face detected: stone, sand, wood or leaves count too)"),
        n => format!("{pct:.0}% of skin-coloured pixels sit on the skin-tone line, with {n} face{} read below", if n == 1 { "" } else { "s" }),
    }
}

/// Nearest-neighbour downscale. Good enough for eyeballing a frame and
/// dependency-free; anything doing measurement should take the
/// full-resolution PNG instead.
pub(crate) fn downscale_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    target_width: u32,
) -> (Vec<u8>, u32, u32) {
    let tw = target_width.max(1);
    let th = ((height as u64 * tw as u64) / width.max(1) as u64).max(1) as u32;
    let mut out = Vec::with_capacity((tw as usize) * (th as usize) * 4);
    for y in 0..th {
        let sy = ((y as u64 * height as u64) / th as u64).min(height as u64 - 1) as usize;
        for x in 0..tw {
            let sx = ((x as u64 * width as u64) / tw as u64).min(width as u64 - 1) as usize;
            let o = (sy * width as usize + sx) * 4;
            out.extend_from_slice(&rgba[o..o + 4]);
        }
    }
    (out, tw, th)
}

pub(crate) fn write_png(
    path: &std::path::Path,
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // A frame someone is waiting to look at: the default compression
    // spent several hundred milliseconds on a 1080p frame to save a
    // file that is read once and thrown away.
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(|e| format!("png header: {e}"))?;
    writer.write_image_data(rgba).map_err(|e| format!("png data: {e}"))?;
    Ok(())
}
