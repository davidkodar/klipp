//! A clip's role badge on the timeline (#3211): a small pill in the
//! role's colour in the clip's bottom-left corner, after a group's
//! badge, naming its role (the sub-role's name for a sub-role) where
//! that fits in half the clip, its initial where only that does.

use klipp_core::roles::RoleSet;
use klipp_timeline::Clip;

/// The badge's height, and a character's width in its font, as its
/// width is reckoned.
const HEIGHT: f32 = 12.0;
const CHAR_W: f32 = 5.6;

/// Where `clip`'s badge sits in `clip_rect` and what it says. `None` for
/// a clip with no role, or one too small to show it.
pub fn layout(roles: &RoleSet, clip: &Clip, clip_rect: egui::Rect) -> Option<(egui::Rect, String)> {
    let role = roles.resolve(clip.role.as_ref()?)?;
    if clip_rect.height() < 24.0 {
        return None;
    }
    let main = roles.main(role.main())?;
    let name = match role.sub() {
        Some(sub) => main.subs.iter().find(|s| s.id == sub).map_or(main.name.as_str(), |s| s.name.as_str()),
        None => main.name.as_str(),
    };
    let left = clip_rect.left() + if clip.group_id.is_some() { 19.0 } else { 3.0 };
    let width = |text: &str| text.chars().count() as f32 * CHAR_W + 8.0;
    let room = (clip_rect.width() / 2.0).min(clip_rect.right() - left - 14.0);
    let initial: String = name.chars().take(1).collect();
    let text = if width(name) <= room {
        name.to_string()
    } else if width(&initial) <= room {
        initial
    } else {
        return None;
    };
    let rect = egui::Rect::from_min_size(egui::pos2(left, clip_rect.bottom() - HEIGHT - 2.0), egui::vec2(width(&text), HEIGHT));
    Some((rect, text))
}

/// The badge drawn, in the colour `theme` gives the clip's role, its
/// text dark or light to read on it.
pub fn draw(painter: &egui::Painter, theme: &crate::theme::KlippTheme, roles: &RoleSet, clip: &Clip, clip_rect: egui::Rect) {
    let Some((rect, text)) = layout(roles, clip, clip_rect) else { return };
    let fill = theme.role_color(roles, clip.role.as_ref());
    let light = 0.2126 * fill.r() as f32 + 0.7152 * fill.g() as f32 + 0.0722 * fill.b() as f32 > 140.0;
    let ink = if light { egui::Color32::from_rgb(20, 20, 20) } else { egui::Color32::from_rgb(240, 240, 240) };
    painter.rect(rect, HEIGHT / 2.0, fill, egui::Stroke::new(1.0_f32, egui::Color32::from_black_alpha(110)));
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(9.0), ink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::roles::RoleRef;
    use klipp_core::{MediaId, Timecode};

    fn clip(role: &str) -> Clip {
        let t = |s: f64| Timecode::from_secs_f64(s, 25);
        let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        c.role = RoleRef::parse(role);
        c
    }

    fn at(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(w, h))
    }

    /// A wide clip names its role, a sub-role by its own name; a narrow
    /// one shows the initial; a sliver, a short track or no role shows
    /// none. It sits bottom-left, past a group's badge, inside the clip.
    #[test]
    fn a_clip_badge_names_its_role_where_it_fits() {
        let roles = RoleSet::built_in();
        let text = |c: &Clip, r: egui::Rect| layout(&roles, c, r).map(|(_, t)| t);
        assert_eq!(text(&clip("dialogue"), at(300.0, 40.0)).as_deref(), Some("Dialogue"));
        assert_eq!(text(&clip("effects.foley"), at(300.0, 40.0)).as_deref(), Some("Foley"));
        assert_eq!(text(&clip("music"), at(40.0, 40.0)).as_deref(), Some("M"));
        assert_eq!(text(&clip("music"), at(16.0, 40.0)), None, "a sliver");
        assert_eq!(text(&clip("music"), at(300.0, 20.0)), None, "a short track");
        assert_eq!(text(&clip("none"), at(300.0, 40.0)), None, "no role");
        let mut grouped = clip("titles");
        grouped.group_id = Some(klipp_core::GroupId::new());
        let r = at(300.0, 40.0);
        let (plain, _) = layout(&roles, &clip("titles"), r).unwrap();
        let (past, _) = layout(&roles, &grouped, r).unwrap();
        assert!(past.left() > plain.left() + 15.0, "past the group's badge");
        assert!(r.contains_rect(plain) && (r.bottom() - plain.bottom()) < 4.0, "inside, at the bottom");
        // A role the project no longer has falls back to its main role.
        assert_eq!(text(&clip("dialogue.gone"), at(300.0, 40.0)).as_deref(), Some("Dialogue"));
    }
}
