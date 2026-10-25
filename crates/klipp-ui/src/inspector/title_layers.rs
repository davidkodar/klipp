//! A title's layers list (#1715): a row per layer, the front one at the
//! top. A click selects; a drag reorders, a line showing where it will
//! land; the eye hides and the lock locks; a double click renames in
//! place (Enter keeps it, Esc leaves it); a layer another layer is
//! masked by wears a Matte badge; the right-click menu restacks,
//! duplicates and deletes. Each change goes out as one command, one
//! undo step.
use crate::widgets::menu::MenuRows;

use klipp_core::{AppCommand, ClipId};
use klipp_timeline::{TitleData, TitleObject, TitleObjectContent, TitleObjectId};

use crate::icons::Icon;
use crate::theme::KlippTheme;

const ROW_H: f32 = 40.0;

/// A layer's name as the list shows it: its own, else its text's first
/// line, else its kind and number.
pub fn layer_name(obj: &TitleObject) -> String {
    if !obj.name.is_empty() {
        return obj.name.clone();
    }
    match &obj.content {
        TitleObjectContent::TextBox(tb) => {
            let trimmed = tb.text.lines().next().unwrap_or("").trim();
            if trimmed.is_empty() {
                format!("Text {}", obj.id.0 + 1)
            } else if trimmed.chars().count() > 24 {
                format!("{}\u{2026}", trimmed.chars().take(22).collect::<String>())
            } else {
                trimmed.to_string()
            }
        }
        TitleObjectContent::Shape(_) => format!("Shape {}", obj.id.0 + 1),
        TitleObjectContent::Image(img) => img.stem(),
    }
}

/// The layers that `obj` masks.
fn masked_by(td: &TitleData, obj: &TitleObject) -> Vec<String> {
    td.objects.iter().filter(|o| o.id != obj.id && o.mask.is_some_and(|m| m.source == obj.id)).map(layer_name).collect()
}

/// The new stacking order (back to front, the model's) when the layer
/// at list row `from` (0 the top, the front) is dropped at slot `to`
/// (0 above the top row, `n` below the bottom one); `None` when that
/// leaves it where it was.
pub fn reordered(td: &TitleData, from: usize, to: usize) -> Option<Vec<u32>> {
    let mut rows: Vec<u32> = td.objects.iter().rev().map(|o| o.id.0).collect();
    if from >= rows.len() {
        return None;
    }
    let to = to.min(rows.len());
    if to == from || to == from + 1 {
        return None;
    }
    let id = rows.remove(from);
    rows.insert(if to > from { to - 1 } else { to }, id);
    rows.reverse();
    Some(rows)
}

/// A layer edit the list sends, made on `td`: a reorder, a layer shown
/// or hidden, locked or unlocked, renamed or duplicated. The handlers
/// make the edit with this, so what the list asks is what the title
/// becomes. Returns the duplicate's id for a duplicate; `None` for
/// any other command, or a layer the title does not have.
pub fn apply(td: &mut TitleData, cmd: &AppCommand) -> Option<TitleObjectId> {
    fn object(td: &mut TitleData, id: u32) -> Option<&mut TitleObject> {
        td.object_mut(TitleObjectId(id))
    }
    match cmd {
        AppCommand::ReorderTitleObjects { new_order, .. } => {
            // In the order asked; a layer it leaves out keeps to the
            // front, in id order (defensive against a stale list).
            let mut by_id: std::collections::HashMap<u32, TitleObject> = td.objects.drain(..).map(|o| (o.id.0, o)).collect();
            for id in new_order {
                if let Some(o) = by_id.remove(id) {
                    td.objects.push(o);
                }
            }
            let mut leftover: Vec<_> = by_id.into_values().collect();
            leftover.sort_by_key(|o| o.id.0);
            td.objects.extend(leftover);
            None
        }
        AppCommand::SetTitleObjectVisible { object_id, visible, .. } => {
            if let Some(o) = object(td, *object_id) {
                o.visible = *visible;
            }
            None
        }
        AppCommand::SetTitleObjectLocked { object_id, locked, .. } => {
            if let Some(o) = object(td, *object_id) {
                o.locked = *locked;
            }
            None
        }
        AppCommand::RenameTitleObject { object_id, name, .. } => {
            if let Some(o) = object(td, *object_id) {
                o.name = name.clone();
            }
            None
        }
        AppCommand::DuplicateTitleObject { object_id, .. } => td.duplicate_object(TitleObjectId(*object_id)),
        _ => None,
    }
}

