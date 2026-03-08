use klipp_core::MediaId;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum ThumbnailEntry {
    Pending,
    Ready { rgba: Vec<u8>, width: u32, height: u32 },
    Failed,
}

/// A single filmstrip frame for timeline display.
#[derive(Debug, Clone)]
pub struct FilmstripFrame {
    pub time_secs: f64,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Default)]
pub struct ThumbnailCache {
    cache: HashMap<MediaId, ThumbnailEntry>,
    filmstrips: HashMap<MediaId, Vec<FilmstripFrame>>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &MediaId) -> Option<&ThumbnailEntry> {
        self.cache.get(id)
    }

    pub fn set_pending(&mut self, id: MediaId) {
        self.cache.insert(id, ThumbnailEntry::Pending);
    }

    pub fn set_ready(&mut self, id: MediaId, rgba: Vec<u8>, width: u32, height: u32) {
        self.cache.insert(id, ThumbnailEntry::Ready { rgba, width, height });
    }

    pub fn set_failed(&mut self, id: MediaId) {
        self.cache.insert(id, ThumbnailEntry::Failed);
    }

    pub fn filmstrip(&self, id: &MediaId) -> Option<&[FilmstripFrame]> {
        self.filmstrips.get(id).map(|v| v.as_slice())
    }

    pub fn set_filmstrip(&mut self, id: MediaId, frames: Vec<FilmstripFrame>) {
        self.filmstrips.insert(id, frames);
    }

    pub fn push_filmstrip_frame(&mut self, id: MediaId, frame: FilmstripFrame) {
        let frames = self.filmstrips.entry(id).or_default();
        // Insert sorted by time; skip if a frame at this time already exists
        let time = frame.time_secs;
        match frames.binary_search_by(|f| f.time_secs.partial_cmp(&time).unwrap_or(std::cmp::Ordering::Equal)) {
            Ok(_) => {} // already have a frame at this time
            Err(pos) => frames.insert(pos, frame),
        }
    }

    /// Returns the minimum gap (in seconds) between adjacent filmstrip frames
    /// for a given media. Used to decide if refinement is needed.
    pub fn filmstrip_min_interval(&self, id: &MediaId) -> Option<f64> {
        let frames = self.filmstrips.get(id)?;
        if frames.len() < 2 {
            return None;
        }
        let mut min_gap = f64::MAX;
        for w in frames.windows(2) {
            let gap = w[1].time_secs - w[0].time_secs;
            if gap < min_gap {
                min_gap = gap;
            }
        }
        Some(min_gap)
    }
}
