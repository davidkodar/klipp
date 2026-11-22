//! Klipp's viewer for Veylvik's: the Veylvik page's viewer shows its
//! picture as Klipp's viewer shows a frame, through the same function:
//! into the project's working space, its display transform (BT.709's
//! encode, BT.2020's basis change and gamut compression, or ACES's RRT and
//! Rec.709 ODT), the monitor's transfer, then the viewer's LUT. So a clip
//! looks the same on the Veylvik page as on Edit.
//!
//! The picture comes from the viewer, at the viewer's size; the encode runs
//! on Veylvik's preview worker when the frame, the exposure or the display
//! changes, never on a pan or a zoom.

use std::sync::Arc;

use klipp_color::lut::LutData;
use klipp_core::{SdrTransfer, ToneMapOp, WorkingColorspace};
use veylvik_editor::host::HostDisplay;
use veylvik_preview::screen::HostEncode;

/// The project's view, as Klipp's viewer has it now.
pub struct KlippDisplay {
    working: WorkingColorspace,
    transfer: SdrTransfer,
    lut: Option<Arc<LutData>>,
    identity: u64,
}

impl KlippDisplay {
    /// The viewer of a project in `working`, on a monitor of `transfer`,
    /// with `lut` (the project's viewing LUT and the monitor's calibration,
    /// as one) when one is set.
    pub fn new(working: WorkingColorspace, transfer: SdrTransfer, lut: Option<Arc<LutData>>) -> Self {
        // Named by what makes its pictures: the same in any session, so a
        // picture Veylvik keeps on disk is never shown under another view.
        let mut identity = crate::mix(0x6b6c_6970_7076_6965 ^ working as u64);
        identity = crate::mix(identity ^ transfer as u64);
        if let Some(lut) = &lut {
            identity = crate::mix(identity ^ lut.size as u64);
            for chunk in lut.data.chunks(64) {
                let mut word = 0u64;
                for v in chunk {
                    word = word.rotate_left(7) ^ u64::from(v.to_bits());
                }
                identity = crate::mix(identity ^ word);
            }
        }
        Self { working, transfer, lut, identity: identity.max(1) }
    }

    /// The display-encoded bytes of `picture` (RGBA, scene-linear Rec.709,
    /// premultiplied), as Klipp's viewer shows them, premultiplied again.
    pub fn encode_pixels(&self, picture: &[f32]) -> Vec<u8> {
        let to_working = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, self.working);
        // Klipp's frames are straight colour in the working space.
        let straight: Vec<f32> = picture
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let a = p[3];
                let c = if a > 1e-6 { [p[0] / a, p[1] / a, p[2] / a] } else { [0.0; 3] };
                let w = to_working.map_or(c, |m| std::array::from_fn(|r| m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]));
                [w[0], w[1], w[2], a]
            })
            .collect();
        // Klipp's viewer's own encode, tone map and all, as it runs on a frame.
        let mut bytes = klipp_color::display::display_transform_ws_dithered_to(&straight, ToneMapOp::None, self.working, self.transfer);
        if let Some(lut) = &self.lut {
            klipp_color::lut::apply_3d_lut(&mut bytes, lut, 1.0);
        }
        // Premultiplied, as Veylvik's viewer takes it; opaque pixels as they are.
        for p in bytes.as_chunks_mut::<4>().0 {
            if p[3] < 255 {
                let a = u16::from(p[3]);
                for c in &mut p[..3] {
                    *c = ((u16::from(*c) * a + 127) / 255) as u8;
                }
            }
        }
        bytes
    }
}

impl HostEncode for KlippDisplay {
    fn identity(&self) -> u64 {
        self.identity
    }

