//! HDR reference output: the shared frame the viewer writes and a
//! separate presenter shows on an HDR surface.
//!
//! # Why a separate process
//!
//! The picture has to reach a surface whose format is `Rgba16Float` in the
//! scRGB colour space — linear, values past 1.0 allowed — because that is
//! the one HDR surface wgpu 22 can create (it maps `Rgba16Float` to
//! `VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT`; every other colour space the
//! driver offers, HDR10 PQ included, it discards in
//! `map_vk_surface_formats`).
//!
//! Klipp's window cannot be that surface. `egui-wgpu` configures *every*
//! surface with one `render_state.target_format`, chosen by
//! `preferred_framebuffer_format`, which returns the first 8-bit format it
//! finds; and egui's render pipeline is built for that format, so a
//! second, HDR viewport would need a second pipeline. Making the main
//! window HDR instead would put the whole UI in a linear framebuffer,
//! where every widget colour is wrong until all of it is colour-managed.
//!
//! So the output is its own process with its own event loop and its own
//! surface, which is also what a reference monitor feed *should* be: it
//! carries the picture and nothing else, and can sit fullscreen on a
//! second display without the UI following it there.
//!
//! # What travels
//!
//! HDR display light, RGBA f32, `1.0` = SDR reference white (203 cd/m²,
//! BT.2408) — the same thing the export writes and the viewer's monitoring
//! measures, so no new colour convention enters here. The presenter's
//! surface is linear, so it writes those values through unchanged; the
//! compositor's colour management is what decides where 1.0 lands on the
//! panel.
//!
//! # How it travels
//!
//! One memory-mapped file with a header and three frame slots. The writer
//! fills the slot the reader is not looking at and then publishes it by
//! bumping a sequence number; the reader takes the newest published slot.
//! No locks, and a reader that misses a frame sees the one after it rather
//! than a torn mixture of two — which for a monitor feed is the right
//! trade, since a stale frame is worse than a dropped one.
//!
//! Uncompressed on purpose: a 4K frame is 133 MB of f32 (four
//! channels at four bytes) and a memcpy into shared memory is cheap,
//! where compressing it would cost more
//! than the frame is worth and add a failure mode to a monitoring path.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Bytes per pixel: RGBA f32.
pub const BYTES_PER_PIXEL: usize = 16;

/// How many frame slots the ring holds.
///
/// Three, not two. With two, frames N and N+2 share a slot, so a reader
/// copying frame N races the writer filling N+2 and has to start over;
/// measured against a writer going flat out that cost about one read in
/// eighty. A third slot gives the reader two frames of slack and the
/// retries go away. It is not what makes the read *correct* — that is the
/// sequence check in [`FrameReader::read_latest`] — only what stops it
/// being wasteful.
pub const SLOTS: usize = 3;

/// Bumped when the layout below changes in a way an older reader would
/// misread. A presenter that sees a version it does not know refuses the
/// file rather than showing a scrambled picture.
pub const PROTOCOL_VERSION: u32 = 1;

/// Magic, so a truncated or unrelated file is not mistaken for a frame.
pub const MAGIC: u64 = 0x4b4c_4950_5f48_4452; // "KLIP_HDR"

/// The header at the start of the mapping.
///
/// `#[repr(C)]` and only fixed-width fields, because two processes built
/// at different times read it.
#[repr(C)]
pub struct Header {
    pub magic: AtomicU64,
    pub version: AtomicU32,
    /// Frame geometry. Read *after* `sequence` confirms a slot, so a
    /// resize cannot be seen half-applied.
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// How many bytes a slot holds — the writer's idea of it, so a reader
    /// mapping a shorter file can refuse rather than read past the end.
    pub slot_bytes: AtomicU64,
    /// Incremented once per published frame. Odd while a slot is being
    /// written, even when it is settled: a reader that sees an odd value,
    /// or a value that changed while it was reading, tries again. The
    /// published slot is `(sequence / 2) % SLOTS`.
    pub sequence: AtomicU64,
    /// The viewer's peak, cd/m², for the presenter's own information —
    /// the picture carries light, not a scale, so this changes nothing
    /// about the pixels.
    pub peak_nits: AtomicU32,
    pub _reserved: AtomicU32,
}

/// Where the header ends and the first slot begins, padded so slots are
/// page-aligned and a `memcpy` into one is not split across a page for no
/// reason.
pub const DATA_OFFSET: usize = 4096;

