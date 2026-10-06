use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::mask_mix::{MaskChannel, MaskMix, MaskMixError};
use wgpu::util::DeviceExt;

const LUMAS: [[f32; 3]; 3] = [
    [0.2126, 0.7152, 0.0722],
    [0.27222872, 0.67408174, 0.05368952],
    [0.2627, 0.6780, 0.0593],
];

#[derive(Clone, Copy)]
struct Case {
    input: [f32; 4],
    effect: [f32; 4],
    mask: Option<[f32; 4]>,
    mix: f32,
    channel: MaskChannel,
    luma: [f32; 3],
    legacy: bool,
}
impl Case {
    fn settings(self) -> MaskMix {
        MaskMix::new(self.mix, self.channel, self.luma).unwrap()
    }
    // Independent f64 arithmetic: no production normalization or mixing calls.
    fn weight(self) -> f64 {
        let coverage = self.mask.map_or(1.0, |m| {
            let x = match self.channel {
                MaskChannel::Red => f64::from(m[0]),
                MaskChannel::Alpha => f64::from(m[3]),
                MaskChannel::Luminance => (0..3)
                    .map(|c| f64::from(m[c]) * f64::from(self.luma[c]))
                    .sum(),
            };
            // Coverage is extracted into f32 storage. A mathematically
            // finite dot product can overflow that representation.
            if (x as f32).is_finite() {
                x.clamp(0.0, 1.0)
            } else {
                0.0
            }
        });
        f64::from(self.mix).clamp(0.0, 1.0) * coverage
    }
    fn oracle(self) -> [f32; 4] {
        let w = self.weight();
        let mut out = if w == 0.0 {
            self.input
        } else if w == 1.0 {
            self.effect
        } else {
            std::array::from_fn(|c| {
                (f64::from(self.input[c])
                    + (f64::from(self.effect[c]) - f64::from(self.input[c])) * w)
                    as f32
            })
        };
        if self.legacy {
            out[3] = 1.0;
        }
        out
    }
}
fn example() -> Case {
    Case {
        input: [0.2, 0.4, 0.6, 0.8],
        effect: [0.8, 0.2, 0.0, 0.0],
        mask: Some([0.5; 4]),
        mix: 0.5,
        channel: MaskChannel::Red,
        luma: LUMAS[0],
        legacy: false,
    }
}
fn cases() -> Vec<Case> {
    let mut cases = vec![example()];
    for mix in [0.0, 0.25, 0.5, 1.0, -0.5, 1.5] {
        for mask in [
            0.0,
            0.25,
            0.5,
            1.0,
            -0.25,
            1.25,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ] {
            for (input, effect) in [
                ([8.0, -0.05, 0.6, 0.0], [-0.05, 4.0, 0.0, 0.8]),
                ([-0.0, 0.4, 0.6, 0.8], [0.8, 0.2, 0.0, 0.0]),
            ] {
                cases.push(Case {
                    input,
                    effect,
                    mix,
                    mask: Some([mask; 4]),
                    ..example()
                });
            }
        }
        cases.push(Case {
            mix,
            mask: None,
            ..example()
        });
        for channel in [MaskChannel::Red, MaskChannel::Luminance, MaskChannel::Alpha] {
            for luma in LUMAS {
                cases.push(Case {
                    mix,
                    channel,
                    luma,
                    mask: Some([0.2, 0.7, 0.9, 0.0]),
                    ..example()
                });
            }
        }
    }
    for mix in [0.0, 0.5, 1.0] {
        cases.push(Case {
            mix,
            legacy: true,
            ..example()
        });
    }
    cases.push(Case {
        mix: 0.0,
        effect: [f32::NAN; 4],
        ..example()
    });
    cases.push(Case {
        mask: Some([f32::INFINITY; 4]),
        effect: [f32::NAN; 4],
        ..example()
    });
    for channel in [MaskChannel::Red, MaskChannel::Luminance, MaskChannel::Alpha] {
        cases.push(Case {
            mix: 1.0,
            channel,
            luma: [1.0, 0.0, 0.0],
            mask: Some([1.0; 4]),
            input: [f32::NAN; 4],
            effect: [-0.0, 8.0, -0.05, 0.0],
            ..example()
        });
    }
    for mask in [
        [1e38, 1e38, 1e38, 1.0],
        [f32::INFINITY, f32::NEG_INFINITY, 1.0, 1.0],
    ] {
        cases.push(Case {
            mix: 1.0,
            channel: MaskChannel::Luminance,
            luma: [2.0; 3],
            mask: Some(mask),
            effect: [f32::NAN; 4],
            ..example()
        });
    }
    cases
}
fn at_pixel(mut c: Case, index: usize) -> Case {
    // Keep the numerical alpha example a constant filled image. Other
    // cases vary spatially, exposing incorrect pixel/mask indexing.
    if c.input != example().input {
        c.input.rotate_left(index % 4);
        c.effect.rotate_left((index / 3) % 4);
        if index % 2 == 1 && c.input.iter().all(|v| v.is_finite()) {
            c.mask = c.mask.map(|mut m| {
                m[0] = -m[0];
                m
            });
        }
    }
    c
}

