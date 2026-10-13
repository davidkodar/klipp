//! The layouts behind [`crate::keybindings::KeymapPreset`].
//!
//! Each table is that NLE's factory default for the Klipp actions it
//! shares, on Windows (Final Cut: Command read as Ctrl, Option as Alt).
//! Actions are matched on what they do, not on what they are called:
//! Premiere's Delete leaves a gap, so it is `lift_clip` there, where in
//! Klipp and Final Cut Delete closes the gap. A key an NLE uses for an
//! action Klipp lacks is not listed, and loose matches (a panel that
//! opens rather than toggles) are left out rather than guessed.
//!
//! Anything not listed keeps Klipp's key, unless a listed action claims
//! it; `KeymapPreset::bindings` then takes it off the Klipp action.

/// Adobe's "Default keyboard shortcuts in Premiere Pro" (helpx.adobe.com,
/// 2025/2026 pages; tools and Zoom to Sequence from the 2018 page).
pub(crate) const PREMIERE: &[(&str, &[&str])] = &[
    ("save_project", &["Ctrl+S"]),
    ("save_project_as", &["Ctrl+Shift+S"]),
    ("open_project", &["Ctrl+O"]),
    ("import_media", &["Ctrl+I"]),
    ("export", &["Ctrl+M"]),
    ("new_project", &["Ctrl+Alt+N"]),
    ("close_project", &["Ctrl+Shift+W"]),
    ("quit", &["Ctrl+Q"]),
    ("undo", &["Ctrl+Z"]),
    ("redo", &["Ctrl+Shift+Z"]),
    ("cut_clip", &["Ctrl+X"]),
    ("paste_attributes", &["Ctrl+Alt+V"]),
    ("select_all", &["Ctrl+A"]),
    ("deselect_all", &["Ctrl+Shift+A"]),
    // Ripple Delete; Delete and Backspace clear and leave the gap.
    ("delete_clip", &["Shift+Delete", "Alt+Backspace"]),
    ("lift_clip", &["Delete", "Backspace"]),
    // Add Edit.
    ("split_at_playhead", &["Ctrl+K"]),
    ("group_clips", &["Ctrl+G"]),
    ("ungroup_clips", &["Ctrl+Shift+G"]),
    ("select_tool", &["V"]),
    ("blade_tool", &["C"]),
    // Ripple Edit (B) and Rolling Edit (N), which Klipp's one trim tool covers.
    ("trim_tool", &["B", "N"]),
    // Type Tool (#1713).
    ("text_tool", &["T"]),
    ("insert_edit", &[","]),
    ("overwrite_edit", &["."]),
    ("lift_edit", &[";"]),
    ("extract_edit", &["'"]),
    // Ripple Trim Previous / Next Edit to Playhead.
    ("trim_in_to_playhead", &["Q"]),
    ("trim_out_to_playhead", &["W"]),
    ("extract_frame_to_file", &["Ctrl+Shift+E"]),
    ("toggle_audio_skim", &["Shift+S"]),
    ("toggle_skimming", &["Shift+H"]),
    ("next_edit_point", &["ArrowDown"]),
    ("prev_edit_point", &["ArrowUp"]),
    ("set_in_point", &["I"]),
    ("set_out_point", &["O"]),
    ("clear_in_point", &["Ctrl+Shift+I"]),
    ("clear_out_point", &["Ctrl+Shift+O"]),
    ("clear_in_out", &["Ctrl+Shift+X"]),
    ("set_range_to_clip", &["X"]),
    ("play_pause", &["Space"]),
    ("step_forward", &["ArrowRight"]),
    ("step_backward", &["ArrowLeft"]),
    ("go_to_start", &["Home"]),
    ("go_to_end", &["End"]),
    ("shuttle_reverse", &["J"]),
    ("shuttle_pause", &["K"]),
    ("shuttle_forward", &["L"]),
    ("toggle_fullscreen", &["Ctrl+`"]),
    ("focus_panel_search", &["Shift+F"]),
    ("toggle_snap", &["S"]),
    ("fit_to_window", &["\\"]),
    ("zoom_in", &["="]),
    ("zoom_out", &["-"]),
    ("match_frame_program_to_source", &["F"]),
    ("match_frame_source_to_program", &["Shift+R"]),
    ("add_marker", &["M"]),
    ("create_title", &["Ctrl+T"]),
    ("multicam_angle_1", &["1"]),
    ("multicam_angle_2", &["2"]),
    ("multicam_angle_3", &["3"]),
    ("multicam_angle_4", &["4"]),
    ("multicam_angle_5", &["5"]),
    ("multicam_angle_6", &["6"]),
    ("multicam_angle_7", &["7"]),
    ("multicam_angle_8", &["8"]),
    ("multicam_angle_9", &["9"]),
    // Toggle Multi-Camera View.
    ("toggle_multicam_mode", &["Shift+0"]),
];

