//! Explicit graph access in either workspace; navigation is session state.
use super::*;
use klipp_graph_format::path::{NodePath, Root};
use klipp_timeline::GradeAccess;

impl AppState {
    /// Keep the deepest surviving ancestor after deleting a compound or
    /// switching grade versions. This repairs navigation, never the grade.
    pub(crate) fn repair_grade_descent(&mut self) -> bool {
        let mut valid = 0;
        if let Some(clip) = self.selected_clip.and_then(|id| self.sequence().all_clips().find(|c| c.id == id)) {
            let mut graph = clip.grade();
            for id in &self.graph_descent_path {
                let Some(node) = graph.nodes.iter().find(|n| n.id == *id && n.kind == klipp_nodes::NodeKind::Compound) else { break };
                let Some(body) = node.params.compound_subgraph.as_ref() else { break };
                graph = body;
                valid += 1;
            }
        }
        if valid == self.graph_descent_path.len() { return false; }
        self.graph_descent_path.truncate(valid);
        self.selected_node_id = None;
        self.selected_node_ids.clear();
        self.selected_node_addr = None;
        true
    }

    pub(crate) fn toggle_graph_dock(&mut self) {
        self.close_path_drag_before_edit();
        self.graph_dock_open = !self.graph_dock_open;
        if self.graph_dock_open {
            self.repair_graph_dock();
            self.focus_graph_dock();
        } else if matches!(self.active_panel, Some(crate::state::PanelKind::PictureGraph | crate::state::PanelKind::NodeGraph)) { self.active_panel = None; }
    }

    pub(crate) fn focus_graph_dock(&mut self) {
        let in_grade = self.graph_dock_root.and_then(|root| match root { Root::Clip { clip } => Some(ClipId(clip)), _ => None })
            .and_then(|id| self.sequence().all_clips().find(|c| c.id == id))
            .and_then(|c| c.picture_v2().ok())
            .is_some_and(|p| p.picture.graph.nodes.iter().any(|n| self.graph_dock_path.first() == Some(&n.id) && n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP));
        self.active_panel = Some(if in_grade { crate::state::PanelKind::NodeGraph } else { crate::state::PanelKind::PictureGraph });
    }

    /// Follow clip selection and retain the longest surviving group path.
    /// Neither opening the dock nor repairing stale navigation edits a clip.
    pub(crate) fn repair_graph_dock(&mut self) {
        if !self.graph_dock_open { return; }
        let exists = |id| self.sequence().all_clips().any(|c| c.id == id);
        let current = self.graph_dock_root.and_then(|r| match r { Root::Clip { clip } => Some(ClipId(clip)), _ => None });
        let clip_id = self.selected_clip.filter(|id| exists(*id)).or_else(|| current.filter(|id| exists(*id)));
        let root = clip_id.map(|clip| Root::Clip { clip: clip.0 });
        if root != self.graph_dock_root {
            self.graph_dock_root = root;
            self.graph_dock_path.clear();
            self.graph_descent_path.clear();
            self.selected_node_addr = None;
            self.selected_node_id = None;
            self.selected_node_ids.clear();
        }
        let Some(clip_id) = clip_id else { self.graph_dock_path.clear(); return };
        let Some(picture) = self.sequence().all_clips().find(|c| c.id == clip_id).and_then(|c| c.picture_v2().ok()) else { return };
        let mut graph = &picture.picture.graph;
        let mut valid = 0;
        for id in &self.graph_dock_path {
            let Some(body) = graph.nodes.iter().find(|n| n.id == *id).and_then(|n| n.group.as_ref()) else { break };
            graph = &body.graph;
            valid += 1;
        }
        if valid != self.graph_dock_path.len() {
            self.graph_dock_path.truncate(valid);
            self.selected_node_addr = None;
            self.selected_node_id = None;
            self.selected_node_ids.clear();
        }
        if self.selected_node_addr.as_ref().is_some_and(|addr| addr.root != root.unwrap() || addr.nodes.split_last().is_none_or(|(id, parent)| parent != self.graph_dock_path || !graph.nodes.iter().any(|n| n.id == *id))) {
            self.selected_node_addr = None;
        }
    }
}

