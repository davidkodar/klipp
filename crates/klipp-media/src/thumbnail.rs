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
}
