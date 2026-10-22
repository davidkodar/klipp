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
                    .unwrap_or((self.state.playhead, ((self.prefs.title.default_duration_secs as f64 * fps_rate as f64).round() as u32).max(1)));
                let track_index = free_text_track(&mut self.state, timeline_in, duration_frames);
                self.handle_title_lifecycle(&AppCommand::CreateTitleClip { track_index, timeline_in, duration_frames }, _needs_refresh)
            }
            AppCommand::CreateTitleInViewer { x, y, box_size } => {
                // The Text tool's title (#1713): made, recorded and put
                // into in-place editing by the same code a test drives.
                let Some(clip_id) = super::text_tool::create_title_in_viewer(
                    &mut self.state, *x, *y, *box_size, &self.prefs.title,
                ) else {
                    return true;
                };
                self.sync_sequence_to_engine();
                self.render_title_to_viewer(clip_id);
                true
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
                // A title from a template comes with its own look; a new
                // one takes the title preferences'.
                if self.state.pending_title_data.is_none() {
                    self.state.pending_title_data = Some(default_title_data(&self.prefs.title));
                }
                let (clip_id, rest) = add_title_clip(&mut self.state, *track_index, *timeline_in, *duration_frames);
                self.sync_sequence_to_engine();
                self.render_title_to_viewer(clip_id);
                // A title placed from a template may name a font this
                // computer lacks: say so, with a replacement (#1720).
                let missing: Vec<_> = self.state.missing_fonts(&crate::missing_fonts::installed).into_iter().filter(|m| m.titles.iter().any(|t| t.clip == clip_id)).collect();
                if !missing.is_empty() {
                    self.state.missing_fonts_dialog = Some(crate::missing_fonts::MissingFontsDialog { fonts: missing, chosen: Default::default() });
                }
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
            picture: Default::default(),
            linked_clip_id: None,
            linked_title_asset_id: Some(asset_media_id),
            group_id: None,
            grade_group: None,
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
            matte_feather_keyframes: Vec::new(),
            matte_choke_keyframes: Vec::new(),
            clip_markers: Vec::new(),
            frame_glide: None,
            role: klipp_core::ClipRole::default(),
            role_source: klipp_core::RoleSource::Unset,
            matched_to: None,
            speed: 1.0,
            speed_keyframes: Vec::new(),
            keep_pitch: false,
            source_rotation: 0,
            enabled: true,
            solo: false,
            blend_mode: klipp_core::BlendMode::Normal,
            depth: 1.0,
            matte_source: None,
            matte_stencil: None,
            consumed_as_matte: false,
            transform_link: None,
            multicam_group: None,
            multicam_angle: 0,
            multicam_cuts: Vec::new(),
            multicam_sound: Default::default(),
            audio_effects: Vec::new(),
            audio_effect_keyframes: std::collections::HashMap::new(),
            track_points: Vec::new(),
            surfaces: Vec::new(),
            surface_pin: None,
            name: None,
            colour_label: 0,
            grade_flag: Default::default(),
            precomp: None,
            baked_own: None,
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
            AppCommand::ReplaceInTitles { find, replace, match_case, whole_word, subtitles, project, only } => {
                let q = klipp_document::find::TextQuery { find: find.clone(), match_case: *match_case, whole_word: *whole_word, subtitles: *subtitles };
                let only = only.map(|(clip_id, object, start, end)| klipp_document::find::TextMatch { clip_id, object: klipp_timeline::TitleObjectId(object), start, end, at_secs: 0.0, line: String::new() });
                let active = self.state.active_sequence_idx;
                let scope: Vec<usize> = if *project { (0..self.state.sequences.len()).collect() } else { vec![active] };
                let mut changed = 0;
                for idx in scope {
                    let edits = klipp_document::find::replace_in_titles(&self.state.sequences[idx], &q, replace, only.as_ref());
                    if edits.is_empty() {
                        continue;
                    }
                    changed += edits.len();
                    // Each sequence's own history takes its step.
                    self.state.active_sequence_idx = idx;
                    self.apply_edits_as_one(&edits, "Replace Text");
                }
                if self.state.active_sequence_idx != active {
                    self.state.active_sequence_idx = active;
                    self.sync_sequence_to_engine();
                }
                self.state.title_find_selected = None;
                self.state.toast.show(if changed == 0 { format!("No title holds \u{201c}{find}\u{201d}") } else { format!("Replaced in {changed} title(s)") });
                true
            }
            AppCommand::CopyTitleStyle { clip_id } => {
                let td = self.state.sequence().all_clips().find(|c| c.id == *clip_id).and_then(|c| c.title_data().cloned());
                match td {
                    Some(td) => {
                        self.state.title_style_clipboard = Some(td);
                        self.state.toast.show("Title style copied".to_string());
                    }
                    None => self.state.toast.show("Not a title: no style copied".to_string()),
                }
                true
            }
            AppCommand::PasteTitleStyle { from, to } => {
                let seq = self.state.sequence();
                let look = match from {
                    Some(id) => seq.all_clips().find(|c| c.id == *id).and_then(|c| c.title_data().cloned()),
                    None => self.state.title_style_clipboard.clone(),
                };
                let Some(look) = look else {
                    self.state.toast.show("Copy a title's style first".to_string());
                    return true;
                };
                let edits = paste_title_style(seq, &look, to);
                if edits.is_empty() {
                    self.state.toast.show("Those titles already look like it".to_string());
                } else {
                    let n = edits.len();
                    self.apply_edits_as_one(&edits, "Paste Title Style");
                    self.state.toast.show(format!("Pasted the style on {n} title(s)"));
                }
                true
            }
            AppCommand::PickTitleStyle { target } => {
                self.state.title_style_pick = *target;
                if target.is_some() {
                    self.state.toast.show("Click the title whose look to take".to_string());
                }
                true
            }
            AppCommand::RevertTitleToStyle { clip_id } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let reverted = seq.all_clips().find(|c| c.id == *clip_id).ok_or_else(|| "the title is gone".to_string()).and_then(|c| klipp_document::ops::revert_to_style(seq, c));
                match reverted {
                    Ok(edit) => self.apply_edits_as_one(&[edit], "Revert to Style"),
                    Err(why) => self.state.toast.show(format!("Not reverted: {why}")),
                }
                true
            }
            AppCommand::UpdateTitlesLike { clip_id } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let found = seq.all_clips().find(|c| c.id == *clip_id).ok_or_else(|| "the title is gone".to_string()).and_then(|c| klipp_document::ops::update_titles_like(seq, c));
                match found {
                    Ok((edits, skipped)) if edits.is_empty() => self.state.toast.show(if skipped > 0 {
                        format!("Nothing updated: {skipped} title(s) have other layers")
                    } else {
                        "Every title like it already looks like it".to_string()
                    }),
                    Ok((edits, skipped)) => {
                        let n = edits.len();
                        self.apply_edits_as_one(&edits, "Update All Titles");
                        self.state.toast.show(if skipped > 0 { format!("Updated {n} title(s); {skipped} have other layers and were left") } else { format!("Updated {n} title(s)") });
                    }
                    Err(why) => self.state.toast.show(format!("Not updated: {why}")),
                }
                true
            }
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
            AppCommand::ToggleFavouriteFont { family } | AppCommand::NoteRecentFont { family } => {
                // A preference, kept across sessions (#1719).
                if matches!(cmd, AppCommand::ToggleFavouriteFont { .. }) {
                    self.prefs.title.toggle_favourite_font(family);
                } else {
                    self.prefs.title.note_recent_font(family);
                }
                self.state.favourite_fonts = self.prefs.title.favourite_fonts.clone();
                self.state.recent_fonts = self.prefs.title.recent_fonts.clone();
                let _ = self.prefs.save();
                true
            }
            AppCommand::ReplaceFont { from, to } => {
                let changed = self.state.replace_font(from, to);
                let active: Vec<_> = self.state.sequences[self.state.active_sequence_idx].all_clips().map(|c| c.id).collect();
                for id in changed.iter().filter(|id| active.contains(id)) {
                    self.render_title_to_viewer(*id);
                }
                if !changed.is_empty() {
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.toast.show(format!("{from} replaced with {to} in {} title{}", changed.len(), if changed.len() == 1 { "" } else { "s" }));
                }
                true
            }
            AppCommand::ShowMissingFonts { family } => {
                let mut fonts = self.state.missing_fonts(&crate::missing_fonts::installed);
                if let Some(f) = family {
                    fonts.retain(|m| &m.family == f);
                }
                if fonts.is_empty() {
                    self.state.toast.show("Every font the titles use is installed");
                } else {
                    self.state.missing_fonts_dialog = Some(crate::missing_fonts::MissingFontsDialog { fonts, chosen: Default::default() });
                }
                true
            }
            AppCommand::LoadCreditsList { path } => {
                match std::fs::read_to_string(path) {
                    Ok(text) => self.state.credits_dialog.get_or_insert_with(Default::default).text = text,
                    Err(e) => self.state.toast.show(format!("Couldn't read {}: {e}", path.display())),
                }
                true
            }
            AppCommand::SetTitleField { clip_id, index, value, done } => {
                match self.state.set_title_field(*clip_id, *index, value, *done) {
                    Ok(()) => {
                        self.render_title_to_viewer(*clip_id);
                        self.sync_sequence_to_engine();
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::SetTitleMotion { clip_id, motion } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            let edited = target_object_mut(td, target_oid).map(|o| {
                                o.fx_mut().motion = *motion;
                                o.id
                            });
                            // A template's Speed starts from this timing now (#1139).
                            if let Some(oid) = edited {
                                td.rebase_speed(oid);
                            }
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
            AppCommand::SetTitleParagraph { clip_id, key, value } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                let value = if value.is_finite() { value.clamp(-2000.0, 2000.0) } else { 0.0 };
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(v) = target_textbox_mut(td, target_oid).and_then(|tb| tb.paragraph.value_mut_for(key)) {
                                *v = value;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Paragraph", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleTabStops { clip_id, stops } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                let mut tabs: Vec<klipp_timeline::TabStop> = stops
                    .iter()
                    .filter(|(at, _)| at.is_finite())
                    .take(32)
                    .map(|&(at, align)| klipp_timeline::TabStop {
                        at: at.clamp(0.0, 4000.0),
                        align: match align {
                            1 => klipp_timeline::TabAlign::Center,
                            2 => klipp_timeline::TabAlign::Right,
                            3 => klipp_timeline::TabAlign::Decimal,
                            _ => klipp_timeline::TabAlign::Left,
                        },
                    })
                    .collect();
                tabs.sort_by(|a, b| a.at.total_cmp(&b.at));
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.paragraph.tabs = tabs.clone();
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Tab Stops", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleList { clip_id, style, indent } => {
                use klipp_timeline::{ListMarker, ListStyle, NumberStyle};
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                let indent = if indent.is_finite() { indent.clamp(0.0, 2000.0) } else { 60.0 };
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                let (start, suffix) = match tb.paragraph.list.as_ref().map(|l| &l.marker) {
                                    Some(ListMarker::Numbered { start, suffix, .. }) => (*start, suffix.clone()),
                                    _ => (1, ".".to_string()),
                                };
                                let count = |style| ListMarker::Numbered { style, start, suffix: suffix.clone() };
                                let marker = match style {
                                    1 => Some(match tb.paragraph.list.as_ref().map(|l| &l.marker) {
                                        Some(m @ ListMarker::Bullet { .. }) => m.clone(),
                                        _ => ListMarker::Bullet { mark: "\u{2022}".into() },
                                    }),
                                    2 => Some(count(NumberStyle::Arabic)),
                                    3 => Some(count(NumberStyle::LowerAlpha)),
                                    4 => Some(count(NumberStyle::UpperAlpha)),
                                    5 => Some(count(NumberStyle::LowerRoman)),
                                    6 => Some(count(NumberStyle::UpperRoman)),
                                    _ => None,
                                };
                                tb.paragraph.list = marker.map(|marker| ListStyle { marker, indent });
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title List", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleMinLastWords { clip_id, words } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.paragraph.min_last_words = (*words).min(6);
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Last Lines", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleLastLine { clip_id, last_line } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.last_line = match last_line {
                                1 => klipp_timeline::LastLineAlign::Center,
                                2 => klipp_timeline::LastLineAlign::Justify,
                                _ => klipp_timeline::LastLineAlign::Left,
                            }; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Last Line", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleFit { clip_id, fit, min_size } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                let min_size = if min_size.is_finite() { min_size.clamp(4.0, 1000.0) } else { 24.0 };
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.fit = match fit {
                                    1 => klipp_timeline::TextFit::Shrink { min_size },
                                    2 => klipp_timeline::TextFit::Grow,
                                    _ => klipp_timeline::TextFit::Wrap,
                                };
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Fit", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleVerticalAlign { clip_id, vertical_align } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.vertical_align = match vertical_align {
                                0 => klipp_timeline::TextVerticalAlign::Top,
                                2 => klipp_timeline::TextVerticalAlign::Bottom,
                                _ => klipp_timeline::TextVerticalAlign::Middle,
                            }; }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Vertical Alignment", old); }
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
            AppCommand::SetTitleFontVariation { clip_id, weight, axes } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.font_weight = *weight;
                                tb.font_axes = axes.clone();
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Font Weight", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleFeatures { clip_id, features } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) { tb.features = features.clone(); }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Font Features", old); }
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
            AppCommand::SetTitleVerticalColumnsLtr { clip_id, left_to_right } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.vertical_columns_ltr = *left_to_right;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Vertical Columns", old); }
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
                                |run| set_run_property(run, prop, val),
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
                                3 => klipp_timeline::TextAlignment::Justify,
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
                                    gradient: None,
                                    stroke: None,
                                }),
                                1 => klipp_timeline::TitleObjectContent::Shape(
                                    klipp_timeline::Shape {
                                        kind: klipp_timeline::ShapeKind::Rectangle {
                                            corner_radius: 0.0,
                                        },
                                        fit: None,
                                        fill: Some([60, 60, 70, 220]),
                                        gradient: None,
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
                            // A layer it masked is no longer masked (#1513).
                            for o in &mut td.objects {
                                if o.mask.is_some_and(|m| m.source == target) {
                                    o.mask = None;
                                }
                            }
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
            AppCommand::SetTitleObjectVisible { clip_id, visible, .. } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            crate::inspector::title_layers::apply(td, cmd);
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, if *visible { "Show Layer" } else { "Hide Layer" }, old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectLocked { clip_id, locked, .. } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            crate::inspector::title_layers::apply(td, cmd);
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, if *locked { "Lock Layer" } else { "Unlock Layer" }, old); }
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
            AppCommand::SetTitleObjectBlend { clip_id, object_id, blend } => {
                let target = klipp_timeline::TitleObjectId(*object_id);
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                o.blend = *blend;
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Layer Blend", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleObjectMask { clip_id, object_id, mask } => {
                let target = klipp_timeline::TitleObjectId(*object_id);
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(o) = td.object_mut(target) {
                                // A layer never masks itself (#1513).
                                o.mask = mask
                                    .filter(|(source, _)| *source != *object_id)
                                    .map(|(source, kind)| klipp_timeline::LayerMask { source: klipp_timeline::TitleObjectId(source), kind });
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Layer Mask", old); }
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
            AppCommand::DuplicateTitleObject { clip_id, .. } => {
                let old = self.snapshot_title(*clip_id);
                let mut copy = None;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            copy = crate::inspector::title_layers::apply(td, cmd);
                        }
                    }
                }
                if let Some(id) = copy {
                    self.state.selected_title_object = Some((*clip_id, id));
                    if let Some(old) = old { self.push_title_undo(*clip_id, "Duplicate Layer", old); }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                }
                true
            }
            AppCommand::RenameTitleObject { clip_id, .. } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            crate::inspector::title_layers::apply(td, cmd);
                        }
                    }
                }
                // One step each (#1715).
                if let Some(old) = old { self.push_title_undo(*clip_id, "Rename Layer", old); }
                self.state.dirty = true;
                true
            }
            AppCommand::ReorderTitleObjects { clip_id, .. } => {
                let old = self.snapshot_title(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            crate::inspector::title_layers::apply(td, cmd);
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
            AppCommand::SetTitleObjectTransform { .. }
            | AppCommand::SetTitleObjectTransformAndFont { .. }
            | AppCommand::SetTitleArcStart { .. }
            | AppCommand::BeginTitleObjectDrag { .. }
            | AppCommand::EndTitleObjectDrag { .. } => {
                // The viewer's handles (#1714), applied on the state.
                if let Some(clip_id) = super::title_handles::apply(&mut self.state, cmd) {
                    self.render_title_to_viewer(clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
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
            AppCommand::SetTitleTextPathLayer { clip_id, layer, contour, offset, align, flip } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                set_text_path_layer(tb, klipp_timeline::TitleObjectId(*layer), *contour, *offset, *align, *flip);
                            }
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
            AppCommand::DrawTitleTextPath { clip_id } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                let mut new_id = None;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            new_id = Some(draw_text_path(td, target_oid));
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Draw Text Path", old); }
                if let Some(id) = new_id {
                    self.state.selected_title_object = Some((*clip_id, id));
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
                clip_id, object_id, axis, keyframe_index, tangent_in, tangent_out, handles,
            } => {
                let old = self.snapshot_title(*clip_id);
                let param = crate::panels::title_curve::TitleCurveParam::Position(*axis);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            param.set_tangent(td, *object_id, *keyframe_index, *tangent_in, *tangent_out, *handles);
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
                                // The ends of a gradient of more stops
                                // (#1509) are the pair's colours.
                                let mut g = tb.gradient.take().unwrap_or_else(|| klipp_timeline::TextGradient::pair(*top_color, *bottom_color, *enabled));
                                g.enabled = *enabled;
                                g.top_color = *top_color;
                                g.bottom_color = *bottom_color;
                                if g.stops.len() >= 2 {
                                    let last = g.stops.len() - 1;
                                    g.stops[0].color = *top_color;
                                    g.stops[last].color = *bottom_color;
                                }
                                tb.gradient = Some(g);
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
            AppCommand::SetTitleGradientStops { clip_id, object_id, enabled, stops, kind } => {
                if stops.len() < 2 || stops.len() > klipp_timeline::MAX_GRADIENT_STOPS {
                    return true;
                }
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                let gradient = klipp_timeline::TextGradient::with_stops(stops, *kind, *enabled);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            match object_id {
                                None => {
                                    if let Some(tb) = target_textbox_mut(td, target_oid) {
                                        // Each stop keeps its colour keys (#1511).
                                        let mut g = gradient.clone();
                                        if let Some(before) = &tb.gradient {
                                            g.carry_keys_from(before);
                                        }
                                        tb.gradient = Some(g);
                                    }
                                }
                                Some(id) => {
                                    if let Some(o) = td.object_mut(klipp_timeline::TitleObjectId(*id)) {
                                        if let klipp_timeline::TitleObjectContent::Shape(s) = &mut o.content {
                                            s.gradient = Some(gradient.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // A stop dragged along the bar is one step (#1509).
                if !self.state.active_history().is_dragging(&format!("gradient:{}", clip_id.0)) {
                    if let Some(old) = old { self.push_title_undo(*clip_id, "Title Gradient", old); }
                }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::BeginTitleGradientDrag { clip_id } => {
                if let Some(data) = self.snapshot_title(*clip_id) {
                    self.state.active_history_mut().begin_drag(format!("gradient:{}", clip_id.0), crate::history::HistoryEntry {
                        command: crate::history::EditCommand::EditTitle { clip_id: *clip_id, description: "Title Gradient", old_data: data.clone(), new_data: data },
                    });
                }
                true
            }
            AppCommand::EndTitleGradientDrag { clip_id } => {
                if self.state.active_history().is_dragging(&format!("gradient:{}", clip_id.0)) {
                    if let Some(data) = self.snapshot_title(*clip_id) {
                        self.state.active_history_mut().end_drag(crate::history::HistoryEntry {
                            command: crate::history::EditCommand::EditTitle { clip_id: *clip_id, description: "Title Gradient", old_data: data.clone(), new_data: data },
                        });
                    }
                }
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
            AppCommand::SetTitleObjectColourKeyframe { clip_id, object_id, param_key, time_secs, color } => {
                let old = self.snapshot_title(*clip_id);
                let tolerance = crate::inspector::keyframe_selection::frame_tolerance(self.state.sequence());
                let target = klipp_timeline::TitleObjectId(*object_id);
                let mut routed = false;
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(klipp_timeline::TitleObjectContent::TextBox(tb)) = td.object_mut(target).map(|o| &mut o.content) {
                                if param_key == klipp_core::title_effect_keys::TEXT_COLOR {
                                    match color {
                                        Some(c) => tb.color_keyframes.set(*time_secs, *c),
                                        None => tb.color_keyframes.remove(*time_secs, tolerance),
                                    }
                                    routed = true;
                                } else if let Some((i, "")) = klipp_core::title_effect_keys::parse_gradient_stop_key(param_key) {
                                    // A gradient stop's colour, by its place in the stops (#1511).
                                    if let Some(g) = tb.gradient.as_mut().filter(|g| i < g.stops().len()) {
                                        if g.stop_keyframes.len() <= i {
                                            g.stop_keyframes.resize(i + 1, Default::default());
                                        }
                                        match color {
                                            Some(c) => g.stop_keyframes[i].set(*time_secs, *c),
                                            None => g.stop_keyframes[i].remove(*time_secs, tolerance),
                                        }
                                        routed = true;
                                    }
                                }
                            }
                        }
                    }
                }
                debug_assert!(routed, "SetTitleObjectColourKeyframe: unknown colour {param_key:?} or not a text layer");
                if let Some(old) = old { self.push_title_undo(*clip_id, if color.is_some() { "Key Colour" } else { "Remove Colour Key" }, old); }
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
                clip_id, object_id, param_key, keyframe_index, tangent_in, tangent_out, handles,
            } => {
                let old = self.snapshot_title(*clip_id);
                let param = crate::panels::title_curve::TitleCurveParam::Effect(param_key.clone());
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            param.set_tangent(td, *object_id, *keyframe_index, *tangent_in, *tangent_out, *handles);
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
            AppCommand::SetTitleText3d { clip_id, three_d } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_text_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(tb) = target_textbox_mut(td, target_oid) {
                                tb.three_d = three_d.clone();
                            }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title 3D", old); }
                self.render_title_to_viewer(*clip_id);
                self.sync_sequence_to_engine();
                let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                self.state.dirty = true;
                true
            }
            AppCommand::SetTitleAnimators { clip_id, animators } => {
                let old = self.snapshot_title(*clip_id);
                let target_oid = self.title_layer_target_id(*clip_id);
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                        if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                            if let Some(fx) = target_fx_mut(td, target_oid) { fx.animators = animators.clone(); }
                        }
                    }
                }
                if let Some(old) = old { self.push_title_undo(*clip_id, "Title Animators", old); }
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

impl KlippApp {
    /// Render a title clip to the viewer's overlay texture for instant UI preview.
    /// This is the FCP approach: title is a separate layer composited by the viewer,
    /// not written to the shared frame buffer (which avoids race conditions with the engine).
    /// Snapshot a title clip's TitleData for undo. Returns None if clip not found or not a title.
    fn snapshot_title(&self, clip_id: klipp_core::ClipId) -> Option<klipp_timeline::TitleData> {
        super::title_handles::snapshot_title(&self.state, clip_id)
    }

    /// Resolve which TextBox a title-style mutation should target.
    /// Returns the selected object's id when it's a TextBox layer of
    /// the named clip; falls back to `None` so the caller routes to
    /// `target_textbox_mut(td, target_oid)` (the first TextBox in the stack).
    /// Used by every per-property title command so Inspector edits
    /// follow the on-canvas / Layers-panel selection.
    /// The selected layer of `clip_id`, whatever it is: the target of a
    /// command on a layer's stack — motion, loops, effects, motion blur
    /// — which a shape or a picture carries as a text does.
    fn title_layer_target_id(
        &self,
        clip_id: klipp_core::ClipId,
    ) -> Option<klipp_timeline::TitleObjectId> {
        let (selected_clip, oid) = self.state.selected_title_object?;
        (selected_clip == clip_id).then_some(oid)
    }

    fn title_text_target_id(
        &self,
        clip_id: klipp_core::ClipId,
    ) -> Option<klipp_timeline::TitleObjectId> {
        let (selected_clip, oid) = self.state.selected_title_object?;
        if selected_clip != clip_id {
            return None;
        }
        // Only return the id when the object is actually a TextBox —
        // a Shape selection shouldn't divert text-style mutations.
        let td = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.title_data())?;
        let obj = td.object(oid)?;
        match &obj.content {
            klipp_timeline::TitleObjectContent::TextBox(_) => Some(oid),
            _ => None,
        }
    }

    /// Push a title edit undo entry if the data changed.
    fn push_title_undo(&mut self, clip_id: klipp_core::ClipId, description: &'static str, old: klipp_timeline::TitleData) {
        super::title_handles::push_title_undo(&mut self.state, clip_id, description, old);
    }

    pub(super) fn snapshot_generator(&self, clip_id: klipp_core::ClipId) -> Option<klipp_core::GeneratorData> {
        self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.generator_data())
            .cloned()
    }

    pub(super) fn push_generator_undo(&mut self, clip_id: klipp_core::ClipId, old: klipp_core::GeneratorData) {
        if let Some(new) = self.snapshot_generator(clip_id) {
            if old != new {
                self.state.active_history_mut().push(crate::history::HistoryEntry {
                    command: crate::history::EditCommand::EditGenerator {
                        clip_id, old_data: old, new_data: new,
                    },
                });
            }
        }
    }

    /// Mark a title clip for overlay texture refresh on the next frame.
    pub(super) fn render_title_to_viewer(&mut self, clip_id: klipp_core::ClipId) {
        // Clear old overlay immediately to prevent ghosting during position/alignment changes.
        // Route the drop through the deferred-drop ring — a bare `= None`
        // here would drop the handle while the current submit may still
        // reference its texture ID.
        if let Some(old) = self.viewer.title_overlay_texture.take() {
            self.defer_drop_texture(old);
        }
        self.state.pending_title_preview_clip = Some(clip_id);
    }

    /// Refresh what in-place title editing needs (glyph positions) for a
    /// pending title. Called from update() where ctx is available.
    pub(super) fn upload_title_overlay(&mut self, ctx: &egui::Context) {
        // Explicit clear request (e.g. scrub passed beyond a title clip).
        if self.state.pending_title_overlay_clear {
            self.state.pending_title_overlay_clear = false;
            if let Some(old) = self.viewer.title_overlay_texture.take() {
                self.defer_drop_texture(old);
            }
        }
        let clip_id = match self.state.pending_title_preview_clip.take() {
            Some(id) => id,
            None => return,
        };
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let td = seq.tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.title_data());
        if let Some(td) = td {
            let mut td = td.clone();
            // While the user is editing one TextBox object inline,
            // suppress its rendered copy so the egui TextEdit
            // floating in the bounding box doesn't double-print
            // against the styled raster underneath. The bounding-
            // box editor IS the WYSIWYG view during edit mode —
            // stamping the rendered text behind it would show the
            // string twice.
            if self.state.viewer_text_edit {
                if let Some(oid) = self.state.viewer_text_edit_object {
                    if let Some(o) = td.object_mut(oid) {
                        o.visible = false;
                    }
                }
            }
            let (w, h) = (seq.resolution.width, seq.resolution.height);

            // The title itself is the engine's frame; no raster of it is
            // made here any more. What in-place editing still needs is
            // where the glyphs fall.
            if self.state.viewer_text_edit {
                self.state.viewer_char_positions = self.char_positions_for_edit(&td, w, h);
                ctx.request_repaint();
            }
        }
    }

    /// Recompute cached character positions when entering text edit mode
    /// or when positions are empty but edit mode is active.
    pub(super) fn recompute_char_positions_if_needed(&mut self) {
        if !self.state.viewer_text_edit {
            return;
        }
        // Recompute if we have no positions cached (just entered edit mode)
        // or if request_focus was just set (initial entry)
        if !self.state.viewer_char_positions.is_empty() {
            return;
        }
        if let Some(clip_id) = self.state.viewer_text_edit_clip {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            let td = seq.tracks.iter()
                .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
                .and_then(|c| c.title_data());
            if let Some(td) = td {
                let td = td.clone();
                let (w, h) = (seq.resolution.width, seq.resolution.height);
                self.state.viewer_char_positions = self.char_positions_for_edit(&td, w, h);
            }
        }
    }

    /// The glyph boxes of the layer being edited (the targeted object,
    /// else the primary text), in frame pixels, for the viewer's caret
    /// and selection.
    fn char_positions_for_edit(&mut self, td: &klipp_timeline::TitleData, w: u32, h: u32) -> Vec<klipp_title::CharPosition> {
        let single = self.state.viewer_text_edit_object.and_then(|oid| td.object(oid)).and_then(|o| match &o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                let mut s = klipp_timeline::TitleData::single_text(tb.clone());
                if let Some(first) = s.objects.first_mut() {
                    first.transform = o.transform.clone();
                }
                Some(s)
            }
            _ => None,
        });
        match single {
            Some(s) => self.title_renderer.compute_char_positions(&s, w, h),
            None => self.title_renderer.compute_char_positions(td, w, h),
        }
    }
}

/// Apply baked-in animation keyframes to a title clip based on an animation identifier string.
///
/// Extract the target `ClipId` from any title-edit `AppCommand`.
/// Returns `None` when the command isn't a title edit (lifecycle /
/// asset commands carry their own clip-id semantics handled
/// elsewhere). Used by the title-command dispatcher to know which
/// clip to propagate from after a successful edit.
pub(super) fn title_edit_clip_id(cmd: &AppCommand) -> Option<klipp_core::ClipId> {
    use AppCommand as C;
    match cmd {
        C::UpdateTitleData { clip_id, .. }
        | C::ApplyTitleStyle { clip_id, .. }
        | C::SetTitleAlignment { clip_id, .. }
        | C::SetTitleLastLine { clip_id, .. }
        | C::SetTitleVerticalAlign { clip_id, .. }
        | C::SetTitlePosition { clip_id, .. }
        | C::SetTitleBackground { clip_id, .. }
        | C::SetTitleShadow { clip_id, .. }
        | C::SetTitleGradient { clip_id, .. }
        | C::SetTitleGradientStops { clip_id, .. }
        | C::AddTitleObjectOpacityKeyframe { clip_id, .. }
        | C::RemoveTitleObjectOpacityKeyframe { clip_id, .. }
        | C::AddTitleObjectPositionKeyframe { clip_id, .. }
        | C::RemoveTitleObjectPositionKeyframe { clip_id, .. }
        | C::AddTitleObjectTransformKeyframe { clip_id, .. }
        | C::RemoveTitleObjectTransformKeyframe { clip_id, .. }
        | C::MoveTitleObjectPositionKeyframe { clip_id, .. }
        | C::SetTitleObjectPositionKeyframeInterpolation { clip_id, .. }
        | C::SetTitleObjectPositionKeyframeTangent { clip_id, .. }
        | C::AddTitleObjectEffectKeyframe { clip_id, .. }
        | C::RemoveTitleObjectEffectKeyframe { clip_id, .. }
        | C::MoveTitleObjectEffectKeyframe { clip_id, .. }
        | C::SetTitleObjectEffectKeyframeInterpolation { clip_id, .. }
        | C::SetTitleObjectColourKeyframe { clip_id, .. }
        | C::SetTitleObjectEffectKeyframeTangent { clip_id, .. }
        | C::SetTitleTexturePreset { clip_id, .. }
        | C::SetTitleTextureParams { clip_id, .. }
        | C::SetTitleOutlineStackEnabled { clip_id, .. }
        | C::SetTitleShadows { clip_id, .. }
        | C::SetTitleShadowLayer { clip_id, .. }
        | C::SetTitleShadowStackEnabled { clip_id, .. }
        | C::SetTitleInnerShadows { clip_id, .. }
        | C::SetTitleInnerShadowLayer { clip_id, .. }
        | C::SetTitleInnerShadowStackEnabled { clip_id, .. }
        | C::SetTitleGlows { clip_id, .. }
        | C::SetTitleGlowLayer { clip_id, .. }
        | C::SetTitleGlowStackEnabled { clip_id, .. } => Some(*clip_id),
        _ => None,
    }
}

/// Animation format: `"type_duration"` or `"type1+type2_duration"` where duration is in seconds.
/// Examples: `"fade_in_0.5"`, `"slide_left_1.0"`, `"fade_in+slide_left_0.8"`, `"fade_in_out_0.5"`
/// Resolve a `&mut TextBox` to mutate inside a TitleData given an
/// optional target object id. Falls back to the title's primary
/// TextBox when `target` is `None` (the historical single-text
/// behaviour). Used by every per-property title command so an
/// Inspector edit follows the active per-object selection.
/// The object a title command targets: the one named, else the first
/// text object — the same choice [`target_textbox_mut`] makes.
pub(super) fn target_object_mut(
    td: &mut klipp_timeline::TitleData,
    target: Option<klipp_timeline::TitleObjectId>,
) -> Option<&mut klipp_timeline::TitleObject> {
    let idx = match target {
        Some(oid) => td.objects.iter().position(|o| o.id == oid),
        None => td.objects.iter().position(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))),
    }?;
    td.objects.get_mut(idx)
}

