//! Background effect thumbnail renderer.
//!
//! Uses a fixed embedded reference image (Bachalpsee landscape) for all
//! effect previews — same approach as FCP. Built-in and OFX effects are
//! rendered eagerly at startup using this reference.

use crate::browser::effect_registry::{EffectSource, ReferenceFrame};
use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// The embedded reference image effect thumbnails are rendered on,
/// until the viewer has a frame of the user's own to use instead.
static REFERENCE_PNG: &[u8] = include_bytes!("../../../../assets/effect_preview_reference.png");

/// Decode the embedded reference PNG into an RGBA ReferenceFrame.
fn load_embedded_reference() -> Option<ReferenceFrame> {
    let decoder = png::Decoder::new(std::io::Cursor::new(REFERENCE_PNG));
    let mut reader = decoder.read_info().ok()?;
    // png 0.18 returns Option<usize> for output_buffer_size —
    // None means the buffer would overflow. The embedded reference
    // PNG is small and known-good; on overflow, just give up.
    let buf_size = reader.output_buffer_size()?;
    let mut buf = vec![0u8; buf_size];
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

/// Render a frei0r filter's preview. frei0r has no notion of a default
/// parameter value, so a few plausible sets are tried and the first
/// that shows the picture is kept: not flat (a zero gamma gives black,
/// three white balance points give black), and, when one exists,
/// visibly different from the reference. `None` keeps the drawn tile.
fn frei0r_preview(type_id: &str, frame: &ReferenceFrame) -> Option<Vec<u8>> {
    use klipp_frei0r::Frei0rValue as V;
    let defaults = klipp_effects::frei0r_effect::default_params(type_id).unwrap_or_default();
    let with = |double: f64, colour: [f32; 3]| -> Vec<V> {
        defaults
            .iter()
            .map(|v| match v {
                V::Double(_) => V::Double(double),
                V::Color(_) => V::Color(colour),
                other => other.clone(),
            })
            .collect()
    };
    // Colour points as a ramp: a balance filter with black, grey and
    // white points wants exactly that, and a single colour lands white.
    let n_colours = defaults.iter().filter(|v| matches!(v, V::Color(_))).count();
    let ramped: Vec<V> = {
        let mut i = 0usize;
        defaults
            .iter()
            .map(|v| match v {
                V::Double(_) => V::Double(0.5),
                V::Color(_) => {
                    let t = if n_colours > 1 { i as f32 / (n_colours - 1) as f32 } else { 1.0 };
                    i += 1;
                    V::Color([t, t, t])
                }
                other => other.clone(),
            })
            .collect()
    };
    let candidates = [
        with(0.5, [1.0, 1.0, 1.0]),
        ramped,
        with(0.5, [0.5, 0.5, 0.5]),
        with(0.25, [0.5, 0.5, 0.5]),
        with(0.75, [0.5, 0.5, 0.5]),
        defaults.clone(),
    ];
    // frei0r wants 8-aligned dimensions: pad, render, crop.
    let (pw, ph) = (frame.width.div_ceil(8) * 8, frame.height.div_ceil(8) * 8);
    let padded = pad_rgba(&frame.rgba, frame.width, frame.height, pw, ph);
    let mut first_visible: Option<Vec<u8>> = None;
    for params in &candidates {
        let out = klipp_effects::frei0r_effect::apply(type_id, params, padded.clone(), pw, ph, 0.5);
        let rgba = crop_rgba(&out, pw, frame.width, frame.height);
        if luma_spread(&rgba) < 4.0 {
            continue; // flat: black, white or one grey
        }
        if differs_from(&rgba, &frame.rgba) {
            return Some(rgba);
        }
        first_visible.get_or_insert(rgba);
    }
    first_visible
}

/// Standard deviation of luma over the frame, in 8-bit code values.
fn luma_spread(rgba: &[u8]) -> f64 {
    let n = (rgba.len() / 4).max(1) as f64;
    let (mut sum, mut sq) = (0.0f64, 0.0f64);
    for px in rgba.chunks_exact(4) {
        let y = 0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64;
        sum += y;
        sq += y * y;
    }
    let mean = sum / n;
    (sq / n - mean * mean).max(0.0).sqrt()
}

/// Whether two frames differ by more than a few code values on average.
fn differs_from(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return true;
    }
    let total: u64 = a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
    total as f64 / a.len().max(1) as f64 > 2.0
}

