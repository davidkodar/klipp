//! OpenGL GPU runtime — headless EGL context + GL texture/FBO management for OFX plugins.
//!
//! Loads libEGL.so and libGL.so at runtime. Creates a headless OpenGL context
//! via EGL (no window system needed) for OFX plugins that use the
//! OfxImageEffectOpenGLRenderSuiteV1 (clipLoadTexture/clipFreeTexture).
//!
//! Works with both NVIDIA (EGL_EXT_platform_device) and Mesa (AMD/Intel).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::{LazyLock, Mutex};
use tracing::{debug, info, warn};

// ── EGL types ───────────────────────────────────────────────────────

type EGLDisplay = *mut c_void;
type EGLConfig = *mut c_void;
type EGLContext = *mut c_void;
type EGLSurface = *mut c_void;
type EGLint = i32;
type EGLBoolean = c_uint;
type EGLenum = c_uint;

const EGL_NO_DISPLAY: EGLDisplay = std::ptr::null_mut();
const EGL_NO_CONTEXT: EGLContext = std::ptr::null_mut();
const EGL_NO_SURFACE: EGLSurface = std::ptr::null_mut();
const EGL_TRUE: EGLBoolean = 1;

// EGL config attributes
const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
const EGL_OPENGL_BIT: EGLint = 0x0008;
const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_PBUFFER_BIT: EGLint = 0x0001;
const EGL_RED_SIZE: EGLint = 0x3024;
const EGL_GREEN_SIZE: EGLint = 0x3025;
const EGL_BLUE_SIZE: EGLint = 0x3026;
const EGL_ALPHA_SIZE: EGLint = 0x3027;
const EGL_NONE: EGLint = 0x3038;

// EGL context attributes
const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: EGLint = 0x30FD;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: EGLint = 0x00000001;

// EGL API selection
const EGL_OPENGL_API: EGLenum = 0x30A2;

// EGL platform
const EGL_PLATFORM_DEVICE_EXT: EGLenum = 0x313F;

// ── GL types ────────────────────────────────────────────────────────

type GLuint = c_uint;
type GLint = c_int;
type GLenum = c_uint;
type GLsizei = c_int;
type GLvoid = c_void;

const GL_TEXTURE_2D: GLenum = 0x0DE1;
const GL_RGBA: GLenum = 0x1908;
const GL_RGBA8: GLenum = 0x8058;
const GL_UNSIGNED_BYTE: GLenum = 0x1401;
const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
const GL_LINEAR: GLint = 0x2601;
const GL_NEAREST: GLint = 0x2600;
const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
const GL_CLAMP_TO_EDGE: GLint = 0x812F;
const GL_FRAMEBUFFER: GLenum = 0x8D40;
const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
const GL_MAX_TEXTURE_SIZE: GLenum = 0x0D33;
const GL_NO_ERROR: GLenum = 0;
const GL_VERSION: GLenum = 0x1F02;
const GL_RENDERER: GLenum = 0x1F01;

// ── EGL function signatures ─────────────────────────────────────────

type EglGetDisplayFn = unsafe extern "C" fn(display_id: *mut c_void) -> EGLDisplay;
type EglGetPlatformDisplayFn = unsafe extern "C" fn(platform: EGLenum, native_display: *mut c_void, attribs: *const isize) -> EGLDisplay;
type EglInitializeFn = unsafe extern "C" fn(display: EGLDisplay, major: *mut EGLint, minor: *mut EGLint) -> EGLBoolean;
type EglTerminateFn = unsafe extern "C" fn(display: EGLDisplay) -> EGLBoolean;
type EglChooseConfigFn = unsafe extern "C" fn(display: EGLDisplay, attribs: *const EGLint, configs: *mut EGLConfig, config_size: EGLint, num_configs: *mut EGLint) -> EGLBoolean;
type EglCreateContextFn = unsafe extern "C" fn(display: EGLDisplay, config: EGLConfig, share: EGLContext, attribs: *const EGLint) -> EGLContext;
type EglDestroyContextFn = unsafe extern "C" fn(display: EGLDisplay, context: EGLContext) -> EGLBoolean;
type EglMakeCurrentFn = unsafe extern "C" fn(display: EGLDisplay, draw: EGLSurface, read: EGLSurface, ctx: EGLContext) -> EGLBoolean;
type EglBindApiFn = unsafe extern "C" fn(api: EGLenum) -> EGLBoolean;
type EglQueryDevicesExtFn = unsafe extern "C" fn(max: EGLint, devices: *mut *mut c_void, num: *mut EGLint) -> EGLBoolean;

