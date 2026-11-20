//! The texts a test reads off a frame's shapes, as a person reads them.
//!
//! egui 0.34 draws a widget's parts as separate galleys where 0.29 drew
//! one: a number field's "100" and its " %" suffix are two text shapes,
//! side by side with no gap, which still read "100 %" on screen. Tests
//! that look for what is written join such neighbours first: galleys on
//! one line whose edges touch are one text.

/// Each text shape of `shapes` with its centre, neighbours that touch on
/// one line joined into one text.
pub fn screen_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Pos2)> {
    let mut parts: Vec<(String, egui::Rect)> = Vec::new();
    for s in shapes {
        if let egui::Shape::Text(t) = &s.shape {
            parts.push((t.galley.text().to_string(), egui::Rect::from_min_size(t.pos, t.galley.size())));
        }
    }
    let mut out: Vec<(String, egui::Rect)> = Vec::new();
    for (text, rect) in parts {
        // A single-line galley that starts where the last one ended, on
        // the same line, continues it.
        if let Some((last_text, last_rect)) = out.last_mut() {
            let same_line = (last_rect.center().y - rect.center().y).abs() < 1.0 && (last_rect.height() - rect.height()).abs() < 1.0;
            let touching = (rect.min.x - last_rect.max.x).abs() < 0.5;
            if same_line && touching && !text.contains('\n') && !last_text.contains('\n') {
                last_text.push_str(&text);
                *last_rect = last_rect.union(rect);
                continue;
            }
        }
        out.push((text, rect));
    }
    out.into_iter().map(|(t, r)| (t, r.center())).collect()
}

#[cfg(test)]
mod tests {
    /// A number field's value and suffix read as one text, as on screen,
    /// and texts apart from each other stay apart.
    #[test]
    fn a_value_and_its_suffix_read_as_one_text() {
        let ctx = egui::Context::default();
        let mut v = 100.0_f32;
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Width");
                ui.add(crate::widgets::number_field(&mut v).suffix(" %"));
            });
        });
        let texts: Vec<String> = super::screen_texts(&out.shapes).into_iter().map(|(t, _)| t).collect();
        assert!(texts.contains(&"100 %".to_string()), "{texts:?}");
        assert!(texts.contains(&"Width".to_string()), "{texts:?}");
    }
}
