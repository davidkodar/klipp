//! HDR10+ dynamic metadata on HEVC PQ exports (#468): SMPTE ST 2094-40
//! per shot, so a display dimmer than the master tone-maps each shot by
//! its own light rather than one curve for the whole programme.
//!
//! Each picture is measured from the Y'CbCr the encoder is given, so the
//! numbers describe the file whichever path rendered it. A shot's values
//! are known once its last picture is measured; its packets wait until
//! then and are written with the shot's metadata in a user-data SEI
//! (`crate::a53` puts it in, the same for every HEVC encoder).

use std::collections::VecDeque;

/// The maxRGB percentiles an HDR10+ window describes, as percentages.
pub const PERCENTAGES: [u8; 9] = [1, 5, 10, 25, 50, 75, 90, 95, 99];

/// One picture's light, cd/m²: the brightest of each channel (maxSCL),
/// the average of each pixel's brightest channel (maxRGB), and maxRGB at
/// [`PERCENTAGES`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Light {
    pub maxscl: [f32; 3],
    pub average: f32,
    pub percentiles: [f32; 9],
}

/// A 10-bit limited-range BT.2020 Y'CbCr PQ sample as R, G, B in cd/m².
fn rgb_nits(y: u16, cb: u16, cr: u16) -> [f32; 3] {
    let y = (y as f32 - 64.0) / 876.0;
    let (cb, cr) = ((cb as f32 - 512.0) / 896.0, (cr as f32 - 512.0) / 896.0);
    let r = y + 1.4746 * cr;
    let b = y + 1.8814 * cb;
    let g = (y - 0.2627 * r - 0.0593 * b) / 0.6780;
    [r, g, b].map(|v| klipp_color::hdr::pq_decode_nits(v.clamp(0.0, 1.0)))
}

/// A picture's [`Light`], from every `step`th pixel of its planes: 10-bit
/// 4:2:0 planar or semi-planar, or 4:2:2 planar. `None` for any other
/// layout.
pub fn measure(frame: &ffmpeg_next::frame::Video, step: usize) -> Option<Light> {
    use ffmpeg_next::format::Pixel as P;
    let (w, h) = (frame.width() as usize, frame.height() as usize);
    let (semi, shift, chroma_rows) = match frame.format() {
        P::YUV420P10LE => (false, 0, 2),
        P::P010LE => (true, 6, 2),
        P::YUV422P10LE => (false, 0, 1),
        _ => return None,
    };
    let at = |plane: usize, x: usize, y: usize| {
        let row = &frame.data(plane)[y * frame.stride(plane)..];
        u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) >> shift
    };
    let step = step.max(1);
    let mut maxrgb: Vec<f32> = Vec::with_capacity((w / step + 1) * (h / step + 1));
    let mut maxscl = [0.0f32; 3];
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let (cx, cy) = (x / 2, y / chroma_rows);
            let (cb, cr) = if semi { (at(1, cx * 2, cy), at(1, cx * 2 + 1, cy)) } else { (at(1, cx, cy), at(2, cx, cy)) };
            let rgb = rgb_nits(at(0, x, y), cb, cr);
            for c in 0..3 {
                maxscl[c] = maxscl[c].max(rgb[c]);
            }
            maxrgb.push(rgb[0].max(rgb[1]).max(rgb[2]));
        }
    }
    if maxrgb.is_empty() {
        return None;
    }
    let average = (maxrgb.iter().map(|&v| v as f64).sum::<f64>() / maxrgb.len() as f64) as f32;
    maxrgb.sort_unstable_by(f32::total_cmp);
    let percentiles = PERCENTAGES.map(|p| maxrgb[((p as usize * maxrgb.len()) / 100).min(maxrgb.len() - 1)]);
    Some(Light { maxscl, average, percentiles })
}

/// A shot's light from its pictures': each channel's brightest, the mean
/// of their averages and of each percentile.
pub fn shot_light(frames: &[Light]) -> Light {
    let n = frames.len().max(1) as f32;
    let mut out = Light::default();
    for f in frames {
        for c in 0..3 {
            out.maxscl[c] = out.maxscl[c].max(f.maxscl[c]);
        }
        out.average += f.average / n;
        for (o, p) in out.percentiles.iter_mut().zip(f.percentiles) {
            *o += p / n;
        }
    }
    out
}

/// Bits, most significant first.
struct Bits(Vec<u8>, u32);

impl Bits {
    fn put(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            if self.1.is_multiple_of(8) {
                self.0.push(0);
            }
            if value >> i & 1 == 1 {
                *self.0.last_mut().unwrap() |= 0x80 >> (self.1 % 8);
            }
            self.1 += 1;
        }
    }
}

/// ST 2094-40's light units: 0.1 cd/m², 17 bits.
fn tenths(nits: f32) -> u32 {
    ((nits.max(0.0) * 10.0).round() as u32).min(100_000)
}

