//! OFX Image Effect Suite — clip access, image allocation, and the render API.
//!
//! When a plugin calls clipGetImage during render, we provide it with a
//! pointer to the actual RGBA frame data via image property sets.

use crate::ffi::*;
use crate::instance;
use crate::property::{self, PropertySet};
use std::collections::HashMap;
use std::ffi::{c_char, c_double, c_int, c_void, CStr};
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
        let mut props = PropertySet::default();
        let row_bytes = width as i32 * 4;

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
        props.set_string("OfxImageEffectPropPixelDepth", 0, crate::constants::kOfxBitDepthByte);
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
    let mut store = IMAGE_STORE.lock().unwrap();
    store.insert(key, image);
    // Update the data pointer AFTER insertion (the Vec has been moved into the HashMap)
    if let Some(img) = store.get_mut(&key) {
        img.update_data_pointer();
    }
    handle
}

pub fn take_image(handle: OfxPropertySetHandle) -> Option<OfxImage> {
    IMAGE_STORE.lock().unwrap().remove(&(handle as usize))
}

/// Access an image by handle for reading (without taking ownership).
pub fn with_image<R>(handle: OfxPropertySetHandle, f: impl FnOnce(&OfxImage) -> R) -> Option<R> {
    IMAGE_STORE.lock().unwrap().get(&(handle as usize)).map(f)
}

pub fn release_image(handle: OfxPropertySetHandle) {
    if let Some(img) = IMAGE_STORE.lock().unwrap().remove(&(handle as usize)) {
        property::remove_propset(img.props_handle);
    }
}

// ── Image memory store (for imageMemoryAlloc/Free/Lock/Unlock) ──────

static IMG_MEM_STORE: LazyLock<Mutex<HashMap<usize, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// ── OfxImageEffectSuiteV1 implementation ─────────────────────────────

unsafe extern "C" fn get_property_set_fn(
    effect: OfxImageEffectHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
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
}

unsafe extern "C" fn get_param_set_fn(
    effect: OfxImageEffectHandle,
    param_set_out: *mut OfxParamSetHandle,
) -> OfxStatus {
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
}

unsafe extern "C" fn clip_define_fn(
    effect: OfxImageEffectHandle,
    name: *const c_char,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
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
}

unsafe extern "C" fn clip_get_handle_fn(
    effect: OfxImageEffectHandle,
    name: *const c_char,
    clip_out: *mut OfxImageClipHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
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
                crate::property::with_propset(clip.props_handle, |ps| {
                    ps.set_int("OfxImageClipPropConnected", 0, 0);
                    ps.set_string("OfxImageEffectPropPixelDepth", 0, crate::constants::kOfxBitDepthByte);
                    ps.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);
                    ps.set_int("OfxImageClipPropOptional", 0, 1);
                    ps.set_string("OfxImageClipPropUnmappedComponents", 0, crate::constants::kOfxImageComponentRGBA);
                    ps.set_string("OfxImageClipPropUnmappedPixelDepth", 0, crate::constants::kOfxBitDepthByte);
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
}

unsafe extern "C" fn clip_get_property_set_fn(
    clip: OfxImageClipHandle,
    props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
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
}

unsafe extern "C" fn clip_get_image_fn(
    clip: OfxImageClipHandle,
    _time: c_double,
    _region: *const OfxRectD,
    img_props_out: *mut OfxPropertySetHandle,
) -> OfxStatus {
    // Find which instance this clip belongs to
    let Some((inst_handle, clip_name)) = instance::lookup_clip(clip) else {
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    // Get the current image handle from the clip
    match instance::with_instance(inst_handle, |inst| {
        inst.clips.get(&clip_name).and_then(|c| c.current_image)
    }) {
        Some(Some(img_handle)) => {
            *img_props_out = img_handle;
            K_OFX_STAT_OK
        }
        Some(None) => {
            K_OFX_STAT_FAILED
        }
        _ => {
            K_OFX_STAT_FAILED
        }
    }
}

unsafe extern "C" fn clip_release_image_fn(
    _img_props: OfxPropertySetHandle,
) -> OfxStatus {
    // Images are managed by the host — release is a no-op during render.
    // The host cleans up after the render action completes.
    K_OFX_STAT_OK
}

unsafe extern "C" fn clip_get_region_of_definition_fn(
    clip: OfxImageClipHandle,
    _time: c_double,
    bounds: *mut OfxRectD,
) -> OfxStatus {
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
    let buf = vec![0u8; n_bytes];
    let key = buf.as_ptr() as usize;
    IMG_MEM_STORE.lock().unwrap().insert(key, buf);
    *handle = key as OfxImageMemoryHandle;
    K_OFX_STAT_OK
}

unsafe extern "C" fn image_memory_free_fn(handle: OfxImageMemoryHandle) -> OfxStatus {
    IMG_MEM_STORE.lock().unwrap().remove(&(handle as usize));
    K_OFX_STAT_OK
}

unsafe extern "C" fn image_memory_lock_fn(
    handle: OfxImageMemoryHandle,
    ptr: *mut *mut c_void,
) -> OfxStatus {
    let key = handle as usize;
    match IMG_MEM_STORE.lock().unwrap().get_mut(&key) {
        Some(buf) => {
            *ptr = buf.as_mut_ptr() as *mut c_void;
            K_OFX_STAT_OK
        }
        None => K_OFX_STAT_ERR_BAD_HANDLE,
    }
}

unsafe extern "C" fn image_memory_unlock_fn(_handle: OfxImageMemoryHandle) -> OfxStatus {
    K_OFX_STAT_OK
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
