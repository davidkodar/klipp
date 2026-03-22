//! Built-in OFX test plugin — invert filter for CI testing.
//!
//! This is a minimal OFX plugin compiled directly into klipp-plugin.
//! It exercises the full host protocol (setHost, Load, Describe,
//! CreateInstance, Render) without needing an external .ofx.bundle.

use crate::ffi::*;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicPtr, Ordering};

static HOST_PTR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

static PLUGIN_ID: &[u8] = b"com.klipp.test.Invert\0";
static PLUGIN_API: &[u8] = b"OfxImageEffectPluginAPI\0";

unsafe extern "C" fn set_host_fn(host: *const c_void) {
    HOST_PTR.store(host as *mut c_void, Ordering::SeqCst);
}

unsafe extern "C" fn main_entry(
    action: *const c_char,
    handle: *const c_void,
    in_args: *const c_void,
    _out_args: *mut c_void,
) -> c_int {
    let action_str = CStr::from_ptr(action).to_string_lossy();
    let host = HOST_PTR.load(Ordering::SeqCst);
    if host.is_null() {
        return K_OFX_STAT_FAILED;
    }

    let host_c = &*(host as *const OfxHostC);
    let fetch_suite = host_c.fetch_suite;
    let host_props = host_c.host;

    match action_str.as_ref() {
        "OfxActionLoad" => K_OFX_STAT_OK,
        "OfxActionDescribe" => {
            // Get image effect suite
            let ie_suite = fetch_suite(
                host_props,
                b"OfxImageEffectSuite\0".as_ptr() as *const c_char,
                1,
            );
            if ie_suite.is_null() {
                return K_OFX_STAT_FAILED;
            }
            let ie = &*(ie_suite as *const OfxImageEffectSuiteV1);

            // Define clips
            let effect = handle as OfxImageEffectHandle;
            let mut _clip_props: OfxPropertySetHandle = std::ptr::null_mut();
            (ie.clip_define)(effect, b"Source\0".as_ptr() as _, &mut _clip_props);
            (ie.clip_define)(effect, b"Output\0".as_ptr() as _, &mut _clip_props);

            // Get param suite and define "Strength" parameter
            let param_suite = fetch_suite(
                host_props,
                b"OfxParameterSuite\0".as_ptr() as *const c_char,
                1,
            );
            if !param_suite.is_null() {
                let ps = &*(param_suite as *const OfxParameterSuiteV1);
                let mut param_set: OfxParamSetHandle = std::ptr::null_mut();
                (ie.get_param_set)(effect, &mut param_set);

                let mut param_props: OfxPropertySetHandle = std::ptr::null_mut();
                (ps.param_define)(
                    param_set,
                    b"OfxParamTypeDouble\0".as_ptr() as _,
                    b"Strength\0".as_ptr() as _,
                    &mut param_props,
                );

                // Set param properties via property suite
                let prop_suite = fetch_suite(
                    host_props,
                    b"OfxPropertySuite\0".as_ptr() as *const c_char,
                    1,
                );
                if !prop_suite.is_null() && !param_props.is_null() {
                    let props = &*(prop_suite as *const OfxPropertySuiteV1);
                    (props.prop_set_string)(
                        param_props,
                        b"OfxPropLabel\0".as_ptr() as _,
                        0,
                        b"Strength\0".as_ptr() as _,
                    );
                    (props.prop_set_double)(
                        param_props,
                        b"OfxParamPropDefault\0".as_ptr() as _,
                        0,
                        1.0,
                    );
                    (props.prop_set_double)(
                        param_props,
                        b"OfxParamPropDisplayMin\0".as_ptr() as _,
                        0,
                        0.0,
                    );
                    (props.prop_set_double)(
                        param_props,
                        b"OfxParamPropDisplayMax\0".as_ptr() as _,
                        0,
                        1.0,
                    );
                }
            }

            K_OFX_STAT_OK
        }
        "OfxImageEffectActionCreateInstance" | "OfxActionCreateInstance" => K_OFX_STAT_OK,
        "OfxImageEffectActionRender" => {
            // Get suites
            let ie_suite = fetch_suite(
                host_props,
                b"OfxImageEffectSuite\0".as_ptr() as *const c_char,
                1,
            );
            let param_suite = fetch_suite(
                host_props,
                b"OfxParameterSuite\0".as_ptr() as *const c_char,
                1,
            );
            let prop_suite = fetch_suite(
                host_props,
                b"OfxPropertySuite\0".as_ptr() as *const c_char,
                1,
            );

            if ie_suite.is_null() || prop_suite.is_null() {
                return K_OFX_STAT_FAILED;
            }

            let ie = &*(ie_suite as *const OfxImageEffectSuiteV1);
            let props = &*(prop_suite as *const OfxPropertySuiteV1);
            let effect = handle as OfxImageEffectHandle;

            // Get render window from inArgs
            let in_args_handle = in_args as OfxPropertySetHandle;
            let mut x1: c_int = 0;
            let mut y1: c_int = 0;
            let mut x2: c_int = 0;
            let mut y2: c_int = 0;
            (props.prop_get_int)(in_args_handle, b"OfxImageEffectPropRenderWindow\0".as_ptr() as _, 0, &mut x1);
            (props.prop_get_int)(in_args_handle, b"OfxImageEffectPropRenderWindow\0".as_ptr() as _, 1, &mut y1);
            (props.prop_get_int)(in_args_handle, b"OfxImageEffectPropRenderWindow\0".as_ptr() as _, 2, &mut x2);
            (props.prop_get_int)(in_args_handle, b"OfxImageEffectPropRenderWindow\0".as_ptr() as _, 3, &mut y2);

            // Read Strength parameter
            let mut strength: f64 = 1.0;
            if !param_suite.is_null() {
                let ps = &*(param_suite as *const OfxParameterSuiteV1);
                let mut param_set: OfxParamSetHandle = std::ptr::null_mut();
                (ie.get_param_set)(effect, &mut param_set);

                let mut param_handle: OfxParamHandle = std::ptr::null_mut();
                let mut param_props: OfxPropertySetHandle = std::ptr::null_mut();
                let status = (ps.param_get_handle)(
                    param_set,
                    b"Strength\0".as_ptr() as _,
                    &mut param_handle,
                    &mut param_props,
                );
                if status == K_OFX_STAT_OK && !param_handle.is_null() {
                    // Use paramGetValue (variadic → typed dispatch)
                    type ParamGetValueFn = unsafe extern "C" fn(OfxParamHandle, *mut f64) -> OfxStatus;
                    let get_value: ParamGetValueFn = std::mem::transmute(ps.param_get_value);
                    get_value(param_handle, &mut strength);
                }
            }

            // Get source clip image
            let mut source_clip: OfxImageClipHandle = std::ptr::null_mut();
            let mut _source_clip_props: OfxPropertySetHandle = std::ptr::null_mut();
            (ie.clip_get_handle)(effect, b"Source\0".as_ptr() as _, &mut source_clip, &mut _source_clip_props);

            let mut source_img: OfxPropertySetHandle = std::ptr::null_mut();
            let status = (ie.clip_get_image)(source_clip, 0.0, std::ptr::null(), &mut source_img);
            if status != K_OFX_STAT_OK {
                return K_OFX_STAT_FAILED;
            }

            // Get output clip image
            let mut output_clip: OfxImageClipHandle = std::ptr::null_mut();
            let mut _output_clip_props: OfxPropertySetHandle = std::ptr::null_mut();
            (ie.clip_get_handle)(effect, b"Output\0".as_ptr() as _, &mut output_clip, &mut _output_clip_props);

            let mut output_img: OfxPropertySetHandle = std::ptr::null_mut();
            let status = (ie.clip_get_image)(output_clip, 0.0, std::ptr::null(), &mut output_img);
            if status != K_OFX_STAT_OK {
                (ie.clip_release_image)(source_img);
                return K_OFX_STAT_FAILED;
            }

            // Get data pointers and row bytes
            let mut src_data: *mut c_void = std::ptr::null_mut();
            let mut dst_data: *mut c_void = std::ptr::null_mut();
            let mut src_row_bytes: c_int = 0;
            let mut dst_row_bytes: c_int = 0;

            (props.prop_get_pointer)(source_img, b"OfxImagePropData\0".as_ptr() as _, 0, &mut src_data);
            (props.prop_get_pointer)(output_img, b"OfxImagePropData\0".as_ptr() as _, 0, &mut dst_data);
            (props.prop_get_int)(source_img, b"OfxImagePropRowBytes\0".as_ptr() as _, 0, &mut src_row_bytes);
            (props.prop_get_int)(output_img, b"OfxImagePropRowBytes\0".as_ptr() as _, 0, &mut dst_row_bytes);

            if src_data.is_null() || dst_data.is_null() {
                (ie.clip_release_image)(source_img);
                (ie.clip_release_image)(output_img);
                return K_OFX_STAT_FAILED;
            }

            // Invert RGB pixels, preserve alpha
            let width = (x2 - x1) as usize;
            let height = (y2 - y1) as usize;
            let src = std::slice::from_raw_parts(src_data as *const u8, height * src_row_bytes as usize);
            let dst = std::slice::from_raw_parts_mut(dst_data as *mut u8, height * dst_row_bytes as usize);

            for y in 0..height {
                let src_row = &src[y * src_row_bytes as usize..];
                let dst_row = &mut dst[y * dst_row_bytes as usize..];
                for x in 0..width {
                    let si = x * 4;
                    let di = x * 4;
                    // Invert RGB, scale by strength
                    dst_row[di] = ((255.0 - src_row[si] as f64) * strength + src_row[si] as f64 * (1.0 - strength)) as u8;
                    dst_row[di + 1] = ((255.0 - src_row[si + 1] as f64) * strength + src_row[si + 1] as f64 * (1.0 - strength)) as u8;
                    dst_row[di + 2] = ((255.0 - src_row[si + 2] as f64) * strength + src_row[si + 2] as f64 * (1.0 - strength)) as u8;
                    dst_row[di + 3] = src_row[si + 3]; // alpha preserved
                }
            }

            (ie.clip_release_image)(source_img);
            (ie.clip_release_image)(output_img);

            K_OFX_STAT_OK
        }
        "OfxImageEffectActionDestroyInstance" | "OfxActionDestroyInstance" => K_OFX_STAT_OK,
        "OfxActionUnload" => K_OFX_STAT_OK,
        _ => K_OFX_STAT_REPLY_DEFAULT,
    }
}

