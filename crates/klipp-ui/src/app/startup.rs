//! Startup: the app built once, with its engine, channels, background workers,
//! preferences, project and every panel's first state (#826).

use super::*;

impl KlippApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd_tx: Sender<AppCommand>,
        event_rx: Receiver<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        source_frame_buffer: SharedFrameBuffer,
        sequence: Arc<klipp_timeline::SharedSequence>,
        media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
        skim_consumer: klipp_engine::audio_output::SkimConsumer,
        spectrum_buf: Option<Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>>,
        spectrum_sample_rate: u32,
        perf_ring: klipp_engine::perf::SharedPerfRing,
        param_change_queue: Arc<ArrayQueue<ParamChange>>,
        foreground_activity: Arc<klipp_jobs::ForegroundActivity>,
    ) -> Self {
        use klipp_core::startup_timer::LogTimer;

        // AI mask cache LRU sweep — trim
        // ~/.cache/klipp/ai_masks/ to AI_MASK_CACHE_BYTES_CAP if
        // it's overgrown. Cheap (stat-only walk + delete oldest)
        // and runs once at startup so it doesn't cost during
        // editing. See execute_apply_mask_to_clip for the schema
        // these files are written under.
        prune_ai_mask_cache();

        let (thumb_tx, thumb_rx) = crossbeam_channel::unbounded();
        let (source_frame_tx, source_frame_rx) = crossbeam_channel::unbounded();
        let (filmstrip_tx, filmstrip_rx) = crossbeam_channel::unbounded();
        let (relink_check_tx, relink_check_rx) = crossbeam_channel::unbounded();
        let (audio_conform_tx, audio_conform_rx) = crossbeam_channel::unbounded();
        let (proxy_event_tx, proxy_event_rx) = crossbeam_channel::unbounded();
        let (proxy_progress_tx, proxy_progress_rx) = crossbeam_channel::unbounded();
        let prefs = {
            let _t = LogTimer::new("ui: Preferences::load");
            crate::prefs::Preferences::load()
        };
        let theme_registry = {
            let _t = LogTimer::new("ui: ThemeRegistry::load");
            crate::theme::ThemeRegistry::load()
        };
        let theme = KlippTheme::from_prefs(&prefs.appearance, &theme_registry);
        {
            let _t = LogTimer::new("ui: theme + fonts apply (egui)");
            theme.apply_to_egui(&cc.egui_ctx);
            theme.apply_fonts_with_size(&cc.egui_ctx, prefs.appearance.ui_text_size);
            crate::icons::install(&cc.egui_ctx);
        }


        // Apply UI scale — Auto (0) detects system DPI via native pixels_per_point
        let scale_pct = prefs.appearance.ui_scale_percent;
        if scale_pct == 0 {
            // Auto: let egui use the system's native scale (zoom_factor = 1.0)
            // egui already reads the system DPI from Wayland/X11
            cc.egui_ctx.set_zoom_factor(1.0);
        } else {
            // Manual: override with user's chosen percentage
            // System already provides native_pixels_per_point, so zoom_factor
            // should be relative: e.g. 100% = 1.0, 125% = 1.25
            cc.egui_ctx.set_zoom_factor(scale_pct as f32 / 100.0);
        }
        let layout = LayoutState::load();
        // Panel visibility is persisted, so a panel collapsed last
        // session is still collapsed. AppState's defaults say visible;
        // the saved layout is what the user actually last chose.
        let (restore_browser, restore_inspector) = (layout.browser_visible, layout.inspector_visible);

        // Heavy field inits — extracted from the struct literal so we can
        // measure each one independently for STARTUP_PERF.
        let title_renderer_ui = {
            let _t = LogTimer::new("ui: TitleRenderer::new (font discovery, second instance)");
            klipp_title::TitleRenderer::new()
        };
        let key_bindings = {
            let _t = LogTimer::new("ui: KeyBindings::load");
            crate::keybindings::KeyBindings::load()
        };
        let cpal_sample_rate = {
            let _t = LogTimer::new("ui: cpal probe_sample_rate (duplicate)");
            klipp_engine::audio_output::probe_sample_rate()
        };
        // NOTE: Audio device cache is NOT primed at startup. An earlier
        // version spawned a background thread for this, but libasound
        // (ALSA) is not thread-safe for concurrent device enumeration.
        // Running enumerate_output_devices() simultaneously with the
        // engine's AudioOutput::new() corrupted cpal's device handles
        // and silently broke audio output. The cache is now populated
        // lazily on first visit to the Audio HW preferences tab — the
        // 5-50ms hitch is acceptable for a one-time UI interaction.

        // Capture the scheduler policy from prefs BEFORE moving
        // `prefs` into the struct literal below.
        let initial_scheduler_policy =
            prefs.playback.background_performance.to_scheduler_policy();

        let mut app = Self {
            cmd_tx: klipp_engine::commands::EngineSender::new(cmd_tx),
            event_rx,
            control_inbox: None,
            frame_buffer,
            source_frame_buffer,
            sequence,
            media_pool_shared,
            state: {
                let mut s = AppState::default();
                if let Some(buf) = spectrum_buf {
                    s.spectrum_ring = buf;
                }
                s.spectrum_sample_rate = spectrum_sample_rate.max(1);
                // Snap config persists in layout.json. Older layouts
                // carried only `snap_enabled: bool` and are migrated
                // to a full SnapConfig on read (see LayoutState::load).
                s.snap = layout.snap;
                s.dual_viewers = layout.dual_viewers;
                s.color_dual_viewers = layout.color_dual_viewers;
                s.export_dual_viewers = layout.export_dual_viewers;
                s
            },
            toolbar: ToolbarPanel { logo_texture: None },
            updates: Default::default(),
            unsaved_settled_at: None,
            viewer: ViewerPanel::default(),
            title_renderer: title_renderer_ui,
            markers: MarkersPanel::default(),
            edit_list: Default::default(),
            transcript_panel: crate::panels::transcript::TranscriptPanel::new(),
            transcript_source: Default::default(),
            colour_check: Default::default(),
            local_control: None,
            colour_tool_reply: None,
            delivery_check: Default::default(),
            delivery_reply: None,
            translation: None,
            sound_over_time: Default::default(),
            narrate: Default::default(),
            preferences_panel: PreferencesPanel::default(),
            transport: TransportBar::default(),
            timeline: {
                // Field assignment rather than a struct literal with
                // `..Default::default()`: TimelinePanel has private
                // fields, so the literal form is not available here.
                let mut tl = TimelinePanel::default();
                tl.embedded = true;
                tl
            },
            thumb_tx,
            thumb_rx,
            source_frame_tx,
            source_frame_rx,
            filmstrip_tx,
            filmstrip_cache_loads: std::collections::HashSet::new(),
            multicam_grid: None,
            multi_edit: None,
            ui_scale_applied: None,
            filmstrip_refining: Default::default(),
            filmstrip_rx,
            relink_check_tx,
            relink_check_rx,
            audio_conform_tx,
            audio_conform_rx,
            proxy_children: Vec::new(),
            skim_worker: {
                let tx = proxy_event_tx.clone();
                klipp_engine::skim::SkimWorker::spawn(3, move |job, result| {
                    let (media_id, target) = job.tag;
                    let event = match result {
                        Ok(f) => AppEvent::BrowserSkimFrameReady { media_id, rgba: f.rgba, width: f.width, height: f.height, target },
                        Err(e) => {
                            tracing::warn!("skim decode failed for {media_id:?} @ {:.3}s: {e}", job.secs);
                            // An empty frame resets the requester's in-flight flag.
                            AppEvent::BrowserSkimFrameReady { media_id, rgba: Vec::new(), width: 0, height: 0, target }
                        }
                    };
                    let _ = tx.send(event);
                })
            },
            proxy_event_tx,
            normalize_measuring: Default::default(),
            proxy_event_rx,
            export_dialog: ExportDialogState::default(),
            prefs,
            prefs_dirty: false,
            key_bindings,
            prefs_last_save: std::time::Instant::now(),
            proxy_progress_tx,
            proxy_progress_rx,
            last_autosave: std::time::Instant::now(),
            untitled_staging: None,
            unnamed_project: None,
            projects_root: None,
            project_copy: None,
            autosave_rx: None,
            last_heartbeat: std::time::Instant::now(),
            frame_count: 0,
            last_heartbeat_frame: 0,
            hb_input_events: 0,
            hb_max_batch: 0,
            last_graded_playhead: None,
            graded_positions: std::collections::HashMap::new(),
            browser_before_workspace: None,
            node_browser_borrow: Default::default(),
            nodes_were_on_right: false,
            last_workspace: crate::state::Workspace::default(),
            hb_max_gap: std::time::Duration::ZERO,
            unsaved_save_waiting: false,
            hb_max_update: std::time::Duration::ZERO,
            last_update_end: std::time::Instant::now(),
            last_frame_at: std::time::Instant::now(),
            last_scope_preview_refresh: std::time::Instant::now()
                - std::time::Duration::from_secs(1),
            last_sent_scope_mode: None,
            last_sent_active_scope_modes: None,
            last_sent_scope_sizes: None,
            last_sent_scope_settings: None,
            last_sent_scope_bloom: None,
            last_sent_scope_low_pass: None,
            last_sent_scope_lut: None,
            last_sent_scopes_visible: None,
            theme,
            theme_registry,
            layout,
            layout_dirty: false,
            browser: BrowserPanel::default(),
            new_inspector: NewInspectorPanel::default(),
            key_column: Default::default(),
            new_timeline: crate::timeline::NewTimelinePanel::default(),
            skim_consumer,
            cpal_sample_rate,
            last_window_title: String::new(),
            launch_screen: crate::panels::launch_screen::LaunchScreen::default(),
            new_project_dialog: crate::panels::new_project_dialog::NewProjectDialog::new(None),
            startup_open: None,
            last_open_error: None,
            relink_panel: Default::default(),
            lock_holder: klipp_project::lock::Holder::current(),
            lock_keeper: None,
            lock_seen_path: None,
            read_only_project: None,
            lock_prompt: None,
            lock_decision: None,
            sequence_settings_dialog: None,
            framerate_mismatch_dialog: None,
            suppress_fps_prompt: false,
            node_graph_panel: crate::panels::node_graph::NodeGraphPanel::default(),
            node_graph_v2_panel: crate::panels::node_graph_v2::NodeGraphV2Panel::default(),
            node_inspector_panel: crate::panels::node_inspector::NodeInspectorPanel::default(),
            looks_panel_state: crate::panels::looks::LooksPanelState::default(),
            grade_library_panel_state: Default::default(),
            node_param_edit_snapshot: None,
            graph_before_input: None,
            timeline_grade_fresh: None,
            last_gesture_sync: std::time::Instant::now(),
            scope_executor: klipp_nodes::GraphExecutor::new_cpu_only(),
            media_watcher: None,
            system_theme_watcher: None,
            system_theme_changed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            desktop_appearance: crate::theme::portal::start(cc.egui_ctx.clone()),
            curve_editor: crate::panels::curve_editor::CurveEditorPanel::new(),
            scope_window_runtime: crate::widgets::scope_window::ScopeWindowRuntime::default(),
            scope_dock_runtime: crate::widgets::scope_window::ScopeWindowRuntime::default(),
            perf_ring,
            param_change_queue,
            // Engine writes is_playing/is_scrubbing to this Arc; the
            // scheduler below reads from the same Arc to know when
            // the user is busy. Both ends share state without locks.
            foreground_activity: Arc::clone(&foreground_activity),
            job_scheduler: klipp_jobs::JobScheduler::new(
                foreground_activity,
                initial_scheduler_policy,
            ),
        };
        // Analysis waits while the person is playing, scrubbing or has
        // just touched the controls, unless they chose throughput over
        // responsiveness for background work.
        {
            let fg = Arc::clone(&app.foreground_activity);
            let sched = Arc::clone(&app.job_scheduler);
            klipp_analysis::hold::install(Arc::new(move || {
                sched.policy() != klipp_jobs::SchedulerPolicy::MaxThroughput
                    && fg.is_user_busy(std::time::Duration::from_millis(1500))
            }));
        }
        // Build false color LUT from loaded preferences
        // A panel collapsed last session stays collapsed. Assigned
        // directly rather than through the setters: this is adopting
        // what was already saved, so there is nothing new to mark dirty.
        app.state.show_browser_panel = restore_browser;
        app.last_workspace = app.state.active_workspace;
        app.state.show_inspector_panel = restore_inspector;
        app.state.false_color_lut = app.prefs.viewer.false_color_palette.build_lut();
        // Probe audio backend status
        app.state.audio_sample_rate = app.cpal_sample_rate;
        app.state.audio_backend_ok = {
            let _t = LogTimer::new("ui: cpal probe_audio_available (duplicate)");
            klipp_engine::audio_output::probe_audio_available()
        };

        // Untitled projects' staged media a crash left behind, a day on.
        crate::project_paths::cleanup_untitled_cache(&crate::project_paths::untitled_cache_dir(), &app.lock_holder, std::time::SystemTime::now(), std::time::Duration::from_secs(24 * 3600));

        // Load saved workspaces
        let ws_path = dirs::config_dir().unwrap_or_default().join("klipp/workspaces.json");
        if let Ok(data) = std::fs::read_to_string(&ws_path) {
            if let Ok(workspaces) = serde_json::from_str::<Vec<crate::state::SavedWorkspaceLayout>>(&data) {
                app.state.custom_workspaces = workspaces;
            }
        }

        // What to open is only noted here and opened on the first frames,
        // through the UI's own dispatcher: OpenProject and ImportMedia are
        // UI commands, and sent to the engine they were dropped (#395).
        // Paths from the command line take the place of the startup
        // behaviour.
        let given = crate::cli::startup_paths();
        if !given.is_empty() {
            app.startup_open = Some(crate::cli::StartupOpen::plan(given));
        } else {
        // Apply startup behavior
        match app.prefs.general.startup_behavior {
            crate::prefs::StartupBehavior::ProjectManager => {
                // Default: project_path is None, launch screen will show
            }
            crate::prefs::StartupBehavior::EmptyProject => {
                // Skip launch screen, go straight to empty project
                app.state.project_path = Some(std::path::PathBuf::from("Untitled"));
            }
            crate::prefs::StartupBehavior::LastProject => {
                // Reopen the most recent project if available
                if let Some(recent) = app.prefs.general.recent_projects.first() {
                    let path = recent.path.clone();
                    if path.exists() {
                        app.startup_open = Some(crate::cli::StartupOpen::project(path));
                    }
                }
            }
        }
        }

        // Sync preferences to state
        app.state.remember_inspector_layout = app.prefs.general.remember_inspector_layout;
        if app.state.remember_inspector_layout {
            app.state.collapsed_sections = app.prefs.general.collapsed_sections.iter().cloned().collect();
            // Seed egui's persistent data with collapsed sections
            let collapsed_key = egui::Id::new("klipp_collapsed_sections");
            cc.egui_ctx.data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                for name in &app.prefs.general.collapsed_sections {
                    set.insert(name.clone());
                }
            });
            // Seed Title-inspector expanded groups (separate set, opt-in
            // per group name; default empty = all collapsed on first run).
            let title_key = egui::Id::new(crate::inspector::TITLE_EXPANDED_GROUPS_KEY);
            cc.egui_ctx.data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(title_key);
                for name in &app.prefs.title.inspector_expanded_groups {
                    set.insert(name.clone());
                }
            });
        }
        app.state.audio_skimming_enabled = app.prefs.playback.skim_audio;
        // The node editor's recent picks from the last session (#1745).
        app.node_graph_v2_panel.restore_recents(&app.prefs.color.node_add_recents);
        app.state.file_browser_video_skim_enabled = app.prefs.file_browser.file_browser_video_skim;
        app.state.file_browser_audio_skim_enabled = app.prefs.file_browser.file_browser_audio_skim;
        app.state.show_recent_files = app.prefs.file_browser.show_recent_files;
        app.state.mute_reminder_interval_secs = app.prefs.audio_hardware.mute_reminder_interval_secs;
        app.state.filmstrip_hover_enabled = app.prefs.file_browser.filmstrip_hover;
        app.state.filmstrip_quality = app.prefs.file_browser.filmstrip_quality;
        app.state.filmstrip_persist = app.prefs.file_browser.filmstrip_persist;
        app.state.file_browser_auto_refresh = app.prefs.file_browser.auto_refresh;
        app.state.recent_files_snapshot = app.prefs.file_browser.recent_files.clone();
        app.state.loop_playback_enabled = app.prefs.playback.loop_playback;
        // Playhead follow: if pref is off, start with follow disabled
        if !app.prefs.editing.playhead_follows_playback {
            app.state.playhead_follow = crate::state::PlayheadFollow::Off;
        }
        app.state.magnetic_timeline_enabled = app.prefs.editing.magnetic_timeline;
        app.state.multicam_show_angles = app.prefs.editing.multicam_show_angles;
        app.state.auto_create_tracks = app.prefs.editing.auto_create_tracks;
        app.state.default_transition_frames = app.prefs.editing.default_transition_frames;
        app.state.shift_step_frames = app.prefs.editing.shift_step_frames;
        app.state.snap_threshold_px = app.prefs.editing.snap_threshold_px;
        app.state.grid_snap_interval = app.prefs.editing.grid_snap_interval;
        app.state.zoom_step = app.prefs.editing.zoom_step;
        app.state.default_sequence_resolution_idx = app.prefs.editing.default_sequence_resolution_idx;
        app.state.timeline_prefs = app.prefs.timeline.clone();
        app.state.viewer_prefs = app.prefs.viewer.clone();
        app.state.toast_duration_secs = app.prefs.advanced.toast_duration_secs;
        app.state.toast.visible_duration = app.prefs.advanced.toast_duration_secs;
        // Carry the AI preferences into the analysis pipeline at startup,
        // so saved opt-outs and the CPU choice take effect before any import.
        klipp_analysis::set_transcription_enabled(app.prefs.ai.transcription);
        klipp_analysis::set_subject_detection_enabled(app.prefs.ai.subject_detection);
        klipp_analysis::set_screen_text_enabled(app.prefs.ai.screen_text);
        klipp_analysis::transcript::set_transcribe_on_cpu(app.prefs.ai.transcribe_on_cpu);
        // Unpack the Voice Isolation model off the audio path, if the
        // user has downloaded it.
        if klipp_audio::dsp::voice_isolation::model_installed() {
            let _ = std::thread::Builder::new()
                .name("klipp-voice-isolation-load".into())
                .spawn(klipp_audio::dsp::voice_isolation::prewarm);
        }
        let _ = app.cmd_tx.send(AppCommand::SetPanLaw(app.prefs.audio.pan_law));
        klipp_engine::pcm_reader::set_anti_click_fade_ms(app.prefs.advanced.anti_click_fade_ms);
        klipp_engine::video_decoder::set_hardware_decode(app.prefs.system.hardware_decode);
        klipp_engine::video_decoder::set_max_playback_height(app.prefs.system.max_playback_height);
        klipp_engine::video_decoder::set_frame_queue_size(app.prefs.system.frame_queue_size);
        klipp_engine::video_decoder::set_decode_threads(app.prefs.system.decode_threads);
        klipp_engine::video_decoder::set_optimised_playback_mode(
            app.prefs.playback.optimised_playback.to_engine_mode(),
        );
        klipp_engine::video_decoder::set_playback_quality_divisor(crate::prefs::playback_quality_divisor(
            app.prefs.playback.playback_quality,
        ));
        klipp_engine::audio_output::set_preferred_device(app.prefs.audio_hardware.output_device.clone());
        // TODO: background_render pref — when true, pre-render complex timeline
        // sections in a background thread. No background render system exists yet;
        // logging the pref value here so it's exercised at startup.
        if app.prefs.playback.background_render {
            tracing::debug!("Background render enabled (not yet implemented)");
        }
        // Sync LUT interpolation mode to engine
        let _ = app.cmd_tx.send(AppCommand::SetLutTetrahedral(
            app.prefs.color.lut_interpolation == crate::prefs::LutInterpolation::Tetrahedral,
        ));
        let _ = app.cmd_tx.send(AppCommand::SetSdrTransfer(app.prefs.color.sdr_display_transfer));
        // Map GpuPreference to wgpu PowerPreference (klipp-effects can't depend on klipp-core)
        let gpu_pref = match app.prefs.system.gpu_preference {
            klipp_core::GpuPreference::Auto => Some(wgpu::PowerPreference::default()),
            klipp_core::GpuPreference::HighPerformance => Some(wgpu::PowerPreference::HighPerformance),
            klipp_core::GpuPreference::LowPower => Some(wgpu::PowerPreference::LowPower),
            klipp_core::GpuPreference::CpuOnly => None, // don't init GPU at all
        };
        klipp_effects::gpu::set_gpu_power_preference(gpu_pref);
        klipp_media::disk_cache::set_cache_dir_overrides(
            app.prefs.media_cache.filmstrip_cache_dir.clone(),
            app.prefs.media_cache.audio_cache_dir.clone(),
        );
        app.state.modal_backdrop_alpha = app.prefs.advanced.modal_backdrop_alpha;
        app.state.scope_quality = app.prefs.color.scope_quality;
        let _ = app.cmd_tx.send(AppCommand::SetRenderCacheEnabled(app.prefs.media_cache.enable_render_cache));
        let _ = app.cmd_tx.send(AppCommand::SetRenderCacheLimitGb(app.prefs.media_cache.max_cache_size_gb));
        app.state.scope_update_interval_ms = app.prefs.color.scope_update_interval_ms;
        app.state.set_history_max_size(app.prefs.editing.undo_limit);
        app.state.audio_fade_overlay_mode = app.prefs.audio.fade_overlay_mode;
        app.state.audio_show_rubber_band = app.prefs.audio.show_rubber_band;
        app.state.audio_show_waveform = app.prefs.audio.show_waveform;
        app.state.audio_anti_click_fade = app.prefs.audio.anti_click_fade;
        app.state.default_fade_secs = app.prefs.audio.default_fade_duration_ms as f32 / 1000.0;
        app.state.default_fade_curve = app.prefs.audio.default_fade_curve;
        app.state.jkl_speed_step = f64::from(app.prefs.playback.jkl_speed_step);
        app.state.track_search_radius = app.prefs.editing.track_search_radius;
        app.state.multicam_default_sync = app.prefs.editing.multicam_sync_method.sync();
        app.state.audio_show_crossfade_indicators = app.prefs.audio.show_crossfade_indicators;
        app.state.lufs_target = match app.prefs.audio.loudness_standard {
            klipp_core::LoudnessStandard::EbuR128 => -23.0,
            klipp_core::LoudnessStandard::AtscA85 => -24.0,
            klipp_core::LoudnessStandard::Off => -23.0,
        };
        app.state.peak_hold_time_secs = app.prefs.audio.peak_hold_time_secs;
        app.browser.browser_view_mode = app.prefs.editing.media_browser_view_mode;
        app.browser.file_thumb_size = app.prefs.file_browser.file_thumb_size;
        app.browser.files_sort_by = app.prefs.file_browser.file_sort_by;
        app.browser.list_columns = app.prefs.file_browser.media_list_columns.clone();
        app.browser.files_sort_ascending = app.prefs.file_browser.file_sort_ascending;
        app.browser.group_by_date = app.prefs.file_browser.group_by_date;
        app.browser.visible_columns = app.prefs.file_browser.visible_columns.clone();
        app.browser.files_type_filter = app.prefs.file_browser.file_type_filter;
        app.browser.pinned_folders = app.prefs.file_browser.pinned_folders.clone();
        app.browser.favorite_effects = app.prefs.effects_browser.favorite_effects.clone();
        app.browser.favorite_transitions = app.prefs.effects_browser.favorite_transitions.clone();
        app.browser.favorite_titles = app.prefs.effects_browser.favorite_titles.clone();
        app.browser.favorite_generators = app.prefs.effects_browser.favorite_generators.clone();
        // Sync loop state to engine (engine defaults to loop_enabled=false)
        if app.state.loop_playback_enabled {
            let _ = app.cmd_tx.send(AppCommand::SetLoopPlayback(true));
        }
        // Sync anti-click fade to engine
        let _ = app.cmd_tx.send(AppCommand::SetAntiClickFade(app.state.audio_anti_click_fade));
        // Sync scrub-audio preference to engine (default true; dispatch
        // unconditionally so the engine reflects the user's saved choice).
        let _ = app.cmd_tx.send(AppCommand::SetScrubAudioEnabled(app.prefs.playback.scrub_audio));

        // Discover system fonts for title clip font selector
        let font_registry = {
            let _t = LogTimer::new("ui: discover_system_fonts (third instance)");
            klipp_title::font_discovery::discover_system_fonts()
        };
        app.state.available_fonts = font_registry.keys().cloned().collect();
        app.state.favourite_fonts = app.prefs.title.favourite_fonts.clone();
        app.state.recent_fonts = app.prefs.title.recent_fonts.clone();
        tracing::info!("UI font discovery: {} families available", app.state.available_fonts.len());

        // Enrich any stale recent project entries with metadata from disk
        let mut prefs_changed = {
            let _t = LogTimer::new("ui: enrich_recent_projects");
            app.prefs.enrich_recent_projects()
        };

        // Generate poster frame thumbnails for recent projects that don't have one
        {
            let _t = LogTimer::new("ui: poster frame generation (recent projects)");
            let cache_dir = dirs::cache_dir().map(|d| d.join("klipp").join("thumbnails"));
            if let Some(ref cache_dir) = cache_dir {
                let _ = std::fs::create_dir_all(cache_dir);
                for rp in &mut app.prefs.general.recent_projects {
                    // Skip if already has a valid thumbnail
                    if rp.thumbnail_path.as_ref().is_some_and(|p| p.exists()) {
                        continue;
                    }
                    if !rp.path.exists() {
                        continue;
                    }
                    // Try to find the first media file from the project's manifest
                    let manifest_path = rp.path.join("media").join("manifest.json");
                    let first_media = std::fs::read_to_string(&manifest_path).ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .and_then(|v| v.get("items")?.as_array()?.first()?.get("path")?.as_str().map(std::path::PathBuf::from));

                    if let Some(media_path) = first_media {
                        if media_path.exists() {
                            // Extract a non-black frame and save as PNG
                            let est_duration = rp.duration_secs.unwrap_or(60.0);
                            if let Ok((rgba, w, h)) = klipp_engine::generate_thumbnail_at_size_non_black(&media_path, 1.0, est_duration, 320) {
                                let stem = rp.path.file_stem().unwrap_or_default().to_string_lossy();
                                let out_path = cache_dir.join(format!("{stem}.png"));
                                if save_rgba_as_png(&rgba, w, h, &out_path) {
                                    rp.thumbnail_path = Some(out_path);
                                    prefs_changed = true;
                                    tracing::info!("Generated poster frame for {:?}", rp.path);
                                }
                            }
                        }
                    }
                }
            }
        }

        if prefs_changed {
            let _ = app.prefs.save();
        }

        // U-5: use the cached plugin_infos instead of spinning up a
        // throwaway OfxHost just to extract metadata. The engine
        // creates its own host for live invocation; this path only
        // needs the read-only list for the effects registry + UI.
        {
            let _t = LogTimer::new("ui: OfxHost plugin_infos (cached)");
            app.state.available_plugins = klipp_plugin::plugin_infos_cached();
            app.state.effect_registry.merge_ofx_plugins(&app.state.available_plugins);
            tracing::info!(
                "OFX: {} cached plugin(s), {} total effects in registry",
                app.state.available_plugins.len(),
                app.state.effect_registry.effects.len()
            );
        }

        // Discover LADSPA plugins at startup. Uses the on-disk cache when
        // available — cache hit is ~1–5 ms, cold scan is 50–500 ms depending
        // on plugin count. Cache invalidates automatically when any LADSPA
        // search directory's mtime changes (i.e. a plugin was installed,
        // removed, or renamed).
        //
        // Load the user's disabled-plugins set first, then filter it out
        // before merging into the effect registry — disabled plugins
        // shouldn't appear in the browser or "+ Add Effect" picker.
        {
            let _t = LogTimer::new("ui: LADSPA discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.disabled_audio_plugins = crate::audio_plugins_disabled::load();
            app.state.ladspa_plugins =
                klipp_audio::ladspa::discover_ladspa_plugins_cached(cache_dir.as_deref());
            let enabled: Vec<klipp_audio::ladspa::LadspaPluginInfo> = app
                .state
                .ladspa_plugins
                .iter()
                .filter(|p| !app.state.disabled_audio_plugins.contains(&p.unique_id))
                .cloned()
                .collect();
            app.state.effect_registry.merge_ladspa_plugins(&enabled);
            tracing::info!(
                "LADSPA: discovered {} plugin(s) ({} enabled, {} disabled)",
                app.state.ladspa_plugins.len(),
                enabled.len(),
                app.state.disabled_audio_plugins.len()
            );
        }
        // frei0r 1.x video-plugin discovery — same disk-cached pattern
        // as LADSPA. Populates the runtime cache in
        // `klipp_effects::frei0r_effect` so any saved-project clip
        // referencing a `BuiltinEffect::Frei0r { type_id, .. }`
        // resolves on first render.
        {
            let _t = LogTimer::new("ui: frei0r discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.frei0r_plugins =
                klipp_frei0r::discover_frei0r_plugins_cached(cache_dir.as_deref());
            klipp_effects::frei0r_effect::set_plugin_registry(
                app.state.frei0r_plugins.clone(),
            );
            app.state
                .effect_registry
                .merge_frei0r_plugins(&app.state.frei0r_plugins);
            tracing::info!(
                "frei0r: discovered {} plugin(s)",
                app.state.frei0r_plugins.len()
            );
        }
        // LV2 audio plugin discovery — disk-cached (livi reads every
        // bundle's manifest, expensive cold). Populates the browser
        // registry so the Audio FX tab shows the LV2 source pill;
        // create_effect("lv2:{uri}", sr) resolves through the
        // klipp_audio::lv2 World singleton.
        {
            let _t = LogTimer::new("ui: LV2 discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.lv2_plugins =
                klipp_audio::lv2::discover_lv2_plugins_cached(cache_dir.as_deref());
            klipp_audio::lv2::set_plugin_registry(app.state.lv2_plugins.clone());
            app.state
                .effect_registry
                .merge_lv2_plugins(&app.state.lv2_plugins);
            tracing::info!(
                "LV2: discovered {} plugin(s)",
                app.state.lv2_plugins.len()
            );
        }
        // CLAP audio-plugin discovery — disk-cached. Populates the
        // browser registry so the Audio FX tab shows the CLAP source
        // pill; create_effect("clap:{id}", sr) resolves through the
        // klipp_audio::clap process-global registry. v1 stub runtime
        // — chain loads, params save/restore via ParamID, but
        // process() passes audio through unchanged until
        // CLAP-NEXT-RUNTIME-WIRING ships.
        {
            let _t = LogTimer::new("ui: CLAP discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.clap_plugins =
                klipp_audio::clap::discover_clap_plugins_cached(cache_dir.as_deref());
            klipp_audio::clap::set_plugin_registry(app.state.clap_plugins.clone());
            app.state
                .effect_registry
                .merge_clap_plugins(&app.state.clap_plugins);
            tracing::info!(
                "CLAP: discovered {} plugin(s)",
                app.state.clap_plugins.len()
            );
        }
        // Klipp closed without saving last time (#2789): say where the
        // work it kept is, once, as the projects page badges it.
        let recent: Vec<std::path::PathBuf> = app.prefs.general.recent_projects.iter().take(12).map(|r| r.path.clone()).collect();
        if let Some(note) = crash_notice(&recent) {
            app.state.toast.show(note);
        }
        klipp_core::startup_timer::mark("KlippApp::new returning");
        app
    }
}

impl KlippApp {
    /// Take the next steps of [`Self::startup_open`]: open its project
    /// through the dispatcher, then import its media once that project is
    /// the one open, saying what could not be done.
    pub(super) fn run_startup_open(&mut self) {
        use crate::cli::Step;
        let Some(open) = self.startup_open.as_mut() else { return };
        let failed = self.last_open_error.take();
        let steps = open.step(self.state.project_path.as_deref(), failed.as_deref());
        if open.done() {
            self.startup_open = None;
        }
        for step in steps {
            match step {
                Step::Open(path) => self.send_commands(vec![AppCommand::OpenProject { path }]),
                Step::NewProject => self.state.project_path = Some(PathBuf::from("Untitled")),
                Step::Import(path) => self.send_commands(vec![AppCommand::ImportMedia { path }]),
                Step::Toast(text) => self.state.toast.show(text),
            }
        }
    }
}
