//! The recorded effects (colour, geometry, blur, film) go into one encoder (node-graph M2 step
//! 7): a run of them is one submit, whatever its length, and gives the
//! same picture as each effect run on its own. The counts include the
//! one submit `apply_effects`'s download makes.
//!
//! Both sides of the picture check go through the recorder, so it proves
//! the slots and the run's order, not each effect's maths: that is
//! `cpu_gpu_effect_parity`'s, which holds every recorded effect to the CPU.
//!
//! A missing GPU is reported as unverified; with `KLIPP_TEST_REQUIRE_GPU=1`
//! it fails instead.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::BuiltinEffect as E;
const W: u32 = 32;
const H: u32 = 16;
fn colour() -> Vec<E> {
    vec![
        E::Opacity { value: 0.8 },
        E::Invert { mix: 0.3 },
        E::Posterize { levels: 6.0 },
        E::ChannelMixer { red_red: 0.9, red_green: 0.1, red_blue: 0.0, green_red: 0.0, green_green: 1.0, green_blue: 0.0, blue_red: 0.0, blue_green: 0.1, blue_blue: 0.9 },
        E::Gamma { gamma: 1.2 },
        E::HSVAdjust { hue_shift: 10.0, saturation: 1.1, value: 0.95 },
        E::Levels { in_black: 0.05, in_white: 0.95, gamma: 1.1, out_black: 0.0, out_white: 1.0 },
        E::Vibrance { amount: 0.2 },
        E::ColorFade { color: [20, 40, 60, 255], duration_secs: 1.0, mode: Default::default(), progress: 0.5 },
    ]
}
fn geometry() -> Vec<E> {
    vec![
        E::Crop { left: 0.05, right: 0.1, top: 0.0, bottom: 0.08, feather: 1.5 },
        E::Transform {
            position_x: 2.0, position_y: -1.0, scale_x: 1.1, scale_y: 0.95, rotation: 4.0,
            anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 6.0, perspective: 1.2, focal_ref: None,
        },
        E::Mirror { horizontal: 1, vertical: 0 },
        E::LensDistortion { distortion: 0.15, scale: 1.05 },
        E::Letterbox { aspect_ratio: 2.39, bar_color: 0x102030, opacity: 0.9 },
    ]
}

fn blur() -> Vec<E> {
    vec![
        E::GaussianBlur { radius: 2.5 },
        E::Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 2 },
        E::TiltShift { center: 0.5, width: 0.2, softness: 0.3, radius: 3.0 },
        E::FocusBlur { radius: 2.0, vertical: false, band: 0.3 },
        E::Dilate { radius: 1 },
        E::DirectionalBlur { angle: 30.0, distance: 4.0, samples: 8 },
        E::EdgeDetect { strength: 0.6, mix: 0.4 },
        E::Emboss { angle: 45.0, strength: 0.5, mix: 0.3 },
        E::Erode { radius: 1 },
        E::Mosaic { block_size: 3.0 },
        E::RadialBlur { center_x: 0.5, center_y: 0.5, amount: 0.2, samples: 8, mode: 0 },
        E::Sharpen { amount: 0.5 },
        E::MedianFilter { radius: 1 },
    ]
}

fn film() -> Vec<E> {
    vec![
        E::ChromaKey { key_color: [20, 200, 40], tolerance: 0.3, soft_edge: 0.1, spill_method: Default::default(), spill_strength: 0.5 },
        E::Deflicker { strength: 0.5, window_secs: 1.0, gain: 1.1 },
        E::FilmGrain { amount: 0.3, size: 1.0, color: 0, shadows: 0.5, seed: 7 },
        E::NoiseGenerator { amount: 0.2, monochrome: 1, seed: 3 },
        E::OldFilm { flicker: 0.2, scratches: 0.3, dust: 0.2, weave: 0.1, sepia: 0.4, seed: 5 },
        E::Vhs { tracking: 0.2, bleed: 0.3, noise: 0.2, scanlines: 0.4, seed: 9 },
        E::CornerPin { tl: [0.02, 0.01], tr: [0.97, 0.0], br: [1.0, 0.98], bl: [0.0, 1.0] },
    ]
}