/// What the list keeps between frames: the row being dragged, and the
/// layer being renamed with its draft name.
#[derive(Clone, Default)]
struct ListMemory {
    dragging: Option<usize>,
    renaming: Option<(u32, String)>,
}

/// The list for `td`, the selected layer `selected` drawn so.
pub fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip_id: ClipId, td: &TitleData, selected: Option<TitleObjectId>, cmds: &mut Vec<AppCommand>) {
    let mem_id = ui.id().with(("title_layers_list", clip_id.0));
    let mut mem: ListMemory = ui.ctx().data(|d| d.get_temp(mem_id)).unwrap_or_default();
    let total = td.objects.len();
    let mut rows: Vec<egui::Rect> = Vec::with_capacity(total);
    let mut drag_end = None;
    for (row, (rev_idx, obj)) in td.objects.iter().enumerate().rev().enumerate() {
        let resp = row_ui(ui, theme, clip_id, td, obj, rev_idx, total, selected == Some(obj.id), &mut mem, cmds);
        if resp.drag_started() {
            mem.dragging = Some(row);
        }
        if resp.drag_stopped() {
            drag_end = resp.interact_pointer_pos().or(ui.input(|i| i.pointer.latest_pos()));
        }
        rows.push(resp.rect);
    }
    // Where a dragged row would land: the gap nearest the pointer.
    let slot = |y: f32| rows.iter().position(|r| y < r.center().y).unwrap_or(rows.len());
    if let Some(from) = mem.dragging {
        if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
            let to = slot(p.y);
            let y = rows.get(to).map_or_else(|| rows.last().map_or(0.0, |r| r.bottom()), |r| r.top());
            if let Some(first) = rows.first() {
                ui.painter().line_segment([egui::pos2(first.left(), y), egui::pos2(first.right(), y)], egui::Stroke::new(2.0_f32, theme.accent));
            }
            if let Some(end) = drag_end {
                if let Some(order) = reordered(td, from, slot(end.y)) {
                    cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: order });
                }
                mem.dragging = None;
            }
        }
        if !ui.input(|i| i.pointer.any_down()) && drag_end.is_none() {
            mem.dragging = None;
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(mem_id, mem));
}

