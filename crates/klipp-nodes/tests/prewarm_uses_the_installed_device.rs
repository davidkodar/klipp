//! A GPU pipeline built ahead (#2108) is built on the device the UI
//! installs, even when the build starts before the install: it waits for
//! the device to be decided rather than making one of its own, which left
//! the UI's install to fail and the engine and the viewer on two devices.
//! A binary of its own: the shared device is the process's, set once.

use klipp_effects::gpu_context::GpuContext;
use klipp_nodes::GraphExecutor;

#[test]
fn a_pipeline_built_ahead_uses_the_device_installed_after_it_started() {
    let Some(ctx) = GpuContext::new() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("unverified: no GPU");
        return;
    };
    let installed = ctx.device();
    let mut executor = GraphExecutor::new();
    // The build starts first, as the app starts the engine before the UI
    // has its device.
    executor.prewarm_gpu();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(GpuContext::install_shared(ctx).is_ok(), "the build ahead made a device of its own first");
    assert!(GpuContext::shared_is_ui_device());
    let pipeline = executor.gpu_pipeline_or_init().expect("a pipeline");
    assert!(std::sync::Arc::ptr_eq(&pipeline.device(), &installed), "built on the installed device");
}