/// Every family recorded so far, in one list.
fn recorded() -> Vec<E> {
    colour().into_iter().chain(geometry()).chain(blur()).chain(film()).collect()
}

fn pipeline() -> Option<GpuEffectsPipeline> {
    let g = GpuEffectsPipeline::new();
    if g.is_none() {
        assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none_or(|v| v != "1"), "required but no GPU");
        eprintln!("UNVERIFIED: no GPU");
    }
    g
}

fn picture() -> Vec<f32> {
    (0..W * H * 4).map(|i| if i % 4 == 3 { 0.25 + (i % 7) as f32 / 10.0 } else { (i % 97) as f32 / 97.0 }).collect()
}

/// Submits `apply_effects` makes for `fx`, and its picture.
fn run(g: &mut GpuEffectsPipeline, fx: &[E]) -> (u64, Vec<f32>) {
    let before = g.submit_count();
    let out = g.apply_effects(fx, &picture(), W, H);
    (g.submit_count() - before, out)
}

#[test]
fn a_run_of_recorded_effects_is_one_submit() {
    let Some(mut g) = pipeline() else { return };
    // One for the run, one for the download.
    assert_eq!(run(&mut g, &colour()[..1]).0, 2);
    assert_eq!(run(&mut g, &colour()).0, 2, "nine effects, one run");
    assert_eq!(run(&mut g, &geometry()).0, 2, "the geometry effects, one run");
    assert_eq!(run(&mut g, &blur()).0, 2, "the blur effects, several passes some of them, one run");
    assert_eq!(run(&mut g, &film()).0, 2, "the film effects, one run");
    assert_eq!(run(&mut g, &recorded()).0, 2, "every recorded effect, one run");
    // Identities record nothing, and a run of only identities submits nothing.
    let idle = [
        E::Opacity { value: 1.0 },
        E::Gamma { gamma: 1.0 },
        E::Invert { mix: 0.0 },
        E::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 },
        E::Mirror { horizontal: 0, vertical: 0 },
        E::LensDistortion { distortion: 0.0, scale: 1.0 },
        E::GaussianBlur { radius: 0.2 },
        E::TiltShift { center: 0.5, width: 0.2, softness: 0.3, radius: 0.0 },
        E::FocusBlur { radius: 2.0, vertical: false, band: 1.0 },
        // No motion data uploaded: the frame passes through.
        E::RollingShutterCorrection { strength: 0.5, readout_ratio: 0.9 },
    ];
    assert_eq!(run(&mut g, &idle).0, 1, "only the download");
    // Seventy passes overflow the 64 slots once: two runs.
    let seventy: Vec<E> = colour().into_iter().cycle().take(70).collect();
    assert_eq!(run(&mut g, &seventy).0, 3);
    // Equalize reads the frame back mid-chain, a named break: the four
    // recorded before it, its readback and pass, the five after.
    let mut eq = colour();
    eq.insert(4, E::Equalize { strength: 0.5 });
    assert_eq!(run(&mut g, &eq).0, 5);
}

#[test]
fn a_recorded_run_draws_what_each_effect_alone_draws() {
    let Some(mut g) = pipeline() else { return };
    // Each effect in a run of its own, one after the other: each pass
    // reads its own parameters there, whatever the slots do.
    let one_by_one = |g: &mut GpuEffectsPipeline, fx: &[E]| {
        fx.iter().fold(picture(), |px, e| g.apply_effects(std::slice::from_ref(e), &px, W, H))
    };
    let together = run(&mut g, &recorded()).1;
    assert_eq!(together, one_by_one(&mut g, &recorded()), "every recorded effect in one run");
    // Across the slots' overflow too.
    let seventy: Vec<E> = colour().into_iter().cycle().take(70).collect();
    assert_eq!(run(&mut g, &seventy).1, one_by_one(&mut g, &seventy), "seventy, across a refill");
    // And the picture is not the input: the effects ran.
    assert_ne!(together, picture());
}