fn close(actual: [f32; 4], expected: [f32; 4]) {
    for c in 0..4 {
        // Allow a few f32 rounding steps at HDR magnitudes. The unit-scale
        // floor covers cancellation near zero without loosening HDR to a
        // fixed absolute error independent of its representable precision.
        let tolerance = 4.0 * f32::EPSILON * expected[c].abs().max(1.0);
        assert!(
            (actual[c] - expected[c]).abs() <= tolerance,
            "channel {c}: {actual:?} != {expected:?}"
        );
    }
}
#[test]
fn the_alpha_example_is_exact() {
    let c = example();
    let out = c.settings().pixel(c.input, c.effect, c.mask);
    close(out, [0.35, 0.35, 0.45, 0.6]);
    assert_ne!(out[3], 0.0);
    assert_ne!(out[3], 1.0);
}
#[test]
fn mask_mix_matches_the_oracle_on_the_grid() {
    for c in cases() {
        let actual = if c.legacy {
            c.settings()
                .pixel_legacy_alpha_one(c.input, c.effect, c.mask)
        } else {
            c.settings().pixel(c.input, c.effect, c.mask)
        };
        close(actual, c.oracle());
        if !c.legacy && c.weight() == 0.0 {
            assert_eq!(actual.map(f32::to_bits), c.input.map(f32::to_bits));
        }
        if !c.legacy && c.weight() == 1.0 {
            assert_eq!(actual.map(f32::to_bits), c.effect.map(f32::to_bits));
        }
    }
}
#[test]
fn settings_and_frame_sizes_are_validated_without_partial_writes() {
    for v in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(matches!(
            MaskMix::new(v, MaskChannel::Red, LUMAS[0]),
            Err(MaskMixError::NonFiniteMix)
        ));
    }
    for c in [MaskChannel::Red, MaskChannel::Luminance, MaskChannel::Alpha] {
        assert_eq!(c.as_str().parse(), Ok(c));
    }
    assert_eq!(
        "green".parse::<MaskChannel>(),
        Err(MaskMixError::UnknownChannel)
    );
    let mut out = [[9.0; 4]; 2];
    assert_eq!(
        example()
            .settings()
            .apply(&[[0.0; 4]], &[[1.0; 4]], None, &mut out),
        Err(MaskMixError::SizeMismatch)
    );
    assert_eq!(out, [[9.0; 4]; 2]);
    for n in [1, 64 * 36] {
        let c = example();
        let mut out = vec![[0.0; 4]; n];
        c.settings()
            .apply(
                &vec![c.input; n],
                &vec![c.effect; n],
                Some(&vec![c.mask.unwrap(); n]),
                &mut out,
            )
            .unwrap();
        for p in out {
            close(p, c.oracle());
        }
    }
}