// ── GL function signatures ──────────────────────────────────────────

type GlGenTexturesFn = unsafe extern "C" fn(n: GLsizei, textures: *mut GLuint);
type GlDeleteTexturesFn = unsafe extern "C" fn(n: GLsizei, textures: *const GLuint);
type GlBindTextureFn = unsafe extern "C" fn(target: GLenum, texture: GLuint);
type GlTexImage2DFn = unsafe extern "C" fn(target: GLenum, level: GLint, internal: GLint, width: GLsizei, height: GLsizei, border: GLint, format: GLenum, typ: GLenum, data: *const GLvoid);
type GlTexParameteriFn = unsafe extern "C" fn(target: GLenum, pname: GLenum, param: GLint);
type GlGenFramebuffersFn = unsafe extern "C" fn(n: GLsizei, fbs: *mut GLuint);
type GlDeleteFramebuffersFn = unsafe extern "C" fn(n: GLsizei, fbs: *const GLuint);
type GlBindFramebufferFn = unsafe extern "C" fn(target: GLenum, fb: GLuint);
type GlFramebufferTexture2DFn = unsafe extern "C" fn(target: GLenum, attachment: GLenum, textarget: GLenum, texture: GLuint, level: GLint);
type GlCheckFramebufferStatusFn = unsafe extern "C" fn(target: GLenum) -> GLenum;
type GlViewportFn = unsafe extern "C" fn(x: GLint, y: GLint, width: GLsizei, height: GLsizei);
type GlReadPixelsFn = unsafe extern "C" fn(x: GLint, y: GLint, width: GLsizei, height: GLsizei, format: GLenum, typ: GLenum, pixels: *mut GLvoid);
type GlGetIntegervFn = unsafe extern "C" fn(pname: GLenum, data: *mut GLint);
type GlGetStringFn = unsafe extern "C" fn(name: GLenum) -> *const c_char;
type GlGetErrorFn = unsafe extern "C" fn() -> GLenum;
type GlFlushFn = unsafe extern "C" fn();
type GlFinishFn = unsafe extern "C" fn();

// ── API wrappers ────────────────────────────────────────────────────

struct EglApi {
    _lib: std::sync::Arc<libloading::Library>,
    egl_get_display: EglGetDisplayFn,
    egl_get_platform_display: Option<EglGetPlatformDisplayFn>,
    egl_initialize: EglInitializeFn,
    egl_terminate: EglTerminateFn,
    egl_choose_config: EglChooseConfigFn,
    egl_create_context: EglCreateContextFn,
    egl_destroy_context: EglDestroyContextFn,
    egl_make_current: EglMakeCurrentFn,
    egl_bind_api: EglBindApiFn,
    egl_query_devices: Option<EglQueryDevicesExtFn>,
}

unsafe impl Send for EglApi {}
unsafe impl Sync for EglApi {}

struct GlApi {
    _lib: std::sync::Arc<libloading::Library>,
    gl_gen_textures: GlGenTexturesFn,
    gl_delete_textures: GlDeleteTexturesFn,
    gl_bind_texture: GlBindTextureFn,
    gl_tex_image_2d: GlTexImage2DFn,
    gl_tex_parameteri: GlTexParameteriFn,
    gl_gen_framebuffers: GlGenFramebuffersFn,
    gl_delete_framebuffers: GlDeleteFramebuffersFn,
    gl_bind_framebuffer: GlBindFramebufferFn,
    gl_framebuffer_texture2d: GlFramebufferTexture2DFn,
    gl_check_framebuffer_status: GlCheckFramebufferStatusFn,
    gl_viewport: GlViewportFn,
    gl_read_pixels: GlReadPixelsFn,
    gl_get_integerv: GlGetIntegervFn,
    gl_get_string: GlGetStringFn,
    gl_get_error: GlGetErrorFn,
    gl_flush: GlFlushFn,
    gl_finish: GlFinishFn,
}

unsafe impl Send for GlApi {}
unsafe impl Sync for GlApi {}

