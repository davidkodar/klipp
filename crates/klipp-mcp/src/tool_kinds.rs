//! What each tool is to the server, in one place: every tool Klipp
//! advertises has its row, so a new one can't be left out of a list and
//! run as a read. A read runs beside others; a write is queued, in order,
//! with an idempotency key; one that changes the picture also makes the
//! next screenshot wait for a frame that shows it.

/// How the server runs a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Reads only: run beside other reads, never replayed.
    Read,
    /// Changes the project or starts work: queued in order, keyed so a
    /// retry after a lost reply is done once.
    Queued,
    /// Queued, and changes what the viewer shows: the next screenshot
    /// waits for a frame that shows it.
    Shows,
}

use Kind::{Queued, Read, Shows};

/// Every advertised tool and its kind.
pub const TOOLS: &[(&str, Kind)] = &[
    ("klipp_add_marker", Queued),
    ("klipp_add_music", Queued),
    ("klipp_add_node", Shows),
    ("klipp_adopt_look", Queued),
    ("klipp_align_audio", Queued),
    ("klipp_alternatives", Shows),
    ("klipp_analysis_queue", Queued),
    ("klipp_analysis_status", Read),
    ("klipp_analyze", Queued),
    ("klipp_append_clip", Shows),
    ("klipp_apply", Shows),
    ("klipp_approve_edit_plan", Queued),
    ("klipp_attach_audio", Shows),
    ("klipp_audio_excerpt", Read),
    ("klipp_audio_plugins", Read),
    ("klipp_audio_spectrum", Read),
    ("klipp_audition", Read),
    ("klipp_auto_redact", Queued),
    ("klipp_auto_sfx", Queued),
    ("klipp_batch", Shows),
    ("klipp_beats", Read),
    ("klipp_beat_sheet", Read),
    ("klipp_bounce_track", Shows),
    ("klipp_brand_kit", Read),
    ("klipp_brief", Read),
    ("klipp_build_edit_plan", Queued),
    ("klipp_cancel_job", Queued),
    ("klipp_check", Read),
    ("klipp_check_delivery", Read),
    ("klipp_clear_frozen", Shows),
    ("klipp_close_project", Shows),
    ("klipp_collect_project", Queued),
    ("klipp_colour_selfcheck", Read),
    ("klipp_connect_nodes", Shows),
    ("klipp_contact_sheet", Read),
    ("klipp_covers", Read),
    ("klipp_create_multicam", Queued),
    ("klipp_cut_points", Read),
    ("klipp_delivery_specs", Read),
    ("klipp_detect_scene_cuts", Shows),
    ("klipp_duplicate_sequence", Shows),
    ("klipp_edit_plan", Read),
    ("klipp_export", Shows),
    ("klipp_export_status", Read),
    ("klipp_export_subtitles", Shows),
    ("klipp_export_timeline", Read),
    ("klipp_find_moments", Read),
    ("klipp_find_shots", Read),
    ("klipp_follow_job", Read),
    ("klipp_fonts", Read),
    ("klipp_frame_at", Read),
    ("klipp_frame_stats", Read),
    ("klipp_freesound_sign_in", Read),
    ("klipp_generator", Shows),
    ("klipp_get_grade", Read),
    ("klipp_grade_chain", Queued),
    ("klipp_grade_to_target", Queued),
    ("klipp_graph", Shows),
    ("klipp_graph_read", Read),
    ("klipp_guide", Read),
    ("klipp_import_media", Queued),
    ("klipp_import_subtitles", Shows),
    ("klipp_import_timeline", Shows),
    ("klipp_job_status", Read),
    ("klipp_list_clips", Read),
    ("klipp_list_markers", Read),
    ("klipp_list_media", Read),
    ("klipp_log_tail", Read),
    ("klipp_look", Queued),
    ("klipp_looks", Read),
    ("klipp_make_proxies", Queued),
    ("klipp_make_shorts", Queued),
    ("klipp_match_grade", Queued),
    ("klipp_match_scenes", Queued),
    ("klipp_measure_region", Read),
    ("klipp_measure_water", Queued),
    ("klipp_media_analysis", Read),
    ("klipp_mix_loudness", Read),
    ("klipp_narrate", Queued),
    ("klipp_new_project", Queued),
    ("klipp_new_sequence", Queued),
    ("klipp_node_types", Read),
    ("klipp_normalize", Shows),
    ("klipp_open_project", Shows),
    ("klipp_operation_status", Read),
    ("klipp_orient", Queued),
    ("klipp_pace", Read),
    ("klipp_pause", Shows),
    ("klipp_perf", Read),
    ("klipp_pick_grey", Shows),
    ("klipp_plan", Read),
    ("klipp_play", Shows),
    ("klipp_prepare_cut", Read),
    ("klipp_project_templates", Read),
    ("klipp_publish_kit", Read),
    ("klipp_read_chart", Shows),
    ("klipp_rebuild_version", Shows),
    ("klipp_recent_projects", Read),
    ("klipp_record", Queued),
    ("klipp_redo", Shows),
    ("klipp_relink_media", Queued),
    ("klipp_remove_clip", Shows),
    ("klipp_remove_marker", Queued),
    ("klipp_remove_node", Shows),
    ("klipp_remove_sequence", Shows),
    ("klipp_rename_node", Queued),
    ("klipp_rename_sequence", Queued),
    ("klipp_render_status", Read),
    ("klipp_reuse_report", Read),
    ("klipp_review", Queued),
    ("klipp_review_finding", Queued),
    ("klipp_save_project", Queued),
    ("klipp_scope", Read),
    ("klipp_screenshot", Read),
    ("klipp_screen_text", Read),
    ("klipp_search_music", Read),
    ("klipp_search_screen_text", Read),
    ("klipp_search_sfx", Read),
    ("klipp_search_transcripts", Read),
    ("klipp_search_visual", Read),
    ("klipp_seek", Shows),
    ("klipp_select_clip", Shows),
    ("klipp_selects", Read),
    ("klipp_set_append_audio", Shows),
    ("klipp_set_background_render", Shows),
    ("klipp_set_brief", Queued),
    ("klipp_set_edit_plan", Queued),
    ("klipp_set_log_format", Shows),
    ("klipp_set_multicam_sound", Queued),
    ("klipp_set_node_param", Shows),
    ("klipp_set_playback_quality", Shows),
    ("klipp_show_matte", Shows),
    ("klipp_skin_balance", Shows),
    ("klipp_spelling", Read),
    ("klipp_status", Read),
    ("klipp_stop", Shows),
    ("klipp_style_profile", Read),
    ("klipp_surface_key", Shows),
    ("klipp_switch_angle", Queued),
    ("klipp_switch_sequence", Shows),
    ("klipp_tag_media", Queued),
    ("klipp_timeline", Read),
    ("klipp_timeline_sheet", Read),
    ("klipp_title_templates", Queued),
    ("klipp_toggle_node", Shows),
    ("klipp_track_mask", Shows),
    ("klipp_track_subject", Shows),
    ("klipp_track_surface", Shows),
    ("klipp_transcript", Read),
    ("klipp_translate_captions", Shows),
    ("klipp_undo", Shows),
    ("klipp_update_marker", Queued),
    ("klipp_veylvik_composition", Shows),
    ("klipp_window_screenshot", Read),
];

