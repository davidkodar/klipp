//! CUDA Driver API — runtime-loaded GPU memory management for OFX plugins.
//!
//! Loads libcuda.so at runtime via libloading. No CUDA toolkit required —
//! only the NVIDIA driver. Provides context creation, device memory
//! allocation, stream management, and host↔device transfers for OFX
//! GPU rendering.

use std::ffi::{c_int, c_uint, c_void};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ── CUDA Driver API types ───────────────────────────────────────────

type CUresult = c_int;
type CUdevice = c_int;
type CUcontext = *mut c_void;
type CUstream = *mut c_void;
type CUdeviceptr = u64; // 64-bit device pointer

const CUDA_SUCCESS: CUresult = 0;

// ── Function signatures ─────────────────────────────────────────────

type CuInitFn = unsafe extern "C" fn(flags: c_uint) -> CUresult;
type CuDeviceGetCountFn = unsafe extern "C" fn(count: *mut c_int) -> CUresult;
type CuDeviceGetFn = unsafe extern "C" fn(device: *mut CUdevice, ordinal: c_int) -> CUresult;
type CuDeviceGetNameFn = unsafe extern "C" fn(name: *mut u8, len: c_int, dev: CUdevice) -> CUresult;
type CuCtxCreateFn = unsafe extern "C" fn(ctx: *mut CUcontext, flags: c_uint, dev: CUdevice) -> CUresult;
type CuCtxDestroyFn = unsafe extern "C" fn(ctx: CUcontext) -> CUresult;
type CuCtxPushCurrentFn = unsafe extern "C" fn(ctx: CUcontext) -> CUresult;
type CuCtxPopCurrentFn = unsafe extern "C" fn(ctx: *mut CUcontext) -> CUresult;
type CuMemAllocFn = unsafe extern "C" fn(dptr: *mut CUdeviceptr, bytesize: usize) -> CUresult;
type CuMemFreeFn = unsafe extern "C" fn(dptr: CUdeviceptr) -> CUresult;
type CuMemcpyHtoDFn = unsafe extern "C" fn(dst: CUdeviceptr, src: *const c_void, bytecount: usize) -> CUresult;
type CuMemcpyDtoHFn = unsafe extern "C" fn(dst: *mut c_void, src: CUdeviceptr, bytecount: usize) -> CUresult;
type CuStreamCreateFn = unsafe extern "C" fn(stream: *mut CUstream, flags: c_uint) -> CUresult;
type CuStreamDestroyFn = unsafe extern "C" fn(stream: CUstream) -> CUresult;
type CuStreamSynchronizeFn = unsafe extern "C" fn(stream: CUstream) -> CUresult;
type CuMemcpyHtoDAsyncFn = unsafe extern "C" fn(dst: CUdeviceptr, src: *const c_void, bytecount: usize, stream: CUstream) -> CUresult;
type CuMemcpyDtoHAsyncFn = unsafe extern "C" fn(dst: *mut c_void, src: CUdeviceptr, bytecount: usize, stream: CUstream) -> CUresult;

// ── CUDA API wrapper ────────────────────────────────────────────────

/// Dynamically loaded CUDA driver API function table.
struct CudaApi {
    _lib: Arc<libloading::Library>,
    cu_init: CuInitFn,
    cu_device_get_count: CuDeviceGetCountFn,
    cu_device_get: CuDeviceGetFn,
    cu_device_get_name: CuDeviceGetNameFn,
    cu_ctx_create: CuCtxCreateFn,
    cu_ctx_destroy: CuCtxDestroyFn,
    cu_ctx_push_current: CuCtxPushCurrentFn,
    cu_ctx_pop_current: CuCtxPopCurrentFn,
    cu_mem_alloc: CuMemAllocFn,
    cu_mem_free: CuMemFreeFn,
    cu_memcpy_htod: CuMemcpyHtoDFn,
    cu_memcpy_dtoh: CuMemcpyDtoHFn,
    cu_stream_create: CuStreamCreateFn,
    cu_stream_destroy: CuStreamDestroyFn,
    cu_stream_synchronize: CuStreamSynchronizeFn,
    cu_memcpy_htod_async: CuMemcpyHtoDAsyncFn,
    cu_memcpy_dtoh_async: CuMemcpyDtoHAsyncFn,
}

// Safety: CUDA driver API is thread-safe when used with proper context management.
unsafe impl Send for CudaApi {}
unsafe impl Sync for CudaApi {}

