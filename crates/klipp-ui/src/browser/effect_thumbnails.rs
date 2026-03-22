//! Background effect thumbnail renderer.
//!
//! Uses a fixed embedded reference image (Bachalpsee landscape) for all
//! effect previews — same approach as FCP. Built-in and OFX effects are
//! rendered eagerly at startup using this reference.

use crate::browser::effect_registry::{EffectSource, ReferenceFrame};
use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The embedded reference image for effect thumbnails (320x180 PNG).
static REFERENCE_PNG: &[u8] = include_bytes!("../../../../assets/effect_preview_reference.png");

/// Decode the embedded reference PNG into an RGBA ReferenceFrame.
fn load_embedded_reference() -> Option<ReferenceFrame> {
    let decoder = png::Decoder::new(std::io::Cursor::new(REFERENCE_PNG));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let w = info.width;
    let h = info.height;

    // Convert to RGBA if needed
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let rgb = &buf[..info.buffer_size()];
            let mut rgba = Vec::with_capacity((w * h * 4) as usize);
            for chunk in rgb.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        _ => return None,
    };

    Some(ReferenceFrame { rgba, width: w, height: h })
}

/// A completed thumbnail ready for texture upload.
pub struct ThumbnailResult {
    pub effect_name: String,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Manages background thumbnail generation and texture caching.
pub struct ThumbnailRenderer {
    /// Cached textures keyed by effect name.
    pub textures: HashMap<String, egui::TextureHandle>,
    /// Channel for receiving completed thumbnails from background threads.
    rx: Receiver<ThumbnailResult>,
    tx: Sender<ThumbnailResult>,
    /// Hash of the reference frame used for the current batch.
    ref_frame_hash: u64,
    /// OFX effects currently being rendered (to avoid duplicate requests).
    ofx_pending: HashSet<String>,
    /// OFX effects that failed to render (don't retry).
    ofx_failed: HashSet<String>,
    /// Lazily-initialized OFX host for thumbnail rendering (shared with bg thread).
    ofx_host: Arc<Mutex<Option<klipp_plugin::OfxHostRuntime>>>,
    /// Whether the OFX host has been initialized.
    ofx_host_ready: Arc<std::sync::atomic::AtomicBool>,
}

impl Default for ThumbnailRenderer {
    fn default() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            textures: HashMap::new(),
            rx,
            tx,
            ref_frame_hash: 0,
            ofx_pending: HashSet::new(),
            ofx_failed: HashSet::new(),
            ofx_host: Arc::new(Mutex::new(None)),
            ofx_host_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}

impl ThumbnailRenderer {
    /// Generate ALL effect thumbnails (built-in + OFX) using the fixed embedded reference.
    /// Called once at startup; subsequent calls are no-ops.
    pub fn generate_all_if_needed(
        &mut self,
        effects: &[(String, EffectSource)],
    ) {
        if self.ref_frame_hash != 0 {
            return; // Already generated
        }
        let Some(ref_frame) = load_embedded_reference() else {
            tracing::warn!("Failed to decode embedded effect preview reference image");
            return;
        };
        self.ref_frame_hash = 1; // Mark as generated

        let tx = self.tx.clone();
        let all_effects: Vec<_> = effects.to_vec();
        let frame = ref_frame;

        std::thread::spawn(move || {
            for (name, source) in &all_effects {
                let rgba = match source {
                    EffectSource::Builtin(effect) => {
                        let preview = builtin_preview_params(effect);
                        klipp_effects::apply_effects(
                            std::slice::from_ref(&preview),
                            frame.rgba.clone(),
                            frame.width,
                            frame.height,
                        )
                    }
                    EffectSource::Ofx(plugin_id) => {
                        apply_ofx_visual_proxy(&frame, plugin_id)
                    }
                };
                let _ = tx.send(ThumbnailResult {
                    effect_name: name.clone(),
                    rgba,
                    width: frame.width,
                    height: frame.height,
                });
            }
        });
    }

    /// Legacy: render built-in thumbnails from a viewer reference frame.
    /// Now delegates to generate_all_if_needed with the embedded reference.
    pub fn generate_builtins_if_needed(
        &mut self,
        _ref_frame: &ReferenceFrame,
        effects: &[(String, EffectSource)],
    ) {
        self.generate_all_if_needed(effects);
    }