/// Copy an RGBA frame into a larger canvas, the extra rows and columns
/// repeating the edge pixels so a filter sees no hard border.
fn pad_rgba(src: &[u8], w: u32, h: u32, pw: u32, ph: u32) -> Vec<u8> {
    let (w, h, pw, ph) = (w as usize, h as usize, pw as usize, ph as usize);
    let mut out = vec![0u8; pw * ph * 4];
    for y in 0..ph {
        let sy = y.min(h - 1);
        for x in 0..pw {
            let sx = x.min(w - 1);
            let si = (sy * w + sx) * 4;
            let di = (y * pw + x) * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    out
}

/// The top-left `w` × `h` of a frame `pw` wide.
fn crop_rgba(src: &[u8], pw: u32, w: u32, h: u32) -> Vec<u8> {
    let (pw, w, h) = (pw as usize, w as usize, h as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let start = y * pw * 4;
        out.extend_from_slice(&src[start..start + w * 4]);
    }
    out
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
                        // Thumbnail renderer speaks sRGB u8; bridge
                        // to match the linear-f32 `apply_effects`.
                        let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&frame.rgba);
                        let out = klipp_effects::apply_effects(
                            std::slice::from_ref(&preview),
                            linear,
                            frame.width,
                            frame.height,
                        );
                        klipp_effects::linear_f32_rgba_to_srgb_u8(&out)
                    }
                    EffectSource::Look(effects) => {
                        // The whole chain, as it will be applied.
                        let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&frame.rgba);
                        let out = klipp_effects::apply_effects(effects, linear, frame.width, frame.height);
                        klipp_effects::linear_f32_rgba_to_srgb_u8(&out)
                    }
                    EffectSource::Ofx(plugin_id) => {
                        apply_ofx_visual_proxy(&frame, plugin_id)
                    }
                    EffectSource::Audio(_) => {
                        // Audio effects have no visual preview
                        continue;
                    }
                    EffectSource::Ladspa(_) => {
                        // LADSPA plugins are audio-only — no visual preview.
                        continue;
                    }
                    EffectSource::Frei0r(info) => {
                        // Filters render the reference through the plugin
                        // at its default parameters. Sources and mixers
                        // need other inputs and keep the drawn tile.
                        if !matches!(info.plugin_type, klipp_frei0r::PluginType::Filter) {
                            continue;
                        }
                        let type_id = info.type_id();
                        match frei0r_preview(&type_id, &frame) {
                            Some(rgba) => rgba,
                            None => continue,
                        }
                    }
                    EffectSource::Lv2(_) => {
                        // LV2 plugins are audio-only — no visual preview.
                        continue;
                    }
                    EffectSource::Clap(_) => {
                        // CLAP plugins are audio-only — no visual preview.
                        continue;
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
                let mut guard = host_arc.lock().unwrap_or_else(|e| e.into_inner());
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
    ///
    /// Any `TextureHandle` displaced from `self.textures` (i.e., a
    /// re-render arrived for an effect that already had a thumbnail)
    /// is appended to `displaced` instead of being dropped inline.
    /// Caller pushes them into the app's deferred-drop bucket so the
    /// underlying wgpu texture survives the current frame's submit —
    /// dropping a `TextureHandle` mid-`update()` panics on the next
    /// queue.submit when an in-flight egui BindGroup still references
    /// the dying texture (`feedback_defer_drop_texture` memory).
    pub fn poll_and_upload(
        &mut self,
        ctx: &egui::Context,
        displaced: &mut Vec<egui::TextureHandle>,
    ) {
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

            let name = format!("fx_thumb_{}", result.effect_name);
            let texture = crate::app::alloc_texture_logged(
                ctx,
                &name,
                image,
                egui::TextureOptions::LINEAR,
            );
            // Defer-drop the previous handle for this effect, if any.
            // HashMap::insert drops the displaced value at end of stmt
            // — exactly the mid-frame TextureHandle drop the
            // defer-drop pattern exists to prevent.
            if let Some(old) = self.textures.insert(result.effect_name, texture) {
                tracing::debug!(
                    "TEX_DEFER_DROP id={:?} name=fx_thumb_re_render reason=re-rendered",
                    old.id(),
                );
                displaced.push(old);
            }
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
        klipp_effects::BuiltinEffect::ColorFade { color, mode, duration_secs, .. } => {
            // Render the thumbnail mid-fade so the user can see the
            // colour mix; default to black if the source had a
            // transparent colour.
            let preview_color = if color[3] == 0 { [0, 0, 0, 255] } else { *color };
            klipp_effects::BuiltinEffect::ColorFade {
                color: preview_color,
                duration_secs: *duration_secs,
                mode: *mode,
                progress: 0.5,
            }
        }
        klipp_effects::BuiltinEffect::Opacity { .. } => {
            klipp_effects::BuiltinEffect::Opacity { value: 0.4 }
        }
        klipp_effects::BuiltinEffect::GaussianBlur { .. } => {
            klipp_effects::BuiltinEffect::GaussianBlur { radius: 6.0 }
        }
        // The layer effects only show where the picture is not the whole
        // frame, so the thumbnail exaggerates: deep corners, and a
        // shadow and a glow wide enough to read at thumbnail size.
        klipp_effects::BuiltinEffect::RoundedCorners { .. } => {
            klipp_effects::BuiltinEffect::RoundedCorners { radius: 0.3, feather: 0.01 }
        }
        klipp_effects::BuiltinEffect::DropShadow { color, .. } => {
            klipp_effects::BuiltinEffect::DropShadow { offset_x: 0.04, offset_y: 0.06, blur: 0.06, color: *color, opacity: 0.8 }
        }
        klipp_effects::BuiltinEffect::OuterGlow { color, .. } => {
            klipp_effects::BuiltinEffect::OuterGlow { radius: 0.12, intensity: 1.0, color: *color }
        }
        klipp_effects::BuiltinEffect::Sharpen { .. } => {
            klipp_effects::BuiltinEffect::Sharpen { amount: 1.5 }
        }
        // A full thumbnail has no bars to fill; it shows as it is.
        klipp_effects::BuiltinEffect::FillBars { .. } => effect.clone(),
        klipp_effects::BuiltinEffect::Crop { .. } => {
            klipp_effects::BuiltinEffect::Crop {
                left: 0.15, right: 0.15, top: 0.15, bottom: 0.15, feather: 0.05, aspect: 0.0, focus_x: 0.5, focus_y: 0.5
            }
        }
        klipp_effects::BuiltinEffect::Transform { .. } => {
            klipp_effects::BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0,
                scale_x: 0.7, scale_y: 0.7,
                rotation: 12.0,
                anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
                focal_ref: None }
        }
        // Nothing to show without a file, or the renderer's tables: the
        // card shows the picture as it is.
        klipp_effects::BuiltinEffect::Lut3D { .. } | klipp_effects::BuiltinEffect::ColourSpaceTransform { .. } | klipp_effects::BuiltinEffect::Merge { .. } | klipp_effects::BuiltinEffect::MediaSource { .. } | klipp_effects::BuiltinEffect::TitleSource { .. } | klipp_effects::BuiltinEffect::ClipBelow {} | klipp_effects::BuiltinEffect::GradeCompound {} => effect.clone(),
        // Smaller and turned, so the card shows a picture inside its frame.
        klipp_effects::BuiltinEffect::PictureTransform { .. } => klipp_effects::BuiltinEffect::PictureTransform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.7, scale_y: 0.7, rotation: 12.0, anchor_x: 0.5, anchor_y: 0.5,
        },
        // A gradient shows what the generator makes.
        klipp_effects::BuiltinEffect::Generator { .. } => klipp_effects::BuiltinEffect::Generator {
            shape: 1, color_a: [30, 60, 160, 255], color_b: [250, 200, 120, 255], angle: 30.0, center_x: 0.5, center_y: 0.5, radius: 0.5,
        },
        // Its own ramp, dusk blue to warm cream: the card shows a map.
        klipp_effects::BuiltinEffect::GradientMap { .. } => klipp_effects::registry::effect_type("gradient_map").map_or_else(|| effect.clone(), |t| (t.sample)()),
        // A twirl, what a bend looks like at a glance.
        klipp_effects::BuiltinEffect::Warp { .. } => klipp_effects::BuiltinEffect::Warp {
            mode: klipp_effects::warp::TWIRL, amplitude: 20.0, wavelength: 0.2, speed: 1.0, direction: 0, angle: 220.0, strength: 0.5, radius: 0.6, center_x: 0.5, center_y: 0.5, band: 0.04, time: 0.0,
        },
        klipp_effects::BuiltinEffect::CornerPin { .. } | klipp_effects::BuiltinEffect::SurfacePin { .. } => {
            // A card leaning back into the frame: the pin as a picture
            // on a surface, not a rectangle.
            klipp_effects::corner_pin::with_corners([[0.12, 0.2], [0.88, 0.08], [0.8, 0.9], [0.2, 0.82]])
        }
        klipp_effects::BuiltinEffect::ChromaKey { .. } => {
            klipp_effects::BuiltinEffect::ChromaKey {
                key_color: [0, 255, 0],
                tolerance: 0.3,
                soft_edge: 0.2,
                spill_method: klipp_effects::SpillMethod::ChannelClamp,
                spill_strength: 0.6,
            }
        }
        // The darker third keyed out, so the card shows a key at all.
        klipp_effects::BuiltinEffect::LumaKey { .. } => {
            klipp_effects::BuiltinEffect::LumaKey { low: 0.0, high: 0.3, soft: 0.1, invert: false }
        }
        klipp_effects::BuiltinEffect::Bloom { .. } => {
            klipp_effects::BuiltinEffect::Bloom { threshold: 0.5, radius: 25.0, intensity: 1.0 }
        }
        klipp_effects::BuiltinEffect::Glow { .. } => {
            klipp_effects::BuiltinEffect::Glow { radius: 35.0, intensity: 0.8, threshold: 0.0 }
        }
        klipp_effects::BuiltinEffect::LightRays { .. } => {
            klipp_effects::BuiltinEffect::LightRays {
                center_x: 0.5, center_y: 0.3, threshold: 0.4, samples: 64, decay: 0.4, intensity: 0.8,
            }
        }
        klipp_effects::BuiltinEffect::Vignette { .. } => {
            klipp_effects::BuiltinEffect::Vignette {
                amount: -0.7, size: 0.6, softness: 0.4, roundness: 0.5,
            }
        }
        klipp_effects::BuiltinEffect::Diffusion { .. } => {
            klipp_effects::BuiltinEffect::Diffusion {
                amount: 0.6, radius: 35.0, threshold: 0.3, desaturation: 0.4,
            }
        }
        klipp_effects::BuiltinEffect::LensFlare { .. } => {
            klipp_effects::BuiltinEffect::LensFlare {
                position_x: 0.7, position_y: 0.3, brightness: 1.0,
                flare_size: 0.25, ghost_count: 5, ghost_intensity: 0.5,
                chromatic_aberration: 0.6,
            }
        }
        klipp_effects::BuiltinEffect::LightWrap { .. } => {
            klipp_effects::BuiltinEffect::LightWrap {
                width: 25.0, intensity: 0.8, softness: 1.5,
            }
        }
        klipp_effects::BuiltinEffect::AnamorphicStreak { .. } => {
            klipp_effects::BuiltinEffect::AnamorphicStreak {
                threshold: 0.5, length: 120.0, intensity: 0.8, tint: 0.3,
            }
        }
        klipp_effects::BuiltinEffect::FilmHalation { .. } => {
            klipp_effects::BuiltinEffect::FilmHalation {
                threshold: 0.5, radius: 25.0, intensity: 0.6, hue: 0.2,
            }
        }
        klipp_effects::BuiltinEffect::Invert { .. } => {
            klipp_effects::BuiltinEffect::Invert { mix: 1.0 }
        }
        klipp_effects::BuiltinEffect::Posterize { .. } => {
            klipp_effects::BuiltinEffect::Posterize { levels: 4.0 }
        }
        klipp_effects::BuiltinEffect::Mosaic { .. } => {
            klipp_effects::BuiltinEffect::Mosaic { block_size: 8.0 }
        }
        klipp_effects::BuiltinEffect::EdgeDetect { .. } => {
            klipp_effects::BuiltinEffect::EdgeDetect { strength: 2.0, mix: 1.0 }
        }
        klipp_effects::BuiltinEffect::ChannelMixer { .. } => {
            // Swap red and blue for a visible thumbnail
            klipp_effects::BuiltinEffect::ChannelMixer {
                red_red: 0.0, red_green: 0.0, red_blue: 1.0,
                green_red: 0.0, green_green: 1.0, green_blue: 0.0,
                blue_red: 1.0, blue_green: 0.0, blue_blue: 0.0,
            }
        }
        klipp_effects::BuiltinEffect::DirectionalBlur { .. } => {
            klipp_effects::BuiltinEffect::DirectionalBlur {
                angle: 0.0, distance: 30.0, samples: 12,
            }
        }
        klipp_effects::BuiltinEffect::Emboss { .. } => {
            klipp_effects::BuiltinEffect::Emboss {
                angle: 135.0, strength: 2.0, mix: 1.0,
            }
        }
        klipp_effects::BuiltinEffect::Mirror { .. } => {
            klipp_effects::BuiltinEffect::Mirror { horizontal: 1, vertical: 0 }
        }
        klipp_effects::BuiltinEffect::LensDistortion { .. } => {
            klipp_effects::BuiltinEffect::LensDistortion { distortion: -0.5, scale: 1.0 }
        }
        klipp_effects::BuiltinEffect::Reflection { .. } => klipp_effects::reflection::surface("calm_water", 0.55).expect("a surface"),
        klipp_effects::BuiltinEffect::Displace { .. } => {
            klipp_effects::BuiltinEffect::Displace { amount: 6.0, size: 24.0, speed: 1.0, direction: 0, seed: 0, time: 0.0 }
        }
        klipp_effects::BuiltinEffect::Dilate { .. } => {
            klipp_effects::BuiltinEffect::Dilate { radius: 3 }
        }
        klipp_effects::BuiltinEffect::Erode { .. } => {
            klipp_effects::BuiltinEffect::Erode { radius: 3 }
        }
        klipp_effects::BuiltinEffect::Denoise { .. } => {
            klipp_effects::BuiltinEffect::Denoise { spatial_sigma: 10.0, color_sigma: 0.3, iterations: 2 }
        }
        klipp_effects::BuiltinEffect::Gamma { .. } => {
            klipp_effects::BuiltinEffect::Gamma { gamma: 0.6 }
        }
        klipp_effects::BuiltinEffect::HSVAdjust { .. } => {
            klipp_effects::BuiltinEffect::HSVAdjust { hue_shift: 90.0, saturation: 1.5, value: 1.0 }
        }
        klipp_effects::BuiltinEffect::NoiseGenerator { .. } => {
            klipp_effects::BuiltinEffect::NoiseGenerator { amount: 0.3, monochrome: 0, seed: 42 }
        }
        klipp_effects::BuiltinEffect::FilmGrain { .. } => {
            klipp_effects::BuiltinEffect::FilmGrain { amount: 0.6, size: 2.5, color: 0, shadows: 0.5, seed: 3 }
        }
        klipp_effects::BuiltinEffect::OldFilm { .. } => {
            klipp_effects::BuiltinEffect::OldFilm { flicker: 0.4, scratches: 1.0, dust: 1.0, weave: 0.5, sepia: 0.7, seed: 5, amount: 1.0, age: 0.0, mono: 0.0, stains: 0.0, frame_lines: 0.0, leaks: 0.0, burn: 0.0 }
        }
        // A still card shows a flicker mid-dip.
        klipp_effects::BuiltinEffect::Flicker { .. } => {
            klipp_effects::BuiltinEffect::Flicker { amount: 0.6, speed: 1.0, smoothness: 0.0, hold: 0.0, seed: 2, target: 1, time: 0.5 }
        }
        klipp_effects::BuiltinEffect::BinocularVignette { .. } => {
            klipp_effects::BuiltinEffect::BinocularVignette { amount: -1.0, spacing: 0.4, width: 0.8, height: 0.8, softness: 0.1, fringe: 6.0 }
        }
        // A still card shows the particles well into their lives (#1141).
        klipp_effects::BuiltinEffect::Particles { style: kind, color, .. } => {
            klipp_effects::BuiltinEffect::Particles { style: *kind, amount: 400.0, size: 6.0, speed: 30.0, direction: 300.0, spread: 0.5, lifetime: 4.0, opacity: 1.0, color: *color, seed: 3, time: 2.0 }
        }
        klipp_effects::BuiltinEffect::Vhs { .. } => {
            klipp_effects::BuiltinEffect::Vhs { tracking: 0.8, bleed: 0.8, noise: 0.6, scanlines: 0.8, seed: 9 }
        }
        klipp_effects::BuiltinEffect::TiltShift { .. } => {
            klipp_effects::BuiltinEffect::TiltShift { center: 0.5, width: 0.25, softness: 0.15, radius: 8.0 }
        }
        klipp_effects::BuiltinEffect::FocusBlur { .. } => {
            klipp_effects::BuiltinEffect::FocusBlur { radius: 8.0, vertical: true, band: 0.3 }
        }
        klipp_effects::BuiltinEffect::MedianFilter { .. } => {
            klipp_effects::BuiltinEffect::MedianFilter { radius: 2 }
        }
        klipp_effects::BuiltinEffect::RadialBlur { .. } => {
            klipp_effects::BuiltinEffect::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 20.0, samples: 16, mode: 0 }
        }
        klipp_effects::BuiltinEffect::Levels { .. } => {
            klipp_effects::BuiltinEffect::Levels { in_black: 0.1, in_white: 0.8, gamma: 1.5, out_black: 0.0, out_white: 1.0 }
        }
        klipp_effects::BuiltinEffect::Equalize { .. } => {
            klipp_effects::BuiltinEffect::Equalize { strength: 1.0 }
        }
        klipp_effects::BuiltinEffect::Letterbox { .. } => {
            klipp_effects::BuiltinEffect::Letterbox {
                aspect_ratio: 2.39, bar_color: 0, opacity: 1.0,
            }
        }
        klipp_effects::BuiltinEffect::ChromaticAberration { .. } => {
            klipp_effects::BuiltinEffect::ChromaticAberration {
                red_shift: 6.0, blue_shift: -6.0, center_x: 0.5, center_y: 0.5,
            }
        }
        klipp_effects::BuiltinEffect::RollingShutterCorrection { .. } => {
            // RSC has no visual effect on a single still frame — return identity.
            klipp_effects::BuiltinEffect::RollingShutterCorrection { strength: 0.0, readout_ratio: 0.5 }
        }
        klipp_effects::BuiltinEffect::TemporalDenoise { .. } => {
            // Needs neighbouring frames; a still has none. Identity.
            klipp_effects::BuiltinEffect::TemporalDenoise { strength: 0.0, threshold: 0.02 }
        }
        klipp_effects::BuiltinEffect::Deflicker { .. } => {
            // Deflicker corrects across frames; a single still has no
            // flicker to show. Identity, as stabilization does.
            klipp_effects::BuiltinEffect::Deflicker { strength: 0.0, window_secs: 1.0, gain: 1.0 }
        }
        klipp_effects::BuiltinEffect::VideoStabilization { .. } => {
            // Stabilization has no visual effect on a single still frame — return identity.
            klipp_effects::BuiltinEffect::VideoStabilization { smoothing: 0.0, crop_ratio: 0.0, zoom: false }
        }
        klipp_effects::BuiltinEffect::SpeedRamp { .. } => {
            effect.clone()
        }
        klipp_effects::BuiltinEffect::Vibrance { .. } => {
            effect.clone()
        }
        klipp_effects::BuiltinEffect::Frei0r { .. } => {
            // Frei0r thumbnail rendering would require dlopen + per-plugin
            // construct against the static reference frame. Out of scope
            // for the static built-in thumbnail path; the browser shows
            // a generic "frei0r" tile thumbnail instead.
            effect.clone()
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
        let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(rgba);
        let out = klipp_effects::apply_effects(
            std::slice::from_ref(&builtin),
            linear, frame.width, frame.height,
        );
        return klipp_effects::linear_f32_rgba_to_srgb_u8(&out);
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
            px[0] = px[0].clamp(30, 220);
            px[1] = px[1].clamp(30, 220);
            px[2] = px[2].clamp(30, 220);
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
                let lin = (v * 14.0 - 7.0).exp2() * 0.18;
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
                out[i] /= 2;
                out[i + 1] /= 2;
                out[i + 2] /= 2;
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
            // px[1] (green) kept unchanged intentionally
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
            left: 0.12, right: 0.12, top: 0.12, bottom: 0.12, feather: 0.04, aspect: 0.0, focus_x: 0.5, focus_y: 0.5
        });
    }
    if n.contains("transform") || n.contains("card3d") || n.contains("cornerpin") || n.contains("lensdistortion") || n.contains("idistort") {
        return Some(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.75, scale_y: 0.75,
            rotation: 10.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None });
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


