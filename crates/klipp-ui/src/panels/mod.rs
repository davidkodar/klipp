pub mod ai_models;
pub mod audio_mixer;
pub mod audio_plugins;
pub mod project_doctor;
pub mod system_probe;
pub mod batch_rename_dialog;
pub mod cache_manager;
pub mod clip_properties;
pub mod curve_editor;
pub mod preset_manager;
// `panels::inspector` was a legacy single-file module that exposed
// `show_color_and_luts`. It has been migrated into
// `crate::inspector::color_and_luts` to live alongside the rest of
// the inspector. See CODE_AUDIT R-DEL-INSPECTOR.
pub mod looks;
pub mod node_graph;
pub mod node_graph_v2;
pub mod node_inspector;
pub mod launch_screen;
pub mod markers;
pub mod new_project_dialog;
pub mod project_info;
pub mod project_notes;
pub mod colour_check;
pub mod edit_plan;
pub mod file_info;
pub mod framerate_mismatch_dialog;
pub mod preferences;
pub mod render_queue;
pub mod sequence_settings_dialog;
pub mod sound_over_time;
pub mod status_bar;
pub mod subtitle_editor;
pub mod timeline;
pub mod timeline_search;
pub mod toolbar;
pub mod transport;
pub mod viewer;
pub mod workspace_manager;