/// A recorded effect only reads its source and leaves its result in its
/// destination, however many passes it takes: a mix after it can still
/// read the input. The multi-pass blurs ping-pong through the scratch.
#[test]
fn a_recorded_effect_never_writes_its_source() {
    let Some(mut g) = pipeline() else { return };
    let (device, queue) = (g.device(), g.queue());
    let bytes = (W * H * 16) as u64;
    let buffer = |label| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    let read = |b: &wgpu::Buffer| -> Vec<f32> {
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("read"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(b, 0, &staging, 0, bytes);
        queue.submit([enc.finish()]);
        staging.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
        device.poll(wgpu::Maintain::Wait);
        let v = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
        v
    };
    let (src, dst, scratch) = (buffer("src"), buffer("dst"), buffer("scratch"));
    let mut slots = klipp_effects::gpu::ParamSlots::new(&device, 16, 256, "test");
    for effect in [blur()[0].clone(), blur()[1].clone(), blur()[2].clone(), blur()[3].clone(), E::Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 3 }] {
        queue.write_buffer(&src, 0, bytemuck::cast_slice(&picture()));
        let mut enc = device.create_command_encoder(&Default::default());
        let mut rec = klipp_effects::gpu::EffectRecording { encoder: &mut enc, slots: &mut slots };
        let outcome = g.record_effect(&effect, &src, &dst, Some(&scratch), W, H, &mut rec);
        assert_eq!(outcome, klipp_effects::gpu::Recorded::Done, "{effect:?}");
        queue.submit([enc.finish()]);
        slots.reset();
        assert_eq!(read(&src), picture(), "{effect:?} wrote its source");
        assert_eq!(read(&dst), g.apply_effects(std::slice::from_ref(&effect), &picture(), W, H), "{effect:?}: the result is in dst");
    }
}

/// An effect that is only ever recorded and could not be (a lost
/// device; forced here) is never silently left out: `apply_effects`
/// takes the CPU chain for the frame instead, and the in-place stack
/// path refuses, which its renderer counts as a decline.
#[test]
fn an_effect_that_could_not_be_recorded_is_never_silently_dropped() {
    let Some(mut g) = pipeline() else { return };
    let fx = [E::Gamma { gamma: 1.4 }, E::GaussianBlur { radius: 2.0 }];
    g.refuse_recording(true);
    let got = g.apply_effects(&fx, &picture(), W, H);
    assert!(!g.last_effects_complete(), "the run says an effect is missing");
    assert_eq!(got, klipp_effects::apply_effects(&fx, picture(), W, H), "the CPU chain's picture, effects included");
    assert!(g.stack_begin(W, H, 1));
    assert!(g.stack_layer_from_linear(0, &picture()));
    assert!(!g.stack_layer_apply_effects(0, &fx), "the stack's layer refuses rather than skip the effect");
    g.stack_end();
    g.refuse_recording(false);
    let _ = g.apply_effects(&fx, &picture(), W, H);
    assert!(g.last_effects_complete());
}

/// An identity answers `Identity` even with no slots left: only an
/// effect that would record anything asks for a refill.
#[test]
fn an_identity_never_asks_for_slots() {
    let Some(g) = pipeline() else { return };
    let device = g.device();
    let frame = |label| {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (W * H * 16) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    };
    let (src, dst, scratch) = (frame("src"), frame("dst"), frame("scratch"));
    let mut slots = klipp_effects::gpu::ParamSlots::new(&device, 1, 256, "full");
    assert!(slots.claim_index().is_some());
    assert_eq!(slots.remaining(), 0);
    let mut enc = device.create_command_encoder(&Default::default());
    let mut rec = klipp_effects::gpu::EffectRecording { encoder: &mut enc, slots: &mut slots };
    use klipp_effects::gpu::Recorded;
    for (effect, want) in [
        (E::Opacity { value: 1.0 }, Recorded::Identity),
        (E::Opacity { value: 0.5 }, Recorded::OutOfSlots),
        (E::GaussianBlur { radius: 0.2 }, Recorded::Identity),
        (E::GaussianBlur { radius: 3.0 }, Recorded::OutOfSlots),
        (E::TiltShift { center: 0.5, width: 0.2, softness: 0.3, radius: 0.0 }, Recorded::Identity),
        (E::Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 1 }, Recorded::OutOfSlots),
    ] {
        assert_eq!(g.record_effect(&effect, &src, &dst, Some(&scratch), W, H, &mut rec), want, "{effect:?}");
    }
}