/// The `user_data_registered_itu_t_t35` payload of HDR10+ metadata for
/// one shot's `light`: the US country code, Samsung's provider code and
/// oriented code, application 4 version 1, one window over the whole
/// picture, no targeted display, its maxSCL, average maxRGB and the nine
/// percentiles, no bright-pixel fraction, no mastering-display peak, no
/// tone-mapping curve and no saturation mapping.
pub fn t35_payload(light: &Light) -> Vec<u8> {
    let mut b = Bits(Vec::new(), 0);
    b.put(0xB5, 8);
    b.put(0x003C, 16);
    b.put(0x0001, 16);
    b.put(4, 8);
    b.put(1, 8);
    b.put(1, 2); // num_windows
    b.put(0, 27); // targeted_system_display_maximum_luminance
    b.put(0, 1); // targeted_system_display_actual_peak_luminance_flag
    for c in light.maxscl {
        b.put(tenths(c), 17);
    }
    b.put(tenths(light.average), 17);
    b.put(PERCENTAGES.len() as u32, 4);
    for (p, v) in PERCENTAGES.iter().zip(light.percentiles) {
        b.put(*p as u32, 7);
        b.put(tenths(v), 17);
    }
    b.put(0, 10); // fraction_bright_pixels
    b.put(0, 1); // mastering_display_actual_peak_luminance_flag
    b.put(0, 1); // tone_mapping_flag
    b.put(0, 1); // color_saturation_mapping_flag
    b.0
}

/// An export's HDR10+: where its shots start (export frames, from 0),
/// each picture's light as it is measured, and the packets waiting for
/// their shot's.
pub struct Hdr10Plus {
    starts: Vec<u64>,
    frames: Vec<Light>,
    waiting: VecDeque<ffmpeg_next::Packet>,
}

impl Hdr10Plus {
    pub fn new(mut starts: Vec<u64>) -> Self {
        starts.push(0);
        starts.sort_unstable();
        starts.dedup();
        Self { starts, frames: Vec::new(), waiting: VecDeque::new() }
    }

    /// The next picture's light, in presentation order. A picture that
    /// could not be measured counts as black.
    pub fn measured(&mut self, light: Option<Light>) {
        self.frames.push(light.unwrap_or_default());
    }

    /// The shot `frame` is in, as its first and one past its last frame.
    fn shot(&self, frame: u64) -> (u64, u64) {
        let i = self.starts.partition_point(|&s| s <= frame).saturating_sub(1);
        (self.starts[i], self.starts.get(i + 1).copied().unwrap_or(u64::MAX))
    }

    /// Hold `packet` until its shot is measured.
    pub fn hold(&mut self, packet: ffmpeg_next::Packet) {
        self.waiting.push_back(packet);
    }

    /// The packets at the front whose shots are measured (all of them
    /// when `done`), each with its shot's metadata put in, in the order
    /// they came.
    pub fn ready(&mut self, done: bool) -> Vec<ffmpeg_next::Packet> {
        let mut out = Vec::new();
        while let Some(p) = self.waiting.front() {
            let frame = p.pts().unwrap_or(0).max(0) as u64;
            let (from, to) = self.shot(frame);
            let measured = self.frames.len() as u64;
            if !done && measured < to {
                break;
            }
            let p = self.waiting.pop_front().expect("a front");
            let span = &self.frames[(from as usize).min(self.frames.len())..(to.min(measured) as usize).max(from as usize).min(self.frames.len())];
            let nal = crate::a53::sei_nal(crate::a53::Codec::Hevc, &t35_payload(&shot_light(span)));
            out.push(match p.data().and_then(|d| crate::a53::insert_before_first_slice(d, &nal, crate::a53::Codec::Hevc)) {
                Some(data) => {
                    let mut with = ffmpeg_next::Packet::copy(&data);
                    with.set_stream(p.stream());
                    with.set_pts(p.pts());
                    with.set_dts(p.dts());
                    with.set_duration(p.duration());
                    with.set_flags(p.flags());
                    with
                }
                None => p,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload ffmpeg reads as HDR10+: the header bytes, then the
    /// fields packed at their widths (checked against a hand count).
    #[test]
    fn the_payload_packs_its_fields() {
        let light = Light { maxscl: [1000.0, 800.0, 400.0], average: 120.0, percentiles: [1.0, 2.0, 5.0, 20.0, 60.0, 150.0, 400.0, 600.0, 950.0] };
        let p = t35_payload(&light);
        assert_eq!(&p[..7], &[0xB5, 0x00, 0x3C, 0x00, 0x01, 0x04, 0x01]);
        // 2 + 27 + 1 + 3×17 + 17 + 4 + 9×(7+17) + 10 + 3 bits after them.
        let bits = 2 + 27 + 1 + 51 + 17 + 4 + 216 + 10 + 3;
        assert_eq!(p.len(), 7 + (bits as usize).div_ceil(8));
        // num_windows 1 in the top two bits of the next byte.
        assert_eq!(p[7] >> 6, 1);
    }

    /// A shot keeps its brightest channel values and averages the rest.
    #[test]
    fn a_shot_is_its_pictures_together() {
        let a = Light { maxscl: [100.0, 50.0, 20.0], average: 10.0, percentiles: [1.0; 9] };
        let b = Light { maxscl: [50.0, 200.0, 20.0], average: 30.0, percentiles: [3.0; 9] };
        let s = shot_light(&[a, b]);
        assert_eq!(s.maxscl, [100.0, 200.0, 20.0]);
        assert_eq!((s.average, s.percentiles[4]), (20.0, 2.0));
    }

    /// PQ's 203 cd/m² white comes back as 203 on every channel.
    #[test]
    fn a_pq_white_reads_as_its_light() {
        let code = (64.0 + klipp_color::hdr::pq_encode_nits(203.0) * 876.0).round() as u16;
        let rgb = rgb_nits(code, 512, 512);
        assert!(rgb.iter().all(|v| (v - 203.0).abs() < 2.0), "{rgb:?}");
    }
}