    fn encode(&self, device: &wgpu::Device, queue: &wgpu::Queue, picture: &wgpu::Buffer, width: u32, height: u32) -> Result<wgpu::Texture, String> {
        let pixels = read_back(device, queue, picture, width, height)?;
        let bytes = self.encode_pixels(&pixels);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("veylvik page, Klipp's view"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The bytes are display-encoded already: no decode on sampling.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit([]);
        Ok(texture)
    }
}

impl HostDisplay for KlippDisplay {
    fn name(&self) -> String {
        let space = match self.working {
            WorkingColorspace::Rec709Linear => "Rec.709",
            WorkingColorspace::Bt2020Linear => "BT.2020",
            WorkingColorspace::AcesCG => "ACEScg, Rec.709 ODT",
        };
        let monitor = match self.transfer {
            SdrTransfer::Srgb => "sRGB",
            SdrTransfer::Bt1886 => "BT.1886",
        };
        let lut = if self.lut.is_some() { ", viewer LUT" } else { "" };
        format!("Klipp: {space}, {monitor}{lut}")
    }
}

/// `width × height` RGBA floats of `buffer`, read back from the card.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, buffer: &wgpu::Buffer, width: u32, height: u32) -> Result<Vec<f32>, String> {
    let size = u64::from(width) * u64::from(height) * 16;
    if buffer.size() < size {
        return Err(format!("the viewer's picture is {} bytes, {width}x{height} needs {size}", buffer.size()));
    }
    let staging = device.create_buffer(&wgpu::BufferDescriptor { label: Some("veylvik page view readback"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("veylvik page view readback") });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    klipp_effects::gpu::poll_until_mapped(device, &rx).map_err(|e| format!("the viewer's picture could not be read: {e}"))?.map_err(|e| format!("the viewer's picture could not be read: {e}"))?;
    let pixels: Vec<f32> = bytemuck::cast_slice(&staging.slice(..).get_mapped_range()).to_vec();
    staging.unmap();
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's view of a picture is Klipp's viewer's, byte for byte: the
    /// same colours through `display_transform_ws_dithered_to` in each
    /// working space and on each monitor, and through a viewer LUT.
    #[test]
    fn the_pages_view_is_klipps_viewer() {
        let colours: Vec<[f32; 4]> = vec![[0.18, 0.18, 0.18, 1.0], [0.9, 0.05, 0.02, 1.0], [0.97, 0.55, 0.03, 1.0], [0.01, 0.4, 0.8, 1.0], [2.5, 1.2, 0.3, 1.0], [0.0, 0.0, 0.0, 1.0]];
        let picture: Vec<f32> = colours.iter().flatten().copied().collect();
        let mut warm = LutData::identity(17);
        for c in warm.data.as_chunks_mut::<3>().0 {
            c[0] = c[0].powf(0.9);
        }
        let warm = Arc::new(warm);
        for working in [WorkingColorspace::Rec709Linear, WorkingColorspace::Bt2020Linear, WorkingColorspace::AcesCG] {
            for transfer in [SdrTransfer::Srgb, SdrTransfer::Bt1886] {
                for lut in [None, Some(Arc::clone(&warm))] {
                    let display = KlippDisplay::new(working, transfer, lut.clone());
                    let m = klipp_color::primaries::working_space_conversion_matrix(WorkingColorspace::Rec709Linear, working);
                    let in_working: Vec<f32> = colours.iter().flat_map(|c| {
                        let w = m.map_or([c[0], c[1], c[2]], |m| std::array::from_fn(|r| m[r][0] * c[0] + m[r][1] * c[1] + m[r][2] * c[2]));
                        [w[0], w[1], w[2], 1.0]
                    }).collect();
                    let mut want = klipp_color::display::display_transform_ws_dithered_to(&in_working, ToneMapOp::None, working, transfer);
                    if let Some(lut) = &lut {
                        klipp_color::lut::apply_3d_lut(&mut want, lut, 1.0);
                    }
                    assert_eq!(display.encode_pixels(&picture), want, "{working:?} {transfer:?} lut={}", lut.is_some());
                }
            }
        }
    }

    /// A display is named by what makes its pictures, the same in any
    /// session; another space, monitor or LUT is another display.
    #[test]
    fn a_displays_identity_is_its_settings() {
        let a = || KlippDisplay::new(WorkingColorspace::AcesCG, SdrTransfer::Srgb, None).identity();
        assert_eq!(a(), a());
        assert_ne!(a(), KlippDisplay::new(WorkingColorspace::Rec709Linear, SdrTransfer::Srgb, None).identity());
        assert_ne!(a(), KlippDisplay::new(WorkingColorspace::AcesCG, SdrTransfer::Bt1886, None).identity());
        let lut = |gain: f32| {
            let mut l = LutData::identity(5);
            l.data.iter_mut().for_each(|v| *v *= gain);
            Some(Arc::new(l))
        };
        let with = |gain| KlippDisplay::new(WorkingColorspace::AcesCG, SdrTransfer::Srgb, lut(gain)).identity();
        assert_ne!(a(), with(1.0));
        assert_eq!(with(0.9), with(0.9));
        assert_ne!(with(0.9), with(0.8));
        assert_ne!(a(), 0);
    }

    /// Premultiplied in and out: a half-covered pixel is its colour at half.
    #[test]
    fn coverage_is_kept() {
        let display = KlippDisplay::new(WorkingColorspace::Rec709Linear, SdrTransfer::Srgb, None);
        let full = display.encode_pixels(&[0.5, 0.5, 0.5, 1.0]);
        let half = display.encode_pixels(&[0.25, 0.25, 0.25, 0.5]);
        assert_eq!(half[3], 128);
        assert!((i32::from(half[0]) - i32::from(full[0]) / 2).abs() <= 1, "{half:?} against {full:?}");
    }
}
