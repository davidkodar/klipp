//! OFX string constants — match the C `#define` values exactly.
//!
//! Using constants prevents typos in string literals (e.g. "OfxImageFieldNone"
//! vs the correct "OfxFieldNone") which cause silent plugin failures.

#![allow(non_upper_case_globals)]

// Field order
pub const kOfxFieldNone: &str = "OfxFieldNone";
pub const kOfxFieldBoth: &str = "OfxFieldBoth";
pub const kOfxFieldLower: &str = "OfxFieldLower";
pub const kOfxFieldUpper: &str = "OfxFieldUpper";

// Pixel depth
pub const kOfxBitDepthByte: &str = "OfxBitDepthByte";
pub const kOfxBitDepthShort: &str = "OfxBitDepthShort";
pub const kOfxBitDepthFloat: &str = "OfxBitDepthFloat";

// Pixel components
pub const kOfxImageComponentRGBA: &str = "OfxImageComponentRGBA";
pub const kOfxImageComponentRGB: &str = "OfxImageComponentRGB";
pub const kOfxImageComponentAlpha: &str = "OfxImageComponentAlpha";

// Premultiplication (standard OFX)
pub const kOfxImageUnPreMultiplied: &str = "OfxImageUnPreMultiplied";
pub const kOfxImagePreMultiplied: &str = "OfxImagePreMultiplied";

// Premultiplication (Natron variant — used by openfx-misc/CImg from Natron builds)
pub const kOfxImageAlphaUnPremultiplied: &str = "OfxImageAlphaUnPremultiplied";
pub const kOfxImageAlphaPremultiplied: &str = "OfxImageAlphaPremultiplied";
pub const kOfxImageOpaque: &str = "OfxImageOpaque";

// Context
pub const kOfxImageEffectContextFilter: &str = "OfxImageEffectContextFilter";
pub const kOfxImageEffectContextTransition: &str = "OfxImageEffectContextTransition";
pub const kOfxImageEffectContextGenerator: &str = "OfxImageEffectContextGenerator";
pub const kOfxImageEffectContextGeneral: &str = "OfxImageEffectContextGeneral";

// Thread safety
pub const kOfxImageEffectRenderFullySafe: &str = "OfxImageEffectRenderFullySafe";
pub const kOfxImageEffectRenderInstanceSafe: &str = "OfxImageEffectRenderInstanceSafe";
pub const kOfxImageEffectRenderUnsafe: &str = "OfxImageEffectRenderUnsafe";

// Pixel depth (GPU-specific)
pub const kOfxBitDepthHalf: &str = "OfxBitDepthHalf";

// ── GPU Rendering (OFX 1.3 OpenGL + OFX 1.5 CUDA/Metal/OpenCL) ─────

// GPU capability properties (string x 1: "false", "true", or "needed" for GL)
// Set on both host descriptor and plugin descriptor.
pub const kOfxImageEffectPropOpenGLRenderSupported: &str = "OfxImageEffectPropOpenGLRenderSupported";
pub const kOfxImageEffectPropCudaRenderSupported: &str = "OfxImageEffectPropCudaRenderSupported";
pub const kOfxImageEffectPropCudaStreamSupported: &str = "OfxImageEffectPropCudaStreamSupported";
pub const kOfxImageEffectPropMetalRenderSupported: &str = "OfxImageEffectPropMetalRenderSupported";
pub const kOfxImageEffectPropOpenCLRenderSupported: &str = "OfxImageEffectPropOpenCLRenderSupported";
pub const kOfxImageEffectPropOpenCLSupported: &str = "OfxImageEffectPropOpenCLSupported";
pub const kOfxImageEffectPropCPURenderSupported: &str = "OfxImageEffectPropCPURenderSupported";

// GPU enable flags (int x 1: 0 or 1) — set on render action inArgs
pub const kOfxImageEffectPropOpenGLEnabled: &str = "OfxImageEffectPropOpenGLEnabled";
pub const kOfxImageEffectPropCudaEnabled: &str = "OfxImageEffectPropCudaEnabled";
pub const kOfxImageEffectPropMetalEnabled: &str = "OfxImageEffectPropMetalEnabled";
pub const kOfxImageEffectPropOpenCLEnabled: &str = "OfxImageEffectPropOpenCLEnabled";

// GPU context/queue properties (pointer x 1) — set on render action inArgs
pub const kOfxImageEffectPropCudaStream: &str = "OfxImageEffectPropCudaStream";
pub const kOfxImageEffectPropMetalCommandQueue: &str = "OfxImageEffectPropMetalCommandQueue";
pub const kOfxImageEffectPropOpenCLCommandQueue: &str = "OfxImageEffectPropOpenCLCommandQueue";

// OpenGL texture properties (on texture handle from clipLoadTexture)
pub const kOfxImageEffectPropOpenGLTextureIndex: &str = "OfxImageEffectPropOpenGLTextureIndex";
pub const kOfxImageEffectPropOpenGLTextureTarget: &str = "OfxImageEffectPropOpenGLTextureTarget";
pub const kOfxOpenGLPropPixelDepth: &str = "OfxOpenGLPropPixelDepth";

// OpenCL image property (pointer x 1, cast to cl_mem)
pub const kOfxImageEffectPropOpenCLImage: &str = "OfxImageEffectPropOpenCLImage";

// OpenGL render suite name
pub const kOfxOpenGLRenderSuite: &str = "OfxImageEffectOpenGLRenderSuite";

// OpenGL context lifecycle actions
pub const kOfxActionOpenGLContextAttached: &str = "OfxActionOpenGLContextAttached";
pub const kOfxActionOpenGLContextDetached: &str = "kOfxActionOpenGLContextDetached";

// GPU render status codes
pub const K_OFX_STAT_GPU_OUT_OF_MEMORY: i32 = 1001;
pub const K_OFX_STAT_GPU_RENDER_FAILED: i32 = 1002;