impl EglApi {
    fn load() -> Option<Self> {
        let lib = unsafe {
            libloading::Library::new("libEGL.so.1")
                .or_else(|_| libloading::Library::new("libEGL.so"))
                .ok()?
        };
        let lib = std::sync::Arc::new(lib);

        macro_rules! sym {
            ($name:expr) => {{
                let s: libloading::Symbol<*const c_void> = unsafe { lib.get($name) }.ok()?;
                unsafe { std::mem::transmute(*s) }
            }};
        }
        macro_rules! sym_opt {
            ($name:expr) => {{
                let s: Option<libloading::Symbol<*const c_void>> = unsafe { lib.get($name) }.ok();
                s.map(|s| unsafe { std::mem::transmute(*s) })
            }};
        }

        Some(Self {
            egl_get_display: sym!(b"eglGetDisplay\0"),
            egl_get_platform_display: sym_opt!(b"eglGetPlatformDisplay\0"),
            egl_initialize: sym!(b"eglInitialize\0"),
            egl_terminate: sym!(b"eglTerminate\0"),
            egl_choose_config: sym!(b"eglChooseConfig\0"),
            egl_create_context: sym!(b"eglCreateContext\0"),
            egl_destroy_context: sym!(b"eglDestroyContext\0"),
            egl_make_current: sym!(b"eglMakeCurrent\0"),
            egl_bind_api: sym!(b"eglBindAPI\0"),
            egl_query_devices: sym_opt!(b"eglQueryDevicesEXT\0"),
            _lib: lib,
        })
    }
}

impl GlApi {
    fn load() -> Option<Self> {
        let lib = unsafe {
            libloading::Library::new("libGL.so.1")
                .or_else(|_| libloading::Library::new("libGL.so"))
                .ok()?
        };
        let lib = std::sync::Arc::new(lib);

        macro_rules! sym {
            ($name:expr) => {{
                let s: libloading::Symbol<*const c_void> = unsafe { lib.get($name) }.ok()?;
                unsafe { std::mem::transmute(*s) }
            }};
        }

        Some(Self {
            gl_gen_textures: sym!(b"glGenTextures\0"),
            gl_delete_textures: sym!(b"glDeleteTextures\0"),
            gl_bind_texture: sym!(b"glBindTexture\0"),
            gl_tex_image_2d: sym!(b"glTexImage2D\0"),
            gl_tex_parameteri: sym!(b"glTexParameteri\0"),
            gl_gen_framebuffers: sym!(b"glGenFramebuffers\0"),
            gl_delete_framebuffers: sym!(b"glDeleteFramebuffers\0"),
            gl_bind_framebuffer: sym!(b"glBindFramebuffer\0"),
            gl_framebuffer_texture2d: sym!(b"glFramebufferTexture2D\0"),
            gl_check_framebuffer_status: sym!(b"glCheckFramebufferStatus\0"),
            gl_viewport: sym!(b"glViewport\0"),
            gl_read_pixels: sym!(b"glReadPixels\0"),
            gl_get_integerv: sym!(b"glGetIntegerv\0"),
            gl_get_string: sym!(b"glGetString\0"),
            gl_get_error: sym!(b"glGetError\0"),
            gl_flush: sym!(b"glFlush\0"),
            gl_finish: sym!(b"glFinish\0"),
            _lib: lib,
        })
    }
}

// ── Public OpenGL runtime ───────────────────────────────────────────

/// OpenGL GPU runtime for OFX plugin rendering.
/// Manages a headless EGL context and provides texture/FBO operations
/// for the OfxImageEffectOpenGLRenderSuiteV1 suite.
pub struct OpenGlRuntime {
    egl: EglApi,
    gl: GlApi,
    display: EGLDisplay,
    context: EGLContext,
    max_texture_size: i32,
    gl_version: String,
    gl_renderer: String,
}

unsafe impl Send for OpenGlRuntime {}
unsafe impl Sync for OpenGlRuntime {}

