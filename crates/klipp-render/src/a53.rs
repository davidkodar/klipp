//! CEA-608 captions carried in an H.264 or HEVC video stream (#1663):
//! ATSC A/53 `cc_data` in a user-data SEI ahead of each picture, so a
//! player or a broadcast chain reads the captions without a sidecar.
//!
//! The SEI is written into each encoded packet rather than handed to the
//! encoder as frame side data: ffmpeg-next gives no safe way to fill
//! side data, and the bitstream is the same for every encoder, software
//! or hardware.

use std::collections::BTreeMap;

/// Which bitstream the packets carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

/// A video stream's captions: CC1's 608 byte pairs by 608 frame (29.97),
/// sent at the stream's own rate.
#[derive(Clone, Debug)]
pub struct Captions {
    pairs: BTreeMap<u64, [u8; 2]>,
    fps: f64,
    codec: Codec,
}

/// How many `cc_data` triplets a picture carries at `fps` (ATSC A/53):
/// 9600 bit/s of caption channel, 20 a frame at 29.97, 25 at 23.976.
pub fn cc_count(fps: f64) -> usize {
    ((600.0 / fps.max(1.0)).floor() as usize).clamp(1, 31)
}

impl Captions {
    pub fn new(pairs: BTreeMap<u64, [u8; 2]>, fps: f64, codec: Codec) -> Self {
        Self { pairs, fps: fps.max(1.0), codec }
    }

    /// Picture `frame`'s `cc_data`: the 608 pairs whose 608 frames fall
    /// in its span, each as a valid field-1 triplet (a null pair where
    /// that 608 frame sends nothing), then unused triplets to the count.
    pub fn cc_data(&self, frame: u64) -> Vec<u8> {
        let ratio = klipp_captions::scc::CC_FPS / self.fps;
        let first = (frame as f64 * ratio).ceil() as u64;
        let end = ((frame + 1) as f64 * ratio).ceil() as u64;
        let count = cc_count(self.fps);
        let mut out = Vec::with_capacity(count * 3);
        for f in first..end {
            let [a, b] = self.pairs.get(&f).copied().unwrap_or([0x80, 0x80]);
            out.extend_from_slice(&[0xFC, a, b]);
        }
        out.truncate(count * 3);
        while out.len() < count * 3 {
            out.extend_from_slice(&[0xFA, 0x00, 0x00]);
        }
        out
    }

    /// `packet` (picture `frame`, as the encoder gave it) with the
    /// picture's captions in an SEI before its first slice. A packet
    /// this cannot read is given back as it was.
    pub fn insert(&self, packet: &[u8], frame: u64) -> Vec<u8> {
        let sei = sei_nal(self.codec, &t35_payload(&self.cc_data(frame), cc_count(self.fps)));
        insert_before_first_slice(packet, &sei, self.codec).unwrap_or_else(|| packet.to_vec())
    }
}

/// The `user_data_registered_itu_t_t35` payload of A/53 caption data:
/// the US country code, ATSC's provider code, "GA94", the cc_data type,
/// then the triplets between the process flags and the marker byte.
pub fn t35_payload(cc: &[u8], count: usize) -> Vec<u8> {
    let mut p = vec![0xB5, 0x00, 0x31, b'G', b'A', b'9', b'4', 0x03, 0x40 | (count as u8 & 0x1F), 0xFF];
    p.extend_from_slice(cc);
    p.push(0xFF);
    p
}

