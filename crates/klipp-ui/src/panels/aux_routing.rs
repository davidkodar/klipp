//! The mixer's aux buses and sends (#2861): a strip for each bus, beside
//! the tracks' and before master, and each track's sends in a popup from
//! its strip. What the controls change is the sequence's routing, each
//! change one undo step (a drag one step when it ends), as the `aux_bus`
//! and `send` ops change it.

use klipp_core::{AppCommand, BusId, TrackId};
use klipp_timeline::routing::{level_text, route_loops, Routing, MAX_BUSES, MAX_SENDS};
use klipp_timeline::{AuxBus, AuxSend};

use crate::history::{EditCommand, HistoryEntry};
use crate::state::AppState;
use crate::theme::KlippTheme;

/// A new bus's name: the first "Bus n" not taken.
pub fn new_bus_name(r: &Routing) -> String {
    (1..).map(|n| format!("Bus {n}")).find(|name| !r.buses.iter().any(|b| b.name.eq_ignore_ascii_case(name))).unwrap_or_default()
}

/// A new bus at the end, or `None` with as many as a sequence may have.
pub fn add_bus(r: &mut Routing) -> Option<BusId> {
    if r.buses.len() >= MAX_BUSES {
        return None;
    }
    let bus = AuxBus::new(&new_bus_name(r));
    let id = bus.id;
    r.buses.push(bus);
    Some(id)
}

/// Bus `at` taken out, every send to it with it, and a bus that fed it
/// feeding master.
pub fn remove_bus(r: &mut Routing, at: usize) {
    if at >= r.buses.len() {
        return;
    }
    let gone = r.buses.remove(at).id;
    for b in &mut r.buses {
        if b.output == Some(gone) {
            b.output = None;
        }
    }
    for (_, sends) in &mut r.sends {
        sends.retain(|s| s.bus != gone);
    }
    r.sends.retain(|(_, s)| !s.is_empty());
}

/// Track `track`'s sends.
pub fn sends_of(r: &Routing, track: TrackId) -> &[AuxSend] {
    r.sends.iter().find(|(t, _)| *t == track).map_or(&[], |(_, s)| s.as_slice())
}

/// Track `track`'s send to `bus`, added after its fader at 0 dB; `false`
/// when it has as many as it may or sends there already.
pub fn add_send(r: &mut Routing, track: TrackId, bus: BusId) -> bool {
    if !r.sends.iter().any(|(t, _)| *t == track) {
        r.sends.push((track, Vec::new()));
    }
    let sends = &mut r.sends.iter_mut().find(|(t, _)| *t == track).expect("pushed above").1;
    if sends.len() >= MAX_SENDS || sends.iter().any(|s| s.bus == bus) {
        r.sends.retain(|(_, s)| !s.is_empty());
        return false;
    }
    sends.push(AuxSend::new(bus));
    true
}

/// Track `track`'s send to `bus`, to change.
pub fn send_mut(r: &mut Routing, track: TrackId, bus: BusId) -> Option<&mut AuxSend> {
    r.sends.iter_mut().find(|(t, _)| *t == track)?.1.iter_mut().find(|s| s.bus == bus)
}

/// Track `track`'s send to `bus` taken off.
pub fn remove_send(r: &mut Routing, track: TrackId, bus: BusId) {
    for (t, sends) in &mut r.sends {
        if *t == track {
            sends.retain(|s| s.bus != bus);
        }
    }
    r.sends.retain(|(_, s)| !s.is_empty());
}

/// Where bus `at` may go: master, and every bus it doesn't feed.
pub fn outputs_for(r: &Routing, at: usize) -> Vec<Option<BusId>> {
    let me = r.buses[at].id;
    std::iter::once(None).chain(r.buses.iter().filter(|b| !route_loops(&r.buses, me, b.id)).map(|b| Some(b.id))).collect()
}