#[cfg(test)]
mod pad_tests {
    use super::*;

    #[test]
    fn flat_frames_are_flat_and_changed_frames_differ() {
        let black = vec![0u8; 16 * 4];
        assert!(luma_spread(&black) < 1.0);
        let mut ramp = Vec::new();
        for i in 0..16u8 {
            ramp.extend_from_slice(&[i * 16, i * 16, i * 16, 255]);
        }
        assert!(luma_spread(&ramp) > 4.0);
        assert!(!differs_from(&ramp, &ramp));
        assert!(differs_from(&ramp, &black));
    }

    #[test]
    fn padding_repeats_the_edge_and_cropping_returns_the_frame() {
        // A 3×2 frame with distinct pixels.
        let w = 3u32;
        let h = 2u32;
        let src: Vec<u8> = (0..(w * h * 4) as u8).collect();
        let padded = pad_rgba(&src, w, h, 8, 8);
        assert_eq!(padded.len(), 8 * 8 * 4);
        // The right edge repeats pixel (2, y); the bottom repeats row 1.
        let px = |v: &[u8], pw: usize, x: usize, y: usize| v[(y * pw + x) * 4..(y * pw + x) * 4 + 4].to_vec();
        assert_eq!(px(&padded, 8, 7, 0), px(&src, 3, 2, 0));
        assert_eq!(px(&padded, 8, 1, 7), px(&src, 3, 1, 1));
        assert_eq!(crop_rgba(&padded, 8, w, h), src);
    }
}