/// `name`'s kind; a name not advertised is a read (it is refused first).
pub fn kind(name: &str) -> Kind {
    TOOLS.iter().find(|(n, _)| *n == name).map_or(Read, |(_, k)| *k)
}

/// Whether `name` is queued as a write.
pub fn queued(name: &str) -> bool {
    kind(name) != Read
}

/// Whether `name` changes what the viewer shows.
pub fn shows(name: &str) -> bool {
    kind(name) == Shows
}

/// Whether `name` gives the session another project (new, opened, saved
/// as, closed): the change it then reports is its own.
pub fn moves_the_project(name: &str) -> bool {
    matches!(name, "klipp_new_project" | "klipp_open_project" | "klipp_close_project" | "klipp_save_project")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each tool the server advertises has one row and each row names a
    /// tool: a new tool fails here until it says whether it writes.
    #[test]
    fn every_advertised_tool_says_what_it_is() {
        let advertised = crate::diagnostics::tool_names();
        for name in advertised.iter() {
            let rows = TOOLS.iter().filter(|(n, _)| *n == name.as_str()).count();
            assert_eq!(rows, 1, "{name}: {rows} rows in tool_kinds::TOOLS");
        }
        for (name, _) in TOOLS {
            assert!(advertised.iter().any(|a| a == name), "{name} is in TOOLS but not advertised");
        }
    }

    /// The tools that edit the project run as writes, in order and keyed,
    /// not beside reads: these were read-lane tools that write.
    #[test]
    fn the_tools_that_edit_are_queued() {
        for name in ["klipp_veylvik_composition", "klipp_rename_node", "klipp_track_mask", "klipp_surface_key", "klipp_track_surface", "klipp_detect_scene_cuts", "klipp_rebuild_version", "klipp_pick_grey", "klipp_make_shorts", "klipp_review_finding"] {
            assert!(queued(name), "{name}");
        }
        assert!(shows("klipp_veylvik_composition") && shows("klipp_detect_scene_cuts"));
        assert_eq!(kind("klipp_status"), Read);
        assert_eq!(kind("klipp_screenshot"), Read);
    }
}