impl CudaApi {
    /// Load libcuda.so and resolve all required symbols.
    fn load() -> Option<Self> {
        let lib = unsafe {
            libloading::Library::new("libcuda.so.1")
                .or_else(|_| libloading::Library::new("libcuda.so"))
                .ok()?
        };
        let lib = Arc::new(lib);

        macro_rules! load_sym {
            ($name:expr) => {{
                let sym: libloading::Symbol<*const c_void> = unsafe { lib.get($name) }.ok()?;
                unsafe { std::mem::transmute(*sym) }
            }};
        }

        Some(Self {
            cu_init: load_sym!(b"cuInit\0"),
            cu_device_get_count: load_sym!(b"cuDeviceGetCount\0"),
            cu_device_get: load_sym!(b"cuDeviceGet\0"),
            cu_device_get_name: load_sym!(b"cuDeviceGetName\0"),
            cu_ctx_create: load_sym!(b"cuCtxCreate_v2\0"),
            cu_ctx_destroy: load_sym!(b"cuCtxDestroy_v2\0"),
            cu_ctx_push_current: load_sym!(b"cuCtxPushCurrent_v2\0"),
            cu_ctx_pop_current: load_sym!(b"cuCtxPopCurrent_v2\0"),
            cu_mem_alloc: load_sym!(b"cuMemAlloc_v2\0"),
            cu_mem_free: load_sym!(b"cuMemFree_v2\0"),
            cu_memcpy_htod: load_sym!(b"cuMemcpyHtoD_v2\0"),
            cu_memcpy_dtoh: load_sym!(b"cuMemcpyDtoH_v2\0"),
            cu_stream_create: load_sym!(b"cuStreamCreate\0"),
            cu_stream_destroy: load_sym!(b"cuStreamDestroy_v2\0"),
            cu_stream_synchronize: load_sym!(b"cuStreamSynchronize\0"),
            cu_memcpy_htod_async: load_sym!(b"cuMemcpyHtoDAsync_v2\0"),
            cu_memcpy_dtoh_async: load_sym!(b"cuMemcpyDtoHAsync_v2\0"),
            _lib: lib,
        })
    }
}

// ── Public CUDA runtime ─────────────────────────────────────────────

/// CUDA GPU runtime for OFX plugin rendering.
/// Manages a CUDA context, streams, and device memory for passing
/// GPU buffers to OFX plugins that support CUDA rendering.
pub struct CudaRuntime {
    api: CudaApi,
    context: CUcontext,
    stream: CUstream,
    device_name: String,
    device_ordinal: i32,
}

// Safety: The CUDA context is pushed/popped per-call. The runtime is
// used single-threaded within the OFX host's render path.
unsafe impl Send for CudaRuntime {}

impl CudaRuntime {
    /// Initialize CUDA: load driver, create context on device 0, create a stream.
    /// Returns None if CUDA is not available (no NVIDIA GPU / driver).
    pub fn new() -> Option<Self> {
        let api = CudaApi::load()?;

        // Initialize the driver
        let r = unsafe { (api.cu_init)(0) };
        if r != CUDA_SUCCESS {
            warn!("cuInit failed with error {r}");
            return None;
        }

        // Check device count
        let mut count: c_int = 0;
        let r = unsafe { (api.cu_device_get_count)(&mut count) };
        if r != CUDA_SUCCESS || count == 0 {
            warn!("No CUDA devices found (count={count}, err={r})");
            return None;
        }

        // Get device 0
        let mut device: CUdevice = 0;
        let r = unsafe { (api.cu_device_get)(&mut device, 0) };
        if r != CUDA_SUCCESS {
            warn!("cuDeviceGet failed: {r}");
            return None;
        }

        // Get device name
        let mut name_buf = [0u8; 256];
        let r = unsafe { (api.cu_device_get_name)(name_buf.as_mut_ptr(), 256, device) };
        let device_name = if r == CUDA_SUCCESS {
            let len = name_buf.iter().position(|&b| b == 0).unwrap_or(256);
            String::from_utf8_lossy(&name_buf[..len]).to_string()
        } else {
            "Unknown".to_string()
        };

        // Create context (flags=0 = default scheduling)
        let mut context: CUcontext = std::ptr::null_mut();
        let r = unsafe { (api.cu_ctx_create)(&mut context, 0, device) };
        if r != CUDA_SUCCESS {
            warn!("cuCtxCreate failed: {r}");
            return None;
        }

        // Pop context so it's not current on this thread by default
        let mut _popped: CUcontext = std::ptr::null_mut();
        unsafe { (api.cu_ctx_pop_current)(&mut _popped) };

        // Create a stream
        let mut stream: CUstream = std::ptr::null_mut();
        unsafe { (api.cu_ctx_push_current)(context) };
        let r = unsafe { (api.cu_stream_create)(&mut stream, 0) }; // 0 = default stream flags
        unsafe { (api.cu_ctx_pop_current)(&mut _popped) };
        if r != CUDA_SUCCESS {
            warn!("cuStreamCreate failed: {r}");
            unsafe { (api.cu_ctx_destroy)(context) };
            return None;
        }

        info!("CUDA: initialized on '{}' (device {})", device_name, device);

        Some(Self {
            api,
            context,
            stream,
            device_name,
            device_ordinal: 0,
        })
    }

