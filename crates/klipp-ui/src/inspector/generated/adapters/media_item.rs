//! A media source's item (#1746): picked from the pool's video and stills,
//! written as the effect's `media` parameter, the item's id. The pool
//! keeps the item's file, so a relinked file is read without picking again.

use crate::inspector::generated::custom::CustomEditor;
use crate::inspector::generated::RowCtx;
use klipp_core::AppCommand;
use klipp_effects::registry::ParamDesc;
use klipp_graph_format::ParamValue;
use serde_json::Value;

pub struct MediaItemEditor;

/// The pool's items a media source can read: files with a picture.
pub fn pickable(pool: &klipp_media::MediaPool) -> Vec<&klipp_media::MediaItem> {
    let mut items: Vec<_> = pool.all().filter(|m| m.kind == klipp_media::MediaItemKind::File && (m.is_still || !m.video_codec.is_empty())).collect();
    items.sort_by(|a, b| a.filename.cmp(&b.filename));
    items
}

impl CustomEditor for MediaItemEditor {
    fn show(&self, ui: &mut egui::Ui, ctx: &mut RowCtx, _desc: &ParamDesc, stored: &Value) {
        let media = stored.as_str().unwrap_or_default().to_owned();
        let pool = ctx.app.map(|a| &a.media_pool);
        let chosen = pool.and_then(|p| klipp_compose::resolve::media_id(&media).and_then(|id| p.get(&id)));
        let shown = match (media.is_empty(), chosen) {
            (true, _) => "None chosen".to_owned(),
            (false, Some(item)) => item.filename.clone(),
            (false, None) => "Missing item".to_owned(),
        };
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.label("Media");
            egui::ComboBox::from_id_salt(("effect_media", ctx.addr)).selected_text(shown).show_ui(ui, |ui| {
                for item in pool.map(pickable).unwrap_or_default() {
                    let id = item.id.0.to_string();
                    if ui.selectable_label(id == media, &item.filename).on_hover_text(item.path.display().to_string()).clicked() {
                        picked = Some(id);
                    }
                }
            });
        });
        // An item the pool hasn't, or whose file isn't here: the picture is
        // clear, as the viewer says.
        match chosen {
            None if !media.is_empty() => {
                ui.colored_label(ctx.theme.marker_warning, "\u{26a0} Media not in the project").on_hover_text("Its item was removed: the picture is clear until another is chosen");
            }
            Some(item) if !item.path.exists() => {
                ui.colored_label(ctx.theme.marker_warning, "\u{26a0} Media file not found").on_hover_text(format!("{} isn't on this machine: the picture is clear until it's relinked", item.path.display()));
            }
            _ => {}
        }
        if let Some(id) = picked.filter(|id| *id != media) {
            ctx.cmds.push(AppCommand::SetParamAt { addr: ctx.addr.clone(), key: "media".into(), value: ParamValue::Text(id) });
        }
    }
}