/// The stack of the object a title command targets — where a title's
/// motion and effects live now — to edit.
pub(super) fn target_fx_mut(
    td: &mut klipp_timeline::TitleData,
    target: Option<klipp_timeline::TitleObjectId>,
) -> Option<&mut klipp_timeline::LayerFx> {
    target_object_mut(td, target).map(klipp_timeline::TitleObject::fx_mut)
}

pub(super) fn target_textbox_mut(
    td: &mut klipp_timeline::TitleData,
    target: Option<klipp_timeline::TitleObjectId>,
) -> Option<&mut klipp_timeline::TextBox> {
    // Resolve which index in `td.objects` the request targets. Done
    // up-front (immutable scan) so the subsequent mutable borrow is
    // a single, simple iter that the borrow checker accepts without
    // Polonius. Falls back to the index of the first TextBox object
    // when no target is specified.
    let idx = match target {
        Some(oid) => td.objects.iter().position(|o| o.id == oid
            && matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))),
        None => td.objects.iter().position(|o|
            matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))),
    };
    let idx = idx?;
    match &mut td.objects[idx].content {
        klipp_timeline::TitleObjectContent::TextBox(tb) => Some(tb),
        _ => None,
    }
}

/// Flatten a Title-inspector style preset into the target TextBox.
/// Each preset writes a deliberate combination of fill colour, glow,
/// outline, shadow, and modern-FX fields. Presets coexist as
/// applied-and-flattened settings — there's no "currently-selected
/// preset" recorded on the data, so the inspector dropdown is a
/// quick-style picker rather than a slot.
pub(crate) fn apply_title_preset(tb: &mut klipp_timeline::TextBox, fx: &mut klipp_timeline::LayerFx, preset_id: u8) {
    use klipp_timeline::{InnerShadow, TextGlow, TextOutline, TitleShadow};
    match preset_id {
        // 0 = "None" sentinel — no-op so the dropdown's default
        // doesn't accidentally clobber the current style.
        0 => {}
        // 1 = Neon. Bright fill (uses current colour as accent),
        // thin dark outline for legibility, strong outer glow tinted
        // to the accent.
        1 => {
            let accent = tb.color;
            fx.outlines = vec![TextOutline {
                color: [10, 10, 14, 180],
                width: 2.0,
                enabled: true,
                width_keyframes: Vec::new(),
            }];
            fx.glows = vec![TextGlow {
                color: accent,
                radius: 12.0,
                intensity: 1.4,
                enabled: true,
                radius_keyframes: Vec::new(),
                intensity_keyframes: Vec::new(),
            }];
            // Clear conflicting effects so the look is clean.
            fx.shadows.clear();
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 2 = Classic. White fill, no decorations.
        2 => {
            tb.color = [255, 255, 255, 255];
            fx.outlines.clear();
            fx.glows.clear();
            fx.shadows.clear();
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.pattern_fill = None;
            tb.pattern_fill_enabled = true;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 3 = Bold Shadow. White fill + soft drop shadow for contrast
        // against any background. Common lower-third look.
        3 => {
            tb.color = [255, 255, 255, 255];
            fx.outlines.clear();
            fx.glows.clear();
            fx.shadows = vec![TitleShadow {
                color: [0, 0, 0, 200],
                offset_x: 4.0,
                offset_y: 4.0,
                blur: 6.0,
                enabled: true,
                offset_x_keyframes: Vec::new(),
                offset_y_keyframes: Vec::new(),
                blur_keyframes: Vec::new(),
            }];
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 4 = Outlined. White fill + thick black outline. Reads on
        // any backdrop without a fill plate.
        4 => {
            tb.color = [255, 255, 255, 255];
            fx.outlines = vec![TextOutline {
                color: [0, 0, 0, 255],
                width: 5.0,
                enabled: true,
                width_keyframes: Vec::new(),
            }];
            fx.glows.clear();
            fx.shadows.clear();
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 5 = VHS. Chromatic aberration + glitch slicing for the
        // tape / synth-wave look. Keep the fill colour as-is so the
        // user's chosen hue tints the channel split.
        5 => {
            fx.outlines.clear();
            fx.glows.clear();
            fx.shadows.clear();
            fx.inner_shadows = vec![InnerShadow {
                color: [0, 0, 0, 100],
                offset_x: 0.0,
                offset_y: 0.0,
                blur: 2.0,
                enabled: true,
                offset_x_keyframes: Vec::new(),
                offset_y_keyframes: Vec::new(),
                blur_keyframes: Vec::new(),
            }];
            tb.chromatic_aberration_px = 6.0;
            tb.glitch_intensity = 0.35;
        }
        _ => {}
    }
}

/// A text track at `timeline_in` with room for `duration_frames`, else a
/// new one on top: where a title made at the playhead goes, from the
/// title button or the Text tool (#1713).
pub(super) fn free_text_track(state: &mut AppState, timeline_in: klipp_core::Timecode, duration_frames: u32) -> usize {
    let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
    let timeline_out = timeline_in.rescale_to(fps_rate) + klipp_core::RationalTime::from_frames(duration_frames as i64, fps_rate);
    let seq = &mut state.sequences[state.active_sequence_idx];
    let free = seq.tracks.iter().position(|t| {
        t.kind == klipp_timeline::TrackKind::Text
            && !t.clips.iter().any(|c| c.timeline_in < timeline_out && c.timeline_out > timeline_in)
    });
    match free {
        Some(i) => i,
        None => {
            let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Text).count() + 1;
            seq.tracks.insert(0, klipp_timeline::Track::new(&format!("Text {n}"), klipp_timeline::TrackKind::Text));
            0
        }
    }
}

/// A title clip of `state.pending_title_data` (else the default title) on
/// track `track_index`, recorded as one undo step and selected. Returns
/// the clip and where it rests, for the caller to park the playhead; the
/// engine sync and the overlay raster are the caller's too.
/// A new title as the title preferences set it: the default title with
/// its text in the default font (none set keeps the title's own), size,
/// colour and alignment.
pub(super) fn default_title_data(prefs: &crate::prefs::TitlePrefs) -> klipp_timeline::TitleData {
    let mut td = klipp_timeline::TitleData::default();
    if let Some(tb) = td.primary_text_mut() {
        if !prefs.default_font.trim().is_empty() {
            tb.font_name = prefs.default_font.trim().to_string();
        }
        tb.font_size = prefs.default_size_pt;
        let [r, g, b] = prefs.default_color;
        tb.color = [r, g, b, 255];
        tb.alignment = match prefs.default_alignment {
            crate::prefs::TextAlignment::Left => klipp_timeline::TextAlignment::Left,
            crate::prefs::TextAlignment::Center => klipp_timeline::TextAlignment::Center,
            crate::prefs::TextAlignment::Right => klipp_timeline::TextAlignment::Right,
        };
    }
    td
}

pub(super) fn add_title_clip(
    state: &mut AppState,
    track_index: usize,
    timeline_in: klipp_core::Timecode,
    duration_frames: u32,
) -> (klipp_core::ClipId, Option<klipp_core::RationalTime>) {
    let ti = track_index;
    // Check if this track was just created (it's a fresh text track with no clips)
    let created_track = state.sequences[state.active_sequence_idx].tracks
        .get(ti)
        .is_some_and(|t| t.kind == klipp_timeline::TrackKind::Text && t.clips.is_empty());
    let fps_rate = state.sequences[state.active_sequence_idx].fps.rate();
    let timeline_in = timeline_in.rescale_to(fps_rate);
    let duration = klipp_core::RationalTime::from_frames(duration_frames as i64, fps_rate);
    let timeline_out = timeline_in + duration;
    let title_data = state.pending_title_data.take()
        .unwrap_or_default();
    let clip = klipp_timeline::Clip {
        id: klipp_core::ClipId::new(),
        kind: klipp_timeline::ClipKind::Title(title_data),
        timeline_in,
        timeline_out,
        source_in: klipp_core::RationalTime::from_frames(0, fps_rate),
        source_out: duration,
        picture: Default::default(),
        linked_clip_id: None,
        linked_title_asset_id: None,
        group_id: None,
        grade_group: None,
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
        matte_feather_keyframes: Vec::new(),
        matte_choke_keyframes: Vec::new(),
        clip_markers: Vec::new(),
        frame_glide: None,
        role: klipp_core::ClipRole::default(),
        role_source: klipp_core::RoleSource::Unset,
        matched_to: None,
        speed: 1.0,
        speed_keyframes: Vec::new(),
        keep_pitch: false,
        source_rotation: 0,
        enabled: true,
        solo: false,
        blend_mode: klipp_core::BlendMode::Normal,
        depth: 1.0,
        matte_source: None,
        matte_stencil: None,
        consumed_as_matte: false,
        transform_link: None,
        multicam_group: None,
        multicam_angle: 0,
        multicam_cuts: Vec::new(),
        multicam_sound: Default::default(),
        audio_effects: Vec::new(),
        audio_effect_keyframes: std::collections::HashMap::new(),
        track_points: Vec::new(),
        surfaces: Vec::new(),
        surface_pin: None,
        name: None,
        colour_label: 0,
        grade_flag: Default::default(),
        precomp: None,
        baked_own: None,
    };
    let clip_id = clip.id;
    let rest = title_rest_time(&clip, fps_rate);
    if let Some(track) = state.sequences[state.active_sequence_idx].tracks.get_mut(ti) {
        track.clips.push(clip.clone());
        track.clips.sort_by_key(|c| c.timeline_in.value);
    }
    state.active_history_mut().push(crate::history::HistoryEntry {
        command: crate::history::EditCommand::AddTitleClip {
            clip,
            track_index: ti,
            created_track,
        },
    });
    state.dirty = true;
    // The new title is the selection, the inspector opens on
    // its text, and the playhead goes where the title rests so
    // the viewer shows it: past its entrance if it has one,
    // else its first frame. A title made at the playhead with
    // no entrance leaves the playhead where it was.
    state.selected_clip = Some(clip_id);
    state.selected_clips = vec![clip_id];
    state.selected_title_object = None;
    state.focus_title_text = 3;
    (clip_id, rest)
}

/// Where a title clip shows at rest: past its entrance when a layer has
/// one (a third of the clip, at most a second in), else its first frame.
/// `None` for a clip that is not a title.
pub(super) fn title_rest_time(clip: &klipp_timeline::Clip, fps_rate: u32) -> Option<klipp_core::RationalTime> {
    let td = clip.title_data()?;
    if !td.objects.iter().any(|o| o.fx().motion.enter.is_some()) {
        return Some(clip.timeline_in);
    }
    let settle = ((clip.timeline_out - clip.timeline_in).to_secs_f64() / 3.0).min(1.0);
    Some(klipp_core::RationalTime::from_secs_f64(clip.timeline_in.to_secs_f64() + settle, fps_rate))
}

/// One property of a title's style run, as `ApplyTitleStyle` names it,
/// set from its value as the inspector writes it: `color` a JSON RGBA,
/// `font_style` a JSON `FontStyle`, `font_name` the family itself, and
/// `font_size`, `tracking` and `baseline_offset` numbers (#1505);
/// `font_weight` a number, and `font_axes` a JSON list of axes, each
/// a tag and a value, set on the run's own axes tag by tag (#1504);
/// `features` a JSON list of OpenType feature settings ("smcp",
/// "-liga", "ss01=1"), each set over the run's own of its tag, and
/// `underline` "true" or "false" (#1710); `ruby` the reading gloss
/// itself, empty for none (#1704). A value that does not read leaves the
/// run as it was.
fn set_run_property(run: &mut klipp_timeline::StyleRun, prop: &str, val: &str) {
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
        "font_name" => {
            if !val.is_empty() {
                run.font_name = Some(val.to_string());
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
        "font_weight" => {
            if let Ok(w) = val.parse::<u16>() {
                run.font_weight = Some(w.clamp(1, 1000));
            }
        }
        "font_axes" => {
            if let Ok(axes) = serde_json::from_str::<Vec<klipp_timeline::FontAxis>>(val) {
                for (tag, value) in axes {
                    match run.font_axes.iter_mut().find(|(t, _)| *t == tag) {
                        Some(axis) => axis.1 = value,
                        None => run.font_axes.push((tag, value)),
                    }
                }
            }
        }
        "features" => {
            // A setting's tag, without its on/off sign, value or range.
            fn tag(s: &str) -> &str {
                let s = s.trim().trim_start_matches(['+', '-']);
                s.split(['=', '[']).next().unwrap_or(s).trim()
            }
            if let Ok(settings) = serde_json::from_str::<Vec<String>>(val) {
                for set in settings.into_iter().filter(|s| !tag(s).is_empty()) {
                    run.features.retain(|f| tag(f) != tag(&set));
                    run.features.push(set);
                }
            }
        }
        "underline" => {
            if let Ok(u) = val.parse::<bool>() {
                run.underline = Some(u);
            }
        }
        "ruby" => run.ruby = (!val.trim().is_empty()).then(|| val.to_string()),
        // Superscript or subscript, or neither (#1721).
        "script" => {
            run.script = match val {
                "superscript" => Some(klipp_timeline::Script::Superscript),
                "subscript" => Some(klipp_timeline::Script::Subscript),
                _ => None,
            };
        }
        _ => {}
    }
}

/// Where to put the playhead when a title is selected: its rest pose if
/// the playhead is outside the title, else nowhere (leave it).
pub(super) fn title_seek_on_select(
    clip: &klipp_timeline::Clip,
    playhead: klipp_core::RationalTime,
    fps_rate: u32,
) -> Option<klipp_core::RationalTime> {
    if playhead >= clip.timeline_in && playhead < clip.timeline_out {
        return None;
    }
    title_rest_time(clip, fps_rate)
}

/// The edits that give each title of `to` in `seq` the look of `look`,
/// each keeping its words (#1718): one per title it changes; clips that
/// are not titles, or on locked tracks, are left.
fn paste_title_style(seq: &klipp_timeline::Sequence, look: &klipp_timeline::TitleData, to: &[klipp_core::ClipId]) -> Vec<EditCommand> {
    seq.tracks
        .iter()
        .filter(|t| !t.locked)
        .flat_map(|t| t.clips.iter())
        .filter(|c| to.contains(&c.id))
        .filter_map(|c| {
            let old = c.title_data()?;
            let new = look.styled_like(old).filter(|n| n != old)?;
            Some(EditCommand::EditTitle { clip_id: c.id, description: "Paste Title Style", old_data: old.clone(), new_data: new })
        })
        .collect()
}

/// `tb` set on contour `contour` of the path layer `layer` (#1508), at
/// `offset` along it, `align` 0 start, 1 centre, 2 end, `flip` to the
/// other side; the offset's keys kept while it stays on that layer.
fn set_text_path_layer(tb: &mut klipp_timeline::TextBox, layer: klipp_timeline::TitleObjectId, contour: usize, offset: f32, align: u8, flip: bool) {
    let align = match align {
        1 => klipp_timeline::PathTextAlign::Center,
        2 => klipp_timeline::PathTextAlign::End,
        _ => klipp_timeline::PathTextAlign::Start,
    };
    let offset_keyframes = match &tb.text_path {
        Some(klipp_timeline::TextPath::Layer { layer: was, offset_keyframes, .. }) if *was == layer => offset_keyframes.clone(),
        _ => Vec::new(),
    };
    tb.text_path = Some(klipp_timeline::TextPath::Layer { layer, contour, offset: offset.clamp(0.0, 1.0), offset_keyframes, align, flip });
}

/// A path for `text`'s words to follow, added to `td` across where the
/// text is and a little wider, unpainted, named "Text path", and the
/// text set on it (#1508): its id.
fn draw_text_path(td: &mut klipp_timeline::TitleData, text: Option<klipp_timeline::TitleObjectId>) -> klipp_timeline::TitleObjectId {
    let at = text.and_then(|o| td.object(o)).map_or(klipp_timeline::TitlePosition { x: 0.5, y: 0.5 }, |o| o.transform.center);
    let xform = klipp_timeline::ObjectTransform { center: at, size: klipp_timeline::TitlePosition { x: 0.6, y: 0.3 }, ..klipp_timeline::default_text_transform() };
    let shape = klipp_timeline::Shape { kind: klipp_timeline::ShapeKind::Path(klipp_timeline::VectorPath::text_guide()), fit: None, fill: None, gradient: None, stroke: None };
    let id = td.add_object(klipp_timeline::TitleObjectContent::Shape(shape), xform);
    if let Some(o) = td.objects.iter_mut().find(|o| o.id == id) {
        o.name = "Text path".into();
    }
    if let Some(tb) = target_textbox_mut(td, text) {
        tb.text_path = None;
        set_text_path_layer(tb, id, 0, 0.0, 0, false);
    }
    id
}

#[cfg(test)]
mod paste_style_tests {
    use super::paste_title_style;
    use klipp_document::history::EditCommand;
    use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};

    /// #1718's done-when: one title's style pasted onto another, whose
    /// text stays and whose look matches, as one edit; a clip that is not
    /// a title is left.
    #[test]
    fn a_titles_style_pastes_onto_another_whose_words_stay() {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        let title = |text: &str, font: &str, size: f32, at: f64| {
            let mut c = Clip::gap(tc(at), tc(at + 1.0));
            c.kind = ClipKind::Title(TitleData::single_text(TextBox { text: text.into(), font_name: font.into(), font_size: size, color: [255, 204, 0, 255], ..Default::default() }));
            c
        };
        let look = title("Look", "Libre Baskerville", 72.0, 0.0);
        let target = title("Ana Diaz", "Inter", 40.0, 2.0);
        let not_a_title = Clip::gap(tc(4.0), tc(5.0));
        let mut seq = Sequence::new_default();
        let mut track = Track::new("Text 1", TrackKind::Text);
        track.clips = vec![look.clone(), target.clone(), not_a_title.clone()];
        seq.tracks = vec![track];
        let edits = paste_title_style(&seq, look.title_data().unwrap(), &[target.id, not_a_title.id]);
        let [EditCommand::EditTitle { clip_id, new_data, .. }] = &edits[..] else { panic!("one edit: {edits:?}") };
        assert_eq!(*clip_id, target.id);
        let tb = new_data.primary_text().unwrap();
        assert_eq!(tb.text, "Ana Diaz", "its words stay");
        let want = look.title_data().unwrap().primary_text().unwrap();
        assert_eq!((tb.font_name.as_str(), tb.font_size, tb.color), (want.font_name.as_str(), want.font_size, want.color), "its look matches");
        // Pasting it again changes nothing.
        let mut seq2 = seq.clone();
        seq2.tracks[0].clips[1].kind = ClipKind::Title(new_data.clone());
        assert!(paste_title_style(&seq2, look.title_data().unwrap(), &[target.id]).is_empty());
    }
}

#[cfg(test)]
mod text_path_tests {
    use super::{draw_text_path, set_text_path_layer};
    use klipp_timeline::{PathTextAlign, TextPath, TitleData, TitleObjectContent};

    /// #1508: drawing a path for a title's text adds an unpainted path
    /// layer across the text and sets the text on it; setting its start,
    /// align and side keeps the travel keyed on it, and moving the text
    /// to another layer drops it.
    #[test]
    fn a_drawn_text_path_is_added_bound_and_edited() {
        let mut td = TitleData::default();
        let text = td.objects[0].id;
        let path = draw_text_path(&mut td, Some(text));
        let layer = td.object(path).unwrap();
        assert_eq!(layer.name, "Text path");
        assert_eq!(layer.transform.center, td.object(text).unwrap().transform.center, "across where the text is");
        let TitleObjectContent::Shape(shape) = &layer.content else { panic!("a shape") };
        let klipp_timeline::ShapeKind::Path(p) = &shape.kind else { panic!("a path") };
        assert!(p.pose.fill[3] == 0.0 && p.pose.stroke[3] == 0.0, "it paints nothing of its own");
        let tb = td.primary_text().unwrap();
        assert!(matches!(&tb.text_path, Some(TextPath::Layer { layer, offset, align: PathTextAlign::Start, flip: false, .. }) if *layer == path && *offset == 0.0));

        let TitleObjectContent::TextBox(tb) = &mut td.objects[0].content else { panic!() };
        klipp_core::keyframe::upsert_keyframe(tb.effect_keyframes_mut_for(klipp_core::title_effect_keys::TEXT_PATH_OFFSET).unwrap(), 1.0, 0.5);
        set_text_path_layer(tb, path, 0, 0.25, 1, true);
        assert!(matches!(&tb.text_path, Some(TextPath::Layer { offset, align: PathTextAlign::Center, flip: true, offset_keyframes, .. }) if *offset == 0.25 && offset_keyframes.len() == 1), "the travel stays: {:?}", tb.text_path);
        set_text_path_layer(tb, klipp_timeline::TitleObjectId(77), 0, 2.0, 2, false);
        assert!(matches!(&tb.text_path, Some(TextPath::Layer { offset, offset_keyframes, align: PathTextAlign::End, .. }) if *offset == 1.0 && offset_keyframes.is_empty()), "another layer starts afresh, the offset kept on the path");
    }
}

#[cfg(test)]
mod title_rest_tests {
    use super::{title_rest_time, title_seek_on_select};
    use klipp_core::RationalTime;
    use klipp_core::title_motion::{Motion, TitleMotion};
    use klipp_timeline::{Clip, ClipKind, TitleData};

    fn t(secs: i64) -> RationalTime { RationalTime { value: secs * 30, rate: 30 } }

    fn title(from: i64, to: i64, entrance: bool) -> Clip {
        let mut td = TitleData::default();
        if entrance {
            td.objects[0].fx_mut().motion = TitleMotion::enter(Motion::default());
        }
        let mut c = Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from));
        c.kind = ClipKind::Title(td);
        c
    }

    #[test]
    fn a_plain_title_rests_on_its_first_frame() {
        assert_eq!(title_rest_time(&title(10, 15, false), 30), Some(t(10)));
    }

    #[test]
    fn a_title_with_an_entrance_rests_past_it() {
        assert_eq!(title_rest_time(&title(10, 15, true), 30), Some(t(11)));
    }

    #[test]
    fn selecting_a_title_off_the_playhead_brings_the_playhead_to_it() {
        let c = title(10, 15, false);
        assert_eq!(title_seek_on_select(&c, t(2), 30), Some(t(10)));
        assert_eq!(title_seek_on_select(&c, t(15), 30), Some(t(10)), "the out point is past the title");
        assert_eq!(title_seek_on_select(&c, t(12), 30), None, "a playhead on the title stays put");
        assert_eq!(title_seek_on_select(&c, t(10), 30), None);
    }

    #[test]
    fn a_clip_that_is_not_a_title_has_no_rest_pose() {
        let c = Clip::media(klipp_core::MediaId::new(), t(0), t(5), t(0), t(5));
        assert_eq!(title_rest_time(&c, 30), None);
    }
}

#[cfg(test)]
mod style_run_property_tests {
    use super::set_run_property;
    use klipp_timeline::{FontStyle, StyleRun};

    /// Every field the inspector offers for a selection reads from the
    /// value it writes (#1505); a value that does not read changes
    /// nothing.
    #[test]
    fn each_field_of_a_selection_reads_from_the_inspectors_value() {
        let mut run = StyleRun::default_for_range(0, 4);
        set_run_property(&mut run, "font_name", "Libre Baskerville");
        set_run_property(&mut run, "font_style", &serde_json::to_string(&FontStyle::BoldItalic).unwrap());
        set_run_property(&mut run, "tracking", "12.5");
        set_run_property(&mut run, "baseline_offset", "-8");
        set_run_property(&mut run, "font_size", "64");
        set_run_property(&mut run, "color", "[1,2,3,4]");
        set_run_property(&mut run, "font_weight", "650");
        set_run_property(&mut run, "font_axes", r#"[["wght",650.0],["wdth",80.0]]"#);
        set_run_property(&mut run, "font_axes", r#"[["wdth",90.0]]"#);
        assert_eq!(run.font_weight, Some(650));
        assert_eq!(run.font_axes, [("wght".to_string(), 650.0), ("wdth".to_string(), 90.0)], "an axis is set over the run's own, tag by tag");
        assert_eq!(run.font_name.as_deref(), Some("Libre Baskerville"));
        assert_eq!(run.font_style, Some(FontStyle::BoldItalic));
        assert_eq!((run.tracking, run.baseline_offset, run.font_size, run.color), (Some(12.5), Some(-8.0), Some(64.0), Some([1, 2, 3, 4])));
        let before = run.clone();
        for prop in ["font_name", "font_style", "tracking", "baseline_offset", "font_weight", "font_axes", "features", "underline"] {
            set_run_property(&mut run, prop, "");
        }
        assert_eq!(run, before);
    }

    /// #1704: a selection's ruby is its gloss, set as the inspector
    /// writes it; empty takes it away. Over a selection that crosses two
    /// runs each carries the one gloss (the renderer sets it once over
    /// both).
    #[test]
    fn a_selections_ruby_reads_from_the_inspectors_value() {
        let mut runs = vec![StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(0, 3) }];
        klipp_timeline::apply_style_to_range(&mut runs, 9, 0, 6, |r| set_run_property(r, "ruby", "かき"));
        let glossed: Vec<(usize, usize)> = runs.iter().filter(|r| r.ruby.as_deref() == Some("かき")).map(|r| (r.start, r.end)).collect();
        assert_eq!(glossed, [(0, 3), (3, 6)], "{runs:?}");
        klipp_timeline::apply_style_to_range(&mut runs, 9, 0, 6, |r| set_run_property(r, "ruby", " "));
        assert!(runs.iter().all(|r| r.ruby.is_none()), "{runs:?}");
        assert_eq!(runs[0].color, Some([255, 0, 0, 255]), "the rest of the style stays");
    }

    /// A selection's features are set over the run's own tag by tag,
    /// on, off or valued, and its underline from "true" or "false"
    /// (#1710).
    #[test]
    fn a_selections_features_and_underline_read_from_the_inspectors_value() {
        let mut run = StyleRun::default_for_range(0, 4);
        set_run_property(&mut run, "features", r#"["smcp","ss02"]"#);
        set_run_property(&mut run, "features", r#"["-smcp","ss02=0","tnum"]"#);
        assert_eq!(run.features, ["-smcp", "ss02=0", "tnum"]);
        set_run_property(&mut run, "features", r#"["+tnum[0:2]"]"#);
        assert_eq!(run.features, ["-smcp", "ss02=0", "+tnum[0:2]"]);
        set_run_property(&mut run, "underline", "true");
        assert_eq!(run.underline, Some(true));
        set_run_property(&mut run, "underline", "false");
        assert_eq!(run.underline, Some(false));
        let before = run.clone();
        set_run_property(&mut run, "features", "smcp");
        set_run_property(&mut run, "underline", "yes");
        assert_eq!(run, before, "a value that does not read changes nothing");
    }
}