/// The mapping's total size for a frame of `width` × `height`.
pub fn mapping_bytes(width: u32, height: u32) -> usize {
    DATA_OFFSET + SLOTS * slot_bytes(width, height)
}

/// One slot's size for a frame of `width` × `height`.
pub fn slot_bytes(width: u32, height: u32) -> usize {
    (width as usize) * (height as usize) * BYTES_PER_PIXEL
}

/// The default path, under the user's runtime directory so it is cleaned
/// up on logout and is not world-readable.
pub fn default_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("klipp-hdr-out.frame")
}

/// The viewer's end: creates the file and publishes frames into it.
pub struct FrameWriter {
    map: memmap2::MmapMut,
    path: PathBuf,
    width: u32,
    height: u32,
}

impl FrameWriter {
    /// Create or re-create the shared frame at `path` for this geometry.
    ///
    /// Re-created rather than resized when the geometry changes: a reader
    /// holding the old mapping keeps a valid view of the old file until it
    /// notices, instead of having the ground move under it.
    pub fn create(path: &Path, width: u32, height: u32) -> std::io::Result<Self> {
        if width == 0 || height == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a frame needs a non-zero size",
            ));
        }
        let bytes = mapping_bytes(width, height);
        // Built beside the real path and renamed over it, never truncated
        // in place. A presenter already holding a mapping keeps a valid
        // view of the old inode until it notices and re-opens; truncating
        // the file it is mapped to would hand it garbage, or SIGBUS if the
        // file shrank.
        let tmp = path.with_extension("frame.new");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.set_len(bytes as u64)?;
        let map = unsafe { memmap2::MmapOptions::new().len(bytes).map_mut(&file)? };
        let w = Self { map, path: path.to_path_buf(), width, height };
        // The magic goes last, so a reader cannot see a valid magic over a
        // half-initialised header.
        let h = w.header();
        h.version.store(PROTOCOL_VERSION, Ordering::Relaxed);
        h.width.store(width, Ordering::Relaxed);
        h.height.store(height, Ordering::Relaxed);
        h.slot_bytes.store(slot_bytes(width, height) as u64, Ordering::Relaxed);
        h.sequence.store(0, Ordering::Relaxed);
        h.peak_nits.store(1000, Ordering::Relaxed);
        h._reserved.store(0, Ordering::Relaxed);
        h.magic.store(MAGIC, Ordering::Release);
        // Published only once the header is complete.
        std::fs::rename(&tmp, path)?;
        Ok(w)
    }

    fn header(&self) -> &Header {
        // SAFETY: the mapping is at least DATA_OFFSET long (checked in
        // `create`, which allocates header + slots), and `Header` is
        // `repr(C)` of atomics with no padding requirements beyond 8.
        unsafe { &*(self.map.as_ptr() as *const Header) }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Record the master's peak, for the presenter's information.
    pub fn set_peak_nits(&self, peak: f32) {
        self.header().peak_nits.store(peak.max(0.0) as u32, Ordering::Relaxed);
    }

    /// Publish one frame of HDR display light (RGBA f32, 1.0 = SDR
    /// reference white).
    ///
    /// `false` if `linear` is not exactly one frame for this geometry —
    /// the caller has the wrong size and publishing it would show a
    /// sheared picture.
    pub fn publish(&mut self, linear: &[f32]) -> bool {
        let want = slot_bytes(self.width, self.height) / 4;
        if linear.len() != want {
            return false;
        }
        let seq = self.header().sequence.load(Ordering::Relaxed);
        // The slot the *next* publish owns: the one the current sequence
        // does not point at.
        let slot = ((seq / 2) as usize + 1) % SLOTS;
        // Odd while writing, so a reader mid-write knows to wait.
        self.header().sequence.store(seq.wrapping_add(1), Ordering::Release);
        let off = DATA_OFFSET + slot * slot_bytes(self.width, self.height);
        let dst = &mut self.map[off..off + slot_bytes(self.width, self.height)];
        dst.copy_from_slice(bytemuck::cast_slice(linear));
        // Even again, now pointing at the slot just filled.
        self.header().sequence.store(seq.wrapping_add(2), Ordering::Release);
        true
    }

    /// The sequence number of the last published frame; two per frame.
    pub fn sequence(&self) -> u64 {
        self.header().sequence.load(Ordering::Acquire)
    }
}

