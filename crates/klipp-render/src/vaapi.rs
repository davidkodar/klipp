//! VAAPI hardware encoding: how an AMD or Intel GPU encodes an export.
//!
//! NVENC and AMF take frames from system memory, but a VAAPI encoder only
//! takes VAAPI surfaces. So the encoder context is given a hardware frames
//! context on a render node before it opens, and every software frame the
//! export writes (NV12, or P010 at ten bits) is uploaded to a surface from
//! that context before it is sent.
//!
//! ffmpeg-next has no safe API for hardware devices or frames, so this
//! module holds the raw FFmpeg calls, and nothing else in the crate does
//! this. Any failure here is an error string, never a panic: the encoder
//! goes on to its next candidate, and in the end to software.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ffmpeg_next::ffi;

/// The render nodes an export may encode on through VAAPI: AMD's and
/// Intel's, in node order.
///
/// NVIDIA's are left out. Its VAAPI driver decodes only, and it writes
/// its errors straight to stderr, past FFmpeg's log level; NVENC is that
/// card's encoder.
pub(crate) fn encode_nodes() -> Vec<PathBuf> {
    klipp_core::GpuVendor::render_nodes()
        .into_iter()
        .filter(|(_, vendor)| matches!(vendor, klipp_core::GpuVendor::Amd | klipp_core::GpuVendor::Intel))
        .map(|(node, _)| node)
        .collect()
}

