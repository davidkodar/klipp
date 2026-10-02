//! A downscale in the NV12 and P010 decode shaders averages each output
//! pixel's footprint in linear light (#744), for every rotation.
//!
//! It used to take one source pixel per output pixel, which drops three
//! in four shrinking 4K to 1080: fine detail aliased in the viewer, and
//! the export could not use the shader at all and went through the CPU.
//!
//! The reference is the shader itself at 1:1, which is exact (one source
//! pixel per output pixel, the conversion every other test pins), box-
//! averaged on the CPU. At an integer ratio the downscale samples exactly
//! the source pixels under each output pixel, so the two must agree to
//! float rounding: a footprint off by a pixel, a rotation mapped the wrong
//! way round or an average in the wrong domain all fail it.

use klipp_effects::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED;
use klipp_effects::nv12_gpu::GpuNv12Converter;
use klipp_effects::p010_gpu::GpuP010Converter;

/// Limited range, BT.709 matrix, transfer and primaries: display-referred.
const BT709_SDR: [u32; 4] = [0, 0, 0, 0];
const W: u32 = 64;
const H: u32 = 32;

fn read(device: &wgpu::Device, queue: &wgpu::Queue, buf: &wgpu::Buffer, pixels: u32) -> Vec<f32> {
    let bytes = pixels as u64 * 16;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("downscale_readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(buf, 0, &staging, 0, bytes);
    queue.submit(std::iter::once(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let out: Vec<f32> = bytemuck::pod_collect_to_vec(&slice.get_mapped_range());
    staging.unmap();
    out
}

/// `full` (`fw` x `fh` RGBA) shrunk by `k` each way, each output pixel the
/// mean of the `k x k` block under it.
fn box_down(full: &[f32], fw: u32, fh: u32, k: u32) -> Vec<f32> {
    let (w, h) = (fw / k, fh / k);
    let mut out = vec![0.0f32; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut sum = 0.0;
                for j in 0..k {
                    for i in 0..k {
                        sum += full[(((y * k + j) * fw + x * k + i) * 4 + c) as usize];
                    }
                }
                out[((y * w + x) * 4 + c) as usize] = sum / (k * k) as f32;
            }
            out[((y * w + x) * 4 + 3) as usize] = 1.0;
        }
    }
    out
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Output size for a source rotated by `rot`, shrunk by `k`.
fn dims(rot: u32, k: u32) -> (u32, u32) {
    if rot % 180 == 90 { (H / k, W / k) } else { (W / k, H / k) }
}

/// Y a ramp with a fine checker on it, so every pixel differs from its
/// neighbours and a dropped one shows; U and V on their own periods.
fn nv12() -> (Vec<u8>, Vec<u8>) {
    let y = (0..H).flat_map(|r| (0..W).map(move |c| (32 + (c * 3 + r * 2) % 160 + if (c + r) % 2 == 0 { 40 } else { 0 }) as u8)).collect();
    let uv = (0..H / 2).flat_map(|r| (0..W / 2).flat_map(move |c| [(80 + (c * 5 + r) % 96) as u8, (100 + (c + r * 7) % 80) as u8])).collect();
    (y, uv)
}

fn p010() -> (Vec<u8>, Vec<u8>) {
    let code = |v: u32| ((v as u16) << 6).to_le_bytes();
    let y = (0..H).flat_map(|r| (0..W).flat_map(move |c| code(128 + (c * 11 + r * 7) % 640 + if (c + r) % 2 == 0 { 160 } else { 0 }))).collect();
    let uv = (0..H / 2).flat_map(|r| (0..W / 2).flat_map(move |c| [code(320 + (c * 13 + r) % 384), code(400 + (c + r * 9) % 320)].concat())).collect();
    (y, uv)
}

#[test]
fn a_downscale_is_the_box_average_of_the_full_size_frame_at_every_rotation() {
    let (Some(mut nv), Some(mut p10)) = (GpuNv12Converter::new(), GpuP010Converter::new()) else {
        eprintln!("skipping: no GPU");
        return;
    };
    let (ny, nuv) = nv12();
    let (py, puv) = p010();
    for rot in [0u32, 90, 180, 270] {
        for k in [2u32, 4] {
            let (fw, fh) = dims(rot, 1);
            let (dw, dh) = dims(rot, k);
            let mut run_nv = |w: u32, h: u32| {
                let buf = nv.convert_nv12_to_linear_on_gpu(&ny, &nuv, W, H, W, W, w, h, rot, BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED);
                read(&nv.device(), &nv.queue(), &buf, w * h)
            };
            let (full, down) = (run_nv(fw, fh), run_nv(dw, dh));
            let err = worst(&down, &box_down(&full, fw, fh, k));
            assert!(err < 1e-5, "NV12, rotation {rot}, 1/{k}: {err}");
            let mut run_p10 = |w: u32, h: u32| {
                let buf = p10.convert_p010_to_linear_on_gpu(&py, &puv, W, H, W * 2, W * 2, w, h, rot, BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED);
                read(&p10.device(), &p10.queue(), &buf, w * h)
            };
            let (full, down) = (run_p10(fw, fh), run_p10(dw, dh));
            let err = worst(&down, &box_down(&full, fw, fh, k));
            assert!(err < 1e-5, "P010, rotation {rot}, 1/{k}: {err}");
        }
    }
}

/// The aliasing itself: a one-pixel black and white checker, halved, is
/// an even grey in linear light. Nearest kept one of the two and showed
/// black or white; averaging the encoded values would show the darker
/// grey of a gamma-space average.
#[test]
fn a_fine_checker_halves_to_its_linear_mean() {
    let Some(mut nv) = GpuNv12Converter::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let y: Vec<u8> = (0..H).flat_map(|r| (0..W).map(move |c| if (c + r) % 2 == 0 { 16 } else { 235 })).collect();
    let uv = vec![128u8; (W * H / 2) as usize];
    let buf = nv.convert_nv12_to_linear_on_gpu(&y, &uv, W, H, W, W, W / 2, H / 2, 0, BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED);
    let out = read(&nv.device(), &nv.queue(), &buf, W * H / 4);
    for px in out.chunks_exact(4) {
        for c in &px[..3] {
            assert!((c - 0.5).abs() < 0.01, "a halved checker should be linear mid-grey, got {px:?}");
        }
    }
}