/// Avid Media Composer's default keyboard: the US keyboard in Avid's
/// MC | First User's Guide and Avid's "Keyboard Shortcuts for Avid
/// Editors". Add Marker, Match Frame and Add Edit have no default key in
/// Avid (the guide says to map them yourself), so they keep Klipp's.
pub(crate) const AVID: &[(&str, &[&str])] = &[
    ("save_project", &["Ctrl+S"]),
    ("undo", &["Ctrl+Z"]),
    ("redo", &["Ctrl+R"]),
    ("set_in_point", &["I", "E"]),
    ("set_out_point", &["O", "R"]),
    ("clear_in_point", &["D"]),
    ("clear_out_point", &["F"]),
    ("clear_in_out", &["G"]),
    // Mark Clip.
    ("set_range_to_clip", &["T"]),
    ("lift_edit", &["Z"]),
    ("extract_edit", &["X"]),
    // Splice-in and Overwrite.
    ("insert_edit", &["V"]),
    ("overwrite_edit", &["B"]),
    ("prev_edit_point", &["A"]),
    ("next_edit_point", &["S"]),
    // Trim mode.
    ("trim_tool", &["U"]),
    ("play_pause", &["Space", "5"]),
    ("step_backward", &["ArrowLeft", "3"]),
    ("step_forward", &["ArrowRight", "4"]),
    ("go_to_start", &["Home"]),
    ("go_to_end", &["End"]),
    ("shuttle_reverse", &["J"]),
    ("shuttle_pause", &["K"]),
    ("shuttle_forward", &["L"]),
    // Add Keyframe (Avid's only adds).
    ("toggle_keyframe", &["'"]),
    ("zoom_in", &["Ctrl+]"]),
    ("zoom_out", &["Ctrl+["]),
    ("fit_to_window", &["Ctrl+/"]),
    ("toggle_fullscreen", &["Ctrl+Shift+F"]),
    // The number row is transport and track selection in Avid, so the
    // angles are not left on it half-mapped.
    ("multicam_angle_1", &[]),
    ("multicam_angle_2", &[]),
    ("multicam_angle_3", &[]),
    ("multicam_angle_4", &[]),
    ("multicam_angle_5", &[]),
    ("multicam_angle_6", &[]),
    ("multicam_angle_7", &[]),
    ("multicam_angle_8", &[]),
    ("multicam_angle_9", &[]),
];

/// DaVinci Resolve's default keyboard, Edit and Color pages. Blackmagic
/// publishes no current list; this is LogicKeyboard's full Resolve 14
/// Windows layout, cross-checked against newer references for the keys
/// Resolve added later (ripple to playhead, Paste Attributes).
pub(crate) const RESOLVE: &[(&str, &[&str])] = &[
    ("save_project", &["Ctrl+S"]),
    ("import_media", &["Ctrl+I"]),
    ("preferences", &["Ctrl+,"]),
    ("quit", &["Ctrl+Q"]),
    ("undo", &["Ctrl+Z"]),
    ("redo", &["Ctrl+Shift+Z"]),
    ("cut_clip", &["Ctrl+X"]),
    ("paste_attributes", &["Alt+V"]),
    ("select_all", &["Ctrl+A"]),
    ("deselect_all", &["Ctrl+Shift+A"]),
    // Ripple Delete; Backspace deletes and leaves the gap.
    ("delete_clip", &["Delete", "Shift+Backspace"]),
    ("lift_clip", &["Backspace"]),
    ("split_at_playhead", &["Ctrl+B", "Ctrl+\\"]),
    ("freeze_frame", &["Shift+R"]),
    ("select_tool", &["A"]),
    ("blade_tool", &["B"]),
    ("trim_tool", &["T"]),
    ("insert_edit", &["F9"]),
    ("overwrite_edit", &["F10"]),
    // Ripple Start / End to Playhead.
    ("trim_in_to_playhead", &["Ctrl+Shift+["]),
    ("trim_out_to_playhead", &["Ctrl+Shift+]"]),
    ("toggle_audio_skim", &["Shift+S"]),
    ("next_edit_point", &["ArrowDown"]),
    ("prev_edit_point", &["ArrowUp"]),
    ("set_in_point", &["I"]),
    ("set_out_point", &["O"]),
    ("clear_in_point", &["Alt+I"]),
    ("clear_out_point", &["Alt+O"]),
    ("clear_in_out", &["Alt+X"]),
    ("set_range_to_clip", &["X"]),
    ("play_pause", &["Space"]),
    ("step_forward", &["ArrowRight"]),
    ("step_backward", &["ArrowLeft"]),
    ("go_to_start", &["Home"]),
    ("go_to_end", &["End"]),
    ("shuttle_reverse", &["J"]),
    ("shuttle_pause", &["K"]),
    ("shuttle_forward", &["L"]),
    ("toggle_loop", &["Ctrl+/"]),
    // Cinema Viewer.
    ("toggle_fullscreen", &["Ctrl+F"]),
    ("toggle_before_after", &["Shift+D"]),
    ("toggle_scopes", &["Ctrl+Shift+W"]),
    ("toggle_snap", &["N"]),
    ("fit_to_window", &["Shift+Z"]),
    ("zoom_in", &["Ctrl+="]),
    ("zoom_out", &["Ctrl+-"]),
    ("match_frame_program_to_source", &["F"]),
    ("add_marker", &["M"]),
    ("prev_keyframe", &["["]),
    ("next_keyframe", &["]"]),
    ("prev_color_node", &["Shift+Alt+;"]),
    ("next_color_node", &["Shift+Alt+'"]),
    ("toggle_node_bypass", &["Ctrl+D"]),
];

