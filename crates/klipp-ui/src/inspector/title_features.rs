//! "Font features" in the Title tab (#1710): the OpenType features a
//! text box, or a selection's run, is shaped with (#1501), as toggles
//! named for what they do, a picker of the face's stylistic sets, and a
//! folded field for any other tag. Over a selection each writes the
//! run's through `ApplyTitleStyle`, else the box's through
//! `SetTitleFeatures`; a click is one command, so one undo step.

use klipp_core::{AppCommand, ClipId};
use klipp_timeline::{FontStyle, TextBox};

use crate::theme::KlippTheme;

/// The toggles: label, tag, what a face without it lacks, and whether
/// the shaper has it on when nothing names it.
const TOGGLES: [(&str, &str, &str, bool); 6] = [
    ("Small caps", "smcp", "small caps", false),
    ("Old-style figures", "onum", "old-style figures", false),
    ("Tabular figures", "tnum", "tabular figures", false),
    ("Slashed zero", "zero", "slashed zero", false),
    ("Ligatures", "liga", "ligatures", true),
    ("Discretionary ligatures", "dlig", "discretionary ligatures", false),
];

/// A feature setting's tag and whether it turns the feature on, in
/// HarfBuzz's syntax: "smcp" and "ss01=1" on, "-liga" and "liga=0" off.
fn setting(s: &str) -> (&str, bool) {
    let s = s.trim();
    let (s, on) = match s.strip_prefix('-') {
        Some(rest) => (rest, false),
        None => (s.strip_prefix('+').unwrap_or(s), true),
    };
    let (tag, value) = match s.split_once('=') {
        Some((t, v)) => (t, Some(v.trim())),
        None => (s, None),
    };
    let tag = tag.split('[').next().unwrap_or(tag).trim();
    (tag, on && value.is_none_or(|v| !matches!(v, "0" | "off" | "false")))
}

/// Whether `tag` is on after `lists`, each set over the one before as
/// a run's are over its box's: the last setting naming it wins, else
/// `default`.
fn is_on(lists: &[&[String]], tag: &str, default: bool) -> bool {
    lists.iter().flat_map(|l| l.iter()).rev().map(|s| setting(s)).find(|(t, _)| *t == tag).map_or(default, |(_, on)| on)
}

/// A stylistic set's tag, "ss01" to "ss20".
fn is_stylistic_set(tag: &str) -> bool {
    tag.strip_prefix("ss").and_then(|n| (n.len() == 2).then(|| n.parse::<u8>().ok()).flatten()).is_some_and(|n| (1..=20).contains(&n))
}

/// The stylistic sets on after `lists`, in tag order.
fn sets_on(lists: &[&[String]]) -> Vec<String> {
    let mut tags: Vec<&str> = lists.iter().flat_map(|l| l.iter()).map(|s| setting(s).0).filter(|t| is_stylistic_set(t)).collect();
    tags.sort();
    tags.dedup();
    tags.into_iter().filter(|t| is_on(lists, t, false)).map(str::to_string).collect()
}

/// `features` with `tag`'s settings taken out and `set`, if any, put in.
fn with_setting(features: &[String], tag: &str, set: Option<String>) -> Vec<String> {
    let mut out: Vec<String> = features.iter().filter(|s| setting(s).0 != tag).cloned().collect();
    out.extend(set);
    out
}