impl Drop for FrameWriter {
    fn drop(&mut self) {
        // Clear the magic so a presenter watching the file stops showing a
        // frame that is no longer being updated, rather than freezing on
        // the last one.
        self.header().magic.store(0, Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The presenter's end: maps the file and reads the newest frame.
pub struct FrameReader {
    map: memmap2::Mmap,
    width: u32,
    height: u32,
    slot_bytes: usize,
    last_seen: u64,
}

/// Why a frame could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadError {
    /// The file is not a Klipp HDR frame, or the writer has gone.
    NotAFrame,
    /// Written by a Klipp that lays the file out differently.
    Version(u32),
    /// The mapping is shorter than its own header says it should be.
    Truncated,
}

impl FrameReader {
    /// Map the shared frame at `path`.
    pub fn open(path: &Path) -> Result<Self, ReadError> {
        let file = std::fs::File::open(path).map_err(|_| ReadError::NotAFrame)?;
        let map = unsafe { memmap2::Mmap::map(&file).map_err(|_| ReadError::NotAFrame)? };
        if map.len() < DATA_OFFSET {
            return Err(ReadError::Truncated);
        }
        // SAFETY: length checked above.
        let h = unsafe { &*(map.as_ptr() as *const Header) };
        if h.magic.load(Ordering::Acquire) != MAGIC {
            return Err(ReadError::NotAFrame);
        }
        let version = h.version.load(Ordering::Relaxed);
        if version != PROTOCOL_VERSION {
            return Err(ReadError::Version(version));
        }
        let (width, height) = (h.width.load(Ordering::Relaxed), h.height.load(Ordering::Relaxed));
        let slot = h.slot_bytes.load(Ordering::Relaxed) as usize;
        if width == 0 || height == 0 || slot != slot_bytes(width, height) {
            return Err(ReadError::NotAFrame);
        }
        if map.len() < DATA_OFFSET + SLOTS * slot {
            return Err(ReadError::Truncated);
        }
        Ok(Self { map, width, height, slot_bytes: slot, last_seen: 0 })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The master's peak as the writer recorded it, cd/m².
    pub fn peak_nits(&self) -> f32 {
        self.header().peak_nits.load(Ordering::Relaxed) as f32
    }

    /// Whether the writer is still there. A reader that keeps presenting a
    /// frame from a closed writer shows a still picture forever.
    pub fn writer_present(&self) -> bool {
        self.header().magic.load(Ordering::Acquire) == MAGIC
    }

    fn header(&self) -> &Header {
        // SAFETY: `open` checked the mapping is at least DATA_OFFSET.
        unsafe { &*(self.map.as_ptr() as *const Header) }
    }

    /// Copy the newest published frame into `dst`, if it is newer than the
    /// last one this reader took.
    ///
    /// `false` when nothing new has been published, or every attempt
    /// landed mid-publish — the caller keeps showing the frame it has.
    ///
    /// It *copies*, and the copy is the point. An earlier version handed
    /// back a slice borrowed from the mapping after checking the sequence,
    /// which is wrong in a way that only shows under load: the caller then
    /// reads the bytes *after* the check, and the writer can come round to
    /// that slot in the meantime. `a_reader_never_sees_two_frames_at_once`
    /// caught it doing exactly that — frame 165 with frame 167 through
    /// it — three times in 249 reads. A seqlock only guards what happens
    /// between its two loads, so the copy has to happen there.
    pub fn read_latest(&mut self, dst: &mut Vec<f32>) -> bool {
        for _ in 0..64 {
            let before = self.header().sequence.load(Ordering::Acquire);
            if !before.is_multiple_of(2) {
                // A publish is in flight.
                std::hint::spin_loop();
                continue;
            }
            if before == 0 || before == self.last_seen {
                return false;
            }
            let slot = ((before / 2) as usize) % SLOTS;
            let off = DATA_OFFSET + slot * self.slot_bytes;
            dst.resize(self.slot_bytes / 4, 0.0);
            bytemuck::cast_slice_mut(&mut dst[..])
                .copy_from_slice(&self.map[off..off + self.slot_bytes]);
            // Re-read *after* the copy: if the writer moved on, what we
            // have may be a mixture of two frames and is thrown away.
            let after = self.header().sequence.load(Ordering::Acquire);
            if after != before {
                continue;
            }
            self.last_seen = before;
            return true;
        }
        false
    }
}