    /// GPU device name (e.g. "NVIDIA GeForce RTX 4080").
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Device ordinal.
    pub fn device_ordinal(&self) -> i32 {
        self.device_ordinal
    }

    /// Get the CUDA stream pointer (for passing to OFX plugins via kOfxImageEffectPropCudaStream).
    pub fn stream_ptr(&self) -> *mut c_void {
        self.stream
    }

    /// Push the CUDA context onto the current thread. Must be called before any CUDA operations.
    pub fn push_context(&self) -> bool {
        let r = unsafe { (self.api.cu_ctx_push_current)(self.context) };
        r == CUDA_SUCCESS
    }

    /// Pop the CUDA context from the current thread. Call after CUDA operations complete.
    pub fn pop_context(&self) {
        let mut _ctx: CUcontext = std::ptr::null_mut();
        unsafe { (self.api.cu_ctx_pop_current)(&mut _ctx) };
    }

    /// Allocate device memory. Returns a device pointer or None on failure.
    pub fn alloc(&self, bytes: usize) -> Option<CUdeviceptr> {
        let mut dptr: CUdeviceptr = 0;
        let r = unsafe { (self.api.cu_mem_alloc)(&mut dptr, bytes) };
        if r != CUDA_SUCCESS {
            warn!("cuMemAlloc({bytes}) failed: {r}");
            return None;
        }
        debug!("CUDA: allocated {bytes} bytes at 0x{:x}", dptr);
        Some(dptr)
    }

    /// Free device memory.
    pub fn free(&self, dptr: CUdeviceptr) {
        let r = unsafe { (self.api.cu_mem_free)(dptr) };
        if r != CUDA_SUCCESS {
            warn!("cuMemFree(0x{:x}) failed: {r}", dptr);
        }
    }

    /// Copy data from host (CPU) to device (GPU). Synchronous.
    pub fn copy_htod(&self, dst: CUdeviceptr, src: &[u8]) -> bool {
        let r = unsafe { (self.api.cu_memcpy_htod)(dst, src.as_ptr() as *const c_void, src.len()) };
        if r != CUDA_SUCCESS {
            warn!("cuMemcpyHtoD failed: {r}");
        }
        r == CUDA_SUCCESS
    }

    /// Copy data from device (GPU) to host (CPU). Synchronous.
    pub fn copy_dtoh(&self, dst: &mut [u8], src: CUdeviceptr) -> bool {
        let r = unsafe { (self.api.cu_memcpy_dtoh)(dst.as_mut_ptr() as *mut c_void, src, dst.len()) };
        if r != CUDA_SUCCESS {
            warn!("cuMemcpyDtoH failed: {r}");
        }
        r == CUDA_SUCCESS
    }

    /// Copy host→device on the stream (async). Call sync_stream() to wait.
    pub fn copy_htod_async(&self, dst: CUdeviceptr, src: &[u8]) -> bool {
        let r = unsafe {
            (self.api.cu_memcpy_htod_async)(dst, src.as_ptr() as *const c_void, src.len(), self.stream)
        };
        r == CUDA_SUCCESS
    }

    /// Copy device→host on the stream (async). Call sync_stream() to wait.
    pub fn copy_dtoh_async(&self, dst: &mut [u8], src: CUdeviceptr) -> bool {
        let r = unsafe {
            (self.api.cu_memcpy_dtoh_async)(dst.as_mut_ptr() as *mut c_void, src, dst.len(), self.stream)
        };
        r == CUDA_SUCCESS
    }

    /// Synchronize the stream — blocks until all enqueued operations complete.
    pub fn sync_stream(&self) -> bool {
        let r = unsafe { (self.api.cu_stream_synchronize)(self.stream) };
        if r != CUDA_SUCCESS {
            warn!("cuStreamSynchronize failed: {r}");
        }
        r == CUDA_SUCCESS
    }
}

