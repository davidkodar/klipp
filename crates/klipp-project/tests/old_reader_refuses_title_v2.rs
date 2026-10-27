//! The reader from before layers carried stacks of their own, frozen
//! here as it stands on the branch this change is built on: the clip
//! kinds it knows, under the tag it reads them by. A file this change
//! writes for a title with a stack is refused by it, by name — not read
//! as a still, bare title. A file for a title without one is read.
//!
//! This is the real pre-change type, copied, not a reader pretending to
//! be older; when `ClipKindJson` changes again, this copy stays as it
//! was, which is the point.

use klipp_media::MediaPool;
use klipp_project::save_project;
use klipp_timeline::*;

fn default_one_f32() -> f32 { 1.0 }
fn default_font_size() -> f32 { 72.0 }
fn default_font_name() -> String { "sans-serif".to_string() }
fn default_font_style() -> String { "Regular".to_string() }
fn default_color_white() -> [u8; 4] { [255, 255, 255, 255] }
fn default_alignment() -> String { "Center".to_string() }
fn default_half() -> f32 { 0.5 }
fn default_true() -> bool { true }

#[allow(dead_code)]
mod before {
    use serde::{Deserialize, Serialize};
    use super::*;
    #[allow(clippy::large_enum_variant)]
    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    pub enum ClipKindJson {
        Media {
            media_id: String,
        },
        Title {
            /// The whole `TitleData`, verbatim: every object, its transform,
            /// the animation, the background. The flat fields that follow are
            /// the legacy single-text shape, still written so older readers
            /// open the file; a reader that finds `title_data` uses it and
            /// ignores them. Before this the flat shape was all that was
            /// saved, and a title lost its animation and every object after
            /// the first on each reload.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            title_data: Option<serde_json::Value>,
            text: String,
            #[serde(default = "default_font_size")]
            font_size: f32,
            #[serde(default = "default_font_name")]
            font_name: String,
            #[serde(default = "default_font_style")]
            font_style: String,
            #[serde(default = "default_color_white")]
            color: [u8; 4],
            #[serde(default = "default_alignment")]
            alignment: String,
            #[serde(default = "default_half")]
            position_x: f32,
            #[serde(default = "default_half")]
            position_y: f32,
            #[serde(default)]
            background: serde_json::Value,
            #[serde(default)]
            shadow: serde_json::Value,
            /// Full drop-shadow stack. Optional so legacy projects with
            /// only the `shadow` field continue to load — load.rs falls
            /// back to synthesising a single-element vec from the legacy
            /// field when this is empty/absent.
            #[serde(default)]
            shadows: serde_json::Value,
            /// Soft-disable for the secondary shadow stack (`shadows[1..]`).
            /// Defaults true so legacy projects render unchanged.
            #[serde(default = "default_true")]
            shadow_stack_enabled: bool,
            #[serde(default)]
            outline: serde_json::Value,
            #[serde(default)]
            outlines: serde_json::Value,
            /// Soft-disable for the secondary outline stack
            /// (`outlines[1..]`). Defaults to true so projects saved
            /// before this field existed render the stack as before.
            #[serde(default = "default_true")]
            outline_stack_enabled: bool,
            #[serde(default)]
            glow: serde_json::Value,
            /// Full glow stack (canonical). Optional so legacy projects
            /// with only the singular `glow` continue to load.
            #[serde(default)]
            glows: serde_json::Value,
            /// Soft-disable for the secondary glow stack.
            #[serde(default = "default_true")]
            glow_stack_enabled: bool,
            #[serde(default)]
            gradient: serde_json::Value,
            #[serde(default)]
            inner_shadow: serde_json::Value,
            /// Full inner-shadow stack (canonical). Optional so legacy
            /// projects with only the singular `inner_shadow` continue
            /// to load — load.rs falls back to wrapping the legacy field
            /// as a one-element vec.
            #[serde(default)]
            inner_shadows: serde_json::Value,
            /// Soft-disable for the secondary inner-shadow stack.
            #[serde(default = "default_true")]
            inner_shadow_stack_enabled: bool,
            #[serde(default)]
            pattern_fill: serde_json::Value,
            #[serde(default)]
            tracking: f32,
            #[serde(default = "default_one_f32")]
            line_spacing: f32,
            #[serde(default = "default_one_f32")]
            write_on: f32,
            #[serde(default)]
            extrusion: serde_json::Value,
            #[serde(default)]
            text_path: serde_json::Value,
            #[serde(default)]
            char_animation: serde_json::Value,
            /// Legacy texture path — kept for downgrade compatibility
            /// and read by load.rs when no `texture_fill` struct is
            /// present. New saves still populate this when the source
            /// is a file so older Klipp versions can keep rendering.
            #[serde(default)]
            texture_fill_path: Option<String>,
            /// Full TextureFill struct (source / tile_mode / scale /
            /// offset / rotation / opacity / tint). Optional so legacy
            /// projects that only have `texture_fill_path` still load.
            #[serde(default)]
            texture_fill: serde_json::Value,
            #[serde(default)]
            motion_blur: serde_json::Value,
            /// Per-character style runs for rich text.
            #[serde(default)]
            style_runs: serde_json::Value,
        },
        Gap,
        Sequence {
            sequence_id: String,
        },
        Generator {
            #[serde(default)]
            data: serde_json::Value,
        },
        AdjustmentLayer,
        /// Explicit feature tag: older readers must refuse editable path titles.
        VectorTitle { title_data: serde_json::Value },
    }
}

