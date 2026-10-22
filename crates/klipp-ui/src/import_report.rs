//! The same import report shown locally and returned to control clients.
use crate::state::AppState;
use klipp_core::AppCommand;

pub fn show_dialog(ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    if !state.show_import_report { return commands; }
    let Some(report) = state.last_import_report.as_ref() else { state.show_import_report = false; return commands; };
    egui::Window::new("Last Import Report").open(&mut state.show_import_report)
        .default_width(560.0).resizable(true).show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
            ui.label(report.source.file_name().unwrap_or_default().to_string_lossy());
            let n = &report.imported;
            ui.label(format!("Imported: {} sequence(s), {} track(s), {} clip(s), {} transition(s), {} marker(s), {} title(s)", n.sequences,n.tracks,n.clips,n.transitions,n.markers,n.titles));
            ui.separator();
            if report.warnings.is_empty() { ui.label("No import warnings."); }
            else {
                ui.label("Skipped or changed during import (counts are warning notices):");
                for group in &report.warnings {
                    ui.strong(format!("{} — {} notice(s)", group.kind, group.warning_count));
                    for message in &group.examples { ui.label(message); }
                }
                ui.collapsing("All import warnings", |ui| { for message in &report.details { ui.label(message); } });
            }
            ui.separator();
            ui.strong(format!("Missing media at import: {}", report.missing_media.len()));
            for media in &report.missing_media { ui.label(&media.name).on_hover_text(media.path.display().to_string()); }
            let missing_now = report.missing_media.iter().any(|m| state.media_pool.get(&m.id).is_some_and(|item| item.offline));
            if ui.add_enabled(missing_now, egui::Button::new("Relink…")).clicked() { commands.push(AppCommand::ShowRelinkDialog); }
        });
    });
    if !commands.is_empty() { state.show_import_report = false; }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_fixture_is_visible_and_relink_opens_the_existing_flow() {
        let path = std::env::temp_dir().join(format!("klipp-ui-report-{}.otio", uuid::Uuid::new_v4()));
        std::fs::write(&path, include_str!("../../klipp-project/tests/fixtures/import_report.otio")).unwrap();
        let result = klipp_project::import_report::load(&path, klipp_core::Fps::film_24(), &klipp_media::MediaPool::new()).unwrap();
        let report = klipp_project::import_report::ImportReport::from_import(&path, &result);
        let mut state = AppState { last_import_report: Some(report), show_import_report: true, media_pool: result.media_pool, ..Default::default() };
        let ctx = egui::Context::default();
        let mut frame = |events| {
            let mut commands = Vec::new();
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0,800.0))), events, ..Default::default() }, |ctx| commands.extend(show_dialog(ctx, &mut state)));
            (out, commands)
        };
        frame(Vec::new());
        let (out, _) = frame(Vec::new());
        let texts: Vec<_> = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t), _ => None }).collect();
        assert!(texts.iter().any(|t| t.galley.text().contains("Effects — 1 notice(s)")));
        assert!(texts.iter().any(|t| t.galley.text() == "Missing media at import: 1"));
        let button = texts.iter().find(|t| t.galley.text() == "Relink…").unwrap();
        let pos = button.pos + button.galley.size()/2.0;
        let click = |pressed| egui::Event::PointerButton { pos, button:egui::PointerButton::Primary,pressed,modifiers:egui::Modifiers::NONE };
        frame(vec![egui::Event::PointerMoved(pos),click(true)]);
        let (_, commands) = frame(vec![click(false)]);
        assert!(matches!(commands.as_slice(), [AppCommand::ShowRelinkDialog]));
        assert!(!state.show_import_report);
        std::fs::remove_file(path).unwrap();
    }

    /// An imported timeline whose media was moved to a folder beside its
    /// own: the search beginning where the import's relink looks finds it
    /// by name, and proposes it.
    #[test]
    fn media_moved_beside_the_timeline_is_found_for_relinking() {
        let root = std::env::temp_dir().join(format!("klipp-relink-{}", uuid::Uuid::new_v4()));
        let (edit, footage) = (root.join("job/edit"), root.join("job/footage"));
        std::fs::create_dir_all(&edit).unwrap();
        std::fs::create_dir_all(&footage).unwrap();
        std::fs::write(footage.join("a001.mov"), b"not really a movie").unwrap();
        let gone = root.join("job/media/a001.mov");
        let otio = serde_json::json!({
            "OTIO_SCHEMA": "Timeline.1", "name": "cut",
            "tracks": { "OTIO_SCHEMA": "Stack.1", "children": [{ "OTIO_SCHEMA": "Track.1", "name": "V1", "kind": "Video", "children": [{
                "OTIO_SCHEMA": "Clip.1", "name": "a001.mov",
                "source_range": { "OTIO_SCHEMA": "TimeRange.1",
                    "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 0.0 },
                    "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24.0, "value": 24.0 } },
                "media_reference": { "OTIO_SCHEMA": "ExternalReference.1", "target_url": format!("file://{}", gone.display()), "metadata": {} }
            }] }] }
        });
        let path = edit.join("cut.otio");
        std::fs::write(&path, otio.to_string()).unwrap();
        let result = klipp_project::import_report::load(&path, klipp_core::Fps::film_24(), &klipp_media::MediaPool::new()).unwrap();
        let wanted: Vec<_> = result.media_pool.all().filter(|m| m.offline).map(klipp_media::relink::Wanted::from_item).collect();
        assert_eq!(wanted.len(), 1, "the clip's media is missing");
        let dir = klipp_project::import_report::relink_search_dir(&path);
        assert_eq!(dir, root.join("job"));
        let criteria = klipp_media::relink::Criteria::default();
        let found = klipp_media::relink::scan(&dir, &wanted, criteria, &std::sync::atomic::AtomicBool::new(false), |_| {});
        let proposals = klipp_media::relink::propose(&wanted, &found.candidates, criteria);
        match &proposals[0] {
            klipp_media::relink::Proposal::Found(hit) => assert_eq!(found.candidates[hit.candidate].path, footage.join("a001.mov")),
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
