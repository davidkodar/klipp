//! The media pool's own edits (rating, tags, colour label, renames, a
//! bin's name) applied as the app's commit applies them and taken back as
//! its undo does: forward sets the new value, inverse the old, on the
//! document alone.

use klipp_core::{Fps, MediaId};
use klipp_document::apply::DocumentView;
use klipp_document::history::EditCommand;
use klipp_document::Document;

fn document_with_one_item() -> (Document, MediaId) {
    let mut d = Document::default();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/a.mov".into(), None);
    item.filename = "a.mov".into();
    item.bin = Some("Rushes".into());
    let id = d.media_pool.add(item);
    d.media_pool.bins.push(klipp_media::Bin { id: klipp_core::BinId::new(), name: "Rushes".into(), parent_id: None });
    (d, id)
}

fn forward_then_back(d: &mut Document, edit: &EditCommand) -> (klipp_media::MediaItem, klipp_media::MediaItem) {
    let mut fps = Fps::film_24();
    let id = d.media_pool.all().next().unwrap().id;
    let mut view = DocumentView { sequences: &mut d.sequences, active: 0, media_pool: &mut d.media_pool, fps: &mut fps };
    view.apply_forward(edit);
    let after = view.media_pool.get(&id).unwrap().clone();
    view.apply_inverse(edit);
    (after, view.media_pool.get(&id).unwrap().clone())
}

#[test]
fn each_pool_edit_goes_forward_and_back() {
    let (mut d, id) = document_with_one_item();
    let (after, back) = forward_then_back(&mut d, &EditCommand::SetMediaRating { media_id: id, old_rating: 0, new_rating: 4 });
    assert_eq!((after.rating, back.rating), (4, 0));
    let (after, back) = forward_then_back(&mut d, &EditCommand::SetMediaTags { media_id: id, old_tags: vec![], new_tags: vec!["b-roll".into()] });
    assert_eq!((after.tags, back.tags), (vec!["b-roll".to_string()], vec![]));
    let (after, back) = forward_then_back(&mut d, &EditCommand::SetMediaColorLabel { media_id: id, old_label: 0, new_label: 3 });
    assert_eq!((after.color_label.to_u8(), back.color_label.to_u8()), (3, 0));
    let (after, back) = forward_then_back(&mut d, &EditCommand::BatchRenameMediaItems { changes: vec![(id, "a.mov".into(), "Interview.mov".into())] });
    assert_eq!((after.filename.as_str(), back.filename.as_str()), ("Interview.mov", "a.mov"));
    let (after, back) = forward_then_back(&mut d, &EditCommand::RenameBin { old_name: "Rushes".into(), new_name: "Selects".into() });
    assert_eq!((after.bin.as_deref(), back.bin.as_deref()), (Some("Selects"), Some("Rushes")), "the item's name for its bin");
    assert_eq!(d.media_pool.bins[0].name, "Rushes", "and the bin's own record, back");
}
