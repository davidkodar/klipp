//! A frei0r effect's editor (M3 step 6): the plug-in's own parameters,
//! typed by what it declares, drawn by the effects tab's body and written
//! as the effect's entry with its values changed.

use crate::inspector::effects_tab::frei0r_body;
use crate::inspector::generated::custom::CustomEditor;
use crate::inspector::generated::RowCtx;
use klipp_core::AppCommand;
use klipp_effects::registry::ParamDesc;
use serde_json::Value;

pub struct Frei0rEditor;

impl CustomEditor for Frei0rEditor {
    fn show(&self, ui: &mut egui::Ui, ctx: &mut RowCtx, _desc: &ParamDesc, _stored: &Value) {
        let (Some(entry), Some(app)) = (ctx.entry, ctx.app) else {
            ui.weak("Plug-in values are edited with the clip open");
            return;
        };
        let type_id = entry.payload.get("type_id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let Ok(mut params) = serde_json::from_value::<Vec<klipp_frei0r::Frei0rValue>>(entry.payload.get("params").cloned().unwrap_or(Value::Array(Vec::new()))) else {
            ui.weak("The plug-in's values can't be read");
            return;
        };
        if !frei0r_body(ui, ctx.theme, app, &type_id, &mut params) {
            return;
        }
        let mut payload = entry.payload.clone();
        payload["params"] = serde_json::to_value(&params).unwrap_or(Value::Array(Vec::new()));
        ctx.cmds.push(AppCommand::SetPayloadAt { addr: ctx.addr.clone(), payload });
    }
}