/// One layer's row; its response is the row's, for selecting and
/// dragging.
#[allow(clippy::too_many_arguments)]
fn row_ui(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    td: &TitleData,
    obj: &TitleObject,
    rev_idx: usize,
    total: usize,
    is_selected: bool,
    mem: &mut ListMemory,
    cmds: &mut Vec<AppCommand>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::click_and_drag());
    if is_selected {
        ui.painter().rect(rect.shrink(1.0), egui::Rounding::same(4.0), theme.accent_subtle, egui::Stroke::new(1.5_f32, theme.accent));
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, egui::Rounding::same(4.0), theme.bg_hover);
    }
    if resp.clicked() {
        cmds.push(AppCommand::SelectTitleObject { clip_id, object_id: Some(obj.id.0) });
    }
    if resp.double_clicked() {
        mem.renaming = Some((obj.id.0, layer_name(obj)));
    }

    // Right-click: restack, duplicate, delete.
    let order: Vec<u32> = td.objects.iter().map(|o| o.id.0).collect();
    let pos = order.iter().position(|&id| id == obj.id.0).unwrap_or(0);
    resp.context_menu_rows(|ui| {
        let (fwd, back) = (pos + 1 < order.len(), pos > 0);
        let mut restack = |ui: &mut egui::Ui, enabled: bool, label: &str, f: &dyn Fn(&mut Vec<u32>)| {
            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                let mut o = order.clone();
                f(&mut o);
                cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: o });
                ui.close_menu();
            }
        };
        restack(ui, fwd, "Bring to Front", &|o| {
            let l = o.remove(pos);
            o.push(l);
        });
        restack(ui, fwd, "Bring Forward", &|o| o.swap(pos, pos + 1));
        restack(ui, back, "Send Backward", &|o| o.swap(pos, pos - 1));
        restack(ui, back, "Send to Back", &|o| {
            let l = o.remove(pos);
            o.insert(0, l);
        });
        ui.separator();
        if ui.button("Rename").clicked() {
            mem.renaming = Some((obj.id.0, layer_name(obj)));
            ui.close_menu();
        }
        if ui.button("Duplicate").clicked() {
            cmds.push(AppCommand::DuplicateTitleObject { clip_id, object_id: obj.id.0 });
            ui.close_menu();
        }
        if ui.button("Delete").clicked() {
            cmds.push(AppCommand::DeleteTitleObject { clip_id, object_id: obj.id.0 });
            ui.close_menu();
        }
    });

    let icon_size = 16.0;
    let icon_y = rect.center().y - icon_size * 0.5;
    let mut x = rect.min.x + 8.0;
    // An icon button: its rect, painted, and whether it was clicked.
    let icon = |ui: &mut egui::Ui, x: f32, key: &str, icon: Icon, tint: egui::Color32, hover: &str| -> bool {
        let r = egui::Rect::from_min_size(egui::pos2(x, icon_y), egui::vec2(icon_size, icon_size));
        let resp = ui.interact(r, ui.id().with((key, obj.id.0)), egui::Sense::click()).on_hover_text(hover);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(r), |ui| {
            ui.add(icon.image(icon_size).tint(tint));
        });
        resp.clicked()
    };
    if icon(ui, x, "title_layer_eye", if obj.visible { Icon::Eye } else { Icon::EyeSlash }, if obj.visible { theme.text_secondary } else { theme.text_dim }, if obj.visible { "Hide" } else { "Show" }) {
        cmds.push(AppCommand::SetTitleObjectVisible { clip_id, object_id: obj.id.0, visible: !obj.visible });
    }
    x += icon_size + 6.0;
    if icon(ui, x, "title_layer_lock", if obj.locked { Icon::Lock } else { Icon::LockOpen }, if obj.locked { theme.accent } else { theme.text_dim }, if obj.locked { "Unlock" } else { "Lock" }) {
        cmds.push(AppCommand::SetTitleObjectLocked { clip_id, object_id: obj.id.0, locked: !obj.locked });
    }
    x += icon_size + 8.0;

    // The kind's badge.
    let badge = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - 10.0), egui::vec2(20.0, 20.0));
    let (fill, ink, letter) = match &obj.content {
        TitleObjectContent::TextBox(_) => (theme.accent, theme.text_on_accent, "T"),
        TitleObjectContent::Shape(_) => (theme.bg_raised, theme.text_primary, "S"),
        TitleObjectContent::Image(_) => (theme.bg_raised, theme.text_primary, "I"),
    };
    ui.painter().rect_filled(badge, egui::Rounding::same(4.0), fill);
    ui.painter().text(badge.center(), egui::Align2::CENTER_CENTER, letter, egui::FontId::proportional(12.0), ink);
    x += 28.0;

    // The right side: delete, forward, the place in the stack, a Matte
    // badge for a layer that masks others.
    let mut x_right = rect.max.x - 8.0 - icon_size;
    let del = egui::Rect::from_min_size(egui::pos2(x_right, icon_y), egui::vec2(icon_size, icon_size));
    let del_resp = ui.interact(del, ui.id().with(("title_layer_del", obj.id.0)), egui::Sense::click()).on_hover_text("Delete");
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(del), |ui| {
        ui.add(Icon::X.image(icon_size).tint(if del_resp.hovered() { theme.marker_error } else { theme.text_dim }));
    });
    if del_resp.clicked() {
        cmds.push(AppCommand::DeleteTitleObject { clip_id, object_id: obj.id.0 });
    }
    x_right -= icon_size + 6.0;
    let up_enabled = rev_idx + 1 < total;
    if icon(ui, x_right, "title_layer_up", Icon::CaretUp, if up_enabled { theme.text_secondary } else { theme.text_dim.gamma_multiply(0.4) }, "Bring forward") && up_enabled {
        let mut o = order.clone();
        o.swap(pos, pos + 1);
        cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: o });
    }
    x_right -= 8.0;
    let place = if total <= 3 {
        if rev_idx + 1 == total { "Front".to_string() } else if rev_idx == 0 { "Back".to_string() } else { "Mid".to_string() }
    } else {
        format!("{}/{}", rev_idx + 1, total)
    };
    let galley = ui.painter().layout_no_wrap(place, egui::FontId::proportional(11.0), theme.text_dim);
    x_right -= galley.size().x;
    ui.painter().galley(egui::pos2(x_right, rect.center().y - galley.size().y * 0.5), galley, theme.text_dim);
    let masks = masked_by(td, obj);
    if !masks.is_empty() {
        let g = ui.painter().layout_no_wrap("Matte".into(), egui::FontId::proportional(10.0), theme.text_on_accent);
        let pill = egui::Rect::from_min_size(egui::pos2(x_right - g.size().x - 14.0, rect.center().y - 8.0), egui::vec2(g.size().x + 8.0, 16.0));
        ui.painter().rect_filled(pill, egui::Rounding::same(8.0), theme.accent);
        ui.painter().galley(pill.min + egui::vec2(4.0, (16.0 - g.size().y) * 0.5), g, theme.text_on_accent);
        ui.interact(pill, ui.id().with(("title_layer_matte", obj.id.0)), egui::Sense::hover()).on_hover_text(format!("Masks {}: a matte, not drawn itself", masks.join(", ")));
        x_right = pill.left() - 6.0;
    }

    // The name, or the field renaming it.
    let name_rect = egui::Rect::from_min_max(egui::pos2(x, rect.top() + 8.0), egui::pos2(x_right.max(x + 40.0), rect.bottom() - 8.0));
    match &mut mem.renaming {
        Some((id, draft)) if *id == obj.id.0 => {
            let edit = ui.put(name_rect, egui::TextEdit::singleline(draft).id(ui.id().with(("title_layer_rename", obj.id.0))));
            if !edit.has_focus() && !edit.lost_focus() {
                edit.request_focus();
            }
            let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
            if esc {
                mem.renaming = None;
            } else if edit.lost_focus() || enter {
                let name = draft.trim().to_string();
                if !name.is_empty() && name != layer_name(obj) {
                    cmds.push(AppCommand::RenameTitleObject { clip_id, object_id: obj.id.0, name });
                }
                mem.renaming = None;
            }
        }
        _ => {
            let colour = if !obj.visible { theme.text_dim } else if is_selected { theme.text_primary } else { theme.text_secondary };
            ui.painter().text(egui::pos2(x, rect.center().y), egui::Align2::LEFT_CENTER, layer_name(obj), egui::FontId::proportional(12.5), colour);
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{default_text_transform, TextBox};

    fn clip() -> ClipId {
        ClipId(uuid::Uuid::nil())
    }

    /// Three text layers, back to front: Back, Mid, Front.
    fn title() -> TitleData {
        let mut td = TitleData::single_text(TextBox { text: "Back".into(), ..Default::default() });
        td.objects[0].name = String::new();
        for text in ["Mid", "Front"] {
            let id = td.add_object(TitleObjectContent::TextBox(TextBox { text: text.into(), ..Default::default() }), default_text_transform());
            td.object_mut(id).unwrap().name = String::new();
        }
        td
    }

    struct Frame {
        texts: Vec<(String, egui::Pos2)>,
        cmds: Vec<AppCommand>,
    }

    fn frame(ctx: &egui::Context, td: &TitleData, events: Vec<egui::Event>) -> Frame {
        let mut cmds = Vec::new();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 600.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &KlippTheme::default(), clip(), td, None, &mut cmds));
            },
        );
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        Frame { texts, cmds }
    }

    fn at(f: &Frame, text: &str) -> egui::Pos2 {
        f.texts.iter().find(|(t, _)| t == text).map(|(_, p)| *p).unwrap_or_else(|| panic!("{text} is drawn"))
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
    }

    /// The front layer is the top row.
    #[test]
    fn the_front_layer_is_the_top_row() {
        let ctx = egui::Context::default();
        let f = frame(&ctx, &title(), Vec::new());
        let (front, mid, back) = (at(&f, "Front"), at(&f, "Mid"), at(&f, "Back"));
        assert!(front.y < mid.y && mid.y < back.y);
    }

    /// Dragging the top row below the bottom one sends it to the back:
    /// one reorder, the model's order back to front.
    #[test]
    fn dragging_a_row_reorders_the_stack() {
        let ctx = egui::Context::default();
        let td = title();
        let f = frame(&ctx, &td, Vec::new());
        let (from, to) = (at(&f, "Front"), at(&f, "Back") + egui::vec2(0.0, 25.0));
        let mut cmds = frame(&ctx, &td, vec![egui::Event::PointerMoved(from), press(from, true)]).cmds;
        for k in 1..=10 {
            let p = from + (to - from) * (k as f32 / 10.0);
            cmds.extend(frame(&ctx, &td, vec![egui::Event::PointerMoved(p)]).cmds);
        }
        cmds.extend(frame(&ctx, &td, vec![press(to, false)]).cmds);
        assert_eq!(cmds.iter().filter(|c| matches!(c, AppCommand::ReorderTitleObjects { .. })).count(), 1, "one reorder: {cmds:?}");
        let mut after = td.clone();
        for c in &cmds {
            apply(&mut after, c);
        }
        let names: Vec<String> = after.objects.iter().map(layer_name).collect();
        assert_eq!(names, ["Front", "Back", "Mid"], "front sent to the back, back to front order");
    }

    /// The eye hides and the lock locks: one command each.
    #[test]
    fn the_eye_hides_and_the_lock_locks() {
        let ctx = egui::Context::default();
        let td = title();
        let f = frame(&ctx, &td, Vec::new());
        let mid = at(&f, "Mid");
        // The eye and the lock sit at the row's left, before the badge:
        // the panel's 8 px margin, the row's 8 px inset, 16 px icons.
        let eye = egui::pos2(8.0 + 8.0 + 8.0, mid.y);
        let lock = egui::pos2(8.0 + 8.0 + 16.0 + 6.0 + 8.0, mid.y);
        let click = |p: egui::Pos2| {
            let mut c = frame(&ctx, &td, vec![egui::Event::PointerMoved(p), press(p, true)]).cmds;
            c.extend(frame(&ctx, &td, vec![press(p, false)]).cmds);
            c
        };
        let (hide, lock_it) = (click(eye), click(lock));
        assert_eq!((hide.len(), lock_it.len()), (1, 1), "one command each");
        let mut after = td.clone();
        for c in hide.iter().chain(&lock_it) {
            apply(&mut after, c);
        }
        let mid_after = &after.objects[1];
        assert_eq!((mid_after.visible, mid_after.locked), (false, true));
        assert!(after.objects.iter().filter(|o| o.id != mid_after.id).all(|o| o.visible && !o.locked), "only the Mid row's");
    }

    /// A double click renames in place: typed, Enter keeps it as one
    /// rename; Esc leaves the name as it was.
    #[test]
    fn a_double_click_renames_in_place() {
        let ctx = egui::Context::default();
        let td = title();
        let f = frame(&ctx, &td, Vec::new());
        let mid = at(&f, "Mid");
        let dbl = |ctx: &egui::Context| {
            for _ in 0..2 {
                frame(ctx, &td, vec![egui::Event::PointerMoved(mid), press(mid, true)]);
                frame(ctx, &td, vec![press(mid, false)]);
            }
            frame(ctx, &td, Vec::new());
        };
        dbl(&ctx);
        let key = |k: egui::Key| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let select_all = egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
        frame(&ctx, &td, vec![select_all, egui::Event::Text("Subtitle".into())]);
        let cmds = frame(&ctx, &td, vec![key(egui::Key::Enter)]).cmds;
        assert!(matches!(&cmds[..], [AppCommand::RenameTitleObject { .. }]), "{cmds:?}");
        let mut after = td.clone();
        apply(&mut after, &cmds[0]);
        assert_eq!(after.objects[1].name, "Subtitle");
        // Esc: no rename.
        let ctx = egui::Context::default();
        frame(&ctx, &td, Vec::new());
        dbl(&ctx);
        frame(&ctx, &td, vec![egui::Event::Text("Other".into())]);
        let cmds = frame(&ctx, &td, vec![key(egui::Key::Escape)]).cmds;
        assert!(cmds.iter().all(|c| !matches!(c, AppCommand::RenameTitleObject { .. })), "{cmds:?}");
    }

    /// The right-click menu duplicates a layer: a copy just above it.
    #[test]
    fn the_menu_duplicates_a_layer() {
        let ctx = egui::Context::default();
        let td = title();
        let f = frame(&ctx, &td, Vec::new());
        let mid = at(&f, "Mid");
        let right = |pressed| egui::Event::PointerButton { pos: mid, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&ctx, &td, vec![egui::Event::PointerMoved(mid), right(true)]);
        frame(&ctx, &td, vec![right(false)]);
        let menu = frame(&ctx, &td, Vec::new());
        let dup = at(&menu, "Duplicate");
        frame(&ctx, &td, vec![egui::Event::PointerMoved(dup), press(dup, true)]);
        let cmds = frame(&ctx, &td, vec![press(dup, false)]).cmds;
        assert!(matches!(&cmds[..], [AppCommand::DuplicateTitleObject { .. }]), "{cmds:?}");
        let mut after = td.clone();
        let copy = apply(&mut after, &cmds[0]).expect("a copy");
        assert_eq!(after.objects.iter().map(layer_name).collect::<Vec<_>>(), ["Back", "Mid", "Mid", "Front"]);
        assert_eq!(after.objects[2].id, copy);
    }

    /// A layer another layer is masked by wears a Matte badge.
    #[test]
    fn a_layer_used_as_a_mask_wears_a_matte_badge() {
        let ctx = egui::Context::default();
        let mut td = title();
        assert!(frame(&ctx, &td, Vec::new()).texts.iter().all(|(t, _)| t != "Matte"));
        let back = td.objects[0].id;
        td.objects[2].mask = Some(klipp_timeline::LayerMask { source: back, kind: Default::default() });
        let f = frame(&ctx, &td, Vec::new());
        let (matte, back_row) = (at(&f, "Matte"), at(&f, "Back"));
        assert!((matte.y - back_row.y).abs() < 4.0, "on the Back row");
    }

    /// Reordering by slot: a drop above or below its own row is no move.
    #[test]
    fn a_drop_on_its_own_place_is_no_move() {
        let td = title();
        let ids: Vec<u32> = td.objects.iter().map(|o| o.id.0).collect();
        assert_eq!(reordered(&td, 0, 0), None);
        assert_eq!(reordered(&td, 0, 1), None);
        assert_eq!(reordered(&td, 0, 3), Some(vec![ids[2], ids[0], ids[1]]));
        assert_eq!(reordered(&td, 2, 0), Some(vec![ids[1], ids[2], ids[0]]));
        assert_eq!(reordered(&td, 1, 0), Some(vec![ids[0], ids[2], ids[1]]));
    }
}