/// Create a raw OFX plugin struct for the test invert plugin.
pub fn create_test_plugin_raw() -> OfxPluginRaw {
    OfxPluginRaw {
        plugin_api: PLUGIN_API.as_ptr() as *const c_char,
        api_version: 1,
        plugin_identifier: PLUGIN_ID.as_ptr() as *const c_char,
        plugin_version_major: 1,
        plugin_version_minor: 0,
        set_host: Some(set_host_fn),
        main_entry: Some(main_entry),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AppliedEffect, OfxHostRuntime, OfxValue};
    use std::collections::HashMap;
    use std::sync::{Mutex, MutexGuard};

    // OFX uses global state (HOST_PTR, global stores), so tests must be serialized.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_tests() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn create_host_with_test_plugin() -> OfxHostRuntime {
        let mut host = OfxHostRuntime::new();
        let idx = host.register_raw_plugin(create_test_plugin_raw());
        host.initialise_plugin(idx).expect("plugin init failed");
        host
    }

    #[test]
    fn test_lifecycle() {
        let _guard = lock_tests();
        let _host = create_host_with_test_plugin();
    }

    #[test]
    fn test_plugin_info() {
        let _guard = lock_tests();
        let host = create_host_with_test_plugin();
        let infos = host.plugin_infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "com.klipp.test.Invert");
        assert_eq!(infos[0].version, (1, 0));
        // Should have declared "Strength" parameter
        assert!(!infos[0].parameters.is_empty(), "expected at least 1 parameter");
        assert_eq!(infos[0].parameters[0].name, "Strength");
    }

    #[test]
    fn test_invert_full_white() {
        let _guard = lock_tests();
        let mut host = create_host_with_test_plugin();

        // 4×4 white frame (RGBA)
        let white = vec![255u8; 4 * 4 * 4];
        let effect = AppliedEffect {
            plugin_name: "com.klipp.test.Invert".to_string(),
            enabled: true,
            param_values: HashMap::new(), // default Strength=1.0
        };

        let result = host.apply_effects_chain(&[effect], &white, 4, 4, 0.0);

        assert_eq!(result.len(), white.len());
        // Inverted white = black (0,0,0) with alpha preserved (255)
        assert_eq!(result[0], 0, "R should be inverted to 0");
        assert_eq!(result[1], 0, "G should be inverted to 0");
        assert_eq!(result[2], 0, "B should be inverted to 0");
        assert_eq!(result[3], 255, "A should be preserved");
    }

    #[test]
    fn test_invert_full_black() {
        let _guard = lock_tests();
        let mut host = create_host_with_test_plugin();

        // 2×2 black frame with alpha=255
        let mut black = vec![0u8; 2 * 2 * 4];
        for pixel in black.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let effect = AppliedEffect {
            plugin_name: "com.klipp.test.Invert".to_string(),
            enabled: true,
            param_values: HashMap::new(),
        };

        let result = host.apply_effects_chain(&[effect], &black, 2, 2, 0.0);

        assert_eq!(result[0], 255, "R should be inverted to 255");
        assert_eq!(result[1], 255, "G should be inverted to 255");
        assert_eq!(result[2], 255, "B should be inverted to 255");
        assert_eq!(result[3], 255, "A should be preserved");
    }

    #[test]
    fn test_invert_strength_half() {
        let _guard = lock_tests();
        let mut host = create_host_with_test_plugin();

        // White frame, Strength=0.5
        let white = vec![255u8; 4 * 4 * 4];
        let mut params = HashMap::new();
        params.insert("Strength".to_string(), OfxValue::Double(0.5));

        let effect = AppliedEffect {
            plugin_name: "com.klipp.test.Invert".to_string(),
            enabled: true,
            param_values: params,
        };

        let result = host.apply_effects_chain(&[effect], &white, 4, 4, 0.0);

        // At strength=0.5: output = (255-255)*0.5 + 255*0.5 = 127.5 ≈ 127
        let expected = 127u8; // or 128 depending on rounding
        assert!(
            (result[0] as i16 - expected as i16).unsigned_abs() <= 1,
            "R={} expected ~{expected}",
            result[0]
        );
        assert_eq!(result[3], 255, "A should be preserved");
    }

    #[test]
    fn test_disabled_effect_passthrough() {
        let _guard = lock_tests();
        let mut host = create_host_with_test_plugin();

        let white = vec![255u8; 4 * 4 * 4];
        let effect = AppliedEffect {
            plugin_name: "com.klipp.test.Invert".to_string(),
            enabled: false,
            param_values: HashMap::new(),
        };

        let result = host.apply_effects_chain(&[effect], &white, 4, 4, 0.0);
        assert_eq!(result, white, "disabled effect should pass through unchanged");
    }

    #[test]
    fn test_property_suite_via_c_fns() {
        let _guard = lock_tests();
        // Test property suite extern "C" functions directly
        let mut ps = crate::property::PropertySet::default();
        ps.set_string("test", 0, "hello");
        ps.set_double("pi", 0, 3.14159);
        ps.set_int("count", 0, 42);

        let handle = crate::property::register_propset(ps);

        // Read back through C function pointers
        unsafe {
            let mut out: *const c_char = std::ptr::null();
            let status = (crate::property::PROPERTY_SUITE_V1.prop_get_string)(
                handle,
                b"test\0".as_ptr() as _,
                0,
                &mut out,
            );
            assert_eq!(status, K_OFX_STAT_OK);
            let s = CStr::from_ptr(out).to_string_lossy();
            assert_eq!(s, "hello");

            let mut dval: f64 = 0.0;
            let status = (crate::property::PROPERTY_SUITE_V1.prop_get_double)(
                handle,
                b"pi\0".as_ptr() as _,
                0,
                &mut dval,
            );
            assert_eq!(status, K_OFX_STAT_OK);
            assert!((dval - 3.14159).abs() < 1e-10);

            let mut ival: c_int = 0;
            let status = (crate::property::PROPERTY_SUITE_V1.prop_get_int)(
                handle,
                b"count\0".as_ptr() as _,
                0,
                &mut ival,
            );
            assert_eq!(status, K_OFX_STAT_OK);
            assert_eq!(ival, 42);
        }

        crate::property::remove_propset(handle);
    }
}