/// Make `ctx`, not yet opened, a VAAPI encoder on `node` that takes
/// `width`×`height` surfaces holding `sw_format` pictures.
pub(crate) fn attach(
    ctx: &mut ffmpeg_next::codec::encoder::video::Video,
    node: &Path,
    sw_format: ffmpeg_next::format::Pixel,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let path = CString::new(node.as_os_str().as_bytes()).map_err(|_| format!("{}: not a device path", node.display()))?;
    let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
    // SAFETY: `device` is an out-pointer to a local, `path` is NUL-terminated
    // and outlives the call, and a null dictionary means no options.
    let ret = unsafe {
        ffi::av_hwdevice_ctx_create(
            &mut device,
            ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            path.as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    if ret < 0 {
        return Err(format!("VAAPI device {}: {}", node.display(), ffmpeg_next::Error::from(ret)));
    }
    // SAFETY: `device` is the valid reference just created. The frames
    // context takes a reference of its own, so ours is released either way.
    let mut frames = unsafe {
        let frames = ffi::av_hwframe_ctx_alloc(device);
        ffi::av_buffer_unref(&mut device);
        frames
    };
    if frames.is_null() {
        return Err(format!("VAAPI frames context on {}: out of memory", node.display()));
    }
    // SAFETY: `av_hwframe_ctx_alloc` returns a buffer whose data is an
    // `AVHWFramesContext`, which may be filled in until it is initialised.
    // The pool size is the one FFmpeg's own VAAPI encoding example uses.
    let ret = unsafe {
        let fc = (*frames).data as *mut ffi::AVHWFramesContext;
        (*fc).format = ffi::AVPixelFormat::AV_PIX_FMT_VAAPI;
        (*fc).sw_format = sw_format.into();
        (*fc).width = width as i32;
        (*fc).height = height as i32;
        (*fc).initial_pool_size = 20;
        ffi::av_hwframe_ctx_init(frames)
    };
    if ret < 0 {
        // SAFETY: `frames` is our only reference; this frees it.
        unsafe { ffi::av_buffer_unref(&mut frames) };
        return Err(format!("VAAPI {sw_format:?} surfaces on {}: {}", node.display(), ffmpeg_next::Error::from(ret)));
    }
    // SAFETY: the codec context is not open and has no frames context yet
    // (it is new for each attempt). It takes ownership of our reference
    // and frees it with itself, as `hw_frames_ctx` is documented to.
    unsafe { (*ctx.as_mut_ptr()).hw_frames_ctx = frames };
    ctx.set_format(ffmpeg_next::format::Pixel::VAAPI);
    Ok(())
}

/// Whether [`attach`] gave `ctx` its surfaces.
pub(crate) fn is_attached(ctx: &ffmpeg_next::codec::Context) -> bool {
    // SAFETY: reads one pointer field of a live codec context.
    unsafe { !(*ctx.as_ptr()).hw_frames_ctx.is_null() }
}

/// Copy `picture`, in the format and size [`attach`] was given, to a VAAPI
/// surface for the encoder `ctx`, keeping its timestamp.
pub(crate) fn upload(
    ctx: &ffmpeg_next::codec::Context,
    picture: &ffmpeg_next::frame::Video,
) -> Result<ffmpeg_next::frame::Video, String> {
    if !is_attached(ctx) {
        return Err("the encoder has no VAAPI surfaces".into());
    }
    let mut surface = ffmpeg_next::frame::Video::empty();
    // SAFETY: the context's frames context was set by `attach` and lives as
    // long as the context. `surface` is an empty frame, which
    // `av_hwframe_get_buffer` fills with a surface from the pool.
    let ret = unsafe { ffi::av_hwframe_get_buffer((*ctx.as_ptr()).hw_frames_ctx, surface.as_mut_ptr(), 0) };
    if ret < 0 {
        return Err(format!("VAAPI surface: {}", ffmpeg_next::Error::from(ret)));
    }
    // SAFETY: both frames are valid and allocated; `picture` has the
    // surfaces' software format and size, which is what the upload reads.
    let ret = unsafe { ffi::av_hwframe_transfer_data(surface.as_mut_ptr(), picture.as_ptr(), 0) };
    if ret < 0 {
        return Err(format!("VAAPI upload: {}", ffmpeg_next::Error::from(ret)));
    }
    surface.set_pts(picture.pts());
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Serial with the encoder tests: on an NVIDIA machine these load
    // nvidia-vaapi-driver, whose constructor calls cuInit inside dlopen,
    // and an NVENC test opening at the same time holds cuInit's lock while
    // it waits for the loader's: the test binary hung for good (#568).
    // A CUDA decoder opening in a test that is not serial met it all the
    // same, so CUDA is made ready before NVIDIA's node is touched.
    use serial_test::serial;

    /// The render nodes of this machine, whatever their vendor: NVIDIA's
    /// VAAPI driver makes surfaces and takes uploads, it just cannot
    /// encode, so on an NVIDIA-only machine this still exercises every
    /// raw call here.
    fn any_vaapi_node() -> Option<PathBuf> {
        let (node, vendor) = klipp_core::GpuVendor::render_nodes().into_iter().next()?;
        if vendor == klipp_core::GpuVendor::Nvidia {
            cuda_ready();
        }
        Some(node)
    }

    /// CUDA initialised for this process, before NVIDIA's VAAPI driver is
    /// loaded: its constructor's cuInit, run under the loader's lock, then
    /// finds the work done instead of waiting on a thread that is in its
    /// first cuInit and wants the loader's lock (#568). Once is enough.
    fn cuda_ready() {
        static READY: std::sync::Once = std::sync::Once::new();
        READY.call_once(|| {
            let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
            // SAFETY: `device` is an out-pointer to a local; a null device
            // name and dictionary pick the default device with no options.
            // A device made is released at once: only the initialisation
            // is wanted.
            unsafe {
                if ffi::av_hwdevice_ctx_create(&mut device, ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA, std::ptr::null(), std::ptr::null_mut(), 0) >= 0 {
                    ffi::av_buffer_unref(&mut device);
                }
            }
        });
    }

    fn context(w: u32, h: u32) -> ffmpeg_next::codec::encoder::video::Video {
        let codec = ffmpeg_next::encoder::find_by_name("h264_vaapi").expect("h264_vaapi in this FFmpeg");
        let mut ctx = ffmpeg_next::codec::context::Context::new_with_codec(codec).encoder().video().unwrap();
        ctx.set_width(w);
        ctx.set_height(h);
        ctx
    }

    #[test]
    #[serial]
    fn a_picture_goes_up_to_a_surface_with_its_timestamp() {
        let _watch = crate::test_watchdog::Watchdog::start();
        ffmpeg_next::init().unwrap();
        let Some(node) = any_vaapi_node() else {
            eprintln!("skipped: no render node");
            return;
        };
        if ffmpeg_next::encoder::find_by_name("h264_vaapi").is_none() {
            eprintln!("skipped: this FFmpeg has no VAAPI encoder");
            return;
        }
        let (w, h) = (320, 180);
        let mut ctx = context(w, h);
        if let Err(e) = attach(&mut ctx, &node, ffmpeg_next::format::Pixel::NV12, w, h) {
            eprintln!("skipped: {e}");
            return;
        }
        assert!(is_attached(&ctx));
        assert_eq!(ctx.format(), ffmpeg_next::format::Pixel::VAAPI);
        let mut picture = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::NV12, w, h);
        picture.set_pts(Some(7));
        let surface = upload(&ctx, &picture).expect("upload");
        assert_eq!(surface.format(), ffmpeg_next::format::Pixel::VAAPI);
        assert_eq!((surface.width(), surface.height()), (w, h));
        assert_eq!(surface.pts(), Some(7));
    }

    #[test]
    #[serial]
    fn a_node_that_is_not_there_is_an_error_not_a_crash() {
        let _watch = crate::test_watchdog::Watchdog::start();
        ffmpeg_next::init().unwrap();
        let mut ctx = context(320, 180);
        let err = attach(&mut ctx, Path::new("/dev/dri/renderD999"), ffmpeg_next::format::Pixel::NV12, 320, 180)
            .expect_err("no such node");
        assert!(err.contains("renderD999"), "{err}");
        assert!(!is_attached(&ctx));
        let picture = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::NV12, 320, 180);
        assert!(upload(&ctx, &picture).is_err());
    }
}