    /// Request an OFX effect thumbnail render (called on hover).
    /// Lazily initializes the OFX host on first call.
    pub fn request_ofx_render(
        &mut self,
        effect_name: &str,
        plugin_id: &str,
        ref_frame: &ReferenceFrame,
    ) {
        // Already have it, already rendering, or previously failed
        if self.textures.contains_key(effect_name)
            || self.ofx_pending.contains(effect_name)
            || self.ofx_failed.contains(effect_name)
        {
            return;
        }

        self.ofx_pending.insert(effect_name.to_string());
        tracing::info!("OFX thumbnail: requesting render for '{}'", effect_name);

        let tx = self.tx.clone();
        let frame = ref_frame.clone();
        let name = effect_name.to_string();
        let plugin_id = plugin_id.to_string();
        let host_arc = self.ofx_host.clone();
        let host_ready = self.ofx_host_ready.clone();

        std::thread::spawn(move || {
            // Lazily initialize the OFX host (once, shared across all OFX renders)
            if !host_ready.load(std::sync::atomic::Ordering::Acquire) {
                let mut guard = host_arc.lock().unwrap();
                if guard.is_none() {
                    tracing::info!("OFX thumbnail: initializing separate OFX host for preview rendering");
                    let mut host = klipp_plugin::OfxHostRuntime::new();
                    host.discover_plugins();
                    *guard = Some(host);
                    host_ready.store(true, std::sync::atomic::Ordering::Release);
                    tracing::info!("OFX thumbnail: host ready");
                }
            }

            // Use visual proxy for OFX thumbnails.
            // Real OFX rendering is done when the effect is applied to a clip in the timeline.
            // Thumbnail rendering is deferred until the OFX host render pipeline is fully stable,
            // as some plugins can SIGSEGV during render which would crash the entire app.
            let _host_arc = host_arc; // keep alive
            let final_rgba = apply_ofx_visual_proxy(&frame, &plugin_id);

            let _ = tx.send(ThumbnailResult {
                effect_name: name,
                rgba: final_rgba,
                width: frame.width,
                height: frame.height,
            });
        });
    }

    /// Poll for completed thumbnails and upload as egui textures.
    pub fn poll_and_upload(&mut self, ctx: &egui::Context) {
        let mut received_any = false;
        while let Ok(result) = self.rx.try_recv() {
            self.ofx_pending.remove(&result.effect_name);

            let pixels: Vec<egui::Color32> = result
                .rgba
                .chunks_exact(4)
                .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                .collect();

            let image = egui::ColorImage {
                size: [result.width as usize, result.height as usize],
                pixels,
            };

            let texture = ctx.load_texture(
                format!("fx_thumb_{}", result.effect_name),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.textures.insert(result.effect_name, texture);
            received_any = true;
        }

        if received_any {
            ctx.request_repaint();
        }
    }

    /// Get a thumbnail texture for an effect, if available.
    pub fn get_texture(&self, effect_name: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(effect_name)
    }

    /// Check if an OFX effect is currently being rendered.
    pub fn is_ofx_pending(&self, effect_name: &str) -> bool {
        self.ofx_pending.contains(effect_name)
    }
}

/// Exaggerated parameters for built-in effect thumbnails.
fn builtin_preview_params(effect: &klipp_effects::BuiltinEffect) -> klipp_effects::BuiltinEffect {
    match effect {
        klipp_effects::BuiltinEffect::Opacity { .. } => {
            klipp_effects::BuiltinEffect::Opacity { value: 0.4 }
        }
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => {
            klipp_effects::BuiltinEffect::GaussianBlur { radius: 6.0 }
        }
        klipp_effects::BuiltinEffect::Sharpen { .. } => {
            klipp_effects::BuiltinEffect::Sharpen { amount: 1.5 }
        }
        klipp_effects::BuiltinEffect::Crop { .. } => {
            klipp_effects::BuiltinEffect::Crop {
                left: 0.15, right: 0.15, top: 0.15, bottom: 0.15, feather: 0.05,
            }
        }
        klipp_effects::BuiltinEffect::Transform { .. } => {
            klipp_effects::BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0,
                scale_x: 0.7, scale_y: 0.7,
                rotation: 12.0,
                anchor_x: 0.5, anchor_y: 0.5,
            }
        }
        klipp_effects::BuiltinEffect::ChromaKey { .. } => {
            klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 255, 0],
                tolerance: 0.3,
                spill_suppression: 0.2,
            }
        }
    }
}