fn saved_clip_kind(objects: Vec<(TitleObjectContent, LayerFx)>) -> serde_json::Value {
    // A directory of its own per call: the tests run in parallel and
    // one removing a shared one while another writes it read as a
    // missing file.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("klipp_old_reader_{}_{call}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut td = TitleData { objects: Vec::new(), next_object_id: 0, animation_type: Default::default(), background: TitleBackground::None, style: None, published: Vec::new(), template: None, protect: Default::default() };
    for (content, fx) in objects {
        let id = td.add_object(content, default_text_transform());
        td.object_mut(id).unwrap().fx = Some(fx);
    }
    let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
    let mut clip = Clip::gap(tc(0.0), tc(4.0));
    clip.kind = ClipKind::Title(td);
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let file: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    file["sequences"][0]["tracks"][0]["clips"][0]["kind"].clone()
}

fn text() -> TitleObjectContent {
    TitleObjectContent::TextBox(TextBox { text: "Hello".into(), ..Default::default() })
}

#[test]
fn the_reader_from_before_refuses_a_title_with_a_stack_by_name() {
    let lit = LayerFx {
        glows: vec![TextGlow { enabled: true, color: [255; 4], radius: 10.0, radius_keyframes: vec![], intensity: 1.0, intensity_keyframes: vec![] }],
        ..Default::default()
    };
    let kind = saved_clip_kind(vec![(text(), lit)]);
    assert_eq!(kind["type"], "title_v2");
    let err = serde_json::from_value::<before::ClipKindJson>(kind).unwrap_err().to_string();
    assert!(err.contains("title_v2"), "refused by name: {err}");
}

#[test]
fn the_reader_from_before_still_reads_a_title_without_one() {
    let kind = saved_clip_kind(vec![(text(), LayerFx::default())]);
    assert_eq!(kind["type"], "title");
    assert!(matches!(serde_json::from_value::<before::ClipKindJson>(kind), Ok(before::ClipKindJson::Title { .. })));

    let path = VectorPath { trim: None, ..Default::default() };
    let shape = TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(path), fill: None, gradient: None, stroke: Some(ShapeStroke { color: [255; 4], width: 2.0 }), fit: None });
    let kind = saved_clip_kind(vec![(shape, LayerFx::default())]);
    assert_eq!(kind["type"], "vector_title");
    assert!(matches!(serde_json::from_value::<before::ClipKindJson>(kind), Ok(before::ClipKindJson::VectorTitle { .. })));
}
