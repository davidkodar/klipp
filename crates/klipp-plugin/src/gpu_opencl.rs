//! OpenCL GPU runtime — runtime-loaded for OFX plugin rendering on AMD/Intel/NVIDIA.
//!
//! Loads libOpenCL.so at runtime via libloading. Supports AMD, Intel, and NVIDIA
//! GPUs through the OpenCL ICD (Installable Client Driver) mechanism.
//! Provides context creation, command queue management, cl_mem buffer allocation,
//! and host↔device transfers for OFX GPU rendering.

use std::ffi::c_void;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ── OpenCL types ────────────────────────────────────────────────────

type ClInt = i32;
type ClUint = u32;
type ClUlong = u64;
type ClPlatformId = *mut c_void;
type ClDeviceId = *mut c_void;
type ClContext = *mut c_void;
type ClCommandQueue = *mut c_void;
type ClMem = *mut c_void;

const CL_SUCCESS: ClInt = 0;

// Platform info
const CL_PLATFORM_NAME: ClUint = 0x0902;

// Device types
const CL_DEVICE_TYPE_GPU: ClUlong = 1 << 2;

// Device info
const CL_DEVICE_NAME: ClUint = 0x102B;
const CL_DEVICE_VENDOR: ClUint = 0x102C;

// Memory flags
const CL_MEM_READ_WRITE: ClUlong = 1 << 0;
const CL_MEM_COPY_HOST_PTR: ClUlong = 1 << 5;

// Command queue properties
const CL_QUEUE_PROFILING_ENABLE: ClUlong = 1 << 1;

// ── Function signatures ─────────────────────────────────────────────

type ClGetPlatformIdsFn = unsafe extern "C" fn(num_entries: ClUint, platforms: *mut ClPlatformId, num_platforms: *mut ClUint) -> ClInt;
type ClGetPlatformInfoFn = unsafe extern "C" fn(platform: ClPlatformId, param: ClUint, size: usize, value: *mut c_void, size_ret: *mut usize) -> ClInt;
type ClGetDeviceIdsFn = unsafe extern "C" fn(platform: ClPlatformId, device_type: ClUlong, num_entries: ClUint, devices: *mut ClDeviceId, num_devices: *mut ClUint) -> ClInt;
type ClGetDeviceInfoFn = unsafe extern "C" fn(device: ClDeviceId, param: ClUint, size: usize, value: *mut c_void, size_ret: *mut usize) -> ClInt;
type ClCreateContextFn = unsafe extern "C" fn(props: *const isize, num_devices: ClUint, devices: *const ClDeviceId, pfn_notify: *const c_void, user_data: *mut c_void, errcode: *mut ClInt) -> ClContext;
type ClReleaseContextFn = unsafe extern "C" fn(context: ClContext) -> ClInt;
type ClCreateCommandQueueWithPropertiesFn = unsafe extern "C" fn(context: ClContext, device: ClDeviceId, properties: *const ClUlong, errcode: *mut ClInt) -> ClCommandQueue;
// Fallback for OpenCL 1.x (AMD older drivers)
type ClCreateCommandQueueFn = unsafe extern "C" fn(context: ClContext, device: ClDeviceId, properties: ClUlong, errcode: *mut ClInt) -> ClCommandQueue;
type ClReleaseCommandQueueFn = unsafe extern "C" fn(queue: ClCommandQueue) -> ClInt;
type ClCreateBufferFn = unsafe extern "C" fn(context: ClContext, flags: ClUlong, size: usize, host_ptr: *mut c_void, errcode: *mut ClInt) -> ClMem;
type ClReleaseMemObjectFn = unsafe extern "C" fn(memobj: ClMem) -> ClInt;
type ClEnqueueWriteBufferFn = unsafe extern "C" fn(queue: ClCommandQueue, buffer: ClMem, blocking: ClUint, offset: usize, size: usize, ptr: *const c_void, num_events: ClUint, events: *const *mut c_void, event: *mut *mut c_void) -> ClInt;
type ClEnqueueReadBufferFn = unsafe extern "C" fn(queue: ClCommandQueue, buffer: ClMem, blocking: ClUint, offset: usize, size: usize, ptr: *mut c_void, num_events: ClUint, events: *const *mut c_void, event: *mut *mut c_void) -> ClInt;
type ClFinishFn = unsafe extern "C" fn(queue: ClCommandQueue) -> ClInt;
type ClFlushFn = unsafe extern "C" fn(queue: ClCommandQueue) -> ClInt;

