//! A title layer's blend and mask in the project file (#1513): they
//! come back from a save as they went in, the file says the title needs
//! a reader that knows them, and a title without them is written as it
//! always was.

use klipp_core::BlendMode;
use klipp_core::Timecode;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::*;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A word over a band, the band screened and the word masked by it.
fn title(mixed: bool) -> TitleData {
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    let band = td.add_object(TitleObjectContent::Shape(Shape { kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fill: Some([200, 40, 40, 255]), gradient: None, stroke: None, fit: None }), default_text_transform());
    let word = td.add_object(TitleObjectContent::TextBox(TextBox { text: "Word".into(), ..Default::default() }), default_text_transform());
    if mixed {
        td.object_mut(band).unwrap().blend = BlendMode::Screen;
        td.object_mut(word).unwrap().mask = Some(LayerMask { source: band, kind: LayerMaskKind::InvertedLuma });
    }
    td
}

fn sequence(td: TitleData) -> Sequence {
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::gap(tc(0.0), tc(4.0));
    clip.kind = ClipKind::Title(td);
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip];
    seq
}

fn written_kind(dir: &std::path::Path) -> serde_json::Value {
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    written["sequences"][0]["tracks"][0]["clips"][0]["kind"].clone()
}

fn reloaded(dir: &std::path::Path) -> TitleData {
    let (sequences, ..) = load_project_strict(dir).unwrap_or_else(|e| panic!("a strict reload: {e}"));
    let ClipKind::Title(td) = &sequences[0].tracks[0].clips[0].kind else { panic!("a title") };
    td.clone()
}

#[test]
fn a_blend_and_a_mask_come_back_from_a_save() {
    let dir = scratch("title_blend_mask");
    let td = title(true);
    save_project(&dir, &[sequence(td.clone())], &MediaPool::new()).unwrap();
    assert_eq!(written_kind(&dir)["type"], "title_v2", "a reader that knows no blends refuses the title by its tag");
    let back = reloaded(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(back.objects[0].blend, BlendMode::Screen);
    assert_eq!(back.objects[1].mask, Some(LayerMask { source: back.objects[0].id, kind: LayerMaskKind::InvertedLuma }));
    assert_eq!(back.objects[0].mask, None);
    assert_eq!(back.objects[1].blend, BlendMode::Normal);
}

/// Normal and no mask are not written: the file is the one a title
/// without them always made.
#[test]
fn a_title_without_them_is_written_as_before() {
    let dir = scratch("title_no_blend_mask");
    save_project(&dir, &[sequence(title(false))], &MediaPool::new()).unwrap();
    let kind = written_kind(&dir);
    let text = serde_json::to_string(&kind).unwrap();
    assert_eq!(kind["type"], "title");
    assert!(!text.contains("\"blend\"") && !text.contains("\"mask\""), "nothing written for them: {text}");
    let back = reloaded(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(back.objects.iter().all(|o| o.blend == BlendMode::Normal && o.mask.is_none()));
}