/// Gain from a fader's decibels, silence at -60 and below.
fn gain(db: f32) -> f32 {
    if db <= -60.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

fn db(gain: f32) -> f32 {
    if gain <= 0.0 { -60.0 } else { (20.0 * gain.log10()).max(-60.0) }
}

/// `change` made to the routing now, one undo step.
fn edit(state: &mut AppState, cmds: &mut Vec<AppCommand>, change: impl FnOnce(&mut Routing)) {
    let old = state.sequence().routing();
    let mut new = old.clone();
    change(&mut new);
    if new != old {
        state.sequence_mut().set_routing(&new);
        state.active_history_mut().push(HistoryEntry { command: EditCommand::SetRouting { old: Box::new(old), new: Box::new(new) } });
        cmds.push(AppCommand::RoutingChanged);
    }
}

/// A control's change: dragged, heard at once and one step when the drag
/// ends; clicked or typed, one step.
fn control(state: &mut AppState, cmds: &mut Vec<AppCommand>, resp: &egui::Response, change: impl FnOnce(&mut Routing)) {
    if resp.changed() {
        if resp.dragged() {
            if state.mixer_routing_drag.is_none() {
                state.mixer_routing_drag = Some(state.sequence().routing());
            }
            let mut r = state.sequence().routing();
            change(&mut r);
            state.sequence_mut().set_routing(&r);
            cmds.push(AppCommand::RoutingChanged);
        } else {
            edit(state, cmds, change);
        }
    }
    if resp.drag_stopped() {
        if let Some(old) = state.mixer_routing_drag.take() {
            let new = state.sequence().routing();
            if new != old {
                state.active_history_mut().push(HistoryEntry { command: EditCommand::SetRouting { old: Box::new(old), new: Box::new(new) } });
                cmds.push(AppCommand::RoutingChanged);
            }
        }
    }
}

/// What the effect card asks of bus `at`'s chain, drawn for it under the
/// clip id `stand_in`: a value moved with a drag is heard at once and one
/// step when the drag ends, as the strip's other controls are; anything
/// else is a step. A bus has no keys, and its chain no file to learn a
/// noise profile from: those asks go nowhere.
fn bus_card_command(state: &mut AppState, cmds: &mut Vec<AppCommand>, at: usize, stand_in: klipp_core::ClipId, cmd: &AppCommand) {
    match cmd {
        AppCommand::BeginAudioChainDrag { clip_id, .. } if *clip_id == stand_in => {
            if state.mixer_routing_drag.is_none() {
                state.mixer_routing_drag = Some(state.sequence().routing());
            }
        }
        AppCommand::EndAudioChainDrag { clip_id, .. } if *clip_id == stand_in => {
            if let Some(old) = state.mixer_routing_drag.take() {
                let new = state.sequence().routing();
                if new != old {
                    state.active_history_mut().push(HistoryEntry { command: EditCommand::SetRouting { old: Box::new(old), new: Box::new(new) } });
                    cmds.push(AppCommand::RoutingChanged);
                }
            }
        }
        AppCommand::OpenModelDownloadDialog { .. } => cmds.push(cmd.clone()),
        _ => {
            let mut r = state.sequence().routing();
            let Some(effects) = r.buses.get_mut(at).map(|b| &mut b.effects) else { return };
            if !bus_chain_edit(effects, stand_in, cmd) {
                return;
            }
            if state.mixer_routing_drag.is_some() {
                state.sequence_mut().set_routing(&r);
                cmds.push(AppCommand::RoutingChanged);
            } else {
                let effects = r.buses[at].effects.clone();
                edit(state, cmds, |n| n.buses[at].effects = effects);
            }
        }
    }
}

/// `cmd`, an effect card's ask addressed to `stand_in`, made on a bus's
/// `effects`: whether it changed them. An effect is added only while the
/// bus has room, and never a Normalize, which measures a clip's file.
fn bus_chain_edit(effects: &mut Vec<klipp_core::AudioEffectInstance>, stand_in: klipp_core::ClipId, cmd: &AppCommand) -> bool {
    let set = |fx: &mut klipp_core::AudioEffectInstance, name: &str, value: f32| match fx.params.iter_mut().find(|(n, _)| n == name) {
        Some(p) => p.1 = value,
        None => fx.params.push((name.to_string(), value)),
    };
    match cmd {
        AppCommand::AddAudioEffect { clip_id, effect_type } if *clip_id == stand_in => {
            if effects.len() >= klipp_timeline::routing::MAX_BUS_EFFECTS || effect_type == "normalize" {
                return false;
            }
            effects.push(klipp_core::AudioEffectInstance { effect_type: effect_type.clone(), enabled: true, wet_dry: 1.0, params: Vec::new() });
        }
        AppCommand::RemoveAudioEffect { clip_id, effect_index } if *clip_id == stand_in && *effect_index < effects.len() => {
            effects.remove(*effect_index);
        }
        AppCommand::ToggleAudioEffect { clip_id, effect_index } if *clip_id == stand_in && *effect_index < effects.len() => {
            effects[*effect_index].enabled = !effects[*effect_index].enabled;
        }
        AppCommand::SetAudioEffectWetDry { clip_id, effect_index, wet_dry } if *clip_id == stand_in && *effect_index < effects.len() => {
            effects[*effect_index].wet_dry = wet_dry.clamp(0.0, 1.0);
        }
        AppCommand::UpdateAudioEffectParam { clip_id, effect_index, param_name, value } if *clip_id == stand_in && *effect_index < effects.len() => {
            set(&mut effects[*effect_index], param_name, *value);
        }
        AppCommand::SetAudioEffectParams { clip_id, effect_index, params } if *clip_id == stand_in && *effect_index < effects.len() => {
            for (name, value) in params {
                set(&mut effects[*effect_index], name, *value);
            }
        }
        AppCommand::ReorderAudioEffect { clip_id, from_index, to_index } if *clip_id == stand_in && *from_index < effects.len() && *to_index < effects.len() => {
            let fx = effects.remove(*from_index);
            effects.insert(*to_index, fx);
        }
        _ => return false,
    }
    true
}

/// Bus `at`'s strip, `w` by `h`: its name (right-click to rename or take
/// it out), where it goes, its effects, its pan, its fader with its
/// meter, and its mute. A way to master longer than is made up for is
/// said on it.
pub fn bus_strip(ui: &mut egui::Ui, theme: &KlippTheme, state: &mut AppState, cmds: &mut Vec<AppCommand>, at: usize, w: f32, h: f32) {
    let routing = state.sequence().routing();
    let Some(bus) = routing.buses.get(at).cloned() else { return };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, theme.bg_panel);
    ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme.accent.gamma_multiply(0.5)));
    let inner = rect.shrink2(egui::vec2(6.0, 6.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Center)));
    let ui = &mut child;
    ui.spacing_mut().item_spacing.y = 3.0;

    // Its name; right-click to rename it or take it out.
    let name = ui.add(egui::Label::new(egui::RichText::new(&bus.name).size(10.0).color(theme.text_primary)).truncate().sense(egui::Sense::click()))
        .on_hover_text("An aux bus: the tracks that send to it mixed through its effects. Right-click to rename it or take it out.");
    name.context_menu(|ui| {
        let key = ui.make_persistent_id(("bus_rename", bus.id));
        let mut text: String = ui.data_mut(|d| d.get_temp(key)).unwrap_or_else(|| bus.name.clone());
        let field = ui.add(egui::TextEdit::singleline(&mut text).desired_width(120.0));
        ui.data_mut(|d| d.insert_temp(key, text.clone()));
        let named = text.trim().to_string();
        let taken = routing.buses.iter().enumerate().any(|(k, b)| k != at && b.name.eq_ignore_ascii_case(&named));
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.add_enabled(!named.is_empty() && !taken, egui::Button::new("Rename")).clicked() || (enter && !named.is_empty() && !taken)) && named != bus.name {
            edit(state, cmds, |r| r.buses[at].name = named.chars().take(klipp_timeline::routing::MAX_BUS_NAME).collect());
            ui.data_mut(|d| d.remove::<String>(key));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Remove Bus").on_hover_text("Take the bus out, and every send to it").clicked() {
            edit(state, cmds, |r| remove_bus(r, at));
            ui.close_menu();
        }
    });

    // Where it goes.
    let out_name = |o: Option<BusId>| o.map_or_else(|| "Master".to_string(), |id| routing.buses.iter().find(|b| b.id == id).map_or_else(String::new, |b| b.name.clone()));
    let mut out = bus.output;
    egui::ComboBox::from_id_salt(("bus_output", bus.id))
        .width(w - 16.0)
        .selected_text(egui::RichText::new(format!("→ {}", out_name(out))).size(9.0))
        .show_ui(ui, |ui| {
            for o in outputs_for(&routing, at) {
                ui.selectable_value(&mut out, o, out_name(o));
            }
        });
    if out != bus.output {
        edit(state, cmds, |r| r.buses[at].output = out);
    }

    // Its effects, in the card the clip inspector draws: each opened by its
    // name for its parameters, switched, mixed, moved or taken off, and one
    // added from the list. A bus's chain had its switches alone, so a
    // delay on it couldn't be set.
    let on = bus.effects.iter().filter(|e| e.enabled).count();
    ui.menu_button(egui::RichText::new(format!("FX {on}/{}", bus.effects.len())).size(9.0), |ui| {
        ui.set_min_width(260.0);
        let stand_in = klipp_core::ClipId(bus.id.0);
        let mut card = Vec::new();
        crate::inspector::audio_fx_inspector::show_bus_content(ui, theme, stand_in, &bus.effects, &state.ladspa_plugins, &mut card);
        for cmd in card {
            bus_card_command(state, cmds, at, stand_in, &cmd);
        }
    });
    if state.buses_held_at_cap.get(at).copied().unwrap_or(false) {
        ui.label(egui::RichText::new("over 2 s late").size(9.0).color(theme.marker_error)).on_hover_text(format!(
            "The effects on this bus and the buses after it take longer than {} s, more than is made up for: what it returns is heard that much early against the rest.",
            klipp_audio::routing::MAX_ROUTE_SECS
        ));
    }

    // Pan.
    let mut pan = bus.pan;
    ui.spacing_mut().slider_width = w - 16.0;
    let resp = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false)).on_hover_text("Pan");
    control(state, cmds, &resp, |r| r.buses[at].pan = pan);

    // The fader and its meter.
    let below = 40.0;
    let fader_h = (ui.available_height() - below).max(40.0);
    let mut level = db(bus.volume);
    let fader_rect = ui.horizontal(|ui| {
        ui.spacing_mut().slider_width = fader_h;
        let resp = ui.add(egui::Slider::new(&mut level, -60.0..=12.0).vertical().show_value(false)).on_hover_text("The bus's level. Double-click for 0 dB.");
        let unity = resp.double_clicked();
        control(state, cmds, &resp, |r| r.buses[at].volume = gain(level));
        if unity {
            edit(state, cmds, |r| r.buses[at].volume = 1.0);
        }
        resp.rect
    }).inner;
    let (pl, pr) = state.bus_peak_levels.get(at).copied().unwrap_or((0.0, 0.0));
    let meter_x = fader_rect.right() + 6.0;
    let painter = ui.painter().clone();
    super::audio_mixer::draw_bus_meters(&painter, meter_x, fader_rect.top(), fader_rect.bottom(), pl, pr, theme);

    ui.label(egui::RichText::new(level_text(bus.volume)).monospace().size(10.0).color(theme.text_secondary));
    let muted = ui.add(egui::Button::new(egui::RichText::new("M").size(10.0).color(if bus.muted { egui::Color32::WHITE } else { theme.text_secondary })).fill(if bus.muted { theme.marker_error } else { theme.bg_raised }).min_size(egui::vec2(22.0, 16.0)))
        .on_hover_text("Mute the bus: what is sent to it is not heard");
    if muted.clicked() {
        edit(state, cmds, |r| r.buses[at].muted = !bus.muted);
    }
}