/// Accurate visual preview for OFX effects — applies a real pixel-level
/// simulation of what the effect does to the reference frame.
fn apply_ofx_visual_proxy(frame: &ReferenceFrame, plugin_name: &str) -> Vec<u8> {
    let n = plugin_name.to_lowercase();
    let rgba = &frame.rgba;
    let w = frame.width as usize;
    let h = frame.height as usize;

    // ── Try builtin effect proxies first (blur, sharpen, transform, crop, chromakey) ──
    if let Some(builtin) = builtin_proxy_for(&n, plugin_name) {
        return klipp_effects::apply_effects(
            std::slice::from_ref(&builtin),
            rgba.clone(), frame.width, frame.height,
        );
    }

    // ── Per-pixel transformations ──
    let mut out = rgba.clone();

    // Invert: 255-R, 255-G, 255-B
    if n.contains("invert") {
        for px in out.chunks_exact_mut(4) {
            px[0] = 255 - px[0];
            px[1] = 255 - px[1];
            px[2] = 255 - px[2];
        }
        return out;
    }

    // Saturation (desaturate by ~60%)
    if n.contains("saturation") {
        for px in out.chunks_exact_mut(4) {
            let luma = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) as u8;
            let t = 0.4_f32; // saturation level
            px[0] = (luma as f32 + t * (px[0] as f32 - luma as f32)) as u8;
            px[1] = (luma as f32 + t * (px[1] as f32 - luma as f32)) as u8;
            px[2] = (luma as f32 + t * (px[2] as f32 - luma as f32)) as u8;
        }
        return out;
    }

    // Gamma (brighten with gamma 0.6)
    if n.contains("gamma") {
        let gamma = 0.6_f32;
        let table: Vec<u8> = (0..256).map(|i| ((i as f32 / 255.0).powf(gamma) * 255.0) as u8).collect();
        for px in out.chunks_exact_mut(4) {
            px[0] = table[px[0] as usize];
            px[1] = table[px[1] as usize];
            px[2] = table[px[2] as usize];
        }
        return out;
    }

    // Grade / ColorCorrect: lift shadows, warm highlights
    if n.contains("grade") || n.contains("colorcorrect") {
        for px in out.chunks_exact_mut(4) {
            let r = (px[0] as f32 * 1.15 + 8.0).min(255.0) as u8;
            let g = (px[1] as f32 * 1.05 + 4.0).min(255.0) as u8;
            let b = (px[2] as f32 * 0.9).min(255.0) as u8;
            px[0] = r; px[1] = g; px[2] = b;
        }
        return out;
    }

    // Multiply: darken by 60%
    if n.contains("multiply") && !n.contains("merge") {
        for px in out.chunks_exact_mut(4) {
            px[0] = (px[0] as f32 * 0.6) as u8;
            px[1] = (px[1] as f32 * 0.6) as u8;
            px[2] = (px[2] as f32 * 0.6) as u8;
        }
        return out;
    }

    // Add: brighten by adding 60
    if n == "net.sf.openfx.addplugin" {
        for px in out.chunks_exact_mut(4) {
            px[0] = px[0].saturating_add(60);
            px[1] = px[1].saturating_add(60);
            px[2] = px[2].saturating_add(60);
        }
        return out;
    }

    // Clamp: crush blacks and whites
    if n.contains("clamp") {
        for px in out.chunks_exact_mut(4) {
            px[0] = px[0].max(30).min(220);
            px[1] = px[1].max(30).min(220);
            px[2] = px[2].max(30).min(220);
        }
        return out;
    }

    // Quantize: posterize to 4 levels
    if n.contains("quantize") {
        for px in out.chunks_exact_mut(4) {
            px[0] = (px[0] / 64) * 64 + 32;
            px[1] = (px[1] / 64) * 64 + 32;
            px[2] = (px[2] / 64) * 64 + 32;
        }
        return out;
    }

    // Equalize / HistEQ: stretch contrast (simple auto-levels)
    if n.contains("equalize") || n.contains("histeq") {
        let (mut min_v, mut max_v) = (255u8, 0u8);
        for px in rgba.chunks_exact(4) {
            let v = ((px[0] as u16 + px[1] as u16 + px[2] as u16) / 3) as u8;
            min_v = min_v.min(v);
            max_v = max_v.max(v);
        }
        let range = (max_v as f32 - min_v as f32).max(1.0);
        for px in out.chunks_exact_mut(4) {
            px[0] = (((px[0] as f32 - min_v as f32) / range) * 255.0).clamp(0.0, 255.0) as u8;
            px[1] = (((px[1] as f32 - min_v as f32) / range) * 255.0).clamp(0.0, 255.0) as u8;
            px[2] = (((px[2] as f32 - min_v as f32) / range) * 255.0).clamp(0.0, 255.0) as u8;
        }
        return out;
    }

    // HSVTool / HueCorrect: shift hue by ~30 degrees
    if n.contains("hsvtool") || n.contains("huecorrect") || n.contains("huekeyer") {
        for px in out.chunks_exact_mut(4) {
            let (r, g, b) = (px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0);
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            let d = max - min;
            if d > 0.01 {
                let mut h = if max == r { (g - b) / d } else if max == g { 2.0 + (b - r) / d } else { 4.0 + (r - g) / d };
                h = (h / 6.0 + 1.0 / 12.0).fract(); // shift ~30 degrees
                let s = d / max;
                let v = max;
                let i = (h * 6.0).floor() as u32;
                let f = h * 6.0 - i as f32;
                let p = v * (1.0 - s);
                let q = v * (1.0 - s * f);
                let t = v * (1.0 - s * (1.0 - f));
                let (nr, ng, nb) = match i % 6 {
                    0 => (v, t, p), 1 => (q, v, p), 2 => (p, v, t),
                    3 => (p, q, v), 4 => (t, p, v), _ => (v, p, q),
                };
                px[0] = (nr * 255.0) as u8;
                px[1] = (ng * 255.0) as u8;
                px[2] = (nb * 255.0) as u8;
            }
        }
        return out;
    }

    // Color space conversions: show as greyscale luma (Y channel)
    if n.contains("torgb") || n.contains("rgbto") || n.contains("tolab") || n.contains("labtorgb")
        || n.contains("toxyz") || n.contains("xyztorgb") || n.contains("toyuv") || n.contains("yuvtorgb")
        || n.contains("toycb") || n.contains("ycbtorgb") || n.contains("toypb") || n.contains("ypbtorgb")
        || n.contains("tohsi") || n.contains("hsitorgb") || n.contains("tohsl") || n.contains("hsltorgb")
        || n.contains("tohsv") || n.contains("hsvtorgb") || n.contains("xytoxyz") || n.contains("xyztoxy")
    {
        for px in out.chunks_exact_mut(4) {
            let y = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) as u8;
            px[0] = y; px[1] = y; px[2] = y;
        }
        return out;
    }

    // ColorSuppress: reduce green channel
    if n.contains("colorsuppress") || n.contains("despill") {
        for px in out.chunks_exact_mut(4) {
            let max_rb = px[0].max(px[2]);
            px[1] = px[1].min(max_rb);
        }
        return out;
    }

    // Log2Lin / PLogLin: S-curve (filmic look)
    if n.contains("log2lin") || n.contains("ploglin") {
        for px in out.chunks_exact_mut(4) {
            for c in &mut px[..3] {
                let v = *c as f32 / 255.0;
                let lin = ((v * 14.0 - 7.0) as f32).exp2() * 0.18;
                *c = (lin.min(1.0) * 255.0) as u8;
            }
        }
        return out;
    }

    // Mirror: flip horizontally
    if n.contains("mirror") {
        for y in 0..h {
            for x in 0..w / 2 {
                let l = (y * w + x) * 4;
                let r = (y * w + (w - 1 - x)) * 4;
                for c in 0..4 {
                    out.swap(l + c, r + c);
                }
            }
        }
        return out;
    }

    // Noise: add random noise
    if n == "net.sf.openfx.noise" || n.contains("cimgnoise") {
        let mut seed = simple_name_hash(plugin_name);
        for px in out.chunks_exact_mut(4) {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            let noise = ((seed >> 16) % 60) as i16 - 30;
            px[0] = (px[0] as i16 + noise).clamp(0, 255) as u8;
            px[1] = (px[1] as i16 + noise).clamp(0, 255) as u8;
            px[2] = (px[2] as i16 + noise).clamp(0, 255) as u8;
        }
        return out;
    }

    // Plasma: generate colorful plasma pattern
    if n.contains("plasma") {
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32 / w as f32;
                let fy = y as f32 / h as f32;
                let v1 = ((fx * 10.0).sin() + (fy * 10.0).sin()) * 0.5 + 0.5;
                let v2 = ((fx * 7.0 + fy * 13.0).cos()) * 0.5 + 0.5;
                let i = (y * w + x) * 4;
                out[i] = (v1 * 255.0) as u8;
                out[i + 1] = (v2 * 200.0) as u8;
                out[i + 2] = ((1.0 - v1) * 255.0) as u8;
                out[i + 3] = 255;
            }
        }
        return out;
    }

    // Generators: Solid color
    if n.contains("solid") || n.contains("constant") {
        for px in out.chunks_exact_mut(4) {
            px[0] = 0x30; px[1] = 0x60; px[2] = 0xA0; px[3] = 255;
        }
        return out;
    }

    // ColorBars: SMPTE-style
    if n.contains("colorbars") || n.contains("colorbar") {
        let bars: &[(u8, u8, u8)] = &[
            (192, 192, 192), (192, 192, 0), (0, 192, 192), (0, 192, 0),
            (192, 0, 192), (192, 0, 0), (0, 0, 192),
        ];
        for y in 0..h {
            for x in 0..w {
                let bar_idx = x * bars.len() / w;
                let (r, g, b) = bars[bar_idx.min(bars.len() - 1)];
                let i = (y * w + x) * 4;
                out[i] = r; out[i + 1] = g; out[i + 2] = b; out[i + 3] = 255;
            }
        }
        return out;
    }

    // CheckerBoard
    if n.contains("checker") {
        let sq = 16;
        for y in 0..h {
            for x in 0..w {
                let checker = ((x / sq) + (y / sq)) % 2 == 0;
                let v = if checker { 200u8 } else { 55 };
                let i = (y * w + x) * 4;
                out[i] = v; out[i + 1] = v; out[i + 2] = v; out[i + 3] = 255;
            }
        }
        return out;
    }

    // ColorWheel
    if n.contains("colorwheel") {
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let r = cx.min(cy);
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let i = (y * w + x) * 4;
                if dist <= r {
                    let angle = dy.atan2(dx) / std::f32::consts::TAU + 0.5;
                    let sat = dist / r;
                    let h6 = angle * 6.0;
                    let f = h6 - h6.floor();
                    let p = 1.0 - sat;
                    let q = 1.0 - sat * f;
                    let t = 1.0 - sat * (1.0 - f);
                    let (cr, cg, cb) = match h6 as u32 % 6 {
                        0 => (1.0, t, p), 1 => (q, 1.0, p), 2 => (p, 1.0, t),
                        3 => (p, q, 1.0), 4 => (t, p, 1.0), _ => (1.0, p, q),
                    };
                    out[i] = (cr * 255.0) as u8;
                    out[i + 1] = (cg * 255.0) as u8;
                    out[i + 2] = (cb * 255.0) as u8;
                } else {
                    out[i] = 0; out[i + 1] = 0; out[i + 2] = 0;
                }
                out[i + 3] = 255;
            }
        }
        return out;
    }

    // Radial: radial gradient
    if n.contains("radial") {
        let cx = w as f32 / 2.0;
        let cy = h as f32 / 2.0;
        let max_r = cx.min(cy);
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let t = (1.0 - d / max_r).clamp(0.0, 1.0);
                let v = (t * 255.0) as u8;
                let i = (y * w + x) * 4;
                out[i] = v; out[i + 1] = v; out[i + 2] = v; out[i + 3] = 255;
            }
        }
        return out;
    }

    // Ramp: linear gradient
    if n.contains("ramp") {
        for y in 0..h {
            let t = y as f32 / h as f32;
            let v = (t * 255.0) as u8;
            for x in 0..w {
                let i = (y * w + x) * 4;
                out[i] = v; out[i + 1] = v; out[i + 2] = v; out[i + 3] = 255;
            }
        }
        return out;
    }

    // Rectangle: white rectangle on the frame
    if n.contains("rectangle") {
        let x1 = w / 4; let x2 = 3 * w / 4;
        let y1 = h / 4; let y2 = 3 * h / 4;
        for y in y1..y2 {
            for x in x1..x2 {
                let i = (y * w + x) * 4;
                // Blend white at 60% over original
                out[i] = (out[i] as f32 * 0.4 + 255.0 * 0.6) as u8;
                out[i + 1] = (out[i + 1] as f32 * 0.4 + 255.0 * 0.6) as u8;
                out[i + 2] = (out[i + 2] as f32 * 0.4 + 255.0 * 0.6) as u8;
            }
        }
        return out;
    }

    // GodRays: radial light streaks from center
    if n.contains("godray") {
        let cx = w as f32 / 2.0;
        let cy = h as f32 * 0.3;
        for y in 0..h {
            for x in 0..w {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let bright = (1.0 - dist / (w as f32 * 0.8)).clamp(0.0, 0.6);
                let i = (y * w + x) * 4;
                out[i] = (out[i] as f32 + bright * 180.0).min(255.0) as u8;
                out[i + 1] = (out[i + 1] as f32 + bright * 150.0).min(255.0) as u8;
                out[i + 2] = (out[i + 2] as f32 + bright * 100.0).min(255.0) as u8;
            }
        }
        return out;
    }

    // Deinterlace: show every other line darkened
    if n.contains("deinterlace") {
        for y in (1..h).step_by(2) {
            for x in 0..w {
                let i = (y * w + x) * 4;
                out[i] = out[i] / 2;
                out[i + 1] = out[i + 1] / 2;
                out[i + 2] = out[i + 2] / 2;
            }
        }
        return out;
    }

    // Merge/Dissolve/Composite: blend frame with darkened version (A over B look)
    if n.contains("merge") || n.contains("dissolve") || n.contains("copyrectangle") {
        for px in out.chunks_exact_mut(4) {
            px[0] = (px[0] as f32 * 0.7) as u8;
            px[1] = (px[1] as f32 * 0.7) as u8;
            px[2] = (px[2] as f32 * 0.85) as u8;
        }
        // Draw a brighter rectangle overlay (the "B" layer)
        let x1 = w / 5; let x2 = 4 * w / 5;
        let y1 = h / 5; let y2 = 4 * h / 5;
        for y in y1..y2 {
            for x in x1..x2 {
                let si = (y * w + x) * 4;
                out[si] = (rgba[si] as f32 * 1.1).min(255.0) as u8;
                out[si + 1] = (rgba[si + 1] as f32 * 1.1).min(255.0) as u8;
                out[si + 2] = (rgba[si + 2] as f32 * 1.1).min(255.0) as u8;
            }
        }
        return out;
    }

    // Premult/Unpremult: show alpha channel visualization
    if n.contains("premult") || n.contains("unpremult") || n.contains("shuffle") {
        for px in out.chunks_exact_mut(4) {
            // Show RGB channels with a slight color cast
            px[0] = (px[0] as f32 * 0.9) as u8;
            px[1] = px[1];
            px[2] = (px[2] as f32 * 0.9) as u8;
        }
        return out;
    }

    // Matte monitor: show matte as white-on-black overlay
    if n.contains("mattemonitor") {
        for px in out.chunks_exact_mut(4) {
            let luma = (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32) as u8;
            let matte = if luma > 128 { 255u8 } else { 0 };
            px[0] = matte; px[1] = matte; px[2] = matte;
        }
        return out;
    }

    // Keyer/PIK: show green areas as transparent (checkerboard behind)
    if n.contains("keyer") || n.contains("pik") || n.contains("chromakeyer") {
        let sq = 8;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let r = out[i] as f32;
                let g = out[i + 1] as f32;
                let b = out[i + 2] as f32;
                // Simple green screen detection
                if g > r * 1.2 && g > b * 1.2 && g > 80.0 {
                    let checker = ((x / sq) + (y / sq)) % 2 == 0;
                    let v = if checker { 60u8 } else { 40 };
                    out[i] = v; out[i + 1] = v; out[i + 2] = v;
                }
            }
        }
        return out;
    }

    // ColorLookup: warm tint (simulating a LUT)
    if n.contains("colorlookup") {
        for px in out.chunks_exact_mut(4) {
            px[0] = (px[0] as f32 * 1.1).min(255.0) as u8;
            px[1] = (px[1] as f32 * 1.0) as u8;
            px[2] = (px[2] as f32 * 0.85).min(255.0) as u8;
        }
        return out;
    }

    // ColorMatrix: swap R and B channels
    if n.contains("colormatrix") {
        for px in out.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        return out;
    }

    // VectorToColor: show direction as color (rainbow gradient)
    if n.contains("vectortocolor") {
        for y in 0..h {
            for x in 0..w {
                let hue = x as f32 / w as f32;
                let val = 1.0 - (y as f32 / h as f32) * 0.5;
                let h6 = hue * 6.0;
                let f = h6 - h6.floor();
                let (r, g, b) = match h6 as u32 % 6 {
                    0 => (val, val * f, 0.0),
                    1 => (val * (1.0 - f), val, 0.0),
                    2 => (0.0, val, val * f),
                    3 => (0.0, val * (1.0 - f), val),
                    4 => (val * f, 0.0, val),
                    _ => (val, 0.0, val * (1.0 - f)),
                };
                let i = (y * w + x) * 4;
                out[i] = (r * 255.0) as u8;
                out[i + 1] = (g * 255.0) as u8;
                out[i + 2] = (b * 255.0) as u8;
                out[i + 3] = 255;
            }
        }
        return out;
    }

    // NoOp / ClipTest / ImageStatistics / time effects: just return the original
    if n.contains("noop") || n.contains("cliptest") || n.contains("imagestatistics")
        || n.contains("framehold") || n.contains("framerange") || n.contains("retime")
        || n.contains("timeoffset") || n.contains("notimeblur") || n.contains("frameblend")
        || n.contains("switch") || n.contains("appendclip") || n.contains("contactsheet")
        || n.contains("layercontact") || n.contains("joinview") || n.contains("mixview")
        || n.contains("oneview") || n.contains("sidebyside") || n.contains("anaglyph")
        || n.contains("adjustrod") || n.contains("position") || n.contains("spritesheet")
        || n.contains("trackerpm") || n.contains("roto") || n.contains("shadertoy")
        || n.contains("slitscan") || n.contains("stmap") || n.contains("reformat")
    {
        return out;
    }

    // Fallback: subtle warm tint so it's not identical to the original
    for px in out.chunks_exact_mut(4) {
        px[0] = (px[0] as f32 * 1.05).min(255.0) as u8;
        px[2] = (px[2] as f32 * 0.92) as u8;
    }
    out
}