impl Drop for CudaRuntime {
    fn drop(&mut self) {
        // Push context to make it current for cleanup
        unsafe { (self.api.cu_ctx_push_current)(self.context) };
        unsafe { (self.api.cu_stream_destroy)(self.stream) };
        let mut _ctx: CUcontext = std::ptr::null_mut();
        unsafe { (self.api.cu_ctx_pop_current)(&mut _ctx) };
        unsafe { (self.api.cu_ctx_destroy)(self.context) };
        debug!("CUDA: runtime destroyed");
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cuda_init_and_device_name() {
        let rt = CudaRuntime::new();
        if let Some(rt) = rt {
            println!("CUDA device: {}", rt.device_name());
            assert!(!rt.device_name().is_empty());
        } else {
            println!("CUDA not available — skipping GPU test");
        }
    }

    #[test]
    fn test_cuda_alloc_free_roundtrip() {
        let Some(rt) = CudaRuntime::new() else {
            println!("CUDA not available — skipping");
            return;
        };

        rt.push_context();

        // Allocate 1KB
        let dptr = rt.alloc(1024).expect("alloc should succeed");
        assert_ne!(dptr, 0);

        // Write pattern to device
        let src: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        assert!(rt.copy_htod(dptr, &src));

        // Read back
        let mut dst = vec![0u8; 1024];
        assert!(rt.copy_dtoh(&mut dst, dptr));

        // Verify round-trip integrity
        assert_eq!(src, dst, "CUDA host→device→host round-trip data mismatch");

        rt.free(dptr);
        rt.pop_context();
        println!("CUDA round-trip test passed");
    }

    #[test]
    fn test_cuda_stream_async_transfer() {
        let Some(rt) = CudaRuntime::new() else {
            println!("CUDA not available — skipping");
            return;
        };

        rt.push_context();

        // Simulate an OFX frame: 64x64 RGBA = 16KB
        let frame_size = 64 * 64 * 4;
        let dptr = rt.alloc(frame_size).expect("alloc");

        // Pattern: white pixels with full alpha
        let src = vec![255u8; frame_size];
        assert!(rt.copy_htod_async(dptr, &src));
        assert!(rt.sync_stream());

        let mut dst = vec![0u8; frame_size];
        assert!(rt.copy_dtoh_async(&mut dst, dptr));
        assert!(rt.sync_stream());

        assert_eq!(src, dst, "async stream transfer data mismatch");

        rt.free(dptr);
        rt.pop_context();
        println!("CUDA async stream test passed");
    }

    #[test]
    fn test_cuda_ofx_frame_simulation() {
        let Some(rt) = CudaRuntime::new() else {
            println!("CUDA not available — skipping");
            return;
        };

        rt.push_context();

        // Simulate the OFX GPU render flow:
        // 1. Host allocates input + output on device
        // 2. Host uploads CPU frame to device input
        // 3. (Plugin would launch kernel here — we copy input→output as identity)
        // 4. Host downloads device output to CPU
        let width = 128u32;
        let height = 128u32;
        let frame_bytes = (width * height * 4) as usize;

        let input_dptr = rt.alloc(frame_bytes).expect("input alloc");
        let output_dptr = rt.alloc(frame_bytes).expect("output alloc");

        // Create a test frame: gradient pattern
        let mut src_frame = vec![0u8; frame_bytes];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                src_frame[idx] = (x & 0xFF) as u8;     // R
                src_frame[idx + 1] = (y & 0xFF) as u8;  // G
                src_frame[idx + 2] = 128;                // B
                src_frame[idx + 3] = 255;                // A
            }
        }

        // Upload input
        assert!(rt.copy_htod(input_dptr, &src_frame));

        // Simulate plugin: copy input → output (identity transform)
        // In real OFX, the plugin would launch a CUDA kernel here
        let mut temp = vec![0u8; frame_bytes];
        assert!(rt.copy_dtoh(&mut temp, input_dptr));
        assert!(rt.copy_htod(output_dptr, &temp));

        // Download output
        let mut result_frame = vec![0u8; frame_bytes];
        assert!(rt.copy_dtoh(&mut result_frame, output_dptr));

        // Verify
        assert_eq!(src_frame, result_frame, "OFX GPU frame simulation failed");

        // Verify the device pointer is a valid GPU address (non-null, high bits set for GPU memory)
        assert!(input_dptr > 0x1000, "device pointer should be in GPU address space");
        println!(
            "OFX GPU frame simulation passed: {}x{} frame, input_dptr=0x{:x}, output_dptr=0x{:x}",
            width, height, input_dptr, output_dptr
        );

        rt.free(input_dptr);
        rt.free(output_dptr);
        rt.pop_context();
    }
}