/// Apple's "Keyboard shortcuts in Final Cut Pro for Mac"
/// (support.apple.com). Shortcuts that need the Control key have no
/// equivalent here, since Ctrl already stands for Command.
pub(crate) const FINAL_CUT: &[(&str, &[&str])] = &[
    ("open_project", &["Ctrl+O"]),
    ("new_project", &["Ctrl+N"]),
    ("import_media", &["Ctrl+I"]),
    ("export", &["Ctrl+E"]),
    ("preferences", &["Ctrl+,"]),
    ("quit", &["Ctrl+Q"]),
    ("undo", &["Ctrl+Z"]),
    ("redo", &["Ctrl+Shift+Z"]),
    ("cut_clip", &["Ctrl+X"]),
    ("paste_attributes", &["Ctrl+Shift+V"]),
    ("duplicate_clip", &["Ctrl+D"]),
    ("select_all", &["Ctrl+A"]),
    ("deselect_all", &["Ctrl+Shift+A"]),
    ("delete_clip", &["Backspace", "Delete"]),
    // Replace with Gap.
    ("lift_clip", &["Shift+Backspace", "Shift+Delete"]),
    // Blade.
    ("split_at_playhead", &["Ctrl+B"]),
    ("freeze_frame", &["Alt+F"]),
    // Break Apart Clip Items.
    ("ungroup_clips", &["Ctrl+Shift+G"]),
    ("select_tool", &["A"]),
    ("blade_tool", &["B"]),
    ("range_tool", &["R"]),
    ("trim_tool", &["T"]),
    ("insert_edit", &["W"]),
    ("overwrite_edit", &["D"]),
    ("connect_clip", &["Q"]),
    // Trim Start / Trim End.
    ("trim_in_to_playhead", &["Alt+["]),
    ("trim_out_to_playhead", &["Alt+]"]),
    ("toggle_skimming", &["S"]),
    ("toggle_audio_skim", &["Shift+S"]),
    ("next_edit_point", &["ArrowDown", "'"]),
    ("prev_edit_point", &["ArrowUp", ";"]),
    ("set_in_point", &["I"]),
    ("set_out_point", &["O"]),
    ("clear_in_point", &["Alt+I"]),
    ("clear_out_point", &["Alt+O"]),
    ("clear_in_out", &["Alt+X"]),
    // Select Clip Range.
    ("set_range_to_clip", &["X"]),
    ("play_pause", &["Space"]),
    ("step_forward", &["ArrowRight"]),
    ("step_backward", &["ArrowLeft"]),
    ("go_to_start", &["Home"]),
    ("go_to_end", &["End"]),
    ("shuttle_reverse", &["J"]),
    ("shuttle_pause", &["K"]),
    ("shuttle_forward", &["L"]),
    ("toggle_loop", &["Ctrl+L"]),
    ("toggle_fullscreen", &["Ctrl+Shift+F"]),
    ("toggle_inspector_panel", &["Ctrl+4"]),
    ("toggle_color_panel", &["Ctrl+6"]),
    ("toggle_scopes", &["Ctrl+7"]),
    ("focus_panel_search", &["Ctrl+F"]),
    ("toggle_snap", &["N"]),
    // Zoom to Fit.
    ("fit_to_window", &["Shift+Z"]),
    ("zoom_in", &["Ctrl+="]),
    ("zoom_out", &["Ctrl+-"]),
    // Reveal Source in Browser.
    ("match_frame_program_to_source", &["Shift+F"]),
    ("add_marker", &["M"]),
    // Cut and Switch to Viewer Angle N.
    ("multicam_angle_1", &["1"]),
    ("multicam_angle_2", &["2"]),
    ("multicam_angle_3", &["3"]),
    ("multicam_angle_4", &["4"]),
    ("multicam_angle_5", &["5"]),
    ("multicam_angle_6", &["6"]),
    ("multicam_angle_7", &["7"]),
    ("multicam_angle_8", &["8"]),
    ("multicam_angle_9", &["9"]),
    // View > Show in Viewer > Angles (Shift-Command-7).
    ("toggle_multicam_mode", &["Ctrl+Shift+7"]),
];
