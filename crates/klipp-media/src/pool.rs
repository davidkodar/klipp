use crate::item::{Bin, MediaItem};
use klipp_core::{BinId, MediaId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaPool {
    items: HashMap<MediaId, MediaItem>,
    #[serde(default)]
    pub bins: Vec<Bin>,
    /// The saved smart bins: rules that gather clips.
    #[serde(default)]
    pub smart_bins: Vec<klipp_core::SmartBin>,
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

    /// Whether the picture of `media` decodes scene-referred (HDR or
    /// camera log): what sets where a new grade's zones go (#2767). A clip
    /// with no media, a generator or a title, is display-referred.
    pub fn scene_referred(&self, media: Option<MediaId>) -> bool {
        media.and_then(|m| self.get(&m)).is_some_and(MediaItem::scene_referred)
    }

    pub fn get_mut(&mut self, id: &MediaId) -> Option<&mut MediaItem> {
        self.items.get_mut(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &MediaItem> {
        self.items.values()
    }

    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut MediaItem> {
        self.items.values_mut()
    }

    pub fn remove(&mut self, id: &MediaId) -> Option<MediaItem> {
        self.items.remove(id)
    }

    /// Find a media item by its absolute file path.
    pub fn find_by_path(&self, path: &Path) -> Option<&MediaItem> {
        self.items.values().find(|item| item.path == path)
    }

    /// Find a media item by content fingerprint (FNV-1a of first 64KB + file_size).
    pub fn find_by_fingerprint(&self, fp: u64) -> Option<&MediaItem> {
        self.items.values().find(|item| item.fingerprint == Some(fp))
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Find a bin by ID.
    pub fn find_bin(&self, id: &BinId) -> Option<&Bin> {
        self.bins.iter().find(|b| b.id == *id)
    }

    /// Find or create a root-level bin with the given name. Returns its BinId.
    pub fn find_or_create_bin(&mut self, name: &str) -> BinId {
        if let Some(bin) = self.bins.iter().find(|b| b.name == name && b.parent_id.is_none()) {
            bin.id
        } else {
            let bin = Bin {
                id: BinId::new(),
                name: name.to_string(),
                parent_id: None,
            };
            let id = bin.id;
            self.bins.push(bin);
            id
        }
    }

    /// "New Bin", or else "New Bin 2", "New Bin 3"…: the first name no
    /// bin has, nor any item's bin.
    pub fn unused_bin_name(&self) -> String {
        let taken = |name: &str| self.bins.iter().any(|b| b.name == name) || self.all().any(|m| m.bin.as_deref() == Some(name));
        std::iter::once("New Bin".to_string())
            .chain((2..).map(|n| format!("New Bin {n}")))
            .find(|name| !taken(name))
            .expect("an unused name")
    }

    /// Get the display name for a bin_id, walking up to build "Parent / Child" path.
    pub fn bin_display_name(&self, bin_id: &BinId) -> String {
        let mut parts = Vec::new();
        let mut current = Some(*bin_id);
        while let Some(id) = current {
            if let Some(bin) = self.find_bin(&id) {
                parts.push(bin.name.clone());
                current = bin.parent_id;
            } else {
                break;
            }
        }
        parts.reverse();
        parts.join(" / ")
    }
}

#[cfg(test)]
mod bin_name_tests {
    use super::*;

    /// A new bin is named apart from the bins there are and from the
    /// bins items name.
    #[test]
    fn a_new_bins_name_is_one_no_bin_has() {
        let mut pool = MediaPool::new();
        assert_eq!(pool.unused_bin_name(), "New Bin");
        pool.find_or_create_bin("New Bin");
        assert_eq!(pool.unused_bin_name(), "New Bin 2");
        let mut item = MediaItem::synthetic_for_test("/footage/a.mov".into(), None);
        item.bin = Some("New Bin 2".into());
        pool.add(item);
        assert_eq!(pool.unused_bin_name(), "New Bin 3");
    }
}
