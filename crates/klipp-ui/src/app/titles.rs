//! Titles: making, editing, laying out, styling and animating a title
//! clip, and the title assets several clips share (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_title_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        // Short-circuiting `||` rather than a chain of
        // `if h(..) { true } else if ..`, because that chain had a bug in
        // it: the animation arm returned `false`, so a handled animation
        // command was reported as *unhandled*. The propagation below was
        // skipped, and the caller's
        // `if handle_title_commands(..) { continue; }` let the command fall
        // through to the rest of the dispatch. Animation was the only kind
        // of title edit that never reached its linked Project Asset.
        //
        // `clippy::needless_bool` caught it, but only because the faulty
        // arm happened to sit next to the closing `else { false }` — the
        // same mistake in a middle arm would have been invisible. Written
        // as `||` the mistake cannot be made: the semantics and the
        // short-circuit order are identical, and there is no per-arm
        // boolean to get wrong.
        let handled = self.handle_title_lifecycle(cmd, needs_refresh)
            || self.handle_title_content(cmd, needs_refresh)
            || self.handle_title_layout(cmd, needs_refresh)
            || self.handle_title_styling(cmd, needs_refresh)
            || self.handle_title_effects(cmd, needs_refresh)
            || self.handle_title_animation(cmd, needs_refresh);
        // Edit propagation: any title edit on a clip linked to a
        // Project Asset is the source-of-truth update for that
        // asset. Snapshot the edited clip's `TitleData` into the
        // asset, then push the asset's data back out to every
        // other linked instance in one tick. Safe to run every
        // frame even on no-op commands — the helper bails when
        // there's nothing to do.
        if handled {
            if let Some(clip_id) = title_edit_clip_id(cmd) {
                self.propagate_title_edit_to_asset(clip_id);
            }
        }
        handled
    }

    fn handle_title_lifecycle(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::BeginTitleEdit { clip_id } => {
                self.state.viewer_text_edit_clip = Some(*clip_id);
                self.state.viewer_text_edit = true;
                // Re-rasterise the title overlay so the editing
                // object disappears from the static preview. The
                // egui TextEdit floating in the bounding box owns
                // the visible text from now until exit.
                self.render_title_to_viewer(*clip_id);
                true
            }
            AppCommand::EndTitleEdit => {
                let clip_id = self.state.viewer_text_edit_clip;
                self.state.viewer_text_edit_clip = None;
                self.state.viewer_text_edit = false;
                self.state.viewer_text_edit_object = None;
                // Re-rasterise so the previously-hidden object's
                // styled text reappears in the overlay now that
                // edit mode is finished.
                if let Some(cid) = clip_id {
                    self.render_title_to_viewer(cid);
                }
                true
            }
            AppCommand::SaveTitleTemplate { clip_id } => {
                let data = self.state.sequences[self.state.active_sequence_idx]
                    .all_clips()
                    .find(|c| c.id == *clip_id)
                    .and_then(|c| match &c.kind {
                        klipp_timeline::ClipKind::Title(td) => Some(td.clone()),
                        _ => None,
                    });
                if let Some(td) = data {
                    self.state.title_template_dialog = Some(crate::title_templates::TemplateDialog::save(td));
                }
                true
            }
            AppCommand::AddTitleAtPlayhead { over_clip } => {
                let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                // Where and how long: the playhead and five seconds,
                // or the span of the clip it goes over.
                let (timeline_in, duration_frames) = over_clip
                    .and_then(|cid| {
                        self.state.sequences[self.state.active_sequence_idx]
                            .tracks
                            .iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == cid)
                            .map(|c| {
                                let secs = (c.timeline_out - c.timeline_in).to_secs_f64().max(0.0);
                                (c.timeline_in, ((secs * fps_rate as f64).round() as u32).max(1))
                            })
                    })
                    .unwrap_or((self.state.playhead, fps_rate * 5));
                let timeline_out = timeline_in.rescale_to(fps_rate) + klipp_core::RationalTime::from_frames(duration_frames as i64, fps_rate);
                // A text track with room for the whole span, else a
                // new one on top.
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let free = seq.tracks.iter().position(|t| {
                    t.kind == klipp_timeline::TrackKind::Text
                        && !t.clips.iter().any(|c| c.timeline_in < timeline_out && c.timeline_out > timeline_in)
                });
                let track_index = match free {
                    Some(i) => i,
                    None => {
                        let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Text).count() + 1;
                        seq.tracks.insert(0, klipp_timeline::Track::new(&format!("Text {n}"), klipp_timeline::TrackKind::Text));
                        0
                    }
                };
                self.handle_title_lifecycle(&AppCommand::CreateTitleClip { track_index, timeline_in, duration_frames }, _needs_refresh)
            }
            AppCommand::InsertCompositionTemplate { template_id } => {
                let Some(template) = self
                    .state
                    .composition_templates
                    .templates()
                    .find(|template| template.id == *template_id)
                    .cloned()
                else {
                    self.state.toast.show("That template is no longer available");
                    return true;
                };
                let sequence_idx = self.state.active_sequence_idx;
                let sequence_id = klipp_core::SequenceId(template.composition_id);
                let Some(duration) = self
                    .state
                    .sequences
                    .iter()
                    .find(|sequence| sequence.id == sequence_id)
                    .map(|sequence| sequence.total_duration())
                else {
                    self.state.toast.show("The template's composition is missing");
                    return true;
                };
                let template_sequence = self.state.sequences.iter().find(|candidate| candidate.id == sequence_id).cloned();
                if self.state.sequences[sequence_idx].id == sequence_id {
                    self.state.toast.show("A template cannot be inserted into its own composition");
                    return true;
                }
                if duration.to_secs_f64() <= 0.0 {
                    self.state.toast.show("The template's composition is empty");
                    return true;
                }
                let fps_rate = self.state.sequences[sequence_idx].fps.rate();
                let timeline_in = self.state.playhead.rescale_to(fps_rate);
                let timeline_out = timeline_in + duration.rescale_to(timeline_in.rate);
                // An unlocked video track with room for the whole span,
                // else a new one on top — as a title finds its track.
                let seq = &mut self.state.sequences[sequence_idx];
                let free = seq.tracks.iter().position(|t| {
                    t.kind == klipp_timeline::TrackKind::Video
                        && !t.locked
                        && !t.clips.iter().any(|c| c.timeline_in < timeline_out && c.timeline_out > timeline_in)
                });
                let track_index = match free {
                    Some(i) => i,
                    None => {
                        let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count() + 1;
                        seq.tracks.insert(0, klipp_timeline::Track::new(&format!("Video {n}"), klipp_timeline::TrackKind::Video));
                        0
                    }
                };
                // A template is composed in isolation and placed as one
                // picture: a precomposition, not a flattened compound.
                let mut clip = klipp_timeline::Clip::precomposition(sequence_id, timeline_in, timeline_out);
                let bindings = template
                    .bindings
                    .iter()
                    .map(|binding| (binding.parameter.clone(), binding.property.clone()))
                    .collect();
                let instance = crate::parameterized_templates::TemplateInstance {
                    template_id: *template_id,
                    bindings,
                    overrides: std::collections::BTreeMap::new(),
                    media_slots: std::collections::BTreeMap::new(),
                };
                let Some(template_sequence) = template_sequence.as_ref() else {
                    self.state.toast.show("Template composition is missing");
                    return false;
                };
                if let Err(error) = klipp_timeline::flatten::validate_template_instance(&instance, template_sequence) {
                    self.state.toast.show(format!("Template cannot be inserted: {error}"));
                    return false;
                }
                clip.kind = klipp_timeline::ClipKind::TemplatePrecomposition {
                    sequence_id,
                    template_instance: instance,
                };
                let clip_id = clip.id;
                seq.tracks[track_index].clips.push(clip.clone());
                seq.tracks[track_index].sort_clips();
                self.state.push_history(klipp_document::history::EditCommand::LegacyAppendClip {
                    clip,
                    track_index,
                });
                self.state.selected_clip = Some(clip_id);
                self.state.selected_clips.clear();
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                self.state.toast.show(format!("Inserted template '{}'", template.name));
                true
            }
            AppCommand::CreateTitleClip { track_index, timeline_in, duration_frames } => {
                let ti = *track_index;
                // Check if this track was just created (it's a fresh text track with no clips)
                let created_track = self.state.sequences[self.state.active_sequence_idx].tracks
                    .get(ti)
                    .is_some_and(|t| t.kind == klipp_timeline::TrackKind::Text && t.clips.is_empty());
                let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                let timeline_in = timeline_in.rescale_to(fps_rate);
                let duration = klipp_core::RationalTime::from_frames(*duration_frames as i64, fps_rate);
                let timeline_out = timeline_in + duration;
                let title_data = self.state.pending_title_data.take()
                    .unwrap_or_default();
                let clip = klipp_timeline::Clip {
                    id: klipp_core::ClipId::new(),
                    kind: klipp_timeline::ClipKind::Title(title_data),
                    timeline_in,
                    timeline_out,
                    source_in: klipp_core::RationalTime::from_frames(0, fps_rate),
                    source_out: duration,
                    node_graph: klipp_nodes::NodeGraph::default(),
                    builtin_effects: Vec::new(),
                    builtin_effects_enabled: Vec::new(),
                    effects: Vec::new(),
                    linked_clip_id: None,
                    linked_title_asset_id: None,
                    group_id: None,
                    grade_group: None,
                    own_node_graph: None,
                    own_effects: None,
                    magnetic_anchor: None,
                    gain: 1.0,
                    volume: 1.0,
                    pan: 0.0,
                    fade_in_secs: 0.0,
                    fade_out_secs: 0.0,
                    fade_in_curve: klipp_timeline::FadeCurve::default(),
                    fade_out_curve: klipp_timeline::FadeCurve::default(),
                    channel_config: klipp_timeline::ChannelConfig::default(),
                    volume_keyframes: Vec::new(),
                    pan_keyframes: Vec::new(),
                    gain_keyframes: Vec::new(),
                    opacity_keyframes: Vec::new(),
                    transform_pos_x_keyframes: Vec::new(),
                    transform_pos_y_keyframes: Vec::new(),
                    transform_scale_keyframes: Vec::new(),
                    transform_rotation_keyframes: Vec::new(),
                    transform_tilt_x_keyframes: Vec::new(),
                    transform_tilt_y_keyframes: Vec::new(),
                    matte_feather_keyframes: Vec::new(),
                    matte_choke_keyframes: Vec::new(),
                    clip_markers: Vec::new(),
                    effect_keyframes: std::collections::HashMap::new(),
                    role: klipp_core::ClipRole::default(),
                    param_modifiers: std::collections::HashMap::new(),
                    speed: 1.0,
                    speed_keyframes: Vec::new(),
                    source_rotation: 0,
                    enabled: true,
                    solo: false,
                    blend_mode: klipp_core::BlendMode::Normal,
                    depth: 1.0,
                    matte_source: None,
                    matte_stencil: None,
                    consumed_as_matte: false,
                    transform_link: None,
                    grade_versions: Vec::new(),
                    active_grade_version: None,
                    multicam_group: None,
                    multicam_angle: 0,
                    multicam_cuts: Vec::new(),
                    multicam_sound: Default::default(),
                    audio_effects: Vec::new(),
                    audio_effect_keyframes: std::collections::HashMap::new(),
                    deflicker_data: None,
                    deflicker_gains: None,
                    stabilization_data: None,
                    smoothed_trajectory: None,
                    track_points: Vec::new(),
                    surfaces: Vec::new(),
                    surface_pin: None,
                    precomp: None,
                };
                let clip_id = clip.id;
                let rest = title_rest_time(&clip, fps_rate);
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(ti) {
                    track.clips.push(clip.clone());
                    track.clips.sort_by_key(|c| c.timeline_in.value);
                }
                self.state.active_history_mut().push(crate::history::HistoryEntry {
                    command: crate::history::EditCommand::AddTitleClip {
                        clip,
                        track_index: ti,
                        created_track,
                    },
                });
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                // The new title is the selection, the inspector opens on
                // its text, and the playhead goes where the title rests so
                // the viewer shows it: past its entrance if it has one,
                // else its first frame. A title made at the playhead with
                // no entrance leaves the playhead where it was.
                self.state.selected_clip = Some(clip_id);
                self.state.selected_clips = vec![clip_id];
                self.state.selected_title_object = None;
                self.state.focus_title_text = 3;
                self.render_title_to_viewer(clip_id);
                if let Some(park) = rest.filter(|&t| t != self.state.playhead) {
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(park));
                }
                true
            }
            AppCommand::ClearTitleOverlay => {
                if let Some(old) = self.viewer.title_overlay_texture.take() {
                    self.defer_drop_texture(old);
                }
                true
            }
            AppCommand::CreateTitleAsset { source_clip_id } => {
                self.handle_create_title_asset(*source_clip_id);
                true
            }
            AppCommand::DetachTitleAsset { clip_id } => {
                self.handle_detach_title_asset(*clip_id);
                true
            }
            AppCommand::RenameTitleAsset { asset_id, new_name } => {
                if let Some(asset) = self.state.title_assets.get_mut(asset_id) {
                    asset.display_name = new_name.clone();
                }
                if let Some(item) = self.state.media_pool.get_mut(asset_id) {
                    item.filename = new_name.clone();
                }
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Promote a title clip on the timeline into a reusable Project
    /// Asset. Mints a fresh `MediaId`, builds a `TitleAsset` from a
    /// snapshot of the clip's `TitleData`, registers a synthetic
    /// `MediaItem` in the pool so the asset shows up in the Media
    /// tab, and links the source clip via `linked_title_asset_id`.
    /// No-ops if the clip can't be located, isn't a title clip, or
    /// is already linked to an asset (re-promotion is meaningless —
    /// the user can rename instead).
    /// Walk every linked-instance of a Project Asset and ensure they
    /// all carry the same `TitleData`. Called after a title-edit
    /// command lands on any single clip — if that clip is linked,
    /// its post-edit data becomes the asset's new canonical state
    /// and the rest of the instances pick it up on this same tick.
    fn propagate_title_edit_to_asset(&mut self, edited_clip_id: klipp_core::ClipId) {
        let seq_idx = self.state.active_sequence_idx;
        // Find the edited clip and its asset id + post-edit data.
        let mut asset_id: Option<klipp_core::MediaId> = None;
        let mut new_data: Option<klipp_timeline::TitleData> = None;
        for track in &self.state.sequences[seq_idx].tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == edited_clip_id) {
                asset_id = clip.linked_title_asset_id;
                if let klipp_timeline::ClipKind::Title(ref td) = clip.kind {
                    new_data = Some(td.clone());
                }
                break;
            }
        }
        let (Some(asset_id), Some(new_data)) = (asset_id, new_data) else {
            return; // Either not linked or not a title clip — nothing to propagate.
        };
        // Update the asset (single source of truth).
        if let Some(asset) = self.state.title_assets.get_mut(&asset_id) {
            asset.data = new_data.clone();
        } else {
            // Asset went missing while linked — defensive: clear the
            // dangling link so the user isn't surprised by a no-op
            // edit. Inline data already has the user's change.
            for track in &mut self.state.sequences[seq_idx].tracks {
                if let Some(clip) = track.clips.iter_mut().find(|c| c.id == edited_clip_id) {
                    clip.linked_title_asset_id = None;
                    break;
                }
            }
            return;
        }
        // Fan out to every other linked instance across all
        // sequences (a single asset can be referenced from any
        // number of sequences in the project).
        for seq in &mut self.state.sequences {
            for track in &mut seq.tracks {
                for clip in &mut track.clips {
                    if clip.id == edited_clip_id {
                        continue;
                    }
                    if clip.linked_title_asset_id == Some(asset_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            *td = new_data.clone();
                        }
                    }
                }
            }
        }
    }

    fn handle_create_title_asset(&mut self, source_clip_id: klipp_core::ClipId) {
        // First pass: find the source clip and snapshot its
        // `TitleData`. We need a separate borrow scope so the
        // subsequent mutation on `state.title_assets` and
        // `state.media_pool` can take their own borrows without
        // tripping the borrow checker.
        let mut snapshot: Option<(klipp_timeline::TitleData, klipp_core::Timecode, klipp_core::Fps)> = None;
        let mut already_linked = false;
        let seq_idx = self.state.active_sequence_idx;
        for track in &self.state.sequences[seq_idx].tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == source_clip_id) {
                if clip.linked_title_asset_id.is_some() {
                    already_linked = true;
                    break;
                }
                if let klipp_timeline::ClipKind::Title(ref td) = clip.kind {
                    let dur = clip.timeline_out - clip.timeline_in;
                    let fps = self.state.sequences[seq_idx].fps;
                    snapshot = Some((td.clone(), dur, fps));
                }
                break;
            }
        }
        if already_linked {
            self.state
                .toast
                .show("Title is already a Project Asset — rename or detach first");
            return;
        }
        let Some((title_data, duration, fps)) = snapshot else {
            self.state.toast.show("Select a title clip first");
            return;
        };

        // Generate a name from the first ~32 chars of the title text,
        // stripping placeholders like "[Your Title]" so the user gets
        // something they can recognise in the Media tab. Falls back
        // to "Title Asset" when the text is empty/placeholder-only.
        let raw_name = title_data
            .text()
            .lines()
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| c == '[' || c == ']' || c.is_whitespace())
            .to_string();
        let display_name = if raw_name.is_empty() {
            "Title Asset".to_string()
        } else if raw_name.chars().count() > 32 {
            let truncated: String = raw_name.chars().take(32).collect();
            format!("{truncated}\u{2026}")
        } else {
            raw_name
        };

        // Mint the asset.
        let asset_id = klipp_core::MediaId::new();
        let asset = klipp_timeline::TitleAsset {
            display_name: display_name.clone(),
            data: title_data.clone(),
            animation: None, // Animation tokens land via Inspector edits later.
        };
        self.state.title_assets.insert(asset_id, asset);

        // Register the synthetic MediaItem so the Media tab can
        // render it and drag-drop can resolve by MediaId.
        let media_item = klipp_media::MediaItem::new_synthetic_title(
            asset_id,
            display_name.clone(),
            duration,
            fps,
        );
        self.state.media_pool.add(media_item);

        // Link the source clip back to the asset.
        for track in &mut self.state.sequences[seq_idx].tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == source_clip_id) {
                clip.linked_title_asset_id = Some(asset_id);
                break;
            }
        }

        self.state.toast.show(format!(
            "Saved \"{display_name}\" to Project Assets — drag from Media tab to reuse"
        ));
        self.state.dirty = true;
        tracing::info!(
            "CreateTitleAsset: clip {:?} -> asset {:?} (\"{}\")",
            source_clip_id, asset_id, display_name,
        );
    }

    /// Drop a synthetic title asset onto the timeline. Routes the
    /// drop to a Text track (creating one if `track_index` doesn't
    /// point at a Text/Subtitle track), creates a new title clip with
    /// `linked_title_asset_id = Some(asset_id)`, and seeds the
    /// inline `TitleData` from the asset so the clip renders
    /// correctly even before the asset-canonical lookup wires up.
    pub(super) fn append_title_asset_to_timeline(
        &mut self,
        asset_media_id: klipp_core::MediaId,
        target_track_idx: usize,
    ) {
        // Resolve asset data + display name. Bail with a clear
        // message if the synthetic MediaItem is in the pool but the
        // backing asset is missing (corrupted load, manual edit).
        let Some(asset) = self.state.title_assets.get(&asset_media_id).cloned() else {
            self.state.toast.show(
                "Project Asset is missing — was it deleted from the project?",
            );
            return;
        };

        // Decide which track receives the clip. If the user dropped
        // on a Text/Subtitle track, use it directly. Otherwise (drop
        // on Video / unknown), find or create a Text track at the
        // top of the stack — same convention as the Library "Apply
        // title" flow.
        let seq_idx = self.state.active_sequence_idx;
        let target_kind = self.state.sequences[seq_idx]
            .tracks
            .get(target_track_idx)
            .map(|t| t.kind);
        let track_idx = if matches!(
            target_kind,
            Some(klipp_timeline::TrackKind::Text)
                | Some(klipp_timeline::TrackKind::Subtitle)
        ) {
            target_track_idx
        } else {
            // Find first Text track or create one.
            let existing = self.state.sequences[seq_idx]
                .tracks
                .iter()
                .position(|t| t.kind == klipp_timeline::TrackKind::Text);
            match existing {
                Some(idx) => idx,
                None => {
                    let track_num = self.state.sequences[seq_idx]
                        .tracks
                        .iter()
                        .filter(|t| t.kind == klipp_timeline::TrackKind::Text)
                        .count()
                        + 1;
                    let new_track = klipp_timeline::Track::new(
                        &format!("Text {track_num}"),
                        klipp_timeline::TrackKind::Text,
                    );
                    self.state.sequences[seq_idx]
                        .tracks
                        .insert(0, new_track);
                    0
                }
            }
        };

        // Pick a default 5-second duration anchored at the playhead,
        // matching `CreateTitleClip`'s default.
        let fps = self.state.sequences[seq_idx].fps;
        let fps_rate = fps.rate();
        let duration_frames = (fps_rate * 5).max(1);
        let timeline_in = self.state.playhead;
        let timeline_out = klipp_core::RationalTime::from_frames(
            timeline_in.value + duration_frames as i64,
            timeline_in.rate,
        );

        // Build the title clip. Mirrors `CreateTitleClip`'s field-
        // by-field constructor — kept in lockstep with that handler
        // so adding new `Clip` fields shows up here at compile time.
        let new_clip_id = klipp_core::ClipId::new();
        let clip = klipp_timeline::Clip {
            id: new_clip_id,
            kind: klipp_timeline::ClipKind::Title(asset.data.clone()),
            timeline_in,
            timeline_out,
            source_in: klipp_core::RationalTime::from_frames(0, fps_rate),
            source_out: klipp_core::RationalTime::from_frames(
                duration_frames as i64,
                fps_rate,
            ),
            node_graph: klipp_nodes::NodeGraph::default(),
            builtin_effects: Vec::new(),
            builtin_effects_enabled: Vec::new(),
            effects: Vec::new(),
            linked_clip_id: None,
            linked_title_asset_id: Some(asset_media_id),
            group_id: None,
            grade_group: None,
            own_node_graph: None,
            own_effects: None,
            magnetic_anchor: None,
            gain: 1.0,
            volume: 1.0,
            pan: 0.0,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            fade_in_curve: klipp_timeline::FadeCurve::default(),
            fade_out_curve: klipp_timeline::FadeCurve::default(),
            channel_config: klipp_timeline::ChannelConfig::default(),
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            transform_tilt_x_keyframes: Vec::new(),
            transform_tilt_y_keyframes: Vec::new(),
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            effect_keyframes: std::collections::HashMap::new(),
            role: klipp_core::ClipRole::default(),
            param_modifiers: std::collections::HashMap::new(),
            speed: 1.0,
            speed_keyframes: Vec::new(),
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            grade_versions: Vec::new(),
            active_grade_version: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            deflicker_data: None,
            deflicker_gains: None,
            stabilization_data: None,
            smoothed_trajectory: None,
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            precomp: None,
        };
        self.state.sequences[seq_idx].tracks[track_idx]
            .clips
            .push(clip);
        self.state.sequences[seq_idx].tracks[track_idx]
            .clips
            .sort_by_key(|c| c.timeline_in.value);
        self.state.selected_clip = Some(new_clip_id);
        self.state.toast.show(format!(
            "\"{}\" added to timeline",
            asset.display_name
        ));
        self.sync_sequence_to_engine();
        self.state.dirty = true;
        tracing::info!(
            "AppendTitleAsset: asset {:?} -> clip {:?} on track {}",
            asset_media_id, new_clip_id, track_idx,
        );
    }

    /// Detach a clip from its linked title asset. Inline `TitleData`
    /// stays as-is (already in sync from the last propagation), the
    /// `linked_title_asset_id` is cleared, and future edits diverge.
    fn handle_detach_title_asset(&mut self, clip_id: klipp_core::ClipId) {
        let seq_idx = self.state.active_sequence_idx;
        let mut detached_from: Option<klipp_core::MediaId> = None;
        for track in &mut self.state.sequences[seq_idx].tracks {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                if let Some(asset_id) = clip.linked_title_asset_id.take() {
                    detached_from = Some(asset_id);
                }
                break;
            }
        }
        if let Some(asset_id) = detached_from {
            let name = self
                .state
                .title_assets
                .get(&asset_id)
                .map(|a| a.display_name.clone())
                .unwrap_or_else(|| "Title Asset".to_string());
            self.state
                .toast
                .show(format!("Detached from \"{name}\" — edits no longer sync"));
            self.state.dirty = true;
            tracing::info!(
                "DetachTitleAsset: clip {:?} detached from asset {:?}",
                clip_id, asset_id,
            );
        }
    }

    fn handle_title_content(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::UpdateTitleData { clip_id, text, font_size, font_name, font_style, color } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            let old_len = td.text().len();
                            let new_style = match font_style {
                                1 => klipp_timeline::FontStyle::Bold,
                                2 => klipp_timeline::FontStyle::Italic,
                                3 => klipp_timeline::FontStyle::BoldItalic,
                                _ => klipp_timeline::FontStyle::Regular,
                            };
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.text = text.clone();
                                tb.font_size = *font_size;
                                tb.font_name = font_name.clone();
                                tb.font_style = new_style;
                                tb.color = *color;
                                if tb.text.len() != old_len {
                                    klipp_timeline::adjust_style_runs_for_text_change(
                                        &mut tb.style_runs,
                                        tb.text.len(),
                                    );
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Edit Title Text", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleMotion { clip_id, motion } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) { fx.motion = *motion; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Motion", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTracking { clip_id, tracking } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.tracking = *tracking; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Tracking", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleLineSpacing { clip_id, line_spacing } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.line_spacing = *line_spacing; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Line Spacing", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleUnderline { clip_id, underline } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.underline = *underline; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Underline", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleStrikethrough { clip_id, strikethrough } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.strikethrough = *strikethrough; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Strikethrough", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleCaseTransform { clip_id, case_kind } => {
                let case = match case_kind {
                    1 => klipp_timeline::TextCase::Uppercase,
                    2 => klipp_timeline::TextCase::Lowercase,
                    3 => klipp_timeline::TextCase::SmallCaps,
                    _ => klipp_timeline::TextCase::None,
                };
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.case_transform = case; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Case", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleColumns { clip_id, columns } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.columns = (*columns).clamp(1, 8);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Columns", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleVerticalLayout { clip_id, vertical } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.vertical_layout = *vertical;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Vertical Layout", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleDirection { clip_id, direction } => {
                let dir = if *direction == 1 {
                    klipp_timeline::TextDirection::Rtl
                } else {
                    klipp_timeline::TextDirection::Ltr
                };
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.text_direction = dir;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Text Direction", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleChromaticAberration { clip_id, offset_px } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                // max/min, not clamp: a NaN offset lands on 0.0
                                // here, where clamp would propagate it into a
                                // saved title and render nothing.
                                #[allow(clippy::manual_clamp)]
                                {
                                    tb.chromatic_aberration_px = (*offset_px).max(0.0).min(40.0);
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Chromatic Aberration", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleGlitch { clip_id, intensity } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.glitch_intensity = (*intensity).clamp(0.0, 1.0);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Glitch", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleNeonPreset { clip_id, accent_color } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some((tb, fx)) = target_object_mut(td, target_oid).and_then(|o| { let klipp_timeline::TitleObject { content, fx, .. } = o; match content { klipp_timeline::TitleObjectContent::TextBox(tb) => Some((tb, fx.get_or_insert_with(Default::default))), _ => None } }) {
                                // Bright fill, thin dark outline for
                                // legibility, strong outer glow tinted
                                // to the accent. Outline alpha is dialled
                                // down so the glow reads through.
                                tb.color = *accent_color;
                                fx.outlines = vec![klipp_timeline::TextOutline {
                                    color: [10, 10, 14, 180],
                                    width: 2.0,
                                    enabled: true,
                                    width_keyframes: Vec::new(),
                                }];
                                fx.glows = vec![klipp_timeline::TextGlow {
                                    color: *accent_color,
                                    radius: 12.0,
                                    intensity: 1.4,
                                    enabled: true,
                                    radius_keyframes: Vec::new(),
                                    intensity_keyframes: Vec::new(),
                                }];
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Neon Preset", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleDropCap { clip_id, scale } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                // 0..0.999 collapses to off — the
                                // renderer ignores anything below 1.0.
                                tb.drop_cap_scale = if *scale < 1.0 { 0.0 } else { (*scale).min(8.0) };
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Drop Cap", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleBaselineShift { clip_id, shift_px } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.baseline_shift = *shift_px; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Baseline Shift", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::ApplyTitleStyle { clip_id, range_start, range_end, property, value } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            // Resolve the target TextBox up-front so
                            // both the special-case branch and the
                            // range-apply path mutate the *real*
                            // style_runs (`td.style_runs()` is a
                            // by-value accessor — mutating its
                            // return value would no-op).
                            let tb = match target_textbox_mut(td, target_oid) {
                                Some(tb) => tb,
                                None => continue,
                            };
                            // Special: clear_all removes all style runs
                            if property == "clear_all" {
                                tb.style_runs.clear();
                                break;
                            }
                            let text_len = tb.text.len();
                            let prop = property.as_str();
                            let val = value.as_str();
                            klipp_timeline::apply_style_to_range(
                                &mut tb.style_runs,
                                text_len,
                                *range_start,
                                *range_end,
                                |run| {
                                    match prop {
                                        "color" => {
                                            if let Ok(c) = serde_json::from_str::<[u8; 4]>(val) {
                                                run.color = Some(c);
                                            }
                                        }
                                        "font_size" => {
                                            if let Ok(s) = val.parse::<f32>() {
                                                run.font_size = Some(s);
                                            }
                                        }
                                        "font_style" => {
                                            if let Ok(fs) = serde_json::from_str::<klipp_timeline::FontStyle>(val) {
                                                run.font_style = Some(fs);
                                            }
                                        }
                                        "tracking" => {
                                            if let Ok(t) = val.parse::<f32>() {
                                                run.tracking = Some(t);
                                            }
                                        }
                                        "baseline_offset" => {
                                            if let Ok(b) = val.parse::<f32>() {
                                                run.baseline_offset = Some(b);
                                            }
                                        }
                                        _ => {}
                                    }
                                },
                            );
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Edit Title Style", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::ClearTitleStyleRuns { clip_id } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.style_runs.clear();
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Clear Title Styles", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn handle_title_layout(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::SetTitleAlignment { clip_id, alignment } => {
                let old_td = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                    .find_map(|t| t.clips.iter().find(|c| c.id == *clip_id))
                    .and_then(|c| c.title_data()).cloned();
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.alignment = match alignment {
                                0 => klipp_timeline::TextAlignment::Left,
                                2 => klipp_timeline::TextAlignment::Right,
                                _ => klipp_timeline::TextAlignment::Center,
                            }; }
                        }
                    }
                }
                if let Some(old) = old_td {
                    if let Some(new) = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .find_map(|t| t.clips.iter().find(|c| c.id == *clip_id))
                        .and_then(|c| c.title_data()).cloned()
                    {
                        self.state.active_history_mut().push(crate::history::HistoryEntry {
                            command: EditCommand::EditTitle {
                                clip_id: *clip_id, description: "Set Alignment",
                                old_data: old, new_data: new,
                            },
                        });
                    }
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitlePosition { clip_id, x, y } => {
                let old = self.snapshot_title(*clip_id);
                // Position edits target the currently selected
                // object regardless of kind (TextBox or Shape) so
                // shape layers can be repositioned from the
                // inspector. Fall back to the first object when
                // nothing is selected (legacy single-text titles).
                let target_oid: Option<klipp_timeline::TitleObjectId> = self
                    .state
                    .selected_title_object
                    .filter(|(c, _)| *c == *clip_id)
                    .map(|(_, oid)| oid);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            let idx = match target_oid {
                                Some(oid) => td.objects.iter()
                                    .position(|o| o.id == oid),
                                None => None,
                            }.unwrap_or(0);
                            if let Some(o) = td.objects.get_mut(idx) {
                                o.transform.center = klipp_timeline::TitlePosition { x: *x, y: *y };
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Move Title", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SelectTitleObject { clip_id, object_id } => {
                self.state.selected_title_object = object_id
                    .map(|id| (*clip_id, klipp_timeline::TitleObjectId(id)));
                true
            }
            AppCommand::AddTitleObject { clip_id, kind } => {
                let old = self.snapshot_title(*clip_id);
                let mut new_id: Option<klipp_timeline::TitleObjectId> = None;
                // A ready-made shape is as wide as it is tall on screen,
                // a line long and thin (#67).
                let res = self.state.sequences[self.state.active_sequence_idx].resolution;
                let tall = res.height as f32 / res.width.max(1) as f32;
                let preset = crate::inspector::title_shape_preset(*kind);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            // New objects ship at half-frame, centred,
                            // so the user immediately sees them at a
                            // sensible default position. Drag /
                            // resize from there.
                            let xform = klipp_timeline::ObjectTransform {
                                center: klipp_timeline::TitlePosition { x: 0.5, y: 0.5 },
                                center_x_keyframes: Vec::new(),
                                center_y_keyframes: Vec::new(),
                                size: match preset {
                                    None => klipp_timeline::TitlePosition { x: 0.5, y: 0.3 },
                                    Some(klipp_timeline::ShapePreset::Line) => klipp_timeline::TitlePosition { x: 0.4, y: 0.02 },
                                    Some(klipp_timeline::ShapePreset::Arrow) => klipp_timeline::TitlePosition { x: 0.3 * tall * 2.0, y: 0.3 },
                                    Some(_) => klipp_timeline::TitlePosition { x: 0.3 * tall, y: 0.3 },
                                },
                                rotation: 0.0,
                                rotation_keyframes: Vec::new(),
                                anchor_norm: klipp_timeline::TitlePosition { x: 0.5, y: 0.5 },
                                size_x_keyframes: Vec::new(), size_y_keyframes: Vec::new(),
                                anchor_x_keyframes: Vec::new(), anchor_y_keyframes: Vec::new(),
                            };
                            let content = match kind {
                                _ if preset.is_some() => klipp_timeline::TitleObjectContent::Shape(klipp_timeline::Shape {
                                    kind: klipp_timeline::ShapeKind::Path(klipp_timeline::VectorPath::preset(
                                        preset.expect("checked"),
                                        [0.92, 0.92, 0.95, 1.0],
                                        [0.92, 0.92, 0.95, 1.0],
                                        if preset == Some(klipp_timeline::ShapePreset::Line) { 6.0 } else { 0.0 },
                                    )),
                                    fit: None,
                                    fill: None,
                                    stroke: None,
                                }),
                                1 => klipp_timeline::TitleObjectContent::Shape(
                                    klipp_timeline::Shape {
                                        kind: klipp_timeline::ShapeKind::Rectangle {
                                            corner_radius: 0.0,
                                        },
                                        fit: None,
                                        fill: Some([60, 60, 70, 220]),
                                        stroke: None,
                                    },
                                ),
                                _ => {
                                    let tb = klipp_timeline::TextBox {
                                        text: "New Text".to_string(),
                                        ..Default::default()
                                    };
                                    klipp_timeline::TitleObjectContent::TextBox(tb)
                                }
                            };
                            new_id = Some(td.add_object(content, xform));
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Add Title Object", old);
                }
                if let Some(id) = new_id {
                    self.state.selected_title_object = Some((*clip_id, id));
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::AddTitleImageObject { clip_id, path } => {
                let old = self.snapshot_title(*clip_id);
                let res = self.state.sequences[self.state.active_sequence_idx].resolution;
                // The box: a third of the frame wide, the picture's
                // aspect, centred.
                let (iw, ih) = image::image_dimensions(path).map(|(w, h)| (w as f32, h as f32)).unwrap_or((1.0, 1.0));
                let box_w = 0.33_f32;
                let box_h = (box_w * (ih / iw.max(1.0)) * (res.width as f32 / res.height.max(1) as f32)).clamp(0.02, 1.0);
                let mut new_id: Option<klipp_timeline::TitleObjectId> = None;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            let xform = klipp_timeline::ObjectTransform {
                                center: klipp_timeline::TitlePosition { x: 0.5, y: 0.5 },
                                center_x_keyframes: Vec::new(),
                                center_y_keyframes: Vec::new(),
                                size: klipp_timeline::TitlePosition { x: box_w, y: box_h },
                                rotation: 0.0,
                                rotation_keyframes: Vec::new(),
                                anchor_norm: klipp_timeline::TitlePosition { x: 0.5, y: 0.5 },
                                size_x_keyframes: Vec::new(), size_y_keyframes: Vec::new(),
                                anchor_x_keyframes: Vec::new(), anchor_y_keyframes: Vec::new(),
                            };
                            let content = klipp_timeline::TitleObjectContent::Image(klipp_timeline::ImageLayer {
                                path: path.clone(),
                                fit: klipp_timeline::ImageFit::Contain,
                            });
                            new_id = Some(td.add_object(content, xform));
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Add Image Layer", old);
                }
                if let Some(id) = new_id {
                    self.state.selected_title_object = Some((*clip_id, id));
                }
                self.title_renderer.forget_image(path);
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleImagePath { clip_id, object_id, path } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let klipp_timeline::TitleObjectContent::Image(img) = &mut o.content {
                                    img.path = path.clone();
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Replace Image", old);
                }
                self.title_renderer.forget_image(path);
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleImageFit { clip_id, object_id, fit } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                let fit = match fit {
                    1 => klipp_timeline::ImageFit::Cover,
                    2 => klipp_timeline::ImageFit::Stretch,
                    _ => klipp_timeline::ImageFit::Contain,
                };
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let klipp_timeline::TitleObjectContent::Image(img) = &mut o.content {
                                    img.fit = fit;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Image Fit", old);
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::DeleteTitleObject { clip_id, object_id } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            td.objects.retain(|o| o.id != target);
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Delete Title Object", old);
                }
                if self.state.selected_title_object == Some((*clip_id, target)) {
                    self.state.selected_title_object = None;
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectVisible { clip_id, object_id, visible } => {
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.visible = *visible;
                            }
                        }
                    }
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectLocked { clip_id, object_id, locked } => {
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.locked = *locked;
                            }
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectOpacity { clip_id, object_id, opacity } => {
                let target = klipp_timeline::TitleObjectId(*object_id);
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.opacity = (*opacity).clamp(0.0, 1.0);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Layer Opacity", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::AddTitleObjectOpacityKeyframe { clip_id, object_id, time_secs, value } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(obj) = td.object_mut(target) {
                                obj.upsert_opacity_keyframe(*time_secs, *value);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Add Title Opacity Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveTitleObjectOpacityKeyframe { clip_id, object_id, time_secs } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(obj) = td.object_mut(target) {
                                obj.remove_opacity_keyframe_at(*time_secs);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Remove Title Opacity Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::RenameTitleObject { clip_id, object_id, name } => {
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.name = name.clone();
                            }
                        }
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::ReorderTitleObjects { clip_id, new_order } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            // Pull objects out, drop them back in
                            // the requested order; any object not in
                            // `new_order` keeps its existing position
                            // at the end (defensive vs partial races).
                            let mut by_id: std::collections::HashMap<u32, klipp_timeline::TitleObject> =
                                td.objects.drain(..).map(|o| (o.id.0, o)).collect();
                            for id in new_order {
                                if let Some(o) = by_id.remove(id) {
                                    td.objects.push(o);
                                }
                            }
                            // Append leftover objects (preserve their
                            // existing relative order via id).
                            let mut leftover: Vec<_> = by_id.into_values().collect();
                            leftover.sort_by_key(|o| o.id.0);
                            td.objects.extend(leftover);
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Reorder Title Objects", old);
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectTransform {
                clip_id, object_id,
                center_x, center_y, size_x, size_y,
                rotation, anchor_x, anchor_y,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                // Mutate fields individually so existing
                                // position keyframes survive an on-canvas
                                // drag — the drag updates the static
                                // fallback only.
                                o.transform.center = klipp_timeline::TitlePosition { x: *center_x, y: *center_y };
                                o.transform.size = klipp_timeline::TitlePosition { x: *size_x, y: *size_y };
                                o.transform.rotation = *rotation;
                                o.transform.anchor_norm = klipp_timeline::TitlePosition { x: *anchor_x, y: *anchor_y };
                            }
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Move Title Object", old);
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::TitlePathError { message } => { self.state.toast.show(message.clone()); true }
            AppCommand::BeginTitlePathDrag { clip_id } => {
                if let Some(data) = self.snapshot_title(*clip_id) {
                    self.state.active_history_mut().begin_drag(format!("path:{}",clip_id.0), crate::history::HistoryEntry {
                        command: crate::history::EditCommand::EditTitle {clip_id:*clip_id, description:"Edit Vector Path",old_data:data.clone(),new_data:data},
                    });
                }
                true
            }
            AppCommand::EndTitlePathDrag { clip_id } => {
                if self.state.active_history().is_dragging(&format!("path:{}",clip_id.0)) {
                    if let Some(data) = self.snapshot_title(*clip_id) {
                        self.state.active_history_mut().end_drag(crate::history::HistoryEntry {
                            command: crate::history::EditCommand::EditTitle {clip_id:*clip_id,description:"Edit Vector Path",old_data:data.clone(),new_data:data},
                        });
                    }
                }
                true
            }
            AppCommand::SetTitleVectorPath { clip_id, object_id, path } => {
                if let Err(e) = path.validate() {
                    self.state.toast.show(e);
                    return true;
                }
                let old = self.snapshot_title(*clip_id);
                let mut changed = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if track.locked { continue; }
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(td) = &mut clip.kind {
                            if let Some(object) = td.object_mut(klipp_timeline::TitleObjectId(*object_id)) {
                                if object.locked { continue; }
                                if let klipp_timeline::TitleObjectContent::Shape(shape) = &mut object.content {
                                    shape.kind = klipp_timeline::ShapeKind::Path(path.clone());
                                    shape.fit = None;
                                    changed = true;
                                }
                            }
                        }
                    }
                }
                if changed {
                    if !self.state.active_history().is_dragging(&format!("path:{}",clip_id.0)) {
                        if let Some(old) = old { self.push_title_undo(*clip_id, "Edit Vector Path", old); }
                    }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::SetTitleShapeFill { clip_id, object_id, color } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let klipp_timeline::TitleObjectContent::Shape(s) = &mut o.content {
                                    s.fill = *color;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Shape Fill", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleShapeStroke { clip_id, object_id, color, width } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let klipp_timeline::TitleObjectContent::Shape(s) = &mut o.content {
                                    s.stroke = color.map(|c| klipp_timeline::ShapeStroke {
                                        color: c, width: *width,
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Shape Stroke", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleShapeFit { clip_id, object_id, target_id, padding_x, padding_y } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(td) = &mut clip.kind {
                            let fit = target_id.and_then(|id| {
                                td.object(klipp_timeline::TitleObjectId(id))
                                    .filter(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_)))
                                    .map(|_| klipp_timeline::ShapeFit {
                                        target: klipp_timeline::TitleObjectId(id),
                                        padding: klipp_timeline::TitlePosition {
                                            x: if padding_x.is_finite() { padding_x.max(0.0) } else { 0.0 },
                                            y: if padding_y.is_finite() { padding_y.max(0.0) } else { 0.0 },
                                        },
                                    })
                            });
                            if let Some(o) = td.object_mut(klipp_timeline::TitleObjectId(*object_id)) {
                                if let klipp_timeline::TitleObjectContent::Shape(shape) = &mut o.content {
                                    shape.fit = fit;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Fit Shape to Text", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleShapeCornerRadius { clip_id, object_id, radius } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let klipp_timeline::TitleObjectContent::Shape(s) = &mut o.content {
                                    s.kind = klipp_timeline::ShapeKind::Rectangle {
                                        corner_radius: *radius,
                                    };
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Shape Corner Radius", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectText { clip_id, object_id, text } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
                                    let old_len = tb.text.len();
                                    tb.text = text.clone();
                                    if tb.text.len() != old_len {
                                        klipp_timeline::adjust_style_runs_for_text_change(
                                            &mut tb.style_runs,
                                            tb.text.len(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Edit Title Text", old);
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectTransformAndFont {
                clip_id, object_id,
                center_x, center_y, size_x, size_y,
                rotation, anchor_x, anchor_y,
                font_size,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                // Mutate fields individually so existing
                                // position keyframes survive a corner-handle
                                // drag — the drag updates the static
                                // fallback only.
                                o.transform.center = klipp_timeline::TitlePosition { x: *center_x, y: *center_y };
                                o.transform.size = klipp_timeline::TitlePosition { x: *size_x, y: *size_y };
                                o.transform.rotation = *rotation;
                                o.transform.anchor_norm = klipp_timeline::TitlePosition { x: *anchor_x, y: *anchor_y };
                                if let klipp_timeline::TitleObjectContent::TextBox(tb) = &mut o.content {
                                    tb.font_size = *font_size;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old {
                    self.push_title_undo(*clip_id, "Resize Title Object", old);
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTextPath { clip_id, path_kind, p1, p2, p3 } => {
                let old = self.snapshot_title(*clip_id);
                let path = match path_kind {
                    1 => Some(klipp_timeline::TextPath::Arc { radius: *p1, start_angle: *p2, sweep: *p3 }),
                    2 => Some(klipp_timeline::TextPath::Circle { radius: *p1 }),
                    3 => Some(klipp_timeline::TextPath::Wave { amplitude: *p1, frequency: *p2, phase: *p3 }),
                    _ => None,
                };
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.text_path = path.clone(); }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Text Path", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::AddTitleObjectPositionKeyframe {
                clip_id, object_id, axis, time_secs, value,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.transform.upsert_position_keyframe(*axis, *time_secs, *value);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Add Position Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveTitleObjectPositionKeyframe {
                clip_id, object_id, axis, time_secs,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.transform.remove_position_keyframe_at(*axis, *time_secs);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Remove Position Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::AddTitleObjectTransformKeyframe {
                clip_id, object_id, property, time_secs, value,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.transform.upsert_transform_keyframe(*property, *time_secs, *value);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Add Transform Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveTitleObjectTransformKeyframe {
                clip_id, object_id, property, time_secs,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.transform.remove_transform_keyframe_at(*property, *time_secs);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Remove Transform Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::MoveTitleObjectPositionKeyframe {
                clip_id, object_id, axis, index, new_time_secs, new_value,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.transform.move_position_keyframe(*axis, *index, *new_time_secs, *new_value);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Move Position Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectPositionKeyframeInterpolation {
                clip_id, object_id, axis, keyframe_index, interpolation,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.transform.set_position_keyframe_interpolation(*axis, *keyframe_index, *interpolation);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Set Position Keyframe Interpolation", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectPositionKeyframeTangent {
                clip_id, object_id, axis, keyframe_index, tangent_in, tangent_out,
            } => {
                let old = self.snapshot_title(*clip_id);
                let param = crate::panels::title_curve::TitleCurveParam::Position(*axis);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            param.set_tangent(td, *object_id, *keyframe_index, *tangent_in, *tangent_out);
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Set Position Keyframe Tangent", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn handle_title_styling(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::SetTitleBackground { clip_id, bg_kind, color } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            td.background = match bg_kind {
                                1 => klipp_timeline::TitleBackground::Color { color: *color, padding: 20.0 },
                                2 => klipp_timeline::TitleBackground::FullFrame { color: *color },
                                _ => klipp_timeline::TitleBackground::None,
                            };
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Background", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleGradient { clip_id, enabled, top_color, bottom_color } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.gradient = Some(klipp_timeline::TextGradient {
                                    top_color: *top_color,
                                    bottom_color: *bottom_color,
                                    enabled: *enabled,
                                });
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Gradient", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitlePattern { clip_id, pattern_kind, color_a, color_b, size } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.pattern_fill = match pattern_kind {
                                1 => Some(klipp_timeline::TextPattern::HorizontalStripes { color_a: *color_a, color_b: *color_b, stripe_width: *size }),
                                2 => Some(klipp_timeline::TextPattern::VerticalStripes { color_a: *color_a, color_b: *color_b, stripe_width: *size }),
                                3 => Some(klipp_timeline::TextPattern::Checkerboard { color_a: *color_a, color_b: *color_b, cell_size: *size }),
                                4 => Some(klipp_timeline::TextPattern::DiagonalStripes { color_a: *color_a, color_b: *color_b, stripe_width: *size }),
                                _ => None,
                            }; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Pattern", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTextureFill { clip_id, path } => {
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                match path.as_ref() {
                                    Some(p) if !p.is_empty() => {
                                        // Preserve existing transform /
                                        // blend params when swapping the
                                        // file path; only set fresh
                                        // defaults when starting from
                                        // scratch.
                                        if let Some(t) = tb.texture_fill.as_mut() {
                                            t.source = klipp_timeline::TextureSource::File(p.clone());
                                        } else {
                                            tb.texture_fill = Some(
                                                klipp_timeline::TextureFill::from_legacy_path(p.clone(), true),
                                            );
                                        }
                                    }
                                    _ => {
                                        // Clear: drop the texture fill
                                        // entirely so the renderer no-ops
                                        // and the inspector shows "(none)".
                                        tb.texture_fill = None;
                                    }
                                }
                            }
                        }
                    }
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitlePatternEnabled { clip_id, enabled } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.pattern_fill_enabled = *enabled;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Pattern Fill", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTextureEnabled { clip_id, enabled } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                // Toggle just flips the soft-disable.
                                // When no TextureFill exists yet, seed
                                // a default one so the toggle has
                                // somewhere to write — first Browse /
                                // Preset pick swaps in the real source.
                                if let Some(t) = tb.texture_fill.as_mut() {
                                    t.enabled = *enabled;
                                } else if *enabled {
                                    tb.texture_fill = Some(klipp_timeline::TextureFill::default());
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Texture Fill", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTexturePreset { clip_id, preset_id } => {
                // Switch source to a procedural preset, preserving
                // every other TextureFill param (scale, offset,
                // rotation, opacity, tint, tile_mode, enabled).
                let old = self.snapshot_title(*clip_id);
                let preset = match *preset_id {
                    0 => klipp_timeline::TexturePresetId::PaperGrain,
                    1 => klipp_timeline::TexturePresetId::Scanlines,
                    2 => klipp_timeline::TexturePresetId::DiagonalHatch,
                    3 => klipp_timeline::TexturePresetId::GradientSweep,
                    _ => return true,
                };
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                if let Some(t) = tb.texture_fill.as_mut() {
                                    t.source = klipp_timeline::TextureSource::Preset(preset);
                                } else {
                                    tb.texture_fill = Some(klipp_timeline::TextureFill {
                                        source: klipp_timeline::TextureSource::Preset(preset),
                                        ..klipp_timeline::TextureFill::default()
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Texture Preset", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTextureParams {
                clip_id, tile_mode, scale, offset_x, offset_y, rotation, opacity, tint,
            } => {
                // Update transform / blend params on the existing
                // TextureFill. Source field is preserved. Seeds a
                // default texture_fill if there isn't one yet so a
                // standalone param edit doesn't drop on the floor.
                let old = self.snapshot_title(*clip_id);
                let mode = match *tile_mode {
                    1 => klipp_timeline::TextureTileMode::Tile,
                    2 => klipp_timeline::TextureTileMode::Fit,
                    3 => klipp_timeline::TextureTileMode::Cover,
                    _ => klipp_timeline::TextureTileMode::Stretch,
                };
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                let tf = tb.texture_fill.get_or_insert_with(klipp_timeline::TextureFill::default);
                                tf.tile_mode = mode;
                                tf.scale = *scale;
                                tf.offset_x = *offset_x;
                                tf.offset_y = *offset_y;
                                tf.rotation = *rotation;
                                tf.opacity = *opacity;
                                tf.tint = *tint;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Texture Params", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::ApplyTitlePreset { clip_id, preset_id } => {
                // Flatten a named preset into the selected TextBox.
                // Preset ids match the inspector's PRESET_REGISTRY:
                //   1 = Neon, 2 = Classic, 3 = Bold Shadow,
                //   4 = Outlined, 5 = VHS.
                // Preset 0 ("None") is a no-op so the dropdown's
                // default sentinel doesn't accidentally clobber the
                // current style.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some((tb, fx)) = target_object_mut(td, target_oid).and_then(|o| {
                                let klipp_timeline::TitleObject { content, fx, .. } = o;
                                match content {
                                    klipp_timeline::TitleObjectContent::TextBox(tb) => Some((tb, fx.get_or_insert_with(Default::default))),
                                    _ => None,
                                }
                            }) {
                                apply_title_preset(tb, fx, *preset_id);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Preset", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn handle_title_effects(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::SetTitleShadow { clip_id, enabled, color, offset_x, offset_y, blur } => {
                // Soft-disable: always write `Some(...)` with `enabled` from
                // the command. Toggling off preserves the user's tweaks so
                // the subcard re-enable round-trips cleanly. Renderer skips
                // the effect when `enabled = false`. Mutate fields in place
                // when a shadow already exists so existing offset/blur
                // keyframes survive the edit.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                // Primary shadow lives at shadows[0].
                                // Preserve any keyframes by mutating in
                                // place when the slot exists; otherwise
                                // seed a fresh primary shadow without
                                // touching shadows[1..].
                                if let Some(s) = fx.shadows.first_mut() {
                                    s.color = *color;
                                    s.offset_x = *offset_x;
                                    s.offset_y = *offset_y;
                                    s.blur = *blur;
                                    s.enabled = *enabled;
                                } else {
                                    fx.shadows.insert(0, klipp_timeline::TitleShadow {
                                        color: *color,
                                        offset_x: *offset_x,
                                        offset_y: *offset_y,
                                        blur: *blur,
                                        enabled: *enabled,
                                        offset_x_keyframes: Vec::new(),
                                        offset_y_keyframes: Vec::new(),
                                        blur_keyframes: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Shadow", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleShadows { clip_id, shadows } => {
                // Rebuild the shadow stack — preserve keyframes on
                // surviving slots in place; new slots start with empty
                // keyframe vecs. Mirrors SetTitleOutlines.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                let new_len = shadows.len();
                                fx.shadows.truncate(new_len);
                                for (i, (color, off_x, off_y, blur)) in shadows.iter().enumerate() {
                                    if let Some(existing) = fx.shadows.get_mut(i) {
                                        existing.color = *color;
                                        existing.offset_x = *off_x;
                                        existing.offset_y = *off_y;
                                        existing.blur = *blur;
                                        existing.enabled = true;
                                    } else {
                                        fx.shadows.push(klipp_timeline::TitleShadow {
                                            color: *color,
                                            offset_x: *off_x,
                                            offset_y: *off_y,
                                            blur: *blur,
                                            enabled: true,
                                            offset_x_keyframes: Vec::new(),
                                            offset_y_keyframes: Vec::new(),
                                            blur_keyframes: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Shadow Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleShadowLayer { clip_id, index, enabled, color, offset_x, offset_y, blur } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(slot) = fx.shadows.get_mut(*index as usize) {
                                    slot.enabled = *enabled;
                                    slot.color = *color;
                                    slot.offset_x = *offset_x;
                                    slot.offset_y = *offset_y;
                                    slot.blur = *blur;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Shadow Layer", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleShadowStackEnabled { clip_id, enabled } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                fx.shadow_stack_enabled = *enabled;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Shadow Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleOutline { clip_id, enabled, color, width } => {
                // Soft-disable on the singular Outline subcard (= outlines[0]).
                // Outline Layers stack (outlines[1..]) is preserved untouched
                // — disabling the primary outline must not nuke the stack.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                // Preserve keyframes on outlines[0] —
                                // mutate fields in place.
                                if let Some(first) = fx.outlines.first_mut() {
                                    first.color = *color;
                                    first.width = *width;
                                    first.enabled = *enabled;
                                } else {
                                    fx.outlines.push(klipp_timeline::TextOutline {
                                        color: *color,
                                        width: *width,
                                        enabled: *enabled,
                                        width_keyframes: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Outline", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleGlow { clip_id, enabled, color, radius, intensity } => {
                // Soft-disable: always Some(...) with enabled from cmd.
                // Mutate static fields in place when a glow already
                // exists so any existing radius/intensity keyframes
                // survive the edit (toggle-mute semantics).
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(g) = fx.glows.first_mut() {
                                    g.color = *color;
                                    g.radius = *radius;
                                    g.intensity = *intensity;
                                    g.enabled = *enabled;
                                } else {
                                    fx.glows.insert(0, klipp_timeline::TextGlow {
                                        color: *color,
                                        radius: *radius,
                                        intensity: *intensity,
                                        enabled: *enabled,
                                        radius_keyframes: Vec::new(),
                                        intensity_keyframes: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Glow", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleGlows { clip_id, glows } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                let new_len = glows.len();
                                fx.glows.truncate(new_len);
                                for (i, (color, radius, intensity)) in glows.iter().enumerate() {
                                    if let Some(existing) = fx.glows.get_mut(i) {
                                        existing.color = *color;
                                        existing.radius = *radius;
                                        existing.intensity = *intensity;
                                        existing.enabled = true;
                                    } else {
                                        fx.glows.push(klipp_timeline::TextGlow {
                                            color: *color,
                                            radius: *radius,
                                            intensity: *intensity,
                                            enabled: true,
                                            radius_keyframes: Vec::new(),
                                            intensity_keyframes: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Glow Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleGlowLayer { clip_id, index, enabled, color, radius, intensity } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(slot) = fx.glows.get_mut(*index as usize) {
                                    slot.enabled = *enabled;
                                    slot.color = *color;
                                    slot.radius = *radius;
                                    slot.intensity = *intensity;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Glow Layer", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleGlowStackEnabled { clip_id, enabled } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                fx.glow_stack_enabled = *enabled;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Glow Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleOutlines { clip_id, outlines } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                // Preserve per-index width_keyframes
                                // when rebuilding the stack: existing
                                // slots get their static fields
                                // updated in place, slots that didn't
                                // previously exist start with empty
                                // keyframe vecs, removed slots lose
                                // their keyframes (which is correct —
                                // a removed outline can't animate).
                                let new_len = outlines.len();
                                fx.outlines.truncate(new_len);
                                for (i, (color, width)) in outlines.iter().enumerate() {
                                    if let Some(existing) = fx.outlines.get_mut(i) {
                                        existing.color = *color;
                                        existing.width = *width;
                                        existing.enabled = true;
                                        // width_keyframes preserved.
                                    } else {
                                        fx.outlines.push(klipp_timeline::TextOutline {
                                            color: *color,
                                            width: *width,
                                            enabled: true,
                                            width_keyframes: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Outline Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleOutlineLayer { clip_id, index, enabled, color, width } => {
                // Per-index static edit — preserves width_keyframes
                // on that index. Used by the keyframe-aware scrubby
                // path in the inspector.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(slot) = fx.outlines.get_mut(*index as usize) {
                                    slot.enabled = *enabled;
                                    slot.color = *color;
                                    slot.width = *width;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Outline Layer", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleOutlineStackEnabled { clip_id, enabled } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                fx.outline_stack_enabled = *enabled;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Outline Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleInnerShadow { clip_id, enabled, color, offset_x, offset_y, blur } => {
                // Mutate fields in place when an inner shadow already
                // exists so existing keyframes survive an Inspector
                // tweak — same pattern as SetTitleShadow.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(s) = fx.inner_shadows.first_mut() {
                                    s.color = *color;
                                    s.offset_x = *offset_x;
                                    s.offset_y = *offset_y;
                                    s.blur = *blur;
                                    s.enabled = *enabled;
                                } else {
                                    fx.inner_shadows.insert(0, klipp_timeline::InnerShadow {
                                        color: *color,
                                        offset_x: *offset_x,
                                        offset_y: *offset_y,
                                        blur: *blur,
                                        enabled: *enabled,
                                        offset_x_keyframes: Vec::new(),
                                        offset_y_keyframes: Vec::new(),
                                        blur_keyframes: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Inner Shadow", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleInnerShadows { clip_id, shadows } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                let new_len = shadows.len();
                                fx.inner_shadows.truncate(new_len);
                                for (i, (color, off_x, off_y, blur)) in shadows.iter().enumerate() {
                                    if let Some(existing) = fx.inner_shadows.get_mut(i) {
                                        existing.color = *color;
                                        existing.offset_x = *off_x;
                                        existing.offset_y = *off_y;
                                        existing.blur = *blur;
                                        existing.enabled = true;
                                    } else {
                                        fx.inner_shadows.push(klipp_timeline::InnerShadow {
                                            color: *color,
                                            offset_x: *off_x,
                                            offset_y: *off_y,
                                            blur: *blur,
                                            enabled: true,
                                            offset_x_keyframes: Vec::new(),
                                            offset_y_keyframes: Vec::new(),
                                            blur_keyframes: Vec::new(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Inner Shadow Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleInnerShadowLayer { clip_id, index, enabled, color, offset_x, offset_y, blur } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(slot) = fx.inner_shadows.get_mut(*index as usize) {
                                    slot.enabled = *enabled;
                                    slot.color = *color;
                                    slot.offset_x = *offset_x;
                                    slot.offset_y = *offset_y;
                                    slot.blur = *blur;
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Inner Shadow Layer", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleInnerShadowStackEnabled { clip_id, enabled } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                fx.inner_shadow_stack_enabled = *enabled;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Inner Shadow Stack", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleExtrusion { clip_id, enabled, depth, angle, face_color, side_color, lighting } => {
                // Mutate fields in place when an extrusion already
                // exists so existing keyframes survive an Inspector
                // tweak — same pattern as SetTitleShadow.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                if let Some(e) = tb.extrusion.as_mut() {
                                    e.depth = *depth;
                                    e.angle = *angle;
                                    e.face_color = *face_color;
                                    e.side_color = *side_color;
                                    e.lighting = *lighting;
                                    e.enabled = *enabled;
                                } else {
                                    tb.extrusion = Some(klipp_timeline::TextExtrusion {
                                        depth: *depth,
                                        angle: *angle,
                                        face_color: *face_color,
                                        side_color: *side_color,
                                        lighting: *lighting,
                                        enabled: *enabled,
                                        depth_keyframes: Vec::new(),
                                        angle_keyframes: Vec::new(),
                                        lighting_keyframes: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Extrusion", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleMotionBlur { clip_id, enabled, shutter_angle, samples } => {
                // Mutate static fields in place when motion blur
                // already exists so existing shutter_angle keyframes
                // survive an Inspector tweak — same pattern as the
                // other effects.
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) {
                                if let Some(m) = fx.motion_blur.as_mut() {
                                    m.shutter_angle = *shutter_angle;
                                    m.samples = *samples;
                                    m.enabled = *enabled;
                                } else {
                                    fx.motion_blur = Some(klipp_timeline::MotionBlur {
                                        shutter_angle: *shutter_angle,
                                        samples: *samples,
                                        enabled: *enabled,
                                        shutter_angle_keyframes: Vec::new(),
                                    });
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Motion Blur", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key, time_secs, value,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                let mut routed = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let Some(kfs) = o.effect_keyframes_mut_for(param_key) {
                                    klipp_core::keyframe::upsert_keyframe(kfs, *time_secs, *value);
                                    routed = true;
                                }
                            }
                        }
                    }
                }
                debug_assert!(
                    routed,
                    "AddTitleObjectEffectKeyframe: unknown param_key {param_key:?} or no glow on object — check title_effect_keys constants"
                );
                if let Some(old) = old { self.push_title_undo(*clip_id, "Add Effect Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id, object_id, param_key, time_secs,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let Some(kfs) = o.effect_keyframes_mut_for(param_key) {
                                    klipp_core::keyframe::remove_keyframe_at(kfs, *time_secs);
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Remove Effect Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::MoveTitleObjectEffectKeyframe {
                clip_id, object_id, param_key, index, new_time_secs, new_value,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let Some(kfs) = o.effect_keyframes_mut_for(param_key) {
                                    klipp_core::keyframe::move_keyframe(kfs, *index, *new_time_secs, *new_value);
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Move Effect Keyframe", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id, object_id, param_key, keyframe_index, interpolation,
            } => {
                let old = self.snapshot_title(*clip_id);
                let target = klipp_timeline::TitleObjectId(*object_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                if let Some(kfs) = o.effect_keyframes_mut_for(param_key) {
                                    klipp_core::keyframe::set_keyframe_interpolation(kfs, *keyframe_index, *interpolation);
                                }
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Set Effect Keyframe Interpolation", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectEffectKeyframeTangent {
                clip_id, object_id, param_key, keyframe_index, tangent_in, tangent_out,
            } => {
                let old = self.snapshot_title(*clip_id);
                let param = crate::panels::title_curve::TitleCurveParam::Effect(param_key.clone());
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            param.set_tangent(td, *object_id, *keyframe_index, *tangent_in, *tangent_out);
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Set Effect Keyframe Tangent", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn handle_title_animation(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::SetTitleWriteOn { clip_id, write_on } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.write_on = *write_on; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Write-On", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleFadeDuration { duration } => {
                self.state.title_fade_duration = *duration;
                true
            }
            AppCommand::SetTitleLoops { clip_id, loops } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) { fx.loops = loops.clone(); }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Loops", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleEdgeFade { clip_id, edge_fade } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) { fx.edge_fade = *edge_fade; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Edge Fade", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }
}
