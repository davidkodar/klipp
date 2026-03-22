//! Background effect thumbnail renderer.
//!
//! Built-in effects are rendered upfront when the reference frame changes.
//! OFX effects are rendered on-demand when the user hovers over a card,
//! using a lazily-initialized separate OfxHostRuntime on a background thread.

use crate::browser::effect_registry::{EffectSource, ReferenceFrame};
use crossbeam_channel::{Receiver, Sender};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

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
    /// Render built-in effect thumbnails when the reference frame changes.
    /// OFX effects are NOT rendered here — they're rendered on hover.
    pub fn generate_builtins_if_needed(
        &mut self,
        ref_frame: &ReferenceFrame,
        effects: &[(String, EffectSource)],
    ) {
        let hash = simple_hash(&ref_frame.rgba);
        if hash == self.ref_frame_hash && !self.textures.is_empty() {
            return;
        }
        self.ref_frame_hash = hash;
        // Only clear built-in textures (cheap to regenerate).
        // Keep OFX textures — they're expensive and don't need refresh on every frame.
        self.textures.retain(|name, _| {
            effects.iter().any(|(n, src)| n == name && matches!(src, EffectSource::Ofx(_)))
        });

        // Only render built-in effects upfront
        let builtins: Vec<_> = effects
            .iter()
            .filter(|(_, src)| matches!(src, EffectSource::Builtin(_)))
            .cloned()
            .collect();

        if builtins.is_empty() {
            return;
        }

        let tx = self.tx.clone();
        let frame = ref_frame.clone();

        std::thread::spawn(move || {
            for (name, source) in builtins {
                if let EffectSource::Builtin(effect) = &source {
                    let preview = builtin_preview_params(effect);
                    let rgba = klipp_effects::apply_effects(
                        std::slice::from_ref(&preview),
                        frame.rgba.clone(),
                        frame.width,
                        frame.height,
                    );
                    let _ = tx.send(ThumbnailResult {
                        effect_name: name,
                        rgba,
                        width: frame.width,
                        height: frame.height,
                    });
                }
            }
        });
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

/// Visual proxy for OFX effects — applies a representative built-in effect
/// based on the plugin name so each thumbnail looks distinct.
fn apply_ofx_visual_proxy(frame: &ReferenceFrame, plugin_name: &str) -> Vec<u8> {
    let name_lower = plugin_name.to_lowercase();
    let proxy = if name_lower.contains("blur") || name_lower.contains("soften") || name_lower.contains("defocus") {
        let radius = 4.0 + (simple_name_hash(plugin_name) % 8) as f32;
        klipp_effects::BuiltinEffect::GaussianBlur { radius }
    } else if name_lower.contains("sharpen") || name_lower.contains("unsharp") || name_lower.contains("edge") || name_lower.contains("laplacian") {
        klipp_effects::BuiltinEffect::Sharpen { amount: 1.5 + (simple_name_hash(plugin_name) % 3) as f32 * 0.5 }
    } else if name_lower.contains("erode") || name_lower.contains("dilate") || name_lower.contains("denoise") || name_lower.contains("smooth") || name_lower.contains("guided") {
        let radius = 2.0 + (simple_name_hash(plugin_name) % 6) as f32;
        klipp_effects::BuiltinEffect::GaussianBlur { radius }
    } else if name_lower.contains("bloom") || name_lower.contains("glow") {
        klipp_effects::BuiltinEffect::GaussianBlur { radius: 10.0 }
    } else if name_lower.contains("color") || name_lower.contains("grade") || name_lower.contains("saturation") || name_lower.contains("hue") || name_lower.contains("tint") {
        klipp_effects::BuiltinEffect::Opacity { value: 0.55 }
    } else if name_lower.contains("crop") || name_lower.contains("reformat") {
        klipp_effects::BuiltinEffect::Crop { left: 0.12, right: 0.12, top: 0.12, bottom: 0.12, feather: 0.04 }
    } else if name_lower.contains("transform") || name_lower.contains("distort") || name_lower.contains("warp") {
        klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 0.75, scale_y: 0.75,
            rotation: 10.0, anchor_x: 0.5, anchor_y: 0.5,
        }
    } else if name_lower.contains("key") || name_lower.contains("chroma") {
        klipp_effects::BuiltinEffect::ChromaKey { key_color: [0, 255, 0], tolerance: 0.25, spill_suppression: 0.15 }
    } else if name_lower.contains("invert") || name_lower.contains("negate") {
        klipp_effects::BuiltinEffect::Opacity { value: 0.3 }
    } else if name_lower.contains("noise") || name_lower.contains("grain") || name_lower.contains("plasma") {
        klipp_effects::BuiltinEffect::Sharpen { amount: 1.0 }
    } else {
        let radius = 1.0 + (simple_name_hash(plugin_name) % 5) as f32;
        klipp_effects::BuiltinEffect::GaussianBlur { radius }
    };
    klipp_effects::apply_effects(std::slice::from_ref(&proxy), frame.rgba.clone(), frame.width, frame.height)
}

fn simple_name_hash(name: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in name.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

fn simple_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let sample_points = [0, data.len() / 4, data.len() / 2, 3 * data.len() / 4, data.len().saturating_sub(1)];
    for &i in &sample_points {
        if i < data.len() {
            h = h.wrapping_mul(0x100000001b3);
            h ^= data[i] as u64;
        }
    }
    h ^= data.len() as u64;
    h
}
