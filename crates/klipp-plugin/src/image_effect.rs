//! OFX Image Effect Suite — clip access, image allocation, and the render API.
//!
//! When a plugin calls clipGetImage during render, we provide it with a
//! pointer to the actual RGBA frame data via image property sets.
//! Supports temporal clip access: plugins can request frames at arbitrary
//! times via the frame provider callback.
//!
//! # Safety conventions
//!
//! Every suite-callback `unsafe extern "C" fn` wraps its body in
//! `ofx_guard!({ ... })` and opens with `check_non_null!(...)` — see
//! the crate-level docs and the property.rs module doc for the
//! shared pattern. Within that envelope, `*out = value` writes and
//! `CStr::from_ptr(name)` reads are sound.
//!
//! The `OfxImage::from_rgba_*` constructors hold a `Vec<u8>` and
//! hand out a `*mut c_void` pointing at its heap buffer via a
//! pointer property. The pointer stays valid as long as the
//! `OfxImage` lives in `IMAGE_STORE` — `take_image` removes the
//! entry before the plugin's `clipReleaseImage` can be called
//! again, so there's no window where the plugin holds a pointer
//! into freed storage. `update_data_pointer` re-writes the pointer
//! property after any `Vec` reallocation would have moved the heap
//! buffer (defensive; current code paths don't grow the Vec after
//! creation).

use crate::ffi::*;
use crate::instance;
use crate::property::{self, PropertySet};
use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr};

// ── Temporal frame provider ─────────────────────────────────────────

/// Frame provider for temporal clip access.
/// Called when a plugin requests a frame at a time different from the current render time.
/// Returns (rgba_bytes, width, height) or None if the frame is unavailable.
pub type FrameProviderFn = Box<dyn Fn(f64) -> Option<(Vec<u8>, u32, u32)> + Send>;

/// Current render context — stores the frame provider and render time.
pub struct RenderContext {
    frame_provider: Option<FrameProviderFn>,
    current_time: f64,
    #[allow(dead_code)]
    width: u32,
    #[allow(dead_code)]
    height: u32,
    pub premult: String,
    pub needs_float: bool,
    /// Cache of temporally-fetched images
    temporal_images: Vec<OfxPropertySetHandle>,
}

// Safety: FrameProviderFn is Send. Raw pointers in temporal_images are
// opaque keys into Mutex-protected global stores.
unsafe impl Send for RenderContext {}

pub static RENDER_CONTEXT: std::sync::LazyLock<std::sync::Mutex<Option<RenderContext>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Held from a render context's setting to its clearing: the context is
/// one for the process, and two hosts rendering at once (the viewer's and
/// an export's, each under its own lock) would otherwise overwrite each
/// other's, and one plug-in's `clipGetImage` read the other render's
/// frames. Taken innermost, after a host's lock and an unsafe plug-in's,
/// and nothing is locked while it is held but the context itself.
static RENDER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A render context in force: the render's alone until dropped, when the
/// context is cleared and its temporal images released.
#[must_use = "the render context is cleared when this is dropped"]
pub struct RenderContextGuard {
    _render: std::sync::MutexGuard<'static, ()>,
}

impl Drop for RenderContextGuard {
    fn drop(&mut self) {
        // Cleared before the lock is let go: `_render` drops after this.
        clear_render_context();
    }
}

/// Set the render context before calling the render action, for as long
/// as the returned guard lives; another render waits for it.
pub fn set_render_context(
    frame_provider: Option<FrameProviderFn>,
    current_time: f64,
    width: u32,
    height: u32,
    premult: &str,
    needs_float: bool,
) -> RenderContextGuard {
    let render = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *RENDER_CONTEXT.lock().unwrap_or_else(|e| e.into_inner()) = Some(RenderContext {
        frame_provider,
        current_time,
        width,
        height,
        premult: premult.to_string(),
        needs_float,
        temporal_images: Vec::new(),
    });
    RenderContextGuard { _render: render }
}

/// Clear the render context and clean up temporal images: by the guard's
/// drop, never while another render's context is in force.
fn clear_render_context() {
    if let Some(ctx) = RENDER_CONTEXT.lock().unwrap_or_else(|e| e.into_inner()).take() {
        // Clean up any temporally-fetched images
        for handle in ctx.temporal_images {
            release_image(handle);
        }
    }
}
use std::sync::{LazyLock, Mutex};
use tracing::warn;

// ── OfxImage ─────────────────────────────────────────────────────────