// ── OpenCL API wrapper ──────────────────────────────────────────────

struct OpenClApi {
    _lib: Arc<libloading::Library>,
    cl_get_platform_ids: ClGetPlatformIdsFn,
    cl_get_platform_info: ClGetPlatformInfoFn,
    cl_get_device_ids: ClGetDeviceIdsFn,
    cl_get_device_info: ClGetDeviceInfoFn,
    cl_create_context: ClCreateContextFn,
    cl_release_context: ClReleaseContextFn,
    cl_create_command_queue_with_properties: Option<ClCreateCommandQueueWithPropertiesFn>,
    cl_create_command_queue: Option<ClCreateCommandQueueFn>,
    cl_release_command_queue: ClReleaseCommandQueueFn,
    cl_create_buffer: ClCreateBufferFn,
    cl_release_mem_object: ClReleaseMemObjectFn,
    cl_enqueue_write_buffer: ClEnqueueWriteBufferFn,
    cl_enqueue_read_buffer: ClEnqueueReadBufferFn,
    cl_finish: ClFinishFn,
    cl_flush: ClFlushFn,
}

unsafe impl Send for OpenClApi {}
unsafe impl Sync for OpenClApi {}

impl OpenClApi {
    fn load() -> Option<Self> {
        let lib = unsafe {
            libloading::Library::new("libOpenCL.so.1")
                .or_else(|_| libloading::Library::new("libOpenCL.so"))
                .ok()?
        };
        let lib = Arc::new(lib);

        macro_rules! load_sym {
            ($name:expr) => {{
                let sym: libloading::Symbol<*const c_void> = unsafe { lib.get($name) }.ok()?;
                unsafe { std::mem::transmute(*sym) }
            }};
        }

        macro_rules! load_sym_opt {
            ($name:expr) => {{
                let sym: Option<libloading::Symbol<*const c_void>> = unsafe { lib.get($name) }.ok();
                sym.map(|s| unsafe { std::mem::transmute(*s) })
            }};
        }

        Some(Self {
            cl_get_platform_ids: load_sym!(b"clGetPlatformIDs\0"),
            cl_get_platform_info: load_sym!(b"clGetPlatformInfo\0"),
            cl_get_device_ids: load_sym!(b"clGetDeviceIDs\0"),
            cl_get_device_info: load_sym!(b"clGetDeviceInfo\0"),
            cl_create_context: load_sym!(b"clCreateContext\0"),
            cl_release_context: load_sym!(b"clReleaseContext\0"),
            cl_create_command_queue_with_properties: load_sym_opt!(b"clCreateCommandQueueWithProperties\0"),
            cl_create_command_queue: load_sym_opt!(b"clCreateCommandQueue\0"),
            cl_release_command_queue: load_sym!(b"clReleaseCommandQueue\0"),
            cl_create_buffer: load_sym!(b"clCreateBuffer\0"),
            cl_release_mem_object: load_sym!(b"clReleaseMemObject\0"),
            cl_enqueue_write_buffer: load_sym!(b"clEnqueueWriteBuffer\0"),
            cl_enqueue_read_buffer: load_sym!(b"clEnqueueReadBuffer\0"),
            cl_finish: load_sym!(b"clFinish\0"),
            cl_flush: load_sym!(b"clFlush\0"),
            _lib: lib,
        })
    }
}

// ── Public OpenCL runtime ───────────────────────────────────────────

/// OpenCL GPU runtime for OFX plugin rendering.
/// Manages an OpenCL context, command queue, and cl_mem buffers for
/// passing GPU buffers to OFX plugins that support OpenCL rendering.
pub struct OpenClRuntime {
    api: OpenClApi,
    context: ClContext,
    queue: ClCommandQueue,
    #[allow(dead_code)]
    device: ClDeviceId,
    device_name: String,
    platform_name: String,
}

unsafe impl Send for OpenClRuntime {}

