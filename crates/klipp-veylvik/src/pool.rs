//! Media from Klipp's pool in a composition: a footage node named by the
//! media's key in Klipp ([`media_key`]), which Klipp answers with the
//! media decoded as its renders decode it, the file beside it for a reader
//! that doesn't know Klipp. Never a path the composition reads itself: a
//! hosted composition reads only its own folder.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::host_key::HostKey;
use klipp_core::MediaId;
use veylvik_document::{Composition, Node, Operation};

/// What a composition names media `id` of Klipp's pool by.
pub fn media_key(id: MediaId) -> String {
    HostKey::Media(id).name()
}

/// The media of Klipp's pool `key` names, if it names one.
pub fn media_of_key(key: &str) -> Option<MediaId> {
    match HostKey::parse(key)? {
        HostKey::Media(id) => Some(id),
        _ => None,
    }
}

/// The keys of Klipp's pool media `composition` has footage of, as it
/// writes them.
pub fn pool_keys(composition: &Composition) -> BTreeSet<String> {
    crate::host_key::host_names(composition).into_iter().filter_map(|(name, k)| matches!(k, HostKey::Media(_)).then_some(name)).collect()
}

/// The composition in `bytes` with media `id` of Klipp's pool laid over its
/// picture, as Veylvik's own import lays a file: a footage node `label`,
/// fitted to the frame, merged over the output. `file` is its file, kept
/// beside the key.
pub fn with_media(bytes: &[u8], id: MediaId, label: &str, file: &std::path::Path) -> Result<Arc<[u8]>, String> {
    let mut c: Composition = serde_json::from_slice(bytes).map_err(|e| format!("the composition can't be read: {e}"))?;
    let footage = Node::new(
        label,
        Operation::Footage(veylvik_document::nodes::Footage {
            asset: media_key(id),
            timing: Default::default(),
            placement: veylvik_document::media::Placement::Fit,
            colour: Default::default(),
            layer: None,
            fallback: file.to_str().map(str::to_owned).unwrap_or_default(),
        }),
        &[],
    );
    let merge = Node::new(&format!("{label} over"), Operation::Merge(veylvik_document::nodes::Merge {}), &[("background", c.output), ("foreground", footage.id)]);
    c.output = merge.id;
    c.nodes.insert(footage.id, footage);
    c.nodes.insert(merge.id, merge);
    c.validate().map_err(|e| e.to_string())?;
    Ok(serde_json::to_vec(&c).map_err(|e| e.to_string())?.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_media_is_laid_over_the_picture_by_its_key_with_its_file_beside() {
        let c = Composition::from_host_footage(crate::CLIP_MEDIA_KEY, "Clip", None, [64, 36], veylvik_document::Time { ticks: 24, rate: 1 }, 24).unwrap();
        let before = c.output;
        let id = MediaId(uuid::Uuid::from_u128(5));
        let after = with_media(&serde_json::to_vec(&c).unwrap(), id, "Logo.png", std::path::Path::new("/media/Logo.png")).unwrap();
        let c: Composition = serde_json::from_slice(&after).unwrap();
        assert_eq!(c.nodes[&c.output].inputs.get("background"), Some(&before), "the picture as it was, under it");
        assert_eq!(pool_keys(&c), BTreeSet::from([media_key(id)]));
        assert_eq!(media_of_key(&media_key(id)), Some(id));
        assert_eq!(media_of_key(crate::CLIP_MEDIA_KEY), None);
        let fallback = c.nodes.values().find_map(|n| match &n.operation {
            Operation::Footage(f) if f.asset == media_key(id) => Some(f.fallback.clone()),
            _ => None,
        });
        assert_eq!(fallback.as_deref(), Some("/media/Logo.png"));
    }
}
