//! The shared-device question: with one wgpu device serving both the UI
//! (submitting every frame, never letting the queue drain) and the
//! engine (reading frames back), does an engine readback still return?
//! It did not on wgpu 22 with `device.poll(Wait)`; every readback now
//! spins on `Maintain::Poll` through `poll_until_mapped`. This holds
//! that under a thread that submits without pause.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_context::GpuContext;

#[test]
fn readbacks_return_while_another_thread_submits_without_pause() {
    let Some(ctx) = GpuContext::shared() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let Some(mut gpu) = GpuEffectsPipeline::with_context(ctx) else {
        eprintln!("skipping: no usable GPU pipeline");
        return;
    };
    let stop = Arc::new(AtomicBool::new(false));
    let submits = Arc::new(AtomicU64::new(0));
    let ui = {
        let (device, queue) = (Arc::clone(&ctx.device), Arc::clone(&ctx.queue));
        let (stop, submits) = (Arc::clone(&stop), Arc::clone(&submits));
        std::thread::spawn(move || {
            // A stand-in for egui's render loop: a small copy every
            // submit so the queue always has work in flight.
            let a = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui_a"), size: 1 << 20, usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
            });
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui_b"), size: 1 << 20, usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
            });
            while !stop.load(Ordering::Relaxed) {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ui_frame") });
                enc.copy_buffer_to_buffer(&a, 0, &b, 0, 1 << 20);
                queue.submit(std::iter::once(enc.finish()));
                submits.fetch_add(1, Ordering::Relaxed);
            }
        })
    };

    let (w, h) = (1920u32, 1080u32);
    let frame = vec![0.25f32; (w * h * 4) as usize];
    let fx = [klipp_effects::BuiltinEffect::Gamma { gamma: 1.2 }];
    let start = Instant::now();
    let rounds = 60;
    for _ in 0..rounds {
        let out = gpu.apply_effects(&fx, &frame, w, h);
        assert_eq!(out.len(), frame.len());
        assert!(start.elapsed() < Duration::from_secs(30), "a readback did not return under submit pressure");
    }
    stop.store(true, Ordering::Relaxed);
    ui.join().unwrap();
    let per = start.elapsed().as_secs_f64() * 1e3 / rounds as f64;
    eprintln!("{rounds} readbacks at 1080p under {} UI submits: {per:.1} ms each", submits.load(Ordering::Relaxed));
}