impl OpenClRuntime {
    /// Initialize OpenCL: load library, find a GPU device, create context + queue.
    /// Returns None if OpenCL is not available or no GPU device found.
    pub fn new() -> Option<Self> {
        let api = OpenClApi::load()?;

        // Get platforms
        let mut num_platforms: ClUint = 0;
        let r = unsafe { (api.cl_get_platform_ids)(0, std::ptr::null_mut(), &mut num_platforms) };
        if r != CL_SUCCESS || num_platforms == 0 {
            debug!("OpenCL: no platforms found (err={r})");
            return None;
        }

        let mut platforms = vec![std::ptr::null_mut(); num_platforms as usize];
        unsafe { (api.cl_get_platform_ids)(num_platforms, platforms.as_mut_ptr(), &mut num_platforms) };

        // Find a GPU device on any platform
        let mut chosen_platform: ClPlatformId = std::ptr::null_mut();
        let mut chosen_device: ClDeviceId = std::ptr::null_mut();

        for &platform in &platforms {
            let mut num_devices: ClUint = 0;
            let r = unsafe {
                (api.cl_get_device_ids)(platform, CL_DEVICE_TYPE_GPU, 0, std::ptr::null_mut(), &mut num_devices)
            };
            if r != CL_SUCCESS || num_devices == 0 {
                continue;
            }

            let mut devices = vec![std::ptr::null_mut(); num_devices as usize];
            unsafe {
                (api.cl_get_device_ids)(platform, CL_DEVICE_TYPE_GPU, num_devices, devices.as_mut_ptr(), &mut num_devices);
            }

            if !devices[0].is_null() {
                chosen_platform = platform;
                chosen_device = devices[0];
                break;
            }
        }

        if chosen_device.is_null() {
            debug!("OpenCL: no GPU device found");
            return None;
        }

        // Get platform name
        let platform_name = get_info_string(&api, chosen_platform, CL_PLATFORM_NAME, true);

        // Get device name and vendor
        let device_name = get_info_string(&api, chosen_device, CL_DEVICE_NAME, false);
        let device_vendor = get_info_string(&api, chosen_device, CL_DEVICE_VENDOR, false);

        // Create context
        let mut err: ClInt = 0;
        let context = unsafe {
            (api.cl_create_context)(
                std::ptr::null(),
                1,
                &chosen_device,
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if err != CL_SUCCESS || context.is_null() {
            warn!("OpenCL: context creation failed (err={err})");
            return None;
        }

        // Create command queue (try 2.0 API first, fall back to 1.x)
        let queue = if let Some(create_cq2) = api.cl_create_command_queue_with_properties {
            let props: [ClUlong; 3] = [
                0x1093, // CL_QUEUE_PROPERTIES
                CL_QUEUE_PROFILING_ENABLE,
                0,
            ];
            let q = unsafe { create_cq2(context, chosen_device, props.as_ptr(), &mut err) };
            if err == CL_SUCCESS && !q.is_null() {
                q
            } else if let Some(create_cq1) = api.cl_create_command_queue {
                unsafe { create_cq1(context, chosen_device, 0, &mut err) }
            } else {
                std::ptr::null_mut()
            }
        } else if let Some(create_cq1) = api.cl_create_command_queue {
            unsafe { create_cq1(context, chosen_device, 0, &mut err) }
        } else {
            warn!("OpenCL: no command queue creation function available");
            unsafe { (api.cl_release_context)(context) };
            return None;
        };

        if err != CL_SUCCESS || queue.is_null() {
            warn!("OpenCL: command queue creation failed (err={err})");
            unsafe { (api.cl_release_context)(context) };
            return None;
        }

        info!("OpenCL: initialized on '{}' ({}, {})", device_name, device_vendor, platform_name);

        Some(Self {
            api,
            context,
            queue,
            device: chosen_device,
            device_name,
            platform_name,
        })
    }

    /// GPU device name.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Platform name (e.g. "AMD Accelerated Parallel Processing", "NVIDIA CUDA").
    pub fn platform_name(&self) -> &str {
        &self.platform_name
    }

    /// Get the command queue pointer (for kOfxImageEffectPropOpenCLCommandQueue).
    pub fn queue_ptr(&self) -> *mut c_void {
        self.queue
    }

    /// Allocate a cl_mem buffer. Returns the cl_mem handle or None.
    pub fn alloc(&self, bytes: usize) -> Option<ClMem> {
        let mut err: ClInt = 0;
        let mem = unsafe {
            (self.api.cl_create_buffer)(
                self.context,
                CL_MEM_READ_WRITE,
                bytes,
                std::ptr::null_mut(),
                &mut err,
            )
        };
        if err != CL_SUCCESS || mem.is_null() {
            warn!("OpenCL: clCreateBuffer({bytes}) failed (err={err})");
            return None;
        }
        debug!("OpenCL: allocated {bytes} bytes buffer {:?}", mem);
        Some(mem)
    }

    /// Allocate a cl_mem buffer initialized with host data.
    pub fn alloc_with_data(&self, data: &[u8]) -> Option<ClMem> {
        let mut err: ClInt = 0;
        let mem = unsafe {
            (self.api.cl_create_buffer)(
                self.context,
                CL_MEM_READ_WRITE | CL_MEM_COPY_HOST_PTR,
                data.len(),
                data.as_ptr() as *mut c_void,
                &mut err,
            )
        };
        if err != CL_SUCCESS || mem.is_null() {
            warn!("OpenCL: clCreateBuffer with data ({} bytes) failed (err={err})", data.len());
            return None;
        }
        Some(mem)
    }

    /// Free a cl_mem buffer.
    ///
    /// # Safety
    /// `mem` must be a valid cl_mem handle returned by `alloc` or `alloc_copy`.
    pub unsafe fn free(&self, mem: ClMem) {
        let r = unsafe { (self.api.cl_release_mem_object)(mem) };
        if r != CL_SUCCESS {
            warn!("OpenCL: clReleaseMemObject failed (err={r})");
        }
    }

    /// Write host data to a cl_mem buffer (blocking).
    ///
    /// # Safety
    /// `mem` must be a valid cl_mem handle with sufficient size for `data`.
    pub unsafe fn write_buffer(&self, mem: ClMem, data: &[u8]) -> bool {
        let r = unsafe {
            (self.api.cl_enqueue_write_buffer)(
                self.queue, mem, 1, // blocking
                0, data.len(),
                data.as_ptr() as *const c_void,
                0, std::ptr::null(), std::ptr::null_mut(),
            )
        };
        if r != CL_SUCCESS {
            warn!("OpenCL: clEnqueueWriteBuffer failed (err={r})");
        }
        r == CL_SUCCESS
    }

    /// Read cl_mem buffer to host (blocking).
    ///
    /// # Safety
    /// `mem` must be a valid cl_mem handle with at least `dst.len()` bytes.
    pub unsafe fn read_buffer(&self, mem: ClMem, dst: &mut [u8]) -> bool {
        let r = unsafe {
            (self.api.cl_enqueue_read_buffer)(
                self.queue, mem, 1, // blocking
                0, dst.len(),
                dst.as_mut_ptr() as *mut c_void,
                0, std::ptr::null(), std::ptr::null_mut(),
            )
        };
        if r != CL_SUCCESS {
            warn!("OpenCL: clEnqueueReadBuffer failed (err={r})");
        }
        r == CL_SUCCESS
    }

    /// Flush the command queue (non-blocking submit).
    pub fn flush(&self) -> bool {
        let r = unsafe { (self.api.cl_flush)(self.queue) };
        r == CL_SUCCESS
    }

    /// Finish — block until all enqueued commands complete.
    pub fn finish(&self) -> bool {
        let r = unsafe { (self.api.cl_finish)(self.queue) };
        if r != CL_SUCCESS {
            warn!("OpenCL: clFinish failed (err={r})");
        }
        r == CL_SUCCESS
    }
}

impl Drop for OpenClRuntime {
    fn drop(&mut self) {
        unsafe { (self.api.cl_release_command_queue)(self.queue) };
        unsafe { (self.api.cl_release_context)(self.context) };
        debug!("OpenCL: runtime destroyed");
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn get_info_string(api: &OpenClApi, handle: *mut c_void, param: ClUint, is_platform: bool) -> String {
    let mut size: usize = 0;
    let r = if is_platform {
        unsafe { (api.cl_get_platform_info)(handle, param, 0, std::ptr::null_mut(), &mut size) }
    } else {
        unsafe { (api.cl_get_device_info)(handle, param, 0, std::ptr::null_mut(), &mut size) }
    };
    if r != CL_SUCCESS || size == 0 {
        return "Unknown".to_string();
    }
    let mut buf = vec![0u8; size];
    if is_platform {
        unsafe { (api.cl_get_platform_info)(handle, param, size, buf.as_mut_ptr() as _, &mut size) };
    } else {
        unsafe { (api.cl_get_device_info)(handle, param, size, buf.as_mut_ptr() as _, &mut size) };
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).trim().to_string()
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opencl_init_and_device_name() {
        let rt = OpenClRuntime::new();
        if let Some(rt) = rt {
            println!("OpenCL device: {} (platform: {})", rt.device_name(), rt.platform_name());
            assert!(!rt.device_name().is_empty());
            assert!(!rt.platform_name().is_empty());
        } else {
            println!("OpenCL not available — skipping");
        }
    }

    #[test]
    fn test_opencl_alloc_free_roundtrip() {
        let Some(rt) = OpenClRuntime::new() else {
            println!("OpenCL not available — skipping");
            return;
        };

        // Allocate 1KB
        let mem = rt.alloc(1024).expect("alloc should succeed");
        assert!(!mem.is_null());

        // Write pattern
        let src: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        assert!(unsafe { rt.write_buffer(mem, &src) });

        // Read back
        let mut dst = vec![0u8; 1024];
        assert!(unsafe { rt.read_buffer(mem, &mut dst) });

        // Verify round-trip
        assert_eq!(src, dst, "OpenCL host→device→host round-trip data mismatch");

        unsafe { rt.free(mem); }
        println!("OpenCL round-trip test passed");
    }

    #[test]
    fn test_opencl_alloc_with_data() {
        let Some(rt) = OpenClRuntime::new() else {
            println!("OpenCL not available — skipping");
            return;
        };

        let src: Vec<u8> = (0..512).map(|i| (i % 256) as u8).collect();
        let mem = rt.alloc_with_data(&src).expect("alloc_with_data should succeed");

        let mut dst = vec![0u8; 512];
        assert!(unsafe { rt.read_buffer(mem, &mut dst) });
        assert_eq!(src, dst, "alloc_with_data round-trip mismatch");

        unsafe { rt.free(mem); }
    }

    #[test]
    fn test_opencl_ofx_frame_simulation() {
        let Some(rt) = OpenClRuntime::new() else {
            println!("OpenCL not available — skipping");
            return;
        };

        // Simulate OFX GPU render: allocate input + output, upload, "process", download
        let width = 64u32;
        let height = 64u32;
        let frame_bytes = (width * height * 4) as usize;

        let input_mem = rt.alloc(frame_bytes).expect("input alloc");
        let output_mem = rt.alloc(frame_bytes).expect("output alloc");

        // Create test frame: gradient
        let mut src_frame = vec![0u8; frame_bytes];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                src_frame[idx] = (x & 0xFF) as u8;
                src_frame[idx + 1] = (y & 0xFF) as u8;
                src_frame[idx + 2] = 128;
                src_frame[idx + 3] = 255;
            }
        }

        // Upload
        assert!(unsafe { rt.write_buffer(input_mem, &src_frame) });

        // Simulate plugin: read input, process, write output
        let mut temp = vec![0u8; frame_bytes];
        assert!(unsafe { rt.read_buffer(input_mem, &mut temp) });
        assert!(unsafe { rt.write_buffer(output_mem, &temp) });

        // Download result
        let mut result = vec![0u8; frame_bytes];
        assert!(unsafe { rt.read_buffer(output_mem, &mut result) });

        assert_eq!(src_frame, result, "OpenCL OFX frame simulation failed");
        println!(
            "OpenCL OFX frame simulation passed: {}x{} frame on '{}'",
            width, height, rt.device_name()
        );

        unsafe { rt.free(input_mem); }
        unsafe { rt.free(output_mem); }
    }
}