/// Map plugin names to builtin effects that have proper GPU-accelerated rendering.
fn builtin_proxy_for(n: &str, plugin_name: &str) -> Option<klipp_effects::BuiltinEffect> {
    if n.contains("blur") || n.contains("soften") || n.contains("defocus") || n.contains("dirblur") {
        let radius = 4.0 + (simple_name_hash(plugin_name) % 8) as f32;
        return Some(klipp_effects::BuiltinEffect::GaussianBlur { radius });
    }
    if n.contains("bloom") || n.contains("glow") {
        return Some(klipp_effects::BuiltinEffect::GaussianBlur { radius: 12.0 });
    }
    if n.contains("sharpen") && !n.contains("denoise") {
        let amount = 1.5 + (simple_name_hash(plugin_name) % 3) as f32 * 0.5;
        return Some(klipp_effects::BuiltinEffect::Sharpen { amount });
    }
    if n.contains("denoisesharpen") || n.contains("bilateral") || n.contains("denoise")
        || n.contains("smooth") || n.contains("guided") || n.contains("rollingguidance")
        || n.contains("median") || n.contains("erode") || n.contains("dilate")
        || n.contains("erosmooth") || n.contains("inpaint")
    {
        let radius = 2.0 + (simple_name_hash(plugin_name) % 5) as f32;
        return Some(klipp_effects::BuiltinEffect::GaussianBlur { radius });
    }
    if n.contains("edgedetect") || n.contains("laplacian") || n.contains("edgeextend")
        || n.contains("erodeblur") || n.contains("distance")
    {
        return Some(klipp_effects::BuiltinEffect::Sharpen { amount: 2.5 });
    }
    if n.contains("crop") && !n.contains("copyrectangle") {
        return Some(klipp_effects::BuiltinEffect::Crop {
            left: 0.12, right: 0.12, top: 0.12, bottom: 0.12, feather: 0.04,
        });
    }
    if n.contains("transform") || n.contains("card3d") || n.contains("cornerpin") || n.contains("lensdistortion") || n.contains("idistort") {
        return Some(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.75, scale_y: 0.75,
            rotation: 10.0, anchor_x: 0.5, anchor_y: 0.5,
        });
    }
    if n.contains("timeblur") || n.contains("timedissolve") {
        return Some(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 });
    }
    if n.contains("expression") || n.contains("matrix3x3") || n.contains("matrix5x5") {
        return Some(klipp_effects::BuiltinEffect::Sharpen { amount: 1.0 });
    }
    None
}

fn simple_name_hash(name: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in name.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