/// An image buffer wrapped with OFX properties for plugin access.
pub struct OfxImage {
    /// The RGBA pixel data. Must stay alive while the image is in use.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Property set describing this image to the plugin.
    pub props_handle: OfxPropertySetHandle,
}

// Safety: props_handle is an opaque key.
unsafe impl Send for OfxImage {}

impl OfxImage {
    /// Create an image from an RGBA buffer, registering all required OFX properties.
    pub fn from_rgba(data: Vec<u8>, width: u32, height: u32, frame_time: f64, premult: &str) -> Self {
        let pixel_bytes = width as usize * 4;
        let row_bytes = pixel_bytes as i32;
        Self::from_data(data, width, height, row_bytes, frame_time, premult, crate::constants::kOfxBitDepthByte)
    }

    /// Create a float (32-bit per channel) image from byte RGBA data.
    /// Converts each byte channel (0–255) to float (0.0–1.0).
    /// Required by CImg and other plugins that only support OfxBitDepthFloat.
    pub fn from_rgba_as_float(data: &[u8], width: u32, height: u32, frame_time: f64, premult: &str) -> Self {
        let pixel_count = (width * height) as usize;
        let float_count = pixel_count * 4;
        let mut float_data = vec![0u8; float_count * 4]; // 4 bytes per f32

        // Convert u8 RGBA to f32 RGBA
        for i in 0..float_count {
            let byte_val = data.get(i).copied().unwrap_or(0);
            let float_val = byte_val as f32 / 255.0;
            let offset = i * 4;
            float_data[offset..offset + 4].copy_from_slice(&float_val.to_ne_bytes());
        }

        let row_bytes = (width as i32) * 4 * 4; // 4 components * 4 bytes per float
        Self::from_data(float_data, width, height, row_bytes, frame_time, premult, crate::constants::kOfxBitDepthFloat)
    }

