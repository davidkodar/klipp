//! The app's handlers driven as the app drives them: an app built by
//! [`KlippApp::for_test`], commands through `send_commands`, and what the
//! state became and what went to the engine read back. The first of the
//! handler tests that had to copy the dispatch by hand before.

use super::*;

/// A file's rating set from the media pool is one project step: the pool
/// holds it, ctrl+z names it and takes it back, redo puts it again, and a
/// rating it already has is no step at all.
#[test]
fn a_rating_is_one_step_undo_takes_back() {
    let (mut app, engine) = KlippApp::for_test();
    let id = app.state.document.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/media/a.mov".into(), None));
    let rating = |app: &KlippApp| app.state.document.media_pool.get(&id).unwrap().rating;

    app.send_commands(vec![AppCommand::SetMediaRating { media_id: id, rating: 4 }]);
    assert_eq!(rating(&app), 4);
    assert!(app.state.dirty, "the project is unsaved");
    assert_eq!(app.state.undo_depth(), 1);

    app.send_commands(vec![AppCommand::SetMediaRating { media_id: id, rating: 4 }]);
    assert_eq!(app.state.undo_depth(), 1, "the same rating again is no step");

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(rating(&app), 0);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(rating(&app), 4);
    // The pool's record: nothing of it goes to the engine.
    assert!(!engine.commands.try_iter().any(|c| matches!(c, AppCommand::SetMediaRating { .. })));
}

/// Files renamed together are one step, and a bin renamed takes its files'
/// name for it along; undo puts both back.
#[test]
fn renames_are_steps_undo_takes_back() {
    let (mut app, _engine) = KlippApp::for_test();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/a.mov".into(), None);
    item.filename = "a.mov".into();
    item.bin = Some("Rushes".into());
    let id = app.state.document.media_pool.add(item);
    app.state.document.media_pool.bins.push(klipp_media::Bin { id: klipp_core::BinId::new(), name: "Rushes".into(), parent_id: None });

    app.send_commands(vec![AppCommand::BatchRenameMediaItems { changes: vec![(id, "Interview.mov".into())] }]);
    app.send_commands(vec![AppCommand::RenameBin { old_name: "Rushes".into(), new_name: "Selects".into() }]);
    let item = app.state.document.media_pool.get(&id).unwrap();
    assert_eq!((item.filename.as_str(), item.bin.as_deref(), app.state.document.media_pool.bins[0].name.as_str()), ("Interview.mov", Some("Selects"), "Selects"));
    assert_eq!(app.state.undo_depth(), 2);

    app.send_commands(vec![AppCommand::Undo, AppCommand::Undo]);
    let item = app.state.document.media_pool.get(&id).unwrap();
    assert_eq!((item.filename.as_str(), item.bin.as_deref(), app.state.document.media_pool.bins[0].name.as_str()), ("a.mov", Some("Rushes"), "Rushes"));
}