/// A whole SEI NAL unit holding one `user_data_registered_itu_t_t35`
/// message with `payload`: its header (H.264 type 6, HEVC prefix SEI 39),
/// the message, the trailing bits, emulation prevention applied.
pub fn sei_nal(codec: Codec, payload: &[u8]) -> Vec<u8> {
    let mut rbsp = vec![4u8];
    let mut size = payload.len();
    while size >= 255 {
        rbsp.push(255);
        size -= 255;
    }
    rbsp.push(size as u8);
    rbsp.extend_from_slice(payload);
    rbsp.push(0x80);
    let mut nal = match codec {
        Codec::H264 => vec![0x06],
        Codec::Hevc => vec![39 << 1, 0x01],
    };
    let mut zeros = 0;
    for &b in &rbsp {
        if zeros >= 2 && b <= 3 {
            nal.push(0x03);
            zeros = 0;
        }
        nal.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    nal
}

/// Whether a NAL unit whose first header byte is `b` is a picture's slice.
fn is_slice(codec: Codec, b: u8) -> bool {
    match codec {
        Codec::H264 => matches!(b & 0x1F, 1..=5),
        Codec::Hevc => (b >> 1) & 0x3F <= 31,
    }
}

/// A packet's NAL units as `(start of the unit, its length prefix or
/// start code's start)`, read as length-prefixed (`len` bytes each) or
/// as Annex B start codes; `None` if it is neither.
fn units(packet: &[u8]) -> Option<(Vec<(usize, usize)>, Option<usize>)> {
    let annex_b = packet.starts_with(&[0, 0, 1]) || packet.starts_with(&[0, 0, 0, 1]);
    if annex_b {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 3 <= packet.len() {
            if packet[i..].starts_with(&[0, 0, 1]) {
                let code = if i > 0 && packet[i - 1] == 0 { i - 1 } else { i };
                out.push((i + 3, code));
                i += 3;
            } else {
                i += 1;
            }
        }
        return Some((out, None));
    }
    for len in [4usize, 2, 1] {
        let mut out = Vec::new();
        let mut i = 0;
        while i + len <= packet.len() {
            let n = packet[i..i + len].iter().fold(0usize, |a, &b| a << 8 | b as usize);
            if n == 0 || i + len + n > packet.len() {
                break;
            }
            out.push((i + len, i));
            i += len + n;
        }
        if i == packet.len() && !out.is_empty() {
            return Some((out, Some(len)));
        }
    }
    None
}

/// `packet` with `nal` put in before its first slice, framed as the
/// packet frames its units; `None` when the packet has no slice or
/// cannot be read.
pub(crate) fn insert_before_first_slice(packet: &[u8], nal: &[u8], codec: Codec) -> Option<Vec<u8>> {
    let (units, len) = units(packet)?;
    let &(_, at) = units.iter().find(|&&(start, _)| packet.get(start).is_some_and(|&b| is_slice(codec, b)))?;
    let mut out = Vec::with_capacity(packet.len() + nal.len() + 4);
    out.extend_from_slice(&packet[..at]);
    match len {
        Some(n) => out.extend_from_slice(&(nal.len() as u32).to_be_bytes()[4 - n..]),
        None => out.extend_from_slice(&[0, 0, 0, 1]),
    }
    out.extend_from_slice(nal);
    out.extend_from_slice(&packet[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ATSC counts: 20 a picture at 29.97, 25 at 23.976 and 24, 24 at
    /// 25, 10 at 59.94.
    #[test]
    fn a_picture_carries_the_count_its_rate_gives() {
        assert_eq!([30000.0 / 1001.0, 24000.0 / 1001.0, 24.0, 25.0, 60000.0 / 1001.0].map(cc_count), [20, 25, 25, 24, 10]);
    }

    /// At 29.97 each picture sends its 608 frame's pair; at 25 some send
    /// two, and the null pair stands in where there is none.
    #[test]
    fn each_picture_sends_the_pairs_of_its_span() {
        let pairs: BTreeMap<u64, [u8; 2]> = [(0, [0x94, 0x20]), (1, [0x94, 0x2C]), (5, [0xC1, 0xC2])].into();
        let ntsc = Captions::new(pairs.clone(), 30000.0 / 1001.0, Codec::H264);
        assert_eq!(&ntsc.cc_data(0)[..6], &[0xFC, 0x94, 0x20, 0xFA, 0x00, 0x00]);
        assert_eq!(&ntsc.cc_data(2)[..3], &[0xFC, 0x80, 0x80], "a null pair");
        assert_eq!(ntsc.cc_data(0).len(), 60);
        let pal = Captions::new(pairs, 25.0, Codec::H264);
        // Picture 4 at 25 spans 608 frames 4.8 to 6.0: frame 5's pair.
        assert_eq!(&pal.cc_data(4)[..6], &[0xFC, 0xC1, 0xC2, 0xFA, 0x00, 0x00]);
        let valid = |f| pal.cc_data(f).chunks(3).filter(|t| t[0] == 0xFC).count();
        assert!((0..25).any(|f| valid(f) == 2), "some pictures at 25 carry two pairs");
        let sent: usize = (0..250).map(|f| pal.cc_data(f).chunks(3).filter(|t| t[0] == 0xFC).count()).sum();
        assert_eq!(sent, 300, "every 608 frame of ten seconds is sent once");
    }

    /// The SEI escapes a run of zeros so it cannot read as a start code,
    /// and ends in its trailing bits.
    #[test]
    fn the_sei_escapes_its_zeros() {
        let nal = sei_nal(Codec::H264, &[0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        assert_eq!(nal, vec![0x06, 4, 6, 0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x03, 0x00, 0x80]);
        assert_eq!(&sei_nal(Codec::Hevc, &[1])[..2], &[0x4E, 0x01]);
    }

    /// The SEI goes in before the picture's first slice, after the access
    /// unit delimiter and parameter sets, framed as the packet is.
    #[test]
    fn the_sei_goes_before_the_first_slice() {
        // Length-prefixed: an AUD (9), an SPS (7), an IDR slice (5).
        let packet = [0, 0, 0, 2, 0x09, 0xF0, 0, 0, 0, 3, 0x67, 1, 2, 0, 0, 0, 2, 0x65, 0x88];
        let out = insert_before_first_slice(&packet, &[0x06, 0xAA], Codec::H264).unwrap();
        assert_eq!(out, [&packet[..13], &[0, 0, 0, 2, 0x06, 0xAA], &packet[13..]].concat());
        // Annex B.
        let packet = [0, 0, 0, 1, 0x09, 0xF0, 0, 0, 1, 0x41, 0x9A];
        let out = insert_before_first_slice(&packet, &[0x06, 0xAA], Codec::H264).unwrap();
        assert_eq!(out, [&packet[..6], &[0, 0, 0, 1, 0x06, 0xAA], &packet[6..]].concat());
        // HEVC: a VPS (32), then a TRAIL_R slice (1).
        let packet = [0, 0, 0, 3, 0x40, 0x01, 0x0C, 0, 0, 0, 3, 0x02, 0x01, 0xD0];
        let out = insert_before_first_slice(&packet, &[0x4E, 0x01], Codec::Hevc).unwrap();
        assert_eq!(out, [&packet[..7], &[0, 0, 0, 2, 0x4E, 0x01], &packet[7..]].concat());
        // Nothing it can read is changed.
        assert!(insert_before_first_slice(&[1, 2, 3], &[6], Codec::H264).is_none());
    }
}