/// The button that adds a bus, at the end of the bus strips.
pub fn add_bus_button(ui: &mut egui::Ui, theme: &KlippTheme, state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    let full = state.sequence().buses.len() >= MAX_BUSES;
    let resp = ui.add_enabled(!full, egui::Button::new(egui::RichText::new("+ Bus").size(10.0).color(theme.text_secondary)))
        .on_hover_text("Add an aux bus: one reverb or delay many tracks send to, with its own effects")
        .on_disabled_hover_text(format!("A sequence has at most {MAX_BUSES} buses"));
    if resp.clicked() {
        edit(state, cmds, |r| {
            add_bus(r);
        });
    }
}

/// Track `track`'s sends, in a popup from `button`: each one's level,
/// whether it is taken before the fader, its mute, and taking it off;
/// a send added to any bus it doesn't send to, or to a new bus.
pub fn sends_popup(ui: &mut egui::Ui, theme: &KlippTheme, state: &mut AppState, cmds: &mut Vec<AppCommand>, track: TrackId, button: &egui::Response) {
    let id = ui.make_persistent_id(("track_sends", track));
    if button.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    egui::popup::popup_below_widget(ui, id, button, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
        ui.set_min_width(230.0);
        let routing = state.sequence().routing();
        let sends: Vec<AuxSend> = sends_of(&routing, track).to_vec();
        if sends.is_empty() {
            ui.label(egui::RichText::new("No sends. A send puts some of this track into a bus, for one reverb many tracks share.").size(10.0).color(theme.text_dim));
        }
        for send in &sends {
            let bus = send.bus;
            let name = routing.buses.iter().find(|b| b.id == bus).map_or("?", |b| b.name.as_str());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(name).size(10.0)).on_hover_text(if send.keys.is_empty() { String::new() } else { format!("Its level follows {} keys", send.keys.len()) });
                let mut level = db(send.level);
                let resp = ui.add(crate::widgets::number_field(&mut level).range(-60.0..=12.0).speed(0.2).suffix(" dB").fixed_decimals(1)).on_hover_text("The send's level");
                control(state, cmds, &resp, |r| {
                    if let Some(s) = send_mut(r, track, bus) {
                        s.level = gain(level);
                    }
                });
                let mut pre = send.pre_fader;
                if ui.toggle_value(&mut pre, egui::RichText::new("Pre").size(9.0)).on_hover_text("Taken before the track's fader and pan, so moving them leaves the send as it is").changed() {
                    edit(state, cmds, |r| {
                        if let Some(s) = send_mut(r, track, bus) {
                            s.pre_fader = pre;
                        }
                    });
                }
                let mut muted = send.muted;
                if ui.toggle_value(&mut muted, egui::RichText::new("M").size(9.0)).on_hover_text("Mute the send").changed() {
                    edit(state, cmds, |r| {
                        if let Some(s) = send_mut(r, track, bus) {
                            s.muted = muted;
                        }
                    });
                }
                if ui.small_button("✕").on_hover_text("Take the send off").clicked() {
                    edit(state, cmds, |r| remove_send(r, track, bus));
                }
            });
        }
        ui.separator();
        let free: Vec<&AuxBus> = routing.buses.iter().filter(|b| !sends.iter().any(|s| s.bus == b.id)).collect();
        ui.add_enabled_ui(sends.len() < MAX_SENDS, |ui| {
            ui.menu_button("Send to", |ui| {
                for b in &free {
                    if ui.button(&b.name).clicked() {
                        let bus = b.id;
                        edit(state, cmds, |r| {
                            add_send(r, track, bus);
                        });
                        ui.close_menu();
                    }
                }
                if !free.is_empty() {
                    ui.separator();
                }
                if ui.add_enabled(routing.buses.len() < MAX_BUSES, egui::Button::new("New Bus")).clicked() {
                    edit(state, cmds, |r| {
                        if let Some(bus) = add_bus(r) {
                            add_send(r, track, bus);
                        }
                    });
                    ui.close_menu();
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// New buses take the first free name; a bus taken out takes the
    /// sends to it and frees what fed it; a send is added once, after the
    /// fader at 0 dB, up to 16, and taken off.
    #[test]
    fn the_mixers_routing_edits() {
        let mut r = Routing::default();
        let a = add_bus(&mut r).unwrap();
        let b = add_bus(&mut r).unwrap();
        assert_eq!(r.buses.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(), ["Bus 1", "Bus 2"]);
        r.buses[0].output = Some(b);
        assert_eq!(outputs_for(&r, 0), vec![None, Some(b)], "bus 1 to master or bus 2, not itself");
        assert_eq!(outputs_for(&r, 1), vec![None], "bus 2 not to bus 1, which feeds it, nor itself");
        let t = TrackId::new();
        assert!(add_send(&mut r, t, a));
        assert!(!add_send(&mut r, t, a), "once");
        assert_eq!(sends_of(&r, t), [AuxSend::new(a)]);
        assert!(add_send(&mut r, t, b));
        remove_bus(&mut r, 1);
        assert_eq!(r.buses[0].output, None, "what fed it goes to master");
        assert_eq!(sends_of(&r, t), [AuxSend::new(a)], "the send to it is gone");
        remove_send(&mut r, t, a);
        assert!(r.sends.is_empty());
        let ids: Vec<BusId> = (0..20).filter_map(|_| add_bus(&mut r)).collect();
        assert!(ids.iter().take(16).all(|id| add_send(&mut r, t, *id)));
        assert!(!add_send(&mut r, t, ids[16]), "16 sends at most");
        while add_bus(&mut r).is_some() {}
        assert_eq!(r.buses.len(), MAX_BUSES);
    }

    /// A routing change from the mixer is one undo step that brings the
    /// routing back.
    #[test]
    fn a_mixer_edit_is_one_undo_step() {
        let mut state = AppState::default();
        let mut cmds = Vec::new();
        edit(&mut state, &mut cmds, |r| {
            add_bus(r);
        });
        assert_eq!(state.sequence().buses.len(), 1);
        assert!(matches!(cmds[..], [AppCommand::RoutingChanged]));
        let entry = state.active_history_mut().undo().expect("a step");
        let EditCommand::SetRouting { old, new } = entry.command else { panic!("a routing step") };
        assert!(old.buses.is_empty() && new.buses.len() == 1);
        edit(&mut state, &mut cmds, |_| {});
        assert_eq!(cmds.len(), 1, "no change, no step");
    }

    /// The card's asks, made on a bus's chain: a delay's time set and its
    /// mix, switched and moved; none past the cap, no Normalize, nothing
    /// asked of another clip.
    #[test]
    fn the_effect_card_edits_a_buss_chain() {
        let me = klipp_core::ClipId::new();
        let fx = |t: &str| klipp_core::AudioEffectInstance { effect_type: t.into(), enabled: true, wet_dry: 1.0, params: Vec::new() };
        let mut chain = vec![fx("delay"), fx("reverb")];
        assert!(bus_chain_edit(&mut chain, me, &AppCommand::UpdateAudioEffectParam { clip_id: me, effect_index: 0, param_name: "delay_time".into(), value: 1.5 }));
        assert!(bus_chain_edit(&mut chain, me, &AppCommand::SetAudioEffectWetDry { clip_id: me, effect_index: 0, wet_dry: 0.4 }));
        assert!(bus_chain_edit(&mut chain, me, &AppCommand::ToggleAudioEffect { clip_id: me, effect_index: 1 }));
        assert_eq!((chain[0].params.clone(), chain[0].wet_dry, chain[1].enabled), (vec![("delay_time".to_string(), 1.5)], 0.4, false));
        assert!(bus_chain_edit(&mut chain, me, &AppCommand::ReorderAudioEffect { clip_id: me, from_index: 0, to_index: 1 }));
        assert_eq!(chain.iter().map(|e| e.effect_type.as_str()).collect::<Vec<_>>(), ["reverb", "delay"]);
        assert!(!bus_chain_edit(&mut chain, klipp_core::ClipId::new(), &AppCommand::RemoveAudioEffect { clip_id: klipp_core::ClipId::new(), effect_index: 0 }), "another clip's");
        assert!(!bus_chain_edit(&mut chain, me, &AppCommand::AddAudioEffect { clip_id: me, effect_type: "normalize".into() }));
        while bus_chain_edit(&mut chain, me, &AppCommand::AddAudioEffect { clip_id: me, effect_type: "eq".into() }) {}
        assert_eq!(chain.len(), klipp_timeline::routing::MAX_BUS_EFFECTS);
    }

    /// A bus's FX menu shows each effect's card, its parameters among it:
    /// a delay's time and feedback can be set where the bus is. The menu
    /// had each effect's switch and remove alone.
    #[test]
    fn a_buss_effects_show_their_parameters() {
        let mut state = AppState::default();
        let mut cmds = Vec::new();
        edit(&mut state, &mut cmds, |r| {
            add_bus(r);
            r.buses[0].effects.push(klipp_core::AudioEffectInstance { effect_type: "delay".into(), enabled: true, wet_dry: 1.0, params: Vec::new() });
        });
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut frame = |state: &mut AppState, events: Vec<egui::Event>| {
            clock += 0.05;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 900.0))), time: Some(clock), events, ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| bus_strip(ui, &KlippTheme::default(), state, &mut Vec::new(), 0, 90.0, 400.0));
            });
            out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            }).collect::<Vec<_>>()
        };
        frame(&mut state, Vec::new());
        let drawn = frame(&mut state, Vec::new());
        let at = drawn.iter().find(|(t, _)| t == "FX 1/1").map(|(_, p)| *p).expect("the FX menu");
        let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&mut state, vec![egui::Event::PointerMoved(at)]);
        frame(&mut state, vec![b(true)]);
        frame(&mut state, vec![b(false)]);
        let open = frame(&mut state, Vec::new());
        let texts: Vec<&str> = open.iter().map(|(t, _)| t.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("Feedback")), "the delay's parameters in the menu: {texts:?}");
        assert!(!texts.iter().any(|t| t.contains('\u{25C6}')), "no keyframe diamonds: a bus has no keys: {texts:?}");
        // Its add list: the bus's effects, not Normalize, which measures a
        // clip's file.
        let add = open.iter().find(|(t, _)| t == "+ Add Effect").map(|(_, p)| *p).expect("the card's add button");
        let b = |pressed| egui::Event::PointerButton { pos: add, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&mut state, vec![egui::Event::PointerMoved(add)]);
        frame(&mut state, vec![b(true)]);
        frame(&mut state, vec![b(false)]);
        let listed = frame(&mut state, Vec::new());
        let names: Vec<&str> = listed.iter().map(|(t, _)| t.as_str()).collect();
        assert!(names.contains(&"Delay") && !names.contains(&"Normalize"), "{names:?}");
    }
}
