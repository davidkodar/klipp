//! A card's presets in the generated stack (M3 step 14): the effect
//! type's built-in presets and the user's, found by its catalog type id,
//! as the hand-written effects tab offers them. A preset is applied by the
//! card's path, its whole stored entry set in one step (`SetPayloadAt`);
//! the current settings are saved as a user preset under the type id.

use super::row::RowCtx;
use crate::state::AppState;
use klipp_core::AppCommand;
use klipp_effects::registry::EffectType;

/// The preset row above a card's parameters, when its type has presets.
pub fn preset_row(ui: &mut egui::Ui, row: &mut RowCtx, ty: &EffectType, state: &AppState) {
    let Some(entry) = row.entry.map(|e| e.payload.clone()) else { return };
    let presets = state.preset_manager.presets_for_type(ty.desc.id);
    // As the old card: no preset row, nor Save, for a type with none.
    if presets.is_empty() {
        return;
    }
    let theme = row.theme;
    let id = ui.make_persistent_id(("generated_presets", row.addr.clone()));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Preset").size(10.0).color(theme.text_dim));
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt(id.with("combo"))
            .selected_text("Select preset...")
            .width((ui.available_width() - 80.0).max(60.0))
            .show_ui(ui, |ui| {
                let (builtins, user): (Vec<&crate::effect_presets::EffectPreset>, Vec<&crate::effect_presets::EffectPreset>) = presets.iter().copied().partition(|p| p.is_builtin);
                for preset in &builtins {
                    if pick(ui, &preset.name, &preset.description) {
                        apply(row, preset.builtin.as_ref());
                    }
                }
                if !builtins.is_empty() && !user.is_empty() {
                    ui.separator();
                }
                for preset in &user {
                    ui.horizontal(|ui| {
                        if pick(ui, &preset.name, &preset.description) {
                            apply(row, preset.builtin.as_ref());
                        }
                        if ui.small_button("\u{00d7}").on_hover_text("Delete preset").clicked() {
                            row.cmds.push(AppCommand::DeleteBuiltinEffectPreset { effect_key: preset.effect_key.clone(), name: preset.name.clone() });
                        }
                    });
                }
            });
        save_button(ui, row, ty, &entry, id);
    });
}

/// A preset's entry in the list: whether it was picked.
fn pick(ui: &mut egui::Ui, name: &str, description: &str) -> bool {
    let r = ui.selectable_label(false, name);
    let r = if description.is_empty() { r } else { r.on_hover_text(description) };
    r.clicked()
}

/// Set the card's node to `preset`'s settings, whole, in one step.
fn apply(row: &mut RowCtx, preset: Option<&klipp_effects::BuiltinEffect>) {
    let Some(payload) = preset.and_then(|fx| serde_json::to_value(fx).ok()) else { return };
    row.cmds.push(AppCommand::SetPayloadAt { addr: row.addr.clone(), payload });
}

/// "Save..." with a name field: the card's current settings as a user
/// preset under its type id.
fn save_button(ui: &mut egui::Ui, row: &mut RowCtx, ty: &EffectType, entry: &serde_json::Value, id: egui::Id) {
    let popup = id.with("save");
    let button = ui.small_button("Save...");
    if button.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup));
    }
    egui::popup_below_widget(ui, popup, &button, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(200.0);
        ui.label(egui::RichText::new("Save Preset").size(11.0).strong());
        let buffer = id.with("name");
        let mut name: String = ui.data_mut(|d| d.get_temp(buffer).unwrap_or_default());
        ui.horizontal(|ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut name);
        });
        ui.data_mut(|d| d.insert_temp(buffer, name.clone()));
        let current = serde_json::from_value::<klipp_effects::BuiltinEffect>(entry.clone()).ok();
        let ok = !name.trim().is_empty() && current.is_some();
        if ui.add_enabled(ok, egui::Button::new("Save")).clicked() {
            if let Some(effect) = current {
                row.cmds.push(AppCommand::SaveBuiltinEffectPreset { name: name.trim().to_owned(), description: String::new(), effect_key: ty.desc.id.to_owned(), effect });
                ui.data_mut(|d| d.remove::<String>(buffer));
                ui.memory_mut(|m| m.close_popup());
            }
        }
    });
}