    /// Read float image data back to byte RGBA.
    pub fn float_data_to_bytes(data: &[u8], width: u32, height: u32) -> Vec<u8> {
        let pixel_count = (width * height) as usize;
        let float_count = pixel_count * 4;
        let mut bytes = vec![0u8; float_count];
        for i in 0..float_count {
            let offset = i * 4;
            if offset + 4 <= data.len() {
                let float_val = f32::from_ne_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
                bytes[i] = (float_val * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
        bytes
    }

    fn from_data(data: Vec<u8>, width: u32, height: u32, row_bytes: i32, frame_time: f64, premult: &str, bit_depth: &str) -> Self {
        let mut props = PropertySet::default();

        // Required by OFX C++ support library's validateImageBaseProperties()
        props.set_string("OfxPropType", 0, "OfxTypeImage");

        props.set_pointer(
            "OfxImagePropData",
            0,
            data.as_ptr() as *mut c_void,
        );
        props.set_int("OfxImagePropBounds", 0, 0);
        props.set_int("OfxImagePropBounds", 1, 0);
        props.set_int("OfxImagePropBounds", 2, width as i32);
        props.set_int("OfxImagePropBounds", 3, height as i32);
        props.set_int("OfxImagePropRegionOfDefinition", 0, 0);
        props.set_int("OfxImagePropRegionOfDefinition", 1, 0);
        props.set_int("OfxImagePropRegionOfDefinition", 2, width as i32);
        props.set_int("OfxImagePropRegionOfDefinition", 3, height as i32);
        props.set_int("OfxImagePropRowBytes", 0, row_bytes);
        props.set_string("OfxImageEffectPropPixelDepth", 0, bit_depth);
        props.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
        props.set_string("OfxImageEffectPropPreMultiplication", 0, premult);
        props.set_double("OfxImageEffectPropPixelAspectRatio", 0, 1.0);
        props.set_double("OfxImagePropPixelAspectRatio", 0, 1.0);
        props.set_string("OfxImagePropField", 0, crate::constants::kOfxFieldNone);
        props.set_string("OfxImagePropUniqueIdentifier", 0, &format!("klipp_{:.6}", frame_time));
        props.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
        props.set_double("OfxImageEffectPropRenderScale", 1, 1.0);

        let props_handle = property::register_propset(props);

        Self {
            data,
            width,
            height,
            props_handle,
        }
    }

    /// Update the data pointer property after the data vec may have been moved.
    pub fn update_data_pointer(&mut self) {
        property::with_propset(self.props_handle, |ps| {
            ps.set_pointer(
                "OfxImagePropData",
                0,
                self.data.as_ptr() as *mut c_void,
            );
        });
    }
}

// ── Global image store ───────────────────────────────────────────────

/// Maps image property set handle (as usize) → OfxImage.
/// Images are registered before render and released after.
static IMAGE_STORE: LazyLock<Mutex<HashMap<usize, OfxImage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn register_image(image: OfxImage) -> OfxPropertySetHandle {
    let handle = image.props_handle;
    let key = handle as usize;
    let mut store = IMAGE_STORE.lock().unwrap_or_else(|e| e.into_inner());
    store.insert(key, image);
    // Update the data pointer AFTER insertion (the Vec has been moved into the HashMap)
    if let Some(img) = store.get_mut(&key) {
        img.update_data_pointer();
    }
    handle
}

pub fn take_image(handle: OfxPropertySetHandle) -> Option<OfxImage> {
    IMAGE_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&(handle as usize))
}

/// Access an image by handle for reading (without taking ownership).
pub fn with_image<R>(handle: OfxPropertySetHandle, f: impl FnOnce(&OfxImage) -> R) -> Option<R> {
    IMAGE_STORE.lock().unwrap_or_else(|e| e.into_inner()).get(&(handle as usize)).map(f)
}

pub fn release_image(handle: OfxPropertySetHandle) {
    if let Some(img) = IMAGE_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&(handle as usize)) {
        property::remove_propset(img.props_handle);
    }
}

// ── Image memory store (for imageMemoryAlloc/Free/Lock/Unlock) ──────

static IMG_MEM_STORE: LazyLock<Mutex<HashMap<usize, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Monotonic counter for image memory handles. Starts at 1 so that 0 can
/// continue to mean "null / invalid handle" in the OFX ABI. Using a
/// counter rather than Vec::as_ptr() avoids two collision classes:
/// (1) zero-byte allocations all return NonNull::dangling(), so every such
///     handle is the same address; (2) the allocator can legitimately reuse
///     an address after a Vec is freed, giving a future alloc the same key
///     as a previously-freed one if a free/alloc interleaves on the store.
static IMG_MEM_NEXT_HANDLE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(1);

// ── OfxImageEffectSuiteV1 implementation ─────────────────────────────

unsafe extern "C" fn get_property_set_fn(
    effect: OfxImageEffectHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(props_out);
    match instance::with_instance(effect, |inst| inst.props_handle) {
        Some(h) => {
            *props_out = h;
            K_OFX_STAT_OK
        }
        None => {
            warn!("[OFX] getPropertySet: BAD_HANDLE effect={:?}", effect);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
    })
}

unsafe extern "C" fn get_param_set_fn(
    effect: OfxImageEffectHandle,
    param_set_out: *mut OfxParamSetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(param_set_out);
    match instance::with_instance(effect, |inst| inst.param_set_handle) {
        Some(h) => {
            *param_set_out = h;
            K_OFX_STAT_OK
        }
        None => {
            warn!("[OFX] getParamSet: BAD_HANDLE effect={:?}", effect);
            K_OFX_STAT_ERR_BAD_HANDLE
        }
    }
    })
}

