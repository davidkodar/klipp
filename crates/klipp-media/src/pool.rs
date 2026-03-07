use crate::item::MediaItem;
use klipp_core::MediaId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaPool {
    items: HashMap<MediaId, MediaItem>,
}

impl MediaPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, item: MediaItem) -> MediaId {
        let id = item.id;
        self.items.insert(id, item);
        id
    }

    pub fn get(&self, id: &MediaId) -> Option<&MediaItem> {
        self.items.get(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &MediaItem> {
        self.items.values()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}
