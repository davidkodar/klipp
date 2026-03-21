//! Centralised time-pixel conversion for the timeline.
//! Every component (ruler, clips, playhead, skimmer, markers) must use
//! these functions. Never inline the math elsewhere.

/// Compute pixels-per-second from the normalised zoom value (0.0-1.0).
/// Uses exponential mapping so zoom feels linear to the user.
pub fn pixels_per_second(zoom: f32) -> f32 {
    let min_pps = 2.0_f32;
    let max_pps = 2000.0_f32;
    min_pps * (max_pps / min_pps).powf(zoom)
}

/// Map a legacy `timeline_zoom` (already in pps) to normalised 0.0–1.0.
pub fn zoom_from_pps(pps: f32) -> f32 {
    let min_pps = 2.0_f32;
    let max_pps = 2000.0_f32;
    ((pps / min_pps).ln() / (max_pps / min_pps).ln()).clamp(0.0, 1.0)
}

/// Timeline position in seconds → x pixel offset relative to content area origin.
pub fn time_to_x(time_secs: f64, pps: f32, scroll_offset_px: f32) -> f32 {
    (time_secs as f32 * pps) - scroll_offset_px
}

/// X pixel offset (relative to content area origin) → timeline position in seconds.
pub fn x_to_time(x_px: f32, pps: f32, scroll_offset_px: f32) -> f64 {
    ((x_px + scroll_offset_px) / pps) as f64
}