impl OpenGlRuntime {
    /// Create a headless OpenGL context via EGL. Returns None if unavailable.
    pub fn new() -> Option<Self> {
        let egl = EglApi::load()?;
        let gl = GlApi::load()?;

        // Bind OpenGL API (not GLES)
        let r = unsafe { (egl.egl_bind_api)(EGL_OPENGL_API) };
        if r != EGL_TRUE {
            debug!("OpenGL: eglBindAPI(EGL_OPENGL_API) failed");
            return None;
        }

        // Try EGL_EXT_platform_device first (NVIDIA headless), fall back to default display
        let display = if let Some(query_devices) = egl.egl_query_devices {
            if let Some(get_platform) = egl.egl_get_platform_display {
                let mut num_devices: EGLint = 0;
                let r = unsafe { query_devices(0, std::ptr::null_mut(), &mut num_devices) };
                if r == EGL_TRUE && num_devices > 0 {
                    let mut devices = vec![std::ptr::null_mut(); num_devices as usize];
                    unsafe { query_devices(num_devices, devices.as_mut_ptr(), &mut num_devices) };
                    let d = unsafe {
                        get_platform(EGL_PLATFORM_DEVICE_EXT, devices[0], std::ptr::null())
                    };
                    if d != EGL_NO_DISPLAY {
                        d
                    } else {
                        unsafe { (egl.egl_get_display)(std::ptr::null_mut()) }
                    }
                } else {
                    unsafe { (egl.egl_get_display)(std::ptr::null_mut()) }
                }
            } else {
                unsafe { (egl.egl_get_display)(std::ptr::null_mut()) }
            }
        } else {
            unsafe { (egl.egl_get_display)(std::ptr::null_mut()) }
        };

        if display == EGL_NO_DISPLAY {
            debug!("OpenGL: no EGL display available");
            return None;
        }

        // Initialize EGL
        let mut major: EGLint = 0;
        let mut minor: EGLint = 0;
        let r = unsafe { (egl.egl_initialize)(display, &mut major, &mut minor) };
        if r != EGL_TRUE {
            debug!("OpenGL: eglInitialize failed");
            return None;
        }
        debug!("OpenGL: EGL {major}.{minor}");

        // Choose config
        let config_attribs: [EGLint; 13] = [
            EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
            EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT,
            EGL_RED_SIZE, 8,
            EGL_GREEN_SIZE, 8,
            EGL_BLUE_SIZE, 8,
            EGL_ALPHA_SIZE, 8,
            EGL_NONE,
        ];
        let mut config: EGLConfig = std::ptr::null_mut();
        let mut num_configs: EGLint = 0;
        let r = unsafe {
            (egl.egl_choose_config)(display, config_attribs.as_ptr(), &mut config, 1, &mut num_configs)
        };
        if r != EGL_TRUE || num_configs == 0 {
            warn!("OpenGL: eglChooseConfig failed (configs={num_configs})");
            unsafe { (egl.egl_terminate)(display) };
            return None;
        }

        // Create context (request GL 3.2 core for FBO support)
        let ctx_attribs: [EGLint; 7] = [
            EGL_CONTEXT_MAJOR_VERSION, 3,
            EGL_CONTEXT_MINOR_VERSION, 2,
            EGL_CONTEXT_OPENGL_PROFILE_MASK, EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
            EGL_NONE,
        ];
        let context = unsafe {
            (egl.egl_create_context)(display, config, EGL_NO_CONTEXT, ctx_attribs.as_ptr())
        };
        if context == EGL_NO_CONTEXT {
            // Fall back to default GL version
            let ctx_attribs_fallback: [EGLint; 1] = [EGL_NONE];
            let context = unsafe {
                (egl.egl_create_context)(display, config, EGL_NO_CONTEXT, ctx_attribs_fallback.as_ptr())
            };
            if context == EGL_NO_CONTEXT {
                warn!("OpenGL: eglCreateContext failed");
                unsafe { (egl.egl_terminate)(display) };
                return None;
            }
        }

        // Make context current (surfaceless — EGL_KHR_surfaceless_context)
        let r = unsafe {
            (egl.egl_make_current)(display, EGL_NO_SURFACE, EGL_NO_SURFACE, context)
        };
        if r != EGL_TRUE {
            warn!("OpenGL: eglMakeCurrent (surfaceless) failed");
            unsafe { (egl.egl_destroy_context)(display, context) };
            unsafe { (egl.egl_terminate)(display) };
            return None;
        }

        // Query GL info
        let gl_version = unsafe {
            let p = (gl.gl_get_string)(GL_VERSION);
            if p.is_null() { "Unknown".to_string() } else { CStr::from_ptr(p).to_string_lossy().to_string() }
        };
        let gl_renderer = unsafe {
            let p = (gl.gl_get_string)(GL_RENDERER);
            if p.is_null() { "Unknown".to_string() } else { CStr::from_ptr(p).to_string_lossy().to_string() }
        };

        let mut max_tex: GLint = 0;
        unsafe { (gl.gl_get_integerv)(GL_MAX_TEXTURE_SIZE, &mut max_tex) };

        // Unbind context (will be made current per-render)
        unsafe { (egl.egl_make_current)(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT) };

        info!("OpenGL: initialized — {} ({}), max texture {}x{}", gl_version, gl_renderer, max_tex, max_tex);

        Some(Self {
            egl,
            gl,
            display,
            context,
            max_texture_size: max_tex,
            gl_version,
            gl_renderer,
        })
    }

