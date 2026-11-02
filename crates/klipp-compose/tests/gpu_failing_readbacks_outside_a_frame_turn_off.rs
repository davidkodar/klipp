//! Readbacks outside a stack frame have no frame to count by (#3406):
//! nine failures in a row turn the GPU off for the session, and one that
//! works starts the count again.
//!
//! Its own binary: the GPU stays off for the rest of the process.

use klipp_effects::gpu::{GpuEffectsPipeline, FAILED_READBACKS_TO_TURN_OFF};
use klipp_effects::gpu_context::gpu_turned_off;

const PIXELS: usize = 64;

#[test]
fn failed_readbacks_with_no_frame_turn_the_gpu_off() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "GPU required");
        eprintln!("UNVERIFIED: no GPU");
        return;
    };
    gpu.ensure_chain_buffers(PIXELS as u64 * 16);
    let fail = |gpu: &GpuEffectsPipeline, times: u32| {
        for _ in 0..times {
            gpu.fail_next_readback();
            gpu.download_chain_output_linear(true, PIXELS);
        }
    };
    fail(&gpu, FAILED_READBACKS_TO_TURN_OFF - 1);
    gpu.download_chain_output_linear(true, PIXELS);
    fail(&gpu, FAILED_READBACKS_TO_TURN_OFF - 1);
    assert_eq!(gpu_turned_off(), None, "a good readback started the count again");
    assert!(!gpu.is_device_lost());
    fail(&gpu, 1);
    assert!(gpu_turned_off().is_some_and(|why| why.contains("9 readbacks")), "{:?}", gpu_turned_off());
    assert!(gpu.is_device_lost());
}