#[test]
fn gpu_mask_mix_matches_the_independent_oracle() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert_ne!(
            std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(),
            Ok("1"),
            "required GPU unavailable"
        );
        eprintln!("unverified: no GPU pipeline for mask_mix");
        return;
    };
    let device = gpu.device();
    let queue = gpu.queue();
    let all = cases();
    // Each chunk deliberately records different uniforms/masks before one
    // submission, catching a shared uniform or image binding being overwritten.
    for (width, height) in [(1, 1), (64, 36)] {
        for chunk in all.chunks(16) {
            gpu.begin_chain();
            let before = gpu.submit_count();
            let mut encoder = device.create_command_encoder(&Default::default());
            let mut readbacks = Vec::new();
            for &c in chunk {
                let n = (width * height) as usize;
                let bytes = (n * 16) as u64;
                let upload = |pixels: Vec<[f32; 4]>| {
                    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mask_mix_fixture"),
                        contents: bytemuck::cast_slice(&pixels),
                        usage: wgpu::BufferUsages::STORAGE,
                    })
                };
                let input = upload((0..n).map(|i| at_pixel(c, i).input).collect());
                let effect = upload((0..n).map(|i| at_pixel(c, i).effect).collect());
                let mask = c
                    .mask
                    .map(|_| upload((0..n).map(|i| at_pixel(c, i).mask.unwrap()).collect()));
                let output = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: bytes,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                gpu.record_mask_mix(
                    &mut encoder,
                    &input,
                    &effect,
                    mask.as_ref(),
                    &output,
                    width,
                    height,
                    c.settings(),
                    c.legacy,
                )
                .unwrap();
                let read = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: bytes,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_buffer_to_buffer(&output, 0, &read, 0, bytes);
                readbacks.push((read, c));
            }
            assert_eq!(gpu.submit_count(), before, "recording must never submit");
            queue.submit(Some(encoder.finish()));
            for (read, c) in readbacks {
                let (tx, rx) = std::sync::mpsc::channel();
                read.slice(..)
                    .map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
                device.poll(wgpu::Maintain::Wait);
                rx.recv_timeout(std::time::Duration::from_secs(30))
                    .unwrap()
                    .unwrap();
                let mapped = read.slice(..).get_mapped_range();
                for (index, p) in bytemuck::cast_slice::<u8, [f32; 4]>(&mapped)
                    .iter()
                    .enumerate()
                {
                    let c = at_pixel(c, index);
                    close(*p, c.oracle());
                    if !c.legacy && c.weight() == 0.0 {
                        assert_eq!(p.map(f32::to_bits), c.input.map(f32::to_bits));
                    }
                    if !c.legacy && c.weight() == 1.0 {
                        assert_eq!(p.map(f32::to_bits), c.effect.map(f32::to_bits));
                    }
                }
                drop(mapped);
                read.unmap();
            }
            gpu.submit_chain();
        }
    }
    // Exhaustion refuses before recording another pass; it never submits
    // the caller's encoder or wraps and overwrites an earlier uniform.
    gpu.begin_chain();
    let before = gpu.submit_count();
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(&[example().input]),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    let mut accepted = 0;
    loop {
        let result = gpu.record_mask_mix(
            &mut encoder,
            &input,
            &input,
            None,
            &output,
            1,
            1,
            example().settings(),
            false,
        );
        if result == Err(MaskMixError::ParamsExhausted) {
            break;
        }
        result.unwrap();
        accepted += 1;
        assert!(accepted < 1024, "parameter ring must be bounded");
    }
    assert!(accepted > 1);
    assert_eq!(gpu.submit_count(), before);
    queue.submit(Some(encoder.finish()));
    device.poll(wgpu::Maintain::Wait);
    gpu.submit_chain();
}