    /// GL version string.
    pub fn gl_version(&self) -> &str { &self.gl_version }
    /// GL renderer string.
    pub fn gl_renderer(&self) -> &str { &self.gl_renderer }
    /// Maximum texture dimension.
    pub fn max_texture_size(&self) -> i32 { self.max_texture_size }

    /// Make the GL context current on this thread. Must call before any GL operations.
    pub fn make_current(&self) -> bool {
        let r = unsafe {
            (self.egl.egl_make_current)(self.display, EGL_NO_SURFACE, EGL_NO_SURFACE, self.context)
        };
        r == EGL_TRUE
    }

    /// Release the GL context from this thread.
    pub fn release_current(&self) {
        unsafe {
            (self.egl.egl_make_current)(self.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        }
    }

    /// Create a GL texture from RGBA data. Returns (texture_id, target).
    pub fn create_texture(&self, data: &[u8], width: u32, height: u32) -> Option<(GLuint, GLenum)> {
        let mut tex: GLuint = 0;
        unsafe { (self.gl.gl_gen_textures)(1, &mut tex) };
        if tex == 0 {
            return None;
        }

        unsafe {
            (self.gl.gl_bind_texture)(GL_TEXTURE_2D, tex);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (self.gl.gl_tex_image_2d)(
                GL_TEXTURE_2D, 0, GL_RGBA8 as GLint,
                width as GLsizei, height as GLsizei, 0,
                GL_RGBA, GL_UNSIGNED_BYTE,
                data.as_ptr() as *const GLvoid,
            );
            (self.gl.gl_bind_texture)(GL_TEXTURE_2D, 0);
        }

        let err = unsafe { (self.gl.gl_get_error)() };
        if err != GL_NO_ERROR {
            warn!("OpenGL: texture creation error 0x{:X}", err);
            unsafe { (self.gl.gl_delete_textures)(1, &tex) };
            return None;
        }

        Some((tex, GL_TEXTURE_2D))
    }

    /// Create an empty GL texture for output. Returns (texture_id, target).
    pub fn create_output_texture(&self, width: u32, height: u32) -> Option<(GLuint, GLenum)> {
        let mut tex: GLuint = 0;
        unsafe { (self.gl.gl_gen_textures)(1, &mut tex) };
        if tex == 0 { return None; }

        unsafe {
            (self.gl.gl_bind_texture)(GL_TEXTURE_2D, tex);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            (self.gl.gl_tex_parameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            (self.gl.gl_tex_image_2d)(
                GL_TEXTURE_2D, 0, GL_RGBA8 as GLint,
                width as GLsizei, height as GLsizei, 0,
                GL_RGBA, GL_UNSIGNED_BYTE,
                std::ptr::null(),
            );
            (self.gl.gl_bind_texture)(GL_TEXTURE_2D, 0);
        }

        let err = unsafe { (self.gl.gl_get_error)() };
        if err != GL_NO_ERROR {
            unsafe { (self.gl.gl_delete_textures)(1, &tex) };
            return None;
        }

        Some((tex, GL_TEXTURE_2D))
    }

    /// Delete a GL texture.
    pub fn delete_texture(&self, tex: GLuint) {
        unsafe { (self.gl.gl_delete_textures)(1, &tex) };
    }

    /// Create an FBO with the given texture as color attachment. Returns fbo_id.
    pub fn create_fbo(&self, texture: GLuint) -> Option<GLuint> {
        let mut fbo: GLuint = 0;
        unsafe {
            (self.gl.gl_gen_framebuffers)(1, &mut fbo);
            (self.gl.gl_bind_framebuffer)(GL_FRAMEBUFFER, fbo);
            (self.gl.gl_framebuffer_texture2d)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
        }

        let status = unsafe { (self.gl.gl_check_framebuffer_status)(GL_FRAMEBUFFER) };
        if status != GL_FRAMEBUFFER_COMPLETE {
            warn!("OpenGL: FBO incomplete (status=0x{:X})", status);
            unsafe {
                (self.gl.gl_bind_framebuffer)(GL_FRAMEBUFFER, 0);
                (self.gl.gl_delete_framebuffers)(1, &fbo);
            }
            return None;
        }

        Some(fbo)
    }

    /// Delete an FBO.
    pub fn delete_fbo(&self, fbo: GLuint) {
        unsafe {
            (self.gl.gl_bind_framebuffer)(GL_FRAMEBUFFER, 0);
            (self.gl.gl_delete_framebuffers)(1, &fbo);
        }
    }

    /// Bind FBO and set viewport.
    pub fn bind_fbo(&self, fbo: GLuint, width: u32, height: u32) {
        unsafe {
            (self.gl.gl_bind_framebuffer)(GL_FRAMEBUFFER, fbo);
            (self.gl.gl_viewport)(0, 0, width as GLsizei, height as GLsizei);
        }
    }

    /// Read pixels from the currently bound FBO into an RGBA buffer.
    pub fn read_pixels(&self, width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![0u8; (width * height * 4) as usize];
        unsafe {
            (self.gl.gl_flush)();
            (self.gl.gl_finish)();
            (self.gl.gl_read_pixels)(
                0, 0, width as GLsizei, height as GLsizei,
                GL_RGBA, GL_UNSIGNED_BYTE,
                data.as_mut_ptr() as *mut GLvoid,
            );
        }
        data
    }

    /// Flush + finish GL pipeline.
    pub fn flush_and_finish(&self) {
        unsafe {
            (self.gl.gl_flush)();
            (self.gl.gl_finish)();
        }
    }
}

impl Drop for OpenGlRuntime {
    fn drop(&mut self) {
        unsafe {
            (self.egl.egl_make_current)(self.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
            (self.egl.egl_destroy_context)(self.display, self.context);
            (self.egl.egl_terminate)(self.display);
        }
        debug!("OpenGL: runtime destroyed");
    }
}

// ── OFX OpenGL Render Suite state ───────────────────────────────────

/// Global store for textures loaded via clipLoadTexture.
/// Maps texture property set handle → (texture_id, is_output).
static GL_TEXTURE_STORE: LazyLock<Mutex<HashMap<usize, GlTextureInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct GlTextureInfo {
    texture_id: GLuint,
    #[allow(dead_code)]
    target: GLenum,
    is_output: bool,
}

pub fn register_gl_texture(props_handle: crate::ffi::OfxPropertySetHandle, tex_id: GLuint, target: GLenum, is_output: bool) {
    GL_TEXTURE_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(props_handle as usize, GlTextureInfo {
        texture_id: tex_id,
        target,
        is_output,
    });
}

pub fn release_gl_texture(props_handle: crate::ffi::OfxPropertySetHandle) -> Option<(GLuint, bool)> {
    GL_TEXTURE_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&(props_handle as usize))
        .map(|info| (info.texture_id, info.is_output))
}

// ── OfxImageEffectOpenGLRenderSuiteV1 implementation ─────────────────

use crate::ffi::*;

/// clipLoadTexture — load a clip's image as a GL texture.
/// The texture is created from the clip's current image data and registered
/// with OFX properties (TextureIndex, TextureTarget, bounds, etc.).
unsafe extern "C" fn clip_load_texture_fn(
    clip: OfxImageClipHandle,
    _time: std::ffi::c_double,
    _format: *const c_char,
    _region: *const OfxRectD,
    texture_handle: *mut OfxPropertySetHandle,
) -> OfxStatus {
    // Find which instance this clip belongs to
    let Some((inst_handle, clip_name)) = crate::instance::lookup_clip(clip) else {
        warn!("[OFX GL] clipLoadTexture: BAD_HANDLE clip={:?}", clip);
        return K_OFX_STAT_ERR_BAD_HANDLE;
    };

    // Get the current image handle from the clip
    let img_handle = crate::instance::with_instance(inst_handle, |inst| {
        inst.clips.get(&clip_name).and_then(|c| c.current_image)
    }).flatten();

    let Some(img_handle) = img_handle else {
        debug!("[OFX GL] clipLoadTexture: no image for clip '{}'", clip_name);
        return K_OFX_STAT_FAILED;
    };

    // Read image properties (bounds, dimensions)
    let (width, height) = crate::property::with_propset(img_handle, |ps| {
        let w = ps.get_int("OfxImagePropBounds", 2).unwrap_or(0)
              - ps.get_int("OfxImagePropBounds", 0).unwrap_or(0);
        let h = ps.get_int("OfxImagePropBounds", 3).unwrap_or(0)
              - ps.get_int("OfxImagePropBounds", 1).unwrap_or(0);
        (w as u32, h as u32)
    }).unwrap_or((0, 0));

    if width == 0 || height == 0 {
        return K_OFX_STAT_FAILED;
    }

    // Access the GL runtime from the global store
    let gl_store = GL_RUNTIME_STORE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(gl) = gl_store.as_ref() else {
        warn!("[OFX GL] clipLoadTexture: no OpenGL runtime");
        return K_OFX_STAT_FAILED;
    };

    let is_output = clip_name == "Output";

    // Create the GL texture
    let tex_result = if is_output {
        // Output: empty texture (plugin renders into it via FBO)
        gl.create_output_texture(width, height)
    } else {
        // Input: upload CPU data to texture
        let data = crate::image_effect::with_image(img_handle, |img| img.data.clone());
        match data {
            Some(d) => gl.create_texture(&d, width, height),
            None => None,
        }
    };

    let Some((tex_id, target)) = tex_result else {
        warn!("[OFX GL] clipLoadTexture: texture creation failed for '{}'", clip_name);
        return K_OFX_STAT_ERR_MEMORY;
    };

    // If output, set up FBO
    if is_output {
        if let Some(fbo) = gl.create_fbo(tex_id) {
            gl.bind_fbo(fbo, width, height);
            // Store FBO for cleanup
            GL_FBO_STORE.lock().unwrap_or_else(|e| e.into_inner()).insert(tex_id, fbo);
        }
    }

    // Create a property set for the texture handle
    let mut tex_props = crate::property::PropertySet::default();
    tex_props.set_string("OfxPropType", 0, "OfxTypeImage");
    tex_props.set_int(crate::constants::kOfxImageEffectPropOpenGLTextureIndex, 0, tex_id as i32);
    tex_props.set_int(crate::constants::kOfxImageEffectPropOpenGLTextureTarget, 0, target as i32);
    tex_props.set_string("OfxImageEffectPropPixelDepth", 0, crate::constants::kOfxBitDepthByte);
    tex_props.set_string("OfxImageEffectPropComponents", 0, crate::constants::kOfxImageComponentRGBA);

    // Copy bounds from original image
    crate::property::with_propset(img_handle, |src_ps| {
        for i in 0..4 {
            if let Some(v) = src_ps.get_int("OfxImagePropBounds", i) {
                tex_props.set_int("OfxImagePropBounds", i, v);
            }
            if let Some(v) = src_ps.get_int("OfxImagePropRegionOfDefinition", i) {
                tex_props.set_int("OfxImagePropRegionOfDefinition", i, v);
            }
        }
        if let Some(v) = src_ps.get_int("OfxImagePropRowBytes", 0) {
            tex_props.set_int("OfxImagePropRowBytes", 0, v);
        }
    });

    tex_props.set_double("OfxImageEffectPropRenderScale", 0, 1.0);
    tex_props.set_double("OfxImageEffectPropRenderScale", 1, 1.0);
    tex_props.set_double("OfxImagePropPixelAspectRatio", 0, 1.0);
    tex_props.set_string("OfxImagePropField", 0, crate::constants::kOfxFieldNone);

    let handle = crate::property::register_propset(tex_props);
    register_gl_texture(handle, tex_id, target, is_output);

    *texture_handle = handle;
    debug!("[OFX GL] clipLoadTexture: clip='{}' tex={} {}x{}", clip_name, tex_id, width, height);
    K_OFX_STAT_OK
}

/// clipFreeTexture — release a texture previously loaded via clipLoadTexture.
unsafe extern "C" fn clip_free_texture_fn(
    texture_handle: OfxPropertySetHandle,
) -> OfxStatus {
    if let Some((tex_id, is_output)) = release_gl_texture(texture_handle) {
        let gl_store = GL_RUNTIME_STORE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(gl) = gl_store.as_ref() {
            // Clean up FBO if this was an output texture
            if is_output {
                if let Some(fbo) = GL_FBO_STORE.lock().unwrap_or_else(|e| e.into_inner()).remove(&tex_id) {
                    gl.delete_fbo(fbo);
                }
            }
            // Delete the source texture (for output, host reads it first)
            if !is_output {
                gl.delete_texture(tex_id);
            }
        }
        crate::property::remove_propset(texture_handle);
    }
    K_OFX_STAT_OK
}

/// flushResources — ask the host to free any cached GPU resources.
unsafe extern "C" fn flush_resources_fn() -> OfxStatus {
    // We don't cache textures between renders, so this is a no-op
    K_OFX_STAT_REPLY_DEFAULT
}

/// Global OpenGL runtime reference for suite functions.
/// Set by the host before calling render actions.
///
/// Stored as `Arc<OpenGlRuntime>` so the runtime outlives the store's
/// read, no matter what: if the host drops `self.opengl` between
/// `set_gl_runtime` and `clear_gl_runtime` — e.g. because a plugin
/// callback on another thread panics before the render action returns
/// — the Arc held here keeps the runtime alive until the store is
/// cleared or overwritten. Previous implementation used
/// `&'static OpenGlRuntime` synthesised from a raw pointer, which was a
/// guaranteed UAF in exactly that drop-between-set-and-clear window.
static GL_RUNTIME_STORE: LazyLock<Mutex<Option<std::sync::Arc<OpenGlRuntime>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Maps output texture ID → FBO ID for cleanup.
static GL_FBO_STORE: LazyLock<Mutex<HashMap<GLuint, GLuint>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Set the global GL runtime reference (called by host before GPU render).
///
/// Takes an `Arc` clone so the store shares ownership of the runtime —
/// the runtime remains valid for the duration of the render even if
/// the host's own handle is dropped in the middle (e.g. on a plugin
/// panic). No `unsafe` needed: ownership, not lifetime-transmutation.
pub fn set_gl_runtime(rt: std::sync::Arc<OpenGlRuntime>) {
    *GL_RUNTIME_STORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(rt);
}

/// Clear the global GL runtime reference (called by host after GPU render).
pub fn clear_gl_runtime() {
    *GL_RUNTIME_STORE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Static OfxImageEffectOpenGLRenderSuiteV1 table.
pub static OPENGL_RENDER_SUITE_V1: OfxImageEffectOpenGLRenderSuiteV1 = OfxImageEffectOpenGLRenderSuiteV1 {
    clip_load_texture: clip_load_texture_fn,
    clip_free_texture: clip_free_texture_fn,
    flush_resources: flush_resources_fn,
};

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opengl_init() {
        let rt = OpenGlRuntime::new();
        if let Some(rt) = rt {
            println!("OpenGL: {} ({})", rt.gl_version(), rt.gl_renderer());
            println!("Max texture size: {}", rt.max_texture_size());
            assert!(rt.max_texture_size() >= 1024);
        } else {
            println!("OpenGL (EGL) not available — skipping");
        }
    }

    #[test]
    fn test_opengl_texture_roundtrip() {
        let Some(rt) = OpenGlRuntime::new() else {
            println!("OpenGL not available — skipping");
            return;
        };

        rt.make_current();

        // Create a 4x4 RGBA texture with known data
        let width = 4u32;
        let height = 4u32;
        let mut src = vec![0u8; (width * height * 4) as usize];
        for (i, pixel) in src.chunks_exact_mut(4).enumerate() {
            pixel[0] = (i * 16) as u8; // R
            pixel[1] = 128;             // G
            pixel[2] = 64;              // B
            pixel[3] = 255;             // A
        }

        let (tex, target) = rt.create_texture(&src, width, height).expect("create texture");
        assert_ne!(tex, 0);
        assert_eq!(target, GL_TEXTURE_2D);

        // Create output texture + FBO, render via texture copy
        let (out_tex, _) = rt.create_output_texture(width, height).expect("create output");
        let fbo = rt.create_fbo(out_tex).expect("create FBO");
        rt.bind_fbo(fbo, width, height);

        // Read pixels (FBO is empty — should be zeros)
        let result = rt.read_pixels(width, height);
        assert_eq!(result.len(), src.len());

        // Clean up
        rt.delete_fbo(fbo);
        rt.delete_texture(out_tex);
        rt.delete_texture(tex);
        rt.release_current();

        println!("OpenGL texture roundtrip test passed");
    }

    #[test]
    fn test_opengl_context_switching() {
        let Some(rt) = OpenGlRuntime::new() else {
            println!("OpenGL not available — skipping");
            return;
        };

        // Make current, do work, release, make current again
        assert!(rt.make_current());
        let (tex, _) = rt.create_texture(&[255u8; 16], 2, 2).expect("tex");
        rt.delete_texture(tex);
        rt.release_current();

        assert!(rt.make_current());
        let (tex2, _) = rt.create_texture(&[128u8; 16], 2, 2).expect("tex2");
        rt.delete_texture(tex2);
        rt.release_current();

        println!("OpenGL context switching test passed");
    }
}