unsafe extern "C" fn clip_define_fn(
    effect: OfxImageEffectHandle,
    name: *const c_char,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    // props_out write below is individually guarded.
    let name_str = CStr::from_ptr(name).to_string_lossy();
    match instance::with_instance(effect, |inst| {
        inst.add_clip(&name_str);
        let clip = inst.clips.get(&*name_str).unwrap();
        // Register clip → instance mapping
        instance::register_clip_mapping(clip.handle, effect, &name_str);
        clip.props_handle
    }) {
        Some(h) => {
            if !props_out.is_null() {
                *props_out = h;
            }
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn clip_get_handle_fn(
    effect: OfxImageEffectHandle,
    name: *const c_char,
    clip_out: *mut OfxImageClipHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(name);
    // clip_out / props_out writes below are individually guarded.
    let name_str = CStr::from_ptr(name).to_string_lossy();
    match instance::with_instance(effect, |inst| {
        inst.clips.get(&*name_str).map(|c| (c.handle, c.props_handle))
    }) {
        Some(Some((handle, props))) => {
            if !clip_out.is_null() {
                *clip_out = handle;
            }
            if !props_out.is_null() {
                *props_out = props;
            }
            K_OFX_STAT_OK
        }
        _ => {
            // Clip not found — create it dynamically as disconnected.
            // OFX plugins (especially CImg) request optional clips like "Mask"
            // during CreateInstance. We create them on-demand with connected=0.
            let created = instance::with_instance(effect, |inst| {
                inst.add_clip(&name_str);
                let clip = inst.clips.get(&*name_str).unwrap();
                instance::register_clip_mapping(clip.handle, effect, &name_str);
                // Mark as disconnected
                // Use the instance's bit depth for consistent pixel format
                let bit_depth = inst.bit_depth.clone();
                crate::property::with_propset(clip.props_handle, |ps| {
                    ps.set_int("OfxImageClipPropConnected", 0, 0);
                    ps.set_string("OfxImageEffectPropPixelDepth", 0, &bit_depth);
                    ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
                    ps.set_int("OfxImageClipPropOptional", 0, 1);
                    ps.set_string("OfxImageClipPropUnmappedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                    ps.set_string("OfxImageClipPropUnmappedPixelDepth", 0, &bit_depth);
                    ps.set_string("OfxImageClipPropFieldOrder", 0, crate::constants::kOfxFieldNone);
                    ps.set_string("OfxImageEffectPropPreMultiplication", 0, crate::constants::kOfxImageAlphaUnPremultiplied);
                    ps.set_double("OfxImageEffectPropPixelAspectRatio", 0, 1.0);
                    ps.set_double("OfxImageEffectPropFrameRate", 0, 24.0);
                });
                (clip.handle, clip.props_handle)
            });
            match created {
                Some((handle, props)) => {
                    if !clip_out.is_null() {
                        *clip_out = handle;
                    }
                    if !props_out.is_null() {
                        *props_out = props;
                    }
                    K_OFX_STAT_OK
                }
                None => {
                    warn!("[OFX] clipGetHandle: can't create clip '{}' on effect={:?}", name_str, effect);
                    K_OFX_STAT_ERR_BAD_HANDLE
                }
            }
        }
    }
    })
}

unsafe extern "C" fn clip_get_property_set_fn(
    clip: OfxImageClipHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(props_out);
    match instance::lookup_clip(clip) {
        Some((inst_handle, clip_name)) => {
            match instance::with_instance(inst_handle, |inst| {
                inst.clips.get(&clip_name).map(|c| c.props_handle)
            }) {
                Some(Some(h)) => {
                    *props_out = h;
                    K_OFX_STAT_OK
                }
                _ => K_OFX_STAT_ERR_BAD_HANDLE,
            }
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn clip_get_image_fn(
    clip: OfxImageClipHandle,
    time: c_double,
    _region: *const OfxRectD,
    img_props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(img_props_out);
    // Find which instance this clip belongs to
    let Some((inst_handle, clip_name)) = instance::lookup_clip(clip) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    // First try: return the pre-set current image for this clip
    let current_img = instance::with_instance(inst_handle, |inst| {
        inst.clips.get(&clip_name).and_then(|c| c.current_image)
    }).flatten();

    if let Some(img_handle) = current_img {
        // Check if we need temporal access (plugin requesting a different time)
        let mut ctx = RENDER_CONTEXT.lock().unwrap_or_else(|e| e.into_inner());
        let is_temporal = if let Some(ref ctx) = *ctx {
            (time - ctx.current_time).abs() > 0.001
        } else {
            false
        };

        if !is_temporal {
            // Current time — return the pre-set image
            *img_props_out = img_handle;
            return K_OFX_STAT_OK;
        }

        // Temporal access: plugin wants a frame at a different time
        if let Some(ref mut ctx) = *ctx {
            if let Some(ref provider) = ctx.frame_provider {
                if let Some((rgba, w, h)) = provider(time) {
                    let premult = ctx.premult.clone();
                    let needs_float = ctx.needs_float;

                    // Create a temporary image from the decoded frame
                    let temp_img = if needs_float {
                        OfxImage::from_rgba_as_float(&rgba, w, h, time, &premult)
                    } else {
                        OfxImage::from_rgba(rgba, w, h, time, &premult)
                    };
                    let temp_handle = register_image(temp_img);
                    ctx.temporal_images.push(temp_handle);

                    *img_props_out = temp_handle;
                    return K_OFX_STAT_OK;
                }
            }
        }

        // A frame the provider hasn't is no frame, never the current one:
        // a node's own instance is handed exactly the frames its node read
        // (M4 step 3), and a plug-in told the current frame for a neighbour
        // would blend it with itself as if it were one (M4 step 4).
        *img_props_out = std::ptr::null_mut();
        K_OFX_STAT_FAILED
    } else {
        // Clip exists but has no image (disconnected)
        // Try temporal provider for disconnected clips too
        let mut ctx = RENDER_CONTEXT.lock().unwrap_or_else(|e| e.into_inner());
        if clip_name == "Source" || clip_name == "SourceFrom" || clip_name == "SourceTo" {
            if let Some(ref mut ctx) = *ctx {
                if let Some(ref provider) = ctx.frame_provider {
                    if let Some((rgba, w, h)) = provider(time) {
                        let premult = ctx.premult.clone();
                        let needs_float = ctx.needs_float;
                        let temp_img = if needs_float {
                            OfxImage::from_rgba_as_float(&rgba, w, h, time, &premult)
                        } else {
                            OfxImage::from_rgba(rgba, w, h, time, &premult)
                        };
                        let temp_handle = register_image(temp_img);
                        ctx.temporal_images.push(temp_handle);
                        *img_props_out = temp_handle;
                        return K_OFX_STAT_OK;
                    }
                }
            }
        }
        K_OFX_STAT_FAILED
    }
    })
}

unsafe extern "C" fn clip_release_image_fn(
    _img_props: OfxPropertySetHandle,
) -> OfxStatus {
    ofx_guard!({
    // Images are managed by the host — release is a no-op during render.
    // The host cleans up after the render action completes.
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn clip_get_region_of_definition_fn(
    clip: OfxImageClipHandle,
    _time: c_double,
    bounds: *mut OfxRectD,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(bounds);
    // Return the clip's image bounds if available
    let Some((inst_handle, clip_name)) = instance::lookup_clip(clip) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    match instance::with_instance(inst_handle, |inst| {
        inst.clips.get(&clip_name).and_then(|c| c.current_image)
    }) {
        Some(Some(img_handle)) => {
            let mut b = OfxRectD::default();
            property::with_propset(img_handle, |ps| {
                b.x1 = ps.get_int("OfxImagePropBounds", 0).unwrap_or(0) as f64;
                b.y1 = ps.get_int("OfxImagePropBounds", 1).unwrap_or(0) as f64;
                b.x2 = ps.get_int("OfxImagePropBounds", 2).unwrap_or(0) as f64;
                b.y2 = ps.get_int("OfxImagePropBounds", 3).unwrap_or(0) as f64;
            });
            *bounds = b;
            K_OFX_STAT_OK
        }
        _ => K_OFX_STAT_FAILED,
    }
    })
}

unsafe extern "C" fn abort_fn(effect: OfxImageEffectHandle) -> c_int {
    instance::with_instance(effect, |inst| {
        if inst.abort_flag.load(std::sync::atomic::Ordering::Relaxed) { 1 } else { 0 }
    }).unwrap_or(0)
}

unsafe extern "C" fn image_memory_alloc_fn(
    _effect: OfxImageEffectHandle,
    n_bytes: usize,
    handle: *mut OfxImageMemoryHandle,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(handle);
    let buf = vec![0u8; n_bytes];
    let key = IMG_MEM_NEXT_HANDLE
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    IMG_MEM_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(key, buf);
    *handle = key as OfxImageMemoryHandle;
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn image_memory_free_fn(handle: OfxImageMemoryHandle) -> OfxStatus {
    ofx_guard!({
    let key = handle as usize;
    IMG_MEM_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
    K_OFX_STAT_OK
    })
}

unsafe extern "C" fn image_memory_lock_fn(
    handle: OfxImageMemoryHandle,
    ptr: *mut *mut c_void,
) -> OfxStatus {
    ofx_guard!({
    check_non_null!(ptr);
    let key = handle as usize;
    match IMG_MEM_STORE.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&key) {
        Some(buf) => {
            *ptr = buf.as_mut_ptr() as *mut c_void;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
    })
}

unsafe extern "C" fn image_memory_unlock_fn(_handle: OfxImageMemoryHandle) -> OfxStatus {
    ofx_guard!({
    K_OFX_STAT_OK
    })
}

/// Static OfxImageEffectSuiteV1 table.
pub static IMAGE_EFFECT_SUITE_V1: OfxImageEffectSuiteV1 = OfxImageEffectSuiteV1 {
    get_property_set: get_property_set_fn,
    get_param_set: get_param_set_fn,
    clip_define: clip_define_fn,
    clip_get_handle: clip_get_handle_fn,
    clip_get_property_set: clip_get_property_set_fn,
    clip_get_image: clip_get_image_fn,
    clip_release_image: clip_release_image_fn,
    clip_get_region_of_definition: clip_get_region_of_definition_fn,
    abort: abort_fn,
    image_memory_alloc: image_memory_alloc_fn,
    image_memory_free: image_memory_free_fn,
    image_memory_lock: image_memory_lock_fn,
    image_memory_unlock: image_memory_unlock_fn,
};

#[cfg(test)]
mod img_mem_handle_tests {
    // ═══════════════════════════════════════════════════════════════
    // CRIT-6 regression.
    //
    // The original tracker entry called out a real handle-collision
    // bug: image_memory_alloc_fn keyed its IMG_MEM_STORE HashMap on
    // `Vec::as_ptr()`, which has two collision classes:
    //   1. Zero-byte allocs all return NonNull::dangling() (typically
    //      0x1) — every zero-byte handle was the same key, so the
    //      second insert silently replaced the first.
    //   2. Allocator address reuse — a freed Vec's address can be
    //      reissued to a fresh alloc, also colliding.
    //
    // The 2026-04-16 fix replaced the ptr-as-key with a monotonic
    // AtomicUsize counter (IMG_MEM_NEXT_HANDLE). These tests pin
    // the contract so a future "let me put the ptr back, it's
    // simpler" cleanup can't re-open the collision.
    // ═══════════════════════════════════════════════════════════════

    use super::*;
    use crate::ffi::{
        OfxImageEffectHandle, OfxImageMemoryHandle, K_OFX_STAT_OK,
    };
    use std::collections::HashSet;

    /// Helper: drive the private `image_memory_alloc_fn` once,
    /// returning the handle it produced.
    fn alloc_one(n_bytes: usize) -> OfxImageMemoryHandle {
        let mut handle: OfxImageMemoryHandle = std::ptr::null_mut();
        let effect: OfxImageEffectHandle = std::ptr::null_mut();
        let status = unsafe {
            image_memory_alloc_fn(effect, n_bytes, &mut handle as *mut _)
        };
        assert_eq!(status, K_OFX_STAT_OK, "alloc must succeed");
        handle
    }

    /// Helper: free a handle produced by `alloc_one` so we leave the
    /// store clean between tests (parallel test runs share the
    /// process-wide IMG_MEM_STORE).
    fn free_one(handle: OfxImageMemoryHandle) {
        let status = unsafe { image_memory_free_fn(handle) };
        assert_eq!(status, K_OFX_STAT_OK, "free must succeed");
    }

    #[test]
    fn zero_byte_allocs_get_distinct_handles() {
        // The exact CRIT-6 scenario: two zero-byte allocations under
        // the old ptr-as-key impl would both be 0x1 (Vec::as_ptr() of
        // an empty Vec → NonNull::dangling()). The second insert
        // would silently overwrite the first. The counter-based fix
        // gives each call a unique, monotonically-incrementing
        // handle.
        let h1 = alloc_one(0);
        let h2 = alloc_one(0);
        let h3 = alloc_one(0);
        assert_ne!(h1, h2, "two zero-byte allocs must produce distinct handles");
        assert_ne!(h2, h3, "three zero-byte allocs must produce three distinct handles");
        assert_ne!(h1, h3);
        free_one(h1);
        free_one(h2);
        free_one(h3);
    }

    #[test]
    fn handles_are_never_zero_so_zero_can_mean_null_in_ofx_abi() {
        // The counter starts at 1 specifically so 0 can continue to
        // mean "null / invalid handle" in the OFX ABI. Pin that
        // contract.
        let h = alloc_one(64);
        assert_ne!(h as usize, 0,
            "an allocated handle must never be 0 (0 is reserved for null in OFX)");
        free_one(h);
    }

    #[test]
    fn many_allocs_in_a_row_are_all_distinct() {
        // Stress: 100 consecutive allocs (mix of zero and non-zero
        // sizes). Every handle should be unique. A regression that
        // re-introduced ptr-as-key would surface here as a HashSet
        // contains() hit.
        let sizes = (0..100usize).map(|i| if i % 3 == 0 { 0 } else { i });
        let handles: Vec<_> = sizes.map(alloc_one).collect();
        let unique: HashSet<usize> = handles.iter().map(|h| *h as usize).collect();
        assert_eq!(unique.len(), 100,
            "all 100 handles must be unique; got {} unique out of 100", unique.len());
        for h in handles {
            free_one(h);
        }
    }
}