/// The settings typed in the raw field: words split at spaces and
/// commas.
fn parse_raw(text: &str) -> Vec<String> {
    text.split([' ', ',']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// The row for the face of `family` in `style`, its features read
/// through klipp-title (#1710). `selection` is the text selected, whose
/// run the row reads and writes; `None` the box's.
#[allow(clippy::too_many_arguments)]
pub(super) fn show_title_features_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    tb: &TextBox,
    family: &str,
    style: FontStyle,
    selection: Option<(usize, usize)>,
    cmds: &mut Vec<AppCommand>,
) {
    let style = match style {
        FontStyle::Regular => "Regular",
        FontStyle::Bold => "Bold",
        FontStyle::Italic => "Italic",
        FontStyle::BoldItalic => "Bold Italic",
    };
    let registry = klipp_title::font_discovery::discover_system_fonts();
    let face = klipp_title::font_discovery::family_feature_tags(&registry, family, style);
    show_features(ui, theme, clip_id, tb, &face, selection, cmds);
}

/// The row itself, for a face whose feature tags are `face`.
pub(super) fn show_features(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    tb: &TextBox,
    face: &[String],
    selection: Option<(usize, usize)>,
    cmds: &mut Vec<AppCommand>,
) {
    // The runs over the selection, each read over the box's own.
    let runs: Vec<&klipp_timeline::StyleRun> = match selection {
        Some(sel) => tb.style_runs.iter().filter(|r| r.start < sel.1 && r.end > sel.0).collect(),
        None => Vec::new(),
    };
    let views: Vec<[&[String]; 2]> = if runs.is_empty() {
        vec![[&tb.features[..], &[][..]]]
    } else {
        runs.iter().map(|r| [&tb.features[..], &r.features[..]]).collect()
    };
    let write = |cmds: &mut Vec<AppCommand>, run: Vec<String>, boxed: Vec<String>| match selection {
        Some(sel) => cmds.push(AppCommand::ApplyTitleStyle {
            clip_id,
            range_start: sel.0,
            range_end: sel.1,
            property: "features".into(),
            value: serde_json::to_string(&run).unwrap_or_default(),
        }),
        None => cmds.push(AppCommand::SetTitleFeatures { clip_id, features: boxed }),
    };
    let has = |tag: &str| face.iter().any(|t| t == tag);

    ui.label(egui::RichText::new("Font features").size(11.0).color(theme.text_secondary));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (label, tag, lacks, default) in TOGGLES {
            let states: Vec<bool> = views.iter().map(|v| is_on(v, tag, default)).collect();
            let on = states.iter().all(|s| *s);
            let mixed = !on && states.iter().any(|s| *s);
            // Lit only while the feature is on throughout; a face
            // without it still takes it, dimmed.
            let (fill, text) = match (on, has(tag)) {
                (true, true) => (theme.accent, theme.text_on_accent),
                (true, false) => (theme.accent.gamma_multiply(0.5), theme.text_dim),
                (false, true) => (theme.bg_raised, theme.text_secondary),
                (false, false) => (theme.bg_raised, theme.text_dim),
            };
            let button = egui::Button::new(egui::RichText::new(label).size(11.0).color(text)).fill(fill).rounding(4.0);
            let mut resp = ui.add(button);
            resp = if !has(tag) {
                resp.on_hover_text(format!("This font has no {lacks}"))
            } else if mixed {
                resp.on_hover_text(format!("Part of the selection has {lacks}"))
            } else {
                resp.on_hover_text(format!("OpenType \u{201c}{tag}\u{201d}"))
            };
            if resp.clicked() {
                // A mixed selection is turned on throughout; the box
                // names a feature only where it differs from the shaper's.
                let set = if on { format!("-{tag}") } else { tag.to_string() };
                let boxed = with_setting(&tb.features, tag, (on == default).then(|| set.clone()));
                write(cmds, vec![set], boxed);
            }
        }
    });

    // The stylistic sets the face has; one on at a time.
    let offered: Vec<&String> = face.iter().filter(|t| is_stylistic_set(t)).collect();
    let current: Vec<Vec<String>> = views.iter().map(|v| sets_on(v)).collect();
    if !offered.is_empty() || current.iter().any(|c| !c.is_empty()) {
        let mixed = current.iter().any(|c| *c != current[0]);
        let label = |sets: &[String]| if sets.is_empty() { "None".to_string() } else { sets.join(", ") };
        let mut picked: Option<Option<String>> = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Stylistic set").size(11.0).color(theme.text_secondary));
            egui::ComboBox::from_id_salt("title_stylistic_set")
                .selected_text(if mixed { "Mixed".to_string() } else { label(&current[0]) })
                .show_ui(ui, |ui| {
                    if ui.selectable_label(!mixed && current[0].is_empty(), "None").clicked() {
                        picked = Some(None);
                    }
                    for tag in &offered {
                        if ui.selectable_label(!mixed && current[0] == [tag.to_string()], tag.as_str()).clicked() {
                            picked = Some(Some(tag.to_string()));
                        }
                    }
                });
        });
        if let Some(set) = picked {
            let mut on: Vec<String> = current.concat();
            on.sort();
            on.dedup();
            let mut run: Vec<String> = on.iter().filter(|t| Some(*t) != set.as_ref()).map(|t| format!("-{t}")).collect();
            run.extend(set.clone());
            let mut boxed: Vec<String> = tb.features.iter().filter(|s| !is_stylistic_set(setting(s).0)).cloned().collect();
            boxed.extend(set);
            write(cmds, run, boxed);
        }
    }

    // Any other tag, as typed: the box's whole list, or settings put
    // over the selection's own tag by tag.
    let own: Vec<String> = match runs.first() {
        Some(r) => r.features.clone(),
        None => tb.features.clone(),
    };
    egui::CollapsingHeader::new(egui::RichText::new("Other features").size(11.0).color(theme.text_secondary))
        .id_salt("title_features_raw")
        .show(ui, |ui| {
            let id = ui.id().with("buffer");
            let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| own.join(" "));
            let resp = ui.add(egui::TextEdit::singleline(&mut text).hint_text("e.g. ss03 cv01 -kern frac").desired_width(f32::INFINITY))
                .on_hover_text("OpenType features as HarfBuzz writes them: a tag turns one on, \u{201c}-tag\u{201d} off, \u{201c}tag=2\u{201d} sets a value");
            if resp.has_focus() {
                ui.data_mut(|d| d.insert_temp(id, text.clone()));
            }
            if resp.lost_focus() {
                ui.data_mut(|d| d.remove::<String>(id));
                let typed = parse_raw(&text);
                if typed != own {
                    write(cmds, typed.clone(), typed);
                }
            }
        });
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> ClipId {
        ClipId(uuid::Uuid::nil())
    }

    fn inter() -> Vec<String> {
        klipp_title::font_discovery::face_feature_tags(klipp_title::bundled::lookup("Inter", "Regular").unwrap())
    }

    struct Frame {
        texts: Vec<(String, egui::Pos2)>,
        cmds: Vec<AppCommand>,
    }

    /// One frame of the row for `tb` on a face with `face`'s features,
    /// with `events`.
    fn frame(ctx: &egui::Context, tb: &TextBox, face: &[String], selection: Option<(usize, usize)>, events: Vec<egui::Event>) -> Frame {
        let mut cmds = Vec::new();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 1200.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_features(ui, &KlippTheme::default(), clip(), tb, face, selection, &mut cmds);
                });
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

    /// A click on the text `label`: the commands the press and the
    /// release gave.
    fn click(ctx: &egui::Context, tb: &TextBox, face: &[String], selection: Option<(usize, usize)>, label: &str) -> Vec<AppCommand> {
        let pos = at(&frame(ctx, tb, face, selection, Vec::new()), label);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, tb, face, selection, vec![egui::Event::PointerMoved(pos), button(true)]).cmds;
        cmds.extend(frame(ctx, tb, face, selection, vec![button(false)]).cmds);
        cmds
    }

    fn boxed(cmds: &[AppCommand]) -> Vec<String> {
        match cmds {
            [AppCommand::SetTitleFeatures { features, .. }] => features.clone(),
            other => panic!("one command, the box's features: {other:?}"),
        }
    }

    /// #1710: Small caps on the box writes "smcp", in one command, and
    /// a second click takes it away; the box's other features stay.
    #[test]
    fn small_caps_toggles_smcp_on_the_box_in_one_step() {
        let ctx = egui::Context::default();
        let face = vec!["smcp".to_string()];
        let mut tb = TextBox { text: "Title".into(), features: vec!["tnum".into()], ..Default::default() };
        tb.features = boxed(&click(&ctx, &tb, &face, None, "Small caps"));
        assert_eq!(tb.features, ["tnum", "smcp"]);
        tb.features = boxed(&click(&ctx, &tb, &face, None, "Small caps"));
        assert_eq!(tb.features, ["tnum"]);
    }

    /// Ligatures are on unless named off: turning them off writes
    /// "-liga", and on again takes it away (#1710).
    #[test]
    fn ligatures_off_writes_minus_liga() {
        let ctx = egui::Context::default();
        let face = vec!["liga".to_string()];
        let mut tb = TextBox { text: "office".into(), ..Default::default() };
        tb.features = boxed(&click(&ctx, &tb, &face, None, "Ligatures"));
        assert_eq!(tb.features, ["-liga"]);
        tb.features = boxed(&click(&ctx, &tb, &face, None, "Ligatures"));
        assert!(tb.features.is_empty());
    }

    /// Over a selection a toggle writes the selection's run, through
    /// `ApplyTitleStyle`, and leaves the box's alone (#1710).
    #[test]
    fn over_a_selection_a_toggle_writes_the_run_only() {
        let ctx = egui::Context::default();
        let mut tb = TextBox { text: "Title text".into(), features: vec!["onum".into()], ..Default::default() };
        tb.style_runs = vec![klipp_timeline::StyleRun::default_for_range(0, 5)];
        let cmds = click(&ctx, &tb, &inter(), Some((0, 5)), "Tabular figures");
        match &cmds[..] {
            [AppCommand::ApplyTitleStyle { range_start: 0, range_end: 5, property, value, .. }] => {
                assert_eq!(property, "features");
                assert_eq!(serde_json::from_str::<Vec<String>>(value).unwrap(), ["tnum"]);
            }
            other => panic!("one command, the run's: {other:?}"),
        }
        // The box's own feature reads as on over the run, and off turns
        // it off for the run alone.
        let cmds = click(&ctx, &tb, &inter(), Some((0, 5)), "Old-style figures");
        assert!(matches!(&cmds[..], [AppCommand::ApplyTitleStyle { value, .. }] if value == r#"["-onum"]"#), "{cmds:?}");
    }

    /// The picker offers the face's stylistic sets and no other: Inter's
    /// ss01 to ss08 (#1710); one picked replaces the set on.
    #[test]
    fn the_stylistic_set_picker_lists_the_faces_sets() {
        let ctx = egui::Context::default();
        let tb = TextBox { text: "Title".into(), features: vec!["ss02".into(), "tnum".into()], ..Default::default() };
        let face = inter();
        frame(&ctx, &tb, &face, None, Vec::new());
        let f = frame(&ctx, &tb, &face, None, Vec::new());
        let combo = at(&f, "ss02");
        let button = |pressed| egui::Event::PointerButton { pos: combo, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame(&ctx, &tb, &face, None, vec![egui::Event::PointerMoved(combo), button(true)]);
        frame(&ctx, &tb, &face, None, vec![button(false)]);
        let open = frame(&ctx, &tb, &face, None, Vec::new());
        let offered: Vec<String> = open.texts.iter().map(|(t, _)| t.clone()).filter(|t| is_stylistic_set(t)).collect();
        let mut offered_once = offered.clone();
        offered_once.sort();
        offered_once.dedup();
        assert_eq!(offered_once, (1..=8).map(|n| format!("ss{n:02}")).collect::<Vec<_>>(), "Inter's sets, none past ss08");
        // ss05 picked: the box's ss02 goes, its other features stay.
        let pos = open.texts.iter().rev().find(|(t, _)| t == "ss05").map(|(_, p)| *p).unwrap();
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(&ctx, &tb, &face, None, vec![egui::Event::PointerMoved(pos), button(true)]).cmds;
        cmds.extend(frame(&ctx, &tb, &face, None, vec![button(false)]).cmds);
        assert_eq!(boxed(&cmds), ["tnum", "ss05"]);
    }

    /// A face's missing feature can still be set (#1710).
    #[test]
    fn a_feature_the_face_lacks_still_sets() {
        let ctx = egui::Context::default();
        let tb = TextBox { text: "Title".into(), ..Default::default() };
        assert!(!inter().iter().any(|t| t == "smcp"));
        assert_eq!(boxed(&click(&ctx, &tb, &inter(), None, "Small caps")), ["smcp"]);
    }

    #[test]
    fn a_setting_reads_as_harfbuzz_writes_it() {
        assert_eq!(setting("smcp"), ("smcp", true));
        assert_eq!(setting("-liga"), ("liga", false));
        assert_eq!(setting("+kern"), ("kern", true));
        assert_eq!(setting("ss01=1"), ("ss01", true));
        assert_eq!(setting("liga=0"), ("liga", false));
        assert_eq!(setting("smcp[3:5]"), ("smcp", true));
        let boxed = vec!["onum".to_string()];
        let run = vec!["-onum".to_string()];
        assert!(!is_on(&[&boxed, &run], "onum", false), "a run's over its box's");
        assert!(is_on(&[&boxed, &[]], "onum", false));
        assert!(is_on(&[&[], &[]], "liga", true));
        assert_eq!(parse_raw("ss03, cv01  -kern"), ["ss03", "cv01", "-kern"]);
    }
}
