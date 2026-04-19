//! Regression test for TitleData::texture_fill_path portability.
//!
//! Before the fix, texture_fill_path was serialized as an absolute string
//! (`data.texture_fill_path.clone()`), which meant title clips with texture
//! fills would break when a project was moved between machines. This test
//! verifies that paths are now stored relative to the project directory
//! and resolve correctly when the project is moved.

use klipp_core::{ClipId, RationalTime};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::{Clip, ClipKind, Sequence, TitleData, Track, TrackKind};

fn make_title_clip_with_texture(texture_path: &str) -> Clip {
    let ti = RationalTime::from_frames(0, 24);
    let to = RationalTime::from_frames(48, 24);
    Clip {
        id: ClipId::new(),
        kind: ClipKind::Title(TitleData {
            texture_fill_path: Some(texture_path.to_string()),
            ..TitleData::default()
        }),
        timeline_in: ti,
        timeline_out: to,
        source_in: RationalTime::from_frames(0, 24),
        source_out: to - ti,
        ..Clip::gap(ti, to)
    }
}

fn make_sequence_with_title(texture_path: &str) -> Sequence {
    let mut seq = Sequence::new_default();
    let mut track = Track::new("Text 1", TrackKind::Text);
    track.clips.push(make_title_clip_with_texture(texture_path));
    seq.tracks.push(track);
    seq
}

#[test]
fn texture_fill_path_round_trips_through_save_load() {
    // Create a temp dir for the project, and a "texture" file inside it.
    let project_dir = std::env::temp_dir().join(format!(
        "klipp_texture_test_{}",
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&project_dir);
    std::fs::create_dir_all(&project_dir).unwrap();

    let texture_path = project_dir.join("pattern.png");
    std::fs::write(&texture_path, b"fake png").unwrap();

    // Build a sequence referencing the texture by absolute path (runtime convention)
    let seq = make_sequence_with_title(texture_path.to_str().unwrap());
    let pool = MediaPool::new();
    let color = ColorSettings::default();

    // Save
    save_project_with_color(&project_dir, &[seq], &pool, &color, None, None, "").unwrap();

    // Load back
    let (sequences, _pool, _color, _active, _created, _notes, _zoom, _scroll) = load_project(&project_dir).unwrap();
    let loaded_seq = &sequences[0];
    let text_track = loaded_seq.tracks.iter()
        .find(|t| matches!(t.kind, TrackKind::Text))
        .expect("text track should exist");
    let clip = &text_track.clips[0];
    let loaded_path = match &clip.kind {
        ClipKind::Title(td) => td.texture_fill_path.clone().expect("texture path should be set"),
        _ => panic!("expected title clip"),
    };

    // The loaded path should point at the actual texture file on disk.
    assert_eq!(
        std::path::Path::new(&loaded_path),
        texture_path.as_path(),
        "loaded texture path should match original absolute path"
    );
    assert!(
        std::path::Path::new(&loaded_path).exists(),
        "loaded path must point at an actual file",
    );

    let _ = std::fs::remove_dir_all(&project_dir);
}

#[test]
fn texture_fill_path_survives_project_directory_move() {
    // Save in one directory, then rename the directory, then load.
    // The texture (kept inside the project dir) must still resolve.
    let original_dir = std::env::temp_dir().join(format!(
        "klipp_texture_move_src_{}",
        std::process::id(),
    ));
    let moved_dir = std::env::temp_dir().join(format!(
        "klipp_texture_move_dst_{}",
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&original_dir);
    let _ = std::fs::remove_dir_all(&moved_dir);
    std::fs::create_dir_all(&original_dir).unwrap();

    let texture_orig = original_dir.join("pattern.png");
    std::fs::write(&texture_orig, b"fake png").unwrap();

    let seq = make_sequence_with_title(texture_orig.to_str().unwrap());
    let pool = MediaPool::new();
    save_project_with_color(&original_dir, &[seq], &pool, &ColorSettings::default(), None, None, "").unwrap();

    // Move the entire project directory (texture travels inside it)
    std::fs::rename(&original_dir, &moved_dir).unwrap();

    // Load from the new location
    let (sequences, _pool, _color, _active, _created, _notes, _zoom, _scroll) = load_project(&moved_dir).unwrap();
    let text_track = sequences[0].tracks.iter()
        .find(|t| matches!(t.kind, TrackKind::Text))
        .expect("text track should exist");
    let clip = &text_track.clips[0];
    let loaded_path = match &clip.kind {
        ClipKind::Title(td) => td.texture_fill_path.clone().expect("texture path should be set"),
        _ => panic!("expected title clip"),
    };

    // The texture should now resolve under the moved directory.
    let expected = moved_dir.join("pattern.png");
    assert_eq!(
        std::path::Path::new(&loaded_path),
        expected.as_path(),
        "texture path should resolve under moved project dir",
    );
    assert!(std::path::Path::new(&loaded_path).exists());

    let _ = std::fs::remove_dir_all(&moved_dir);
}
