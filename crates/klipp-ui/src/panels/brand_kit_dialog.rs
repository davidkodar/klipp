//! Timeline → Apply Brand Kit… (#2722): a kit, and which of its parts
//! the open sequence takes — its look, its caption look, its intro, its
//! outro, its logo as a watermark.

use crate::state::{AppState, BrandKitParts};

/// One kit as the dialog lists it: its name, and what it has.
#[derive(Debug, Clone)]
struct Listed {
    name: String,
    builtin: bool,
    has: [bool; 5],
}

/// The kits as found when the dialog opened, kept in egui's memory
/// until it closes so the folder is read once.
fn kits(ctx: &egui::Context) -> Vec<Listed> {
    let id = egui::Id::new("apply_brand_kit_list");
    if let Some(k) = ctx.data(|d| d.get_temp::<std::sync::Arc<Vec<Listed>>>(id)) {
        return (*k).clone();
    }
    let found = klipp_project::brand_kit::list(klipp_timeline::template_places::data_root().as_deref());
    let listed: Vec<Listed> = found
        .into_iter()
        .map(|f| {
            let k = &f.kit;
            let look = !k.title_styles().is_empty() || k.colours.primary.is_some();
            Listed { name: k.name.clone(), builtin: f.source == klipp_project::brand_kit::Source::Builtin, has: [look, k.caption_look().is_some(), k.intro.is_some(), k.outro.is_some(), k.logo.is_some()] }
        })
        .collect();
    ctx.data_mut(|d| d.insert_temp(id, std::sync::Arc::new(listed.clone())));
    listed
}

/// The dialog, while `state.show_apply_brand_kit` is set: the kit and
/// parts to take in when Apply is pressed.
pub fn show(ctx: &egui::Context, state: &mut AppState) -> Option<(String, BrandKitParts)> {
    let listed = kits(ctx);
    let (name, parts) = &mut state.apply_brand_kit;
    if !listed.iter().any(|k| k.name == *name) {
        *name = listed.first().map(|k| k.name.clone()).unwrap_or_default();
    }
    let has = listed.iter().find(|k| k.name == *name).map_or([false; 5], |k| k.has);
    let mut open = true;
    let mut apply = None;
    egui::Window::new("Apply Brand Kit")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            if listed.is_empty() {
                ui.label("There are no brand kits. Make one in Preferences \u{2192} Brand Kits.");
                return;
            }
            ui.horizontal(|ui| {
                ui.label("Kit");
                egui::ComboBox::from_id_salt("apply_brand_kit_kit").selected_text(name.as_str()).width(220.0).show_ui(ui, |ui| {
                    for k in &listed {
                        let label = if k.builtin { format!("{} (starter)", k.name) } else { k.name.clone() };
                        ui.selectable_value(name, k.name.clone(), label);
                    }
                });
            });
            ui.separator();
            let rows: [(&mut bool, &str, &str); 5] = [
                (&mut parts.look, "Look", "Its colours and title looks in the brief; titles that follow a look change with it"),
                (&mut parts.captions, "Captions", "Its caption look on every subtitle track without one of its own"),
                (&mut parts.intro, "Intro", "Its intro at the start, everything else moved on"),
                (&mut parts.outro, "Outro", "Its outro after the last clip"),
                (&mut parts.watermark, "Logo watermark", "Its logo in its corner on a track of its own, over the whole piece"),
            ];
            for ((tick, label, hover), has) in rows.into_iter().zip(has) {
                let r = ui.add_enabled(has, egui::Checkbox::new(tick, label));
                if has {
                    r.on_hover_text(hover);
                } else {
                    r.on_disabled_hover_text("This kit has none");
                }
            }
            ui.separator();
            let chosen = BrandKitParts { look: parts.look && has[0], captions: parts.captions && has[1], intro: parts.intro && has[2], outro: parts.outro && has[3], watermark: parts.watermark && has[4] };
            let any = chosen != BrandKitParts { look: false, captions: false, intro: false, outro: false, watermark: false };
            ui.horizontal(|ui| {
                if ui.add_enabled(any, egui::Button::new("Apply")).on_disabled_hover_text("Tick a part the kit has").clicked() {
                    apply = Some((name.clone(), chosen));
                }
                ui.label(egui::RichText::new("Each part can be undone.").weak());
            });
        });
    if apply.is_some() {
        open = false;
    }
    if !open {
        ctx.data_mut(|d| d.remove::<std::sync::Arc<Vec<Listed>>>(egui::Id::new("apply_brand_kit_list")));
    }
    state.show_apply_brand_kit = open;
    apply
}