/// Whether the picture graph's selection, changed from `before`, names an
/// effect node of the clip, whose card the inspector reveals (#1347).
pub(super) fn reveals_card(state: &AppState, before: Option<&NodePath>) -> bool {
    let Some(addr) = state.selected_node_addr.as_ref().filter(|a| Some(*a) != before) else { return false };
    let Some(clip) = addr.root_clip().and_then(|id| state.sequence().all_clips().find(|c| c.id.0 == id)) else { return false };
    let Ok(lifted) = clip.picture_v2() else { return false };
    crate::inspector::stack_view::node_at(&lifted.picture.graph, addr).and_then(klipp_graph_lift::effect_params::tag).is_some()
}

impl KlippApp {
    pub(super) fn render_graph_dock(&mut self, ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, rect: egui::Rect, theme: &KlippTheme) {
        self.state.repair_graph_dock();
        self.refresh_node_thumbnails();
        let root = self.state.graph_dock_root;
        let clip_id = root.and_then(|root| match root { Root::Clip { clip } => Some(ClipId(clip)), _ => None });
        let grade = clip_id.and_then(|id| self.state.sequence().all_clips().find(|c| c.id == id)).and_then(|c| c.picture_v2().ok()).and_then(|p| p.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).and_then(|n| n.group.as_ref().map(|g| (n.id, g.graph.output))));
        let in_grade = grade.is_some_and(|(id, _)| self.state.graph_dock_path.first() == Some(&id));
        ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
        let mut header_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true)));
        {
            let ui = &mut header_ui;
            if ui.selectable_label(!in_grade, "Picture").clicked() {
                self.state.active_panel = Some(crate::state::PanelKind::PictureGraph);
                if let Some(root) = root { cmds.push(AppCommand::SetGraphDockRoot { root }); }
            }
            if ui.add_enabled(grade.is_some(), egui::Button::new("Grade").selected(in_grade)).clicked() {
                if let (Some(root), Some((group, output))) = (root, grade) {
                    self.state.active_panel = Some(crate::state::PanelKind::NodeGraph);
                    self.state.graph_descent_path.clear();
                    cmds.push(AppCommand::RevealGraph { path: NodePath::new(root, [group, output]), select: false });
                }
            }
            ui.separator();
            for (placement, label) in [(crate::layout_state::NodeGraphPlacement::Below, "Below"), (crate::layout_state::NodeGraphPlacement::Right, "Right")] {
                if ui.selectable_label(self.layout.node_graph_placement == placement, label).clicked() {
                    self.layout.node_graph_placement = placement;
                    self.layout_dirty = true;
                }
            }
            if ui.small_button("×").on_hover_text("Close graph").clicked() { cmds.push(AppCommand::ToggleGraphDock); }
        }
        let header_height = header_ui.min_rect().height().max(28.0);
        let body = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + header_height), rect.max);
        if !body.is_positive() { return; }
        if in_grade {
            self.render_color_ws_node_strip(ui, cmds, body, theme);
            return;
        }
        crate::ui_util::allocate_clipped_ui(ui, body, |ui| {
            let Some(clip) = clip_id.and_then(|id| self.state.sequences[self.state.active_sequence_idx].all_clips().find(|c| c.id == id)) else {
                ui.centered_and_justified(|ui| { ui.label("Select a clip to show its graph"); });
                return;
            };
            let mut level = NodePath::new(root.unwrap(), self.state.graph_dock_path.clone());
            let time = (self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
            let keyboard_enabled = self.state.active_panel == Some(crate::state::PanelKind::PictureGraph);
            match self.node_graph_v2_panel.show_picture(ui, clip, &mut level, &mut self.state.selected_node_id, &mut self.state.selected_node_ids, theme, time, keyboard_enabled, &self.state.node_thumbs, self.state.node_thumbs_gen, &mut self.state.deferred_texture_drops[self.state.deferred_texture_drops_slot]) {
                Ok(updates) => cmds.extend(updates),
                Err(why) => { ui.label(why); }
            }
            self.state.graph_dock_path = level.nodes.clone();
            let before = self.state.selected_node_addr.take();
            self.state.selected_node_addr = self.state.selected_node_id.map(|node| NodePath::new(level.root, level.nodes.iter().copied().chain([node])));
            // An effect node picked: its card, on the Effects tab, which
            // scrolls it into view (#1347). Layout only, never the document.
            if reveals_card(&self.state, before.as_ref()) {
                self.layout.active_inspector_tab = crate::layout_state::InspectorTab::Effects;
                self.set_inspector_visible(true);
            }
            crate::state::note_active_panel(&mut self.state, crate::state::PanelKind::PictureGraph, ui.ctx(), body);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (AppState, ClipId, NodePath) {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let id = clip.id;
        let picture = clip.picture_v2().unwrap();
        let group = picture.picture.graph.nodes.iter().find(|n| n.group.is_some()).unwrap();
        let path = NodePath::new(Root::Clip { clip: id.0 }, [group.id, group.group.as_ref().unwrap().graph.output]);
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.selected_clip = Some(id);
        (state, id, path)
    }

    /// A state holding a clip with ten blurs, and their nodes' paths.
    fn blurred() -> (AppState, klipp_timeline::Clip, Vec<NodePath>) {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let clip = klipp_timeline::test_clip::TestClip::of(klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0)))
            .effects((0..10).map(|i| klipp_effects::BuiltinEffect::GaussianBlur { radius: i as f32 + 1.0 }))
            .build();
        let lifted = clip.picture_v2().unwrap();
        let graph = &lifted.picture.graph;
        let blurs = graph.nodes.iter().filter(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).map(|n| NodePath::new(Root::Clip { clip: clip.id.0 }, [n.id])).collect::<Vec<_>>();
        assert_eq!(blurs.len(), 10);
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip.clone());
        state.selected_clip = Some(clip.id);
        (state, clip, blurs)
    }

    /// Picking an effect node in the picture graph reveals its card, by
    /// the node's full path; the same pick again, a node that isn't an
    /// effect, or no pick reveals nothing (#1347).
    #[test]
    fn picking_an_effect_node_reveals_its_card() {
        let (mut s, clip, blurs) = blurred();
        s.selected_node_addr = Some(blurs[3].clone());
        assert!(reveals_card(&s, None), "an effect picked");
        assert!(reveals_card(&s, Some(&blurs[0])), "another effect picked");
        assert!(!reveals_card(&s, Some(&blurs[3])), "the same pick, again");
        let lifted = clip.picture_v2().unwrap();
        let graph = &lifted.picture.graph;
        let source = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n).is_none()).unwrap();
        s.selected_node_addr = Some(NodePath::new(Root::Clip { clip: clip.id.0 }, [source.id]));
        assert!(!reveals_card(&s, None), "{} isn't an effect", source.type_id.as_str());
        s.selected_node_addr = None;
        assert!(!reveals_card(&s, Some(&blurs[3])), "nothing picked");
    }

    /// The picked effect's card scrolls into view in the Effects tab's
    /// stack, once for each pick, and the document is left alone: no
    /// command, no step (#1347).
    #[test]
    fn the_picked_card_scrolls_into_view_once_without_an_edit() {
        let (mut s, clip, blurs) = blurred();
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut frame = |s: &AppState, scroll: Option<f32>| {
            clock += 0.1;
            let mut cmds = Vec::new();
            let mut offset = 0.0;
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 600.0))), time: Some(clock), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let mut area = egui::ScrollArea::vertical().id_salt("stack").max_height(160.0).animated(false);
                    if let Some(y) = scroll {
                        area = area.vertical_scroll_offset(y);
                    }
                    offset = area.show(ui, |ui| crate::inspector::stack_view::show(ui, &KlippTheme::default(), s, &clip, &mut cmds, 0.0)).state.offset.y;
                });
            });
            assert!(cmds.is_empty(), "drawing the stack sent {cmds:?}");
            offset
        };
        for _ in 0..3 {
            assert_eq!(frame(&s, None), 0.0, "nothing picked: the top");
        }
        s.selected_node_addr = Some(blurs[9].clone());
        let mut offset = 0.0;
        for _ in 0..3 {
            offset = frame(&s, None);
        }
        assert!(offset > 100.0, "the last blur's card scrolled to: {offset}");
        // Scrolled back by hand: the same pick doesn't pull it down again.
        frame(&s, Some(0.0));
        for _ in 0..2 {
            assert_eq!(frame(&s, None), 0.0, "revealed once per pick");
        }
        assert_eq!(s.active_history().undo_count(), 0, "no step");
        assert!(!s.dirty, "the document untouched");
    }

    fn nested_grade() -> (AppState, ClipId, Vec<klipp_nodes::NodeId>) {
        fn wrap(body: klipp_nodes::NodeGraph) -> (klipp_nodes::NodeGraph, klipp_nodes::NodeId) {
            let mut graph = klipp_nodes::NodeGraph::default();
            let id = graph.ensure_corrector();
            let node = graph.nodes.iter_mut().find(|n| n.id == id).unwrap();
            node.kind = klipp_nodes::NodeKind::Compound;
            node.params = klipp_nodes::NodeParams { compound_subgraph: Some(body), ..Default::default() };
            (graph, id)
        }
        let (inner, inner_id) = wrap(Default::default());
        let (outer, outer_id) = wrap(inner);
        let (mut s, id, _) = state();
        s.sequences[0].all_clips_mut().find(|c| c.id == id).unwrap().set_grade(outer);
        s.graph_descent_path = vec![outer_id, inner_id];
        (s, id, vec![outer_id, inner_id])
    }

    #[test]
    fn a_grade_version_switch_keeps_a_valid_path() {
        let (mut s, id, path) = nested_grade();
        for name in ["First", "Second"] {
            assert!(s.grade_version_command(&AppCommand::SaveGradeVersion { clip_id: id, name: name.into() }));
        }
        for version_index in [0, 1] {
            assert!(s.grade_version_command(&AppCommand::SwitchGradeVersion { clip_id: id, version_index }));
            let before = s.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap();
            let dirty = s.dirty;
            assert!(!s.repair_grade_descent());
            assert_eq!(s.graph_descent_path, path);
            assert_eq!(s.selected_clip, Some(id));
            assert_eq!(s.dirty, dirty);
            assert_eq!(s.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap(), before);
        }
    }

    #[test]
    fn deleted_groups_repair_the_crumbs_to_the_nearest_ancestor() {
        let (mut s, id, path) = nested_grade();
        s.selected_node_id = Some(uuid::Uuid::new_v4());
        s.selected_node_ids.extend(s.selected_node_id);
        s.sequences[0].all_clips_mut().find(|c| c.id == id).unwrap().edit_grade(|g| {
            g.nodes.iter_mut().find(|n| n.id == path[0]).unwrap().params.compound_subgraph = Some(Default::default());
        });
        let before = s.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap();
        assert!(s.repair_grade_descent());
        assert_eq!(s.graph_descent_path, path[..1]);
        assert!(s.selected_node_id.is_none() && s.selected_node_ids.is_empty());
        assert!(!s.dirty);
        assert_eq!(s.active_history().undo_count(), 0);
        assert_eq!(s.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap(), before);
        s.sequences[0].all_clips_mut().find(|c| c.id == id).unwrap().set_grade(Default::default());
        assert!(s.repair_grade_descent());
        assert!(s.graph_descent_path.is_empty());
        assert_eq!(s.selected_clip, Some(id));
        assert!(!s.repair_grade_descent());
    }

    #[test]
    fn open_graph_toggles_in_each_workspace_without_editing() {
        for workspace in [crate::state::Workspace::Edit, crate::state::Workspace::Color] {
            let (mut s, id, _) = state();
            s.active_workspace = workspace;
            s.show_color_panel = workspace == crate::state::Workspace::Color;
            let before = s.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap();
            let dirty = s.dirty;
            for open in [true, false, true, false] {
                assert_eq!(s.path_command(&AppCommand::ToggleGraphDock), Some(false));
                assert_eq!(s.graph_dock_open, open);
                assert_eq!(s.active_workspace, workspace);
                assert_eq!(s.dirty, dirty);
            }
            assert_eq!(s.sequence().all_clips().find(|c| c.id == id).unwrap().picture_v2().unwrap(), before);
            assert_eq!(s.active_history().undo_count(), 0);
        }
    }

    #[test]
    fn stale_graph_paths_repair_to_the_nearest_surviving_group() {
        let (mut s, _, path) = state();
        s.path_command(&AppCommand::RevealGraph { path: path.clone(), select: true });
        let group = path.nodes[0];
        s.graph_dock_path = vec![group, uuid::Uuid::new_v4()];
        s.repair_graph_dock();
        assert_eq!(s.graph_dock_path, [group]);
        s.graph_dock_path = vec![uuid::Uuid::new_v4(), group];
        s.repair_graph_dock();
        assert!(s.graph_dock_path.is_empty());
        assert!(!s.dirty);
        assert_eq!(s.active_history().undo_count(), 0);
    }

    #[test]
    fn reveal_is_idempotent_and_respects_a_manual_browser_choice() {
        let (mut s, _, path) = state();
        s.active_workspace = crate::state::Workspace::Color;
        s.show_color_panel = true;
        s.show_browser_panel = true;
        let mut borrow = crate::layout_state::NodeBrowserBorrow::default();
        for _ in 0..2 {
            s.path_command(&AppCommand::RevealGraph { path: path.clone(), select: true });
            borrow.enter_if_active(s.graph_dock_open, &mut s.show_browser_panel);
        }
        assert!(!s.show_browser_panel);
        s.show_browser_panel = true;
        borrow.manual_override();
        s.path_command(&AppCommand::RevealGraph { path: path.clone(), select: true });
        borrow.enter_if_active(s.graph_dock_open, &mut s.show_browser_panel);
        assert!(s.graph_dock_open && s.show_browser_panel);
        assert_eq!(s.selected_node_addr, Some(path));
        assert_eq!(s.active_workspace, crate::state::Workspace::Color);
        assert!(!s.dirty);
    }

    #[test]
    fn graph_layout_stays_usable_below_right_and_in_narrow_windows() {
        for workspace in [crate::state::Workspace::Edit, crate::state::Workspace::Color] {
            for placement in [crate::layout_state::NodeGraphPlacement::Below, crate::layout_state::NodeGraphPlacement::Right] {
                for width in [900.0, 1280.0, 1920.0] {
                    let s = AppState { active_workspace: workspace, show_color_panel: workspace == crate::state::Workspace::Color, graph_dock_open: true, ..Default::default() };
                    let layout = crate::layout_state::LayoutState { node_graph_placement: placement, ..Default::default() };
                    let full = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1080.0));
                    let r = crate::layout::compute_layout(full, &layout, &s);
                    assert!(r.nodes_rect.is_positive() && full.contains_rect(r.nodes_rect));
                    assert!(r.viewer_rect.is_positive());
                    let center = r.right_resizer_rect.left() - r.left_resizer_rect.right();
                    assert_eq!(r.nodes_on_right, placement == crate::layout_state::NodeGraphPlacement::Right && center >= 640.0);
                    if r.nodes_on_right { assert!(r.viewer_rect.right() <= r.nodes_rect.left()); }
                    else { assert!(r.nodes_rect.top() >= r.timeline_rect.bottom()); }
                }
            }
        }
    }
}
