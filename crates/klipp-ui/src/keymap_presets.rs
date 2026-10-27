//! The layouts behind [`crate::keybindings::KeymapPreset`], written from
//! the fixtures in `keymap_fixtures/` (#2934): each editor's own published
//! default shortcuts, current version, on Windows and Linux (Final Cut:
//! Command read as Ctrl, Option as Alt). The fixtures name the sources;
//! `app::keymap_tests` holds each table to its fixture.
//!
//! Actions are matched on what they do, not on what they are called. An
//! action the editor has with no default key has none here (`&[]`). The
//! keys the editor uses for commands Klipp lacks are in each `_TAKEN` list:
//! no Klipp action keeps one of them under that preset. Anything else keeps
//! Klipp's key: an action the editor lacks, a loose match (a panel that
//! opens rather than toggles), and one the published list doesn't settle.
//! Where the editor's one command is two Klipp actions on one key, the
//! dispatcher picks by what is selected (`keybindings::resolved`), or the
//! table gives the key to one and says why.

/// A preset's rows, as written: each action and its keys.
#[cfg(test)]
pub(crate) fn rows(preset: crate::keybindings::KeymapPreset) -> &'static [(&'static str, &'static [&'static str])] {
    use crate::keybindings::KeymapPreset as K;
    match preset {
        K::Klipp => &[],
        K::Premiere => PREMIERE,
        K::Avid => AVID,
        K::Resolve => RESOLVE,
        K::FinalCut => FINAL_CUT,
    }
}


/// Premiere Pro, Current Premiere (helpx now titles it 'Premiere'); default-shortcuts page 'Last updated on Jan 7, 2026', no app version stated: `keymap_fixtures/premiere.json`.
pub(crate) const PREMIERE: &[(&str, &[&str])] = &[
    ("add_audio_crossfade", &["Ctrl+Shift+D"]), // Apply Audio Transition
    ("add_default_transition", &["Ctrl+D"]), // Apply Video Transition
    ("add_marker", &["M"]), // Add Marker
    ("clear_in_out", &["Ctrl+Shift+X"]), // Clear In and Out
    ("clear_in_point", &["Ctrl+Shift+I"]), // Clear In
    ("clear_out_point", &["Ctrl+Shift+O"]), // Clear Out
    ("close_project", &["Ctrl+Shift+W"]), // Close Project
    ("copy_clip", &["Ctrl+C"]), // Copy
    ("create_multicam", &[]), // Create Multi-Camera Source Sequence
    ("create_title", &["Ctrl+T"]), // Text
    ("cut_clip", &["Ctrl+X"]), // Cut
    ("delete_clip", &["Shift+Delete", "Alt+Backspace"]), // Ripple Delete
    ("deselect_all", &["Ctrl+Shift+A"]), // Deselect All
    ("export", &["Ctrl+M"]), // Export Media
    ("extend_edit", &["E"]), // Extend Selected Edit to Playhead
    ("extract_edit", &["'"]), // Extract
    ("focus_panel_search", &["Shift+F"]), // Select Find Box
    ("go_to_end", &["End"]), // Go to Sequence-Clip End
    ("go_to_start", &["Home"]), // Go to Sequence-Clip Start
    ("group_clips", &["Ctrl+G"]), // Group
    ("import_media", &["Ctrl+I"]), // Import...
    ("insert_edit", &[","]), // Insert
    ("key_ease_in", &[]), // Ease In
    ("key_ease_out", &[]), // Ease Out
    ("key_linear", &[]), // Linear
    ("lift_clip", &["Delete", "Backspace"]), // Clear
    ("lift_edit", &[";"]), // Lift
    ("mask_tool", &["P"]), // Pen Tool
    ("match_frame_program_to_source", &["F"]), // Match Frame
    ("match_frame_source_to_program", &["Shift+R"]), // Reverse Match Frame
    ("multicam_angle_1", &["1"]), // Select Camera 1
    ("multicam_angle_2", &["2"]), // Select Camera 2
    ("multicam_angle_3", &["3"]), // Select Camera 3
    ("multicam_angle_4", &["4"]), // Select Camera 4
    ("multicam_angle_5", &["5"]), // Select Camera 5
    ("multicam_angle_6", &["6"]), // Select Camera 6
    ("multicam_angle_7", &["7"]), // Select Camera 7
    ("multicam_angle_8", &["8"]), // Select Camera 8
    ("multicam_angle_9", &["9"]), // Select Camera 9
    ("new_project", &["Ctrl+Alt+N"]), // New Project …
    ("next_edit_point", &["ArrowDown"]), // Go to Next Edit Point
    ("next_marker", &["Shift+M"]), // Go to Next Marker
    ("nudge_clips_back", &["Alt+ArrowLeft"]), // Nudge Clip Selection Left One Frame
    ("nudge_clips_back_many", &["Shift+Alt+ArrowLeft"]), // Nudge Clip Selection Left Five Frames
    ("nudge_clips_forward", &["Alt+ArrowRight"]), // Nudge Clip Selection Right One Frame
    ("nudge_clips_forward_many", &["Shift+Alt+ArrowRight"]), // Nudge Clip Selection Right Five Frames
    ("open_project", &["Ctrl+O"]), // Open Project...
    ("overwrite_edit", &["."]), // Overwrite
    ("paste_attributes", &["Ctrl+Alt+V"]), // Paste Attributes
    ("paste_clip", &["Ctrl+V"]), // Paste
    ("paste_insert", &["Ctrl+Shift+V"]), // Paste Insert
    ("play_around_edit", &["Shift+K"]), // Play Around
    ("play_in_to_out", &["Ctrl+Shift+Space"]), // Play In to Out
    ("play_pause", &["Space"]), // Play-Stop Toggle
    ("prev_edit_point", &["ArrowUp"]), // Go to Previous Edit Point
    ("prev_marker", &["Ctrl+Shift+M"]), // Go to Previous Marker
    ("quit", &["Ctrl+Q"]), // Exit
    ("redo", &["Ctrl+Shift+Z"]), // Redo
    // ripple_trim_head: left out: Q is one Premiere command, Ripple Trim Previous Edit to Playhead, which trim_in_to_playhead already is in Klipp; a ripple trim of the selected clip would run with it.
    // ripple_trim_tail: left out: W is one Premiere command, Ripple Trim Next Edit to Playhead, which trim_out_to_playhead already is in Klipp; a ripple trim of the selected clip would run with it.
    ("save_project", &["Ctrl+S"]), // Save
    ("save_project_as", &["Ctrl+Shift+S"]), // Save As...
    ("select_all", &["Ctrl+A"]), // Select All
    ("select_tool", &["V"]), // Selection Tool
    ("set_in_point", &["I"]), // Mark In
    ("set_out_point", &["O"]), // Mark Out
    ("set_range_to_clip", &["X"]), // Mark Clip
    ("shuttle_forward", &["L"]), // Shuttle Right
    ("shuttle_pause", &["K"]), // Shuttle Stop
    ("shuttle_reverse", &["J"]), // Shuttle Left
    ("split_at_playhead", &["Ctrl+K"]), // Add Edit
    ("step_backward", &["ArrowLeft"]), // Step Back
    ("step_forward", &["ArrowRight"]), // Step Forward
    ("toggle_audio_skim", &["Shift+S"]), // Toggle Audio During Scrubbing
    ("toggle_fullscreen", &["Ctrl+`"]), // Toggle Full Screen
    ("toggle_loop", &[]), // Loop Playback
    ("toggle_multicam_mode", &["Shift+0"]), // Toggle Multi-Camera View
    ("toggle_safe_areas", &[]), // Safe Margins
    ("toggle_skimming", &["Shift+H"]), // Hover Scrub
    ("toggle_snap", &["S"]), // Snap in Timeline
    ("trim_edit_back", &["Ctrl+ArrowLeft"]), // Trim Backward
    ("trim_edit_back_many", &["Ctrl+Shift+ArrowLeft"]), // Trim Backward Many
    ("trim_edit_forward", &["Ctrl+ArrowRight"]), // Trim Forward
    ("trim_edit_forward_many", &["Ctrl+Shift+ArrowRight"]), // Trim Forward Many
    ("trim_in_to_playhead", &["Q"]), // Ripple Trim Previous Edit To Playhead
    ("trim_out_to_playhead", &["W"]), // Ripple Trim Next Edit To Playhead
    ("trim_tool", &["B", "N"]), // Ripple Edit Tool / Rolling Edit Tool
    ("undo", &["Ctrl+Z"]), // Undo
    ("ungroup_clips", &["Ctrl+Shift+G"]), // Ungroup
    ("zoom_in", &["="]), // Zoom In
    ("zoom_out", &["-"]), // Zoom Out
];

/// Premiere Pro's keys for commands Klipp lacks.
pub(crate) const PREMIERE_TAKEN: &[&str] = &[
    "/", // Mark Selection
    "Alt+,", // Slide Clip Selection Left One Frame
    "Alt+-", // Decrease Audio Tracks Height
    "Alt+.", // Slide Clip Selection Right One Frame
    "Alt+=", // Increase Audio Tracks Height
    "Alt+[", // Set Work Area Bar In Point
    "Alt+]", // Set Work Area Bar Out Point
    "Ctrl+-", // Decrease Video Tracks Height
    "Ctrl+/", // Bin (File > New)
    "Ctrl+0", // Toggle All Video Targets
    "Ctrl+9", // Toggle All Audio Targets
    "Ctrl+;", // Show Guides (Program Monitor panel)
    "Ctrl+=", // Increase Video Tracks Height
    "Ctrl+Alt+0", // Toggle All Source Video
    "Ctrl+Alt+9", // Toggle All Source Audio
    "Ctrl+Alt+A", // Add new caption track
    "Ctrl+Alt+ArrowDown", // Go to the next caption segment
    "Ctrl+Alt+ArrowLeft", // Slip Clip Selection Left One Frame
    "Ctrl+Alt+ArrowRight", // Slip Clip Selection Right One Frame
    "Ctrl+Alt+ArrowUp", // Go to the previous caption segment
    "Ctrl+Alt+C", // Add caption at playhead
    "Ctrl+Alt+E", // Ellipse (New Layer)
    "Ctrl+Alt+I", // Import from Media Browser
    "Ctrl+Alt+K", // Keyboard Shortcuts
    "Ctrl+Alt+M", // Clear Selected Marker
    "Ctrl+Alt+Q", // Trim Previous Edit to Playhead
    "Ctrl+Alt+R", // Rectangle (New Layer)
    "Ctrl+Alt+S", // Save a Copy...
    "Ctrl+Alt+T", // Show/Hide Tracks (Audio Track Mixer panel)
    "Ctrl+Alt+W", // Trim Next Edit to Playhead
    "Ctrl+Alt+[", // Select Previous Layer
    "Ctrl+Alt+]", // Select Next Layer
    "Ctrl+ArrowDown", // Nudge Selected Object up/right/left/down by one frame (Program Monitor panel)
    "Ctrl+ArrowUp", // Nudge Selected Object up/right/left/down by one frame (Program Monitor panel)
    "Ctrl+B", // New Bin (Project panel)
    "Ctrl+Delete", // Delete Selection with Options (Project panel)
    "Ctrl+E", // Edit Original
    "Ctrl+F", // Find...
    "Ctrl+L", // Link
    "Ctrl+N", // New Sequence…
    "Ctrl+PageDown", // Icon (Project panel)
    "Ctrl+PageUp", // List (Project panel)
    "Ctrl+R", // Speed/Duration...
    "Ctrl+Shift+,", // Select Previous Panel
    "Ctrl+Shift+.", // Select Next Panel
    "Ctrl+Shift+/", // Duplicate
    "Ctrl+Shift+;", // Previous in Sequence
    "Ctrl+Shift+Alt+ArrowLeft", // Slip Clip Selection Left Five Frames
    "Ctrl+Shift+Alt+ArrowRight", // Slip Clip Selection Right Five Frames
    "Ctrl+Shift+Alt+M", // Clear All Markers
    "Ctrl+Shift+Alt+R", // Lock Guides (Program Monitor panel)
    "Ctrl+Shift+ArrowDown", // Nudge Selected Object up/right/left/down by five frames (Program Monitor panel)
    "Ctrl+Shift+ArrowUp", // Nudge Selected Object up/right/left/down by five frames (Program Monitor panel)
    "Ctrl+Shift+F", // Reveal Nested Sequence
    "Ctrl+Shift+H", // Get Media File Properties for Selection...
    "Ctrl+Shift+K", // Add Edit to All Tracks
    "Ctrl+Shift+[", // Send to Back
    "Ctrl+Shift+]", // Bring to Front
    "Ctrl+Space", // Play from Playhead to Out Point
    "Ctrl+U", // Make Subclip...
    "Ctrl+W", // Close
    "Ctrl+[", // Send Backward
    "Ctrl+]", // Bring Forward
    "D", // Select Clip at Playhead
    "Enter", // Render Effects In to Out
    "F1", // Premiere Pro Help...
    "G", // Audio Gain…
    "PageDown", // Show Next Screen
    "PageUp", // Show Previous Screen
    "R", // Rate Stretch Tool
    "Shift+-", // Minimize All Tracks
    "Shift+2", // Source Monitor (Window)
    "Shift+3", // Timeline (Window)
    "Shift+4", // Program Monitor (Window)
    "Shift+7", // Effects (Window)
    "Shift+8", // Media Browser (Window)
    "Shift+9", // Audio Clip Mixer (Window)
    "Shift+;", // Next in Sequence
    "Shift+Alt+,", // Slide Clip Selection Left Five Frames
    "Shift+Alt+.", // Slide Clip Selection Right Five Frames
    "Shift+Alt+0", // Reset to Saved Layout
    "Shift+Alt+1", // Workspaces > All Panels
    "Shift+Alt+2", // Workspaces > Assembly
    "Shift+Alt+3", // Workspaces > Audio
    "Shift+Alt+4", // Workspaces > Captions and Graphics
    "Shift+Alt+6", // Workspaces > Editing
    "Shift+Alt+7", // Workspaces > Effects
    "Shift+Alt+8", // Workspaces > Essential
    "Shift+Alt+9", // Workspaces > Learning
    "Shift+Alt+M", // Send to Media Encoder
    "Shift+ArrowDown", // Go to Next Edit Point on Any Track
    "Shift+ArrowLeft", // Select Directory List (Media Browser panel)
    "Shift+ArrowRight", // Select Media List (Media Browser panel)
    "Shift+ArrowUp", // Go to Previous Edit Point on Any Track
    "Shift+D", // Apply Default Transitions to Selection
    "Shift+E", // Enable
    "Shift+End", // Go to Selected Clip End
    "Shift+G", // Audio Channels...
    "Shift+Home", // Go to Selected Clip Start
    "Shift+I", // Go to In
    "Shift+J", // Shuttle Slow Left
    "Shift+L", // Shuttle Slow Right
    "Shift+O", // Go to Out
    "Shift+P", // Set Poster Frame
    "Shift+Space", // Play In to Out with Preroll/Postroll
    "Shift+T", // Toggle Trim Type
    "Shift+U", // Make Subsequence
    "Shift+[", // Thumbnail Size Previous (Project panel)
    "Shift+\\", // Toggle View (Project panel)
    "Shift+]", // Thumbnail Size Next (Project panel)
    "Shift+`", // Maximize or Restore Active Frame
    "]", // Increase Clip Volume
    "`", // Maximize or Restore Frame Under Cursor
];

/// Media Composer, 2026.x (current release 2026.8): `keymap_fixtures/avid.json`.
pub(crate) const AVID: &[(&str, &[&str])] = &[
    // add_audio_crossfade: left out: \\\\ is one Media Composer command, Add Dissolve, on the selected tracks; Klipp has two actions, and \\\\ goes to add_default_transition.
    ("add_default_transition", &["\\"]), // Add Dissolve
    ("add_marker", &[]), // Add Marker
    ("clear_in_out", &["G"]), // Clear Both Marks
    ("clear_in_point", &["D"]), // Clear IN Mark
    ("clear_out_point", &["F"]), // Clear OUT Mark
    ("close_project", &["Ctrl+Shift+W"]), // Close Project
    ("copy_clip", &["Ctrl+C"]), // Copy
    ("create_multicam", &["Ctrl+Shift+G"]), // Group Clip (Clip > Group Clips)
    ("cut_clip", &["Ctrl+X"]), // Cut
    ("delete_clip", &[]), // Delete (Extract/Splice-in segment tool)
    ("extend_edit", &[]), // Extend
    ("extract_edit", &["X"]), // Extract
    ("fit_to_fill_edit", &[]), // Fit to Fill
    ("fit_to_window", &["Ctrl+/"]), // Show entire sequence
    ("freeze_frame", &[]), // Freeze Frame (Composer menu)
    ("go_to_end", &["End"]), // End
    ("go_to_start", &["Home"]), // Home
    ("insert_edit", &["V"]), // Splice-in
    ("key_linear", &[]), // Linear (keyframe interpolation, right-click menu)
    ("leave_compound", &[]), // Step Out
    ("lift_clip", &["Delete"]), // Delete (Lift/Overwrite segment tool, the default)
    ("lift_edit", &["Z"]), // Lift
    ("match_frame_program_to_source", &[]), // Match Frame
    ("match_frame_source_to_program", &[]), // Reverse Match Frame
    ("move_clips_down_a_track", &["ArrowDown"]), // Move Clip Down
    ("move_clips_up_a_track", &["ArrowUp"]), // Move Clip Up
    ("multicam_angle_1", &["F9"]), // MultiCam key M1
    ("multicam_angle_2", &["F10"]), // MultiCam key M2
    ("multicam_angle_3", &["F11"]), // MultiCam key M3
    ("multicam_angle_4", &["F12"]), // MultiCam key M4
    ("multicam_angle_5", &[]), // Numeric keypad angle switch (Nine Split)
    ("multicam_angle_6", &[]), // Numeric keypad angle switch (Nine Split)
    ("multicam_angle_7", &[]), // Numeric keypad angle switch (Nine Split)
    ("multicam_angle_8", &[]), // Numeric keypad angle switch (Nine Split)
    ("multicam_angle_9", &[]), // Numeric keypad angle switch (Nine Split)
    ("next_edit_point", &["S"]), // Go to Next Edit
    ("next_marker", &[]), // Go to Next Marker
    ("nudge_clips_back", &[","]), // Trim Left 1 Frame (on selected segments)
    ("nudge_clips_back_many", &["M"]), // Trim Left 10 Frames (on selected segments)
    ("nudge_clips_forward", &["."]), // Trim Right 1 Frame (on selected segments)
    ("nudge_clips_forward_many", &["/"]), // Trim Right 10 Frames (on selected segments)
    ("overwrite_edit", &["B"]), // Overwrite
    ("paste_clip", &["Ctrl+V"]), // Paste
    ("play_around_edit", &[]), // Play Loop (Trim mode)
    ("play_in_to_out", &["6"]), // Play IN to OUT
    ("play_pause", &["Space", "5", "`", "Tab"]), // Play
    ("prev_edit_point", &["A"]), // Go to Previous Edit
    ("prev_marker", &[]), // Go to Previous Marker
    ("redo", &["Ctrl+R"]), // Redo
    ("replace_edit", &[]), // Replace Edit
    ("ripple_trim_head", &[]), // Top
    ("ripple_trim_tail", &[]), // Tail
    ("select_edit_point", &["U"]), // Trim Mode (select nearest transition)
    ("set_in_point", &["I", "E"]), // Mark IN
    ("set_out_point", &["O", "R"]), // Mark OUT
    ("set_range_to_clip", &["T"]), // Mark Clip
    ("shuttle_forward", &["L"]), // Play Forward
    ("shuttle_pause", &["K"]), // Pause
    ("shuttle_reverse", &["J"]), // Play Reverse
    ("split_at_playhead", &[]), // Add Edit
    ("step_backward", &["ArrowLeft", "3"]), // Step Backward (1 frame)
    ("step_forward", &["ArrowRight", "4"]), // Step Forward (1 frame)
    ("toggle_fullscreen", &["Ctrl+Shift+F"]), // Full Screen Playback (Composer menu)
    ("toggle_keyframe", &["'"]), // Add Keyframe
    ("trim_edit_back", &[","]), // Trim Left 1 Frame
    ("trim_edit_back_many", &["M"]), // Trim Left 10 Frames
    ("trim_edit_forward", &["."]), // Trim Right 1 Frame
    ("trim_edit_forward_many", &["/"]), // Trim Right 10 Frames
    ("trim_in_to_playhead", &[]), // Top
    ("trim_out_to_playhead", &[]), // Tail
    // trim_tool: left out: U is one Media Composer command, Trim Mode, entered at the nearest transition; select_edit_point does that in Klipp, and trim_tool would arm a tool as well.
    ("undo", &["Ctrl+Z"]), // Undo
    ("zoom_in", &["Ctrl+]"]), // More Detail (Timeline)
    ("zoom_out", &["Ctrl+["]), // Less Detail (Timeline)
];

/// Media Composer's keys for commands Klipp lacks.
pub(crate) const AVID_TAKEN: &[&str] = &[
    "-", // Select A3 Track
    "0", // Select A2 Track
    "1", // Step Backward 10 Frames
    "2", // Step Forward 10 Frames
    "7", // Select V2 Track
    "8", // Select V1 Track
    "9", // Select A1 Track
    ";", // Mouse Shuttle
    "=", // Select A4 Track
    "Alt+C", // Select clips/segments copy: Copy to Source monitor / Lift to Source / Extract to Source
    "Alt+I", // Go to IN
    "Alt+T", // Mark Clip ignoring track selection
    "Alt+X", // Select clips/segments copy: Copy to Source monitor / Lift to Source / Extract to Source
    "Alt+Z", // Select clips/segments copy: Copy to Source monitor / Lift to Source / Extract to Source
    "C", // Copy to Clipboard (IN-OUT marked material)
    "Ctrl+.", // Stop render / waveform redraw
    "Ctrl+0", // Timeline window
    "Ctrl+1", // Audio tool
    "Ctrl+2", // Avid Calculator
    "Ctrl+3", // Command Palette
    "Ctrl+4", // Composer window
    "Ctrl+6", // Console
    "Ctrl+7", // Capture tool
    "Ctrl+8", // Effect Palette
    "Ctrl+9", // Project window
    "Ctrl+=", // Settings for the active window
    "Ctrl+A", // Select all tracks / Deselect all tracks
    "Ctrl+Alt+K", // Enlarge / Reduce audio waveform only
    "Ctrl+Alt+L", // Enlarge / Reduce audio waveform only
    "Ctrl+Alt+P", // Place Markers window info in Console
    "Ctrl+ArrowDown", // Switch windows / switch tabs
    "Ctrl+ArrowUp", // Switch windows / switch tabs
    "Ctrl+D", // Duplicate (bin clip/sequence)
    "Ctrl+F", // Find (Edit > Find)
    "Ctrl+G", // Find next
    "Ctrl+I", // Clip Info / bin info
    "Ctrl+J", // Zoom (drag an area) / Zoom back
    "Ctrl+K", // Enlarge / Reduce track
    "Ctrl+L", // Enlarge / Reduce track
    "Ctrl+M", // Zoom (drag an area) / Zoom back
    "Ctrl+N", // New tape name (Capture tool)
    "Ctrl+Shift+A", // Select all tracks / Deselect all tracks
    "Ctrl+Shift+N", // New sequence from the active Sequence Template
    "Ctrl+Shift+V", // Paste Audio Keyframes
    "Ctrl+Tab", // Switch windows / switch tabs
    "Ctrl+U", // New video track / New audio track
    "Ctrl+W", // Close window / bin
    "Ctrl+Y", // New video track / New audio track
    "Escape", // Toggle Source/Record (switch active monitor); exits Trim mode
    "F1", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "F2", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "F4", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "F5", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "F6", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "F7", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "F8", // Capture-mode function keys: subclip IN/OUT, log/capture, colored markers
    "H", // Focus
    "N", // Mouse Jog
    "P", // Trim A Side
    "Q", // Go to IN
    "W", // Go to OUT
    "Y", // Source/Record Mode
    "[", // Trim AB Sides (both sides)
    "]", // Trim B Side
];

/// DaVinci Resolve, DaVinci Resolve 21 (Reference Manual dated July 2026): `keymap_fixtures/resolve.json`.
pub(crate) const RESOLVE: &[(&str, &[&str])] = &[
    ("add_audio_crossfade", &["Shift+T"]), // Timeline > Add Audio Only Transition
    ("add_clip_marker", &["M"]), // Add Marker (with clips selected)
    ("add_default_transition", &["Ctrl+T"]), // Timeline > Add Transition
    ("add_marker", &["M"]), // Add Marker
    ("blade_tool", &["B"]), // Blade Edit Mode (Razor)
    ("clear_in_out", &["Alt+X"]), // Clear In and Out
    ("clear_in_point", &["Alt+I"]), // Clear In
    ("clear_out_point", &["Alt+O"]), // Clear Out
    ("close_project", &[]), // File > Close Project
    ("copy_clip", &["Ctrl+C"]), // Edit > Copy
    ("copy_color_nodes", &["Ctrl+C"]), // Edit > Copy (node selected, Color page)
    ("create_multicam", &[]), // Create New Multicam Clip Using Selected Clips (Media Pool)
    ("cut_clip", &["Ctrl+Shift+X"]), // Edit > Ripple Cut
    ("delete_clip", &["Delete"]), // Ripple Delete (Forward Delete key)
    ("deselect_all", &["Ctrl+Shift+A"]), // Deselect All
    ("export", &[]), // File > Quick Export
    ("extend_edit", &["E"]), // Trim > Extend Edit
    ("extract_edit", &["Delete"]), // Ripple Delete of the marked In/Out range (Forward Delete)
    ("extract_frame_to_file", &[]), // File > Export > Current Frame as Still
    ("fit_to_fill_edit", &["Shift+F11"]), // Fit to Fill
    ("fit_to_window", &["Shift+Z"]), // View > Zoom > Zoom to Fit
    ("freeze_frame", &["Shift+R"]), // Clip > Freeze Frame
    ("go_to_end", &["End"]), // Timeline End
    ("go_to_start", &["Home"]), // Timeline Start
    ("import_media", &["Ctrl+I"]), // Import Media
    ("insert_edit", &["F9"]), // Insert
    ("lift_clip", &["Backspace"]), // Delete Selected (leave gap)
    ("lift_edit", &["Backspace"]), // Delete of the marked In/Out range (leave gap)
    ("match_frame_program_to_source", &["F"]), // Match Frame
    ("match_frame_source_to_program", &["F"]), // Match Frame (from the Source Viewer)
    ("move_clips_down_a_track", &["Alt+ArrowDown"]), // Move clip down a track
    ("move_clips_up_a_track", &["Alt+ArrowUp"]), // Move clip up a track
    ("multicam_angle_1", &["1"]), // Clip > Multicam Cut > Angle 1
    ("multicam_angle_2", &["2"]), // Clip > Multicam Cut > Angle 2
    ("multicam_angle_3", &["3"]), // Clip > Multicam Cut > Angle 3
    ("multicam_angle_4", &["4"]), // Clip > Multicam Cut > Angle 4
    ("multicam_angle_5", &["5"]), // Clip > Multicam Cut > Angle 5
    ("multicam_angle_6", &["6"]), // Clip > Multicam Cut > Angle 6
    ("multicam_angle_7", &["7"]), // Clip > Multicam Cut > Angle 7
    ("multicam_angle_8", &["8"]), // Clip > Multicam Cut > Angle 8
    ("multicam_angle_9", &["9"]), // Clip > Multicam Cut > Angle 9
    ("new_project", &[]), // File > New Project
    ("next_color_node", &["Shift+Alt+'"]), // Color > Nodes > Next Node
    ("next_edit_point", &["ArrowDown"]), // Next Clip/Edit
    ("next_keyframe", &["Shift+'"]), // Next Keyframe
    ("next_marker", &["Shift+ArrowDown"]), // Next Marker
    ("nudge_clips_back", &[","]), // Nudge One Frame Left
    ("nudge_clips_back_many", &["Shift+,"]), // Fast Nudge Left
    ("nudge_clips_forward", &["."]), // Nudge One Frame Right
    ("nudge_clips_forward_many", &["Shift+."]), // Fast Nudge Right
    ("overwrite_edit", &["F10"]), // Overwrite
    ("paste_attributes", &["Alt+V"]), // Edit > Paste Attributes
    ("paste_clip", &["Ctrl+V"]), // Edit > Paste
    ("paste_color_nodes", &["Ctrl+V"]), // Edit > Paste (Node Editor)
    ("paste_insert", &["Ctrl+Shift+V"]), // Edit > Paste Insert
    ("place_on_top_edit", &["F12"]), // Place On Top
    ("play_around_edit", &["/"]), // Play Around Selection
    ("play_in_to_out", &["Alt+/"]), // Play In to Out
    ("play_pause", &["Space"]), // Play/Stop
    ("preferences", &["Ctrl+,"]), // DaVinci Resolve > Preferences
    ("prev_color_node", &["Shift+Alt+;"]), // Color > Nodes > Previous Node
    ("prev_edit_point", &["ArrowUp"]), // Previous Clip/Edit
    ("prev_keyframe", &["Shift+;"]), // Previous Keyframe
    ("prev_marker", &["Shift+ArrowUp"]), // Previous Marker
    ("printer_blue_down", &[]), // Printer Light Hotkeys: Minus Blue
    ("printer_blue_up", &[]), // Printer Light Hotkeys: Plus Blue
    ("printer_green_down", &[]), // Printer Light Hotkeys: Minus Green
    ("printer_green_up", &[]), // Printer Light Hotkeys: Plus Green
    ("printer_red_down", &[]), // Printer Light Hotkeys: Minus Red
    ("printer_red_up", &[]), // Printer Light Hotkeys: Plus Red
    ("quit", &["Ctrl+Q"]), // Quit DaVinci Resolve
    ("redo", &["Ctrl+Shift+Z"]), // Edit > Redo
    ("replace_edit", &["F11"]), // Replace
    ("reset_current_node", &["Shift+Home"]), // Reset Selected Node Grade
    ("ripple_trim_head", &["Shift+["]), // Trim > Trim Start
    ("ripple_trim_tail", &["Shift+]"]), // Trim > Trim End
    ("save_project", &["Ctrl+S"]), // File > Save Project
    ("save_project_as", &["Ctrl+Shift+S"]), // File > Save Project As
    ("select_all", &["Ctrl+A"]), // Edit > Select All
    ("select_backward", &["Ctrl+Alt+Y"]), // Timeline > Select Clips Backward > On All Tracks
    ("select_edit_point", &["V", "U"]), // Select Nearest Edit Point (V) / Edit Point Type (U)
    ("select_forward", &["Alt+Y"]), // Timeline > Select Clips Forward > On All Tracks
    ("select_tool", &["A"]), // Selection Mode
    ("select_track_backward", &["Ctrl+Y"]), // Timeline > Select Clips Backward > On This Track
    ("select_track_forward", &["Y"]), // Timeline > Select Clips Forward > On This Track
    ("set_in_point", &["I"]), // Mark In
    ("set_out_point", &["O"]), // Mark Out
    ("set_range_to_clip", &["X"]), // Mark Clip
    ("shuttle_forward", &["L"]), // Play Forward
    ("shuttle_pause", &["K"]), // Stop
    ("shuttle_reverse", &["J"]), // Play Reverse
    ("split_at_playhead", &["Ctrl+\\", "Ctrl+B"]), // Timeline > Split Clip / Timeline > Razor
    ("step_backward", &["ArrowLeft"]), // Step One Frame Reverse
    ("step_forward", &["ArrowRight"]), // Step One Frame Forward
    ("toggle_audio_mixer", &[]), // Show/hide Mixer panel (Interface Toolbar)
    ("toggle_audio_skim", &["Shift+S"]), // Timeline > Audio Scrubbing
    ("toggle_before_after", &["Shift+D"]), // Bypass Color Grades and Fusion Effects
    ("toggle_browser_panel", &[]), // Show/hide Media Pool panel (Interface Toolbar)
    ("toggle_fullscreen", &["Ctrl+F"]), // Workspace > Viewer Mode > Cinema Viewer
    ("toggle_inspector_panel", &[]), // Show/hide Inspector panel (Interface Toolbar)
    ("toggle_keyframe", &["Ctrl+["]), // Mark > Add Keyframe
    ("toggle_keyframe_editor", &[]), // Show/hide Keyframes panel / curve (Interface Toolbar)
    ("toggle_loop", &["Ctrl+/"]), // Playback > Loop/Unloop
    ("toggle_multicam_mode", &[]), // Source Viewer mode drop-down > Multicam
    ("toggle_node_bypass", &["Ctrl+D"]), // Color > Nodes > Enable/Disable Selected Nodes
    ("toggle_project_notes", &[]), // File > Project Notes
    ("toggle_safe_areas", &[]), // View > Safe Area > On/Off
    ("toggle_scopes", &["Ctrl+Shift+W"]), // Workspace > Video Scopes > On/Off
    ("toggle_snap", &["N"]), // Snapping
    ("trim_edit_back", &[","]), // Nudge One Frame Left (edit point selected)
    ("trim_edit_back_many", &["Shift+,"]), // Fast Nudge Left (edit point selected)
    ("trim_edit_forward", &["."]), // Nudge One Frame Right (edit point selected)
    ("trim_edit_forward_many", &["Shift+."]), // Fast Nudge Right (edit point selected)
    ("trim_in_to_playhead", &["Ctrl+Shift+["]), // Trim > Ripple Start to Playhead
    ("trim_out_to_playhead", &["Ctrl+Shift+]"]), // Trim > Ripple End to Playhead
    ("trim_tool", &["T"]), // Trim Edit Mode
    ("undo", &["Ctrl+Z"]), // Edit > Undo
    ("zoom_in", &["Ctrl+="]), // Zoom In
    ("zoom_out", &["Ctrl+-"]), // Zoom Out
];

/// DaVinci Resolve's keys for commands Klipp lacks.
pub(crate) const RESOLVE_TAKEN: &[&str] = &[
    "'", // Last Frame of clip at playhead
    ";", // First Frame of clip at playhead
    "Alt+,", // Trim > Slip Audio One Frame Reverse / Forward
    "Alt+.", // Trim > Slip Audio One Frame Reverse / Forward
    "Alt+1", // Clip > Multicam Switch > Angle 1-9
    "Alt+2", // Clip > Multicam Switch > Angle 1-9
    "Alt+3", // Clip > Multicam Switch > Angle 1-9
    "Alt+4", // Clip > Multicam Switch > Angle 1-9
    "Alt+5", // Clip > Multicam Switch > Angle 1-9
    "Alt+6", // Clip > Multicam Switch > Angle 1-9
    "Alt+7", // Clip > Multicam Switch > Angle 1-9
    "Alt+8", // Clip > Multicam Switch > Angle 1-9
    "Alt+9", // Clip > Multicam Switch > Angle 1-9
    "Alt+ArrowLeft", // Trim > Slip Audio One Subframe Reverse / Forward
    "Alt+ArrowRight", // Trim > Slip Audio One Subframe Reverse / Forward
    "Alt+B", // Mark > Create Subclip
    "Alt+C", // Color > Nodes > Add Serial Node + CPW / LPW / PPW / PCW
    "Alt+D", // Color > Nodes > Enable/Disable All Nodes
    "Alt+F", // Clip > Find Clip in Media Pool
    "Alt+F1", // Auto Select video track 1-8 / all video
    "Alt+F2", // Auto Select video track 1-8 / all video
    "Alt+F3", // Auto Select video track 1-8 / all video
    "Alt+F4", // Auto Select video track 1-8 / all video
    "Alt+F5", // Auto Select video track 1-8 / all video
    "Alt+F6", // Auto Select video track 1-8 / all video
    "Alt+F7", // Auto Select video track 1-8 / all video
    "Alt+F8", // Auto Select video track 1-8 / all video
    "Alt+F9", // Auto Select video track 1-8 / all video
    "Alt+G", // Color > Nodes > Add Serial Node + CPW / LPW / PPW / PCW
    "Alt+K", // Stop and go to last position
    "Alt+L", // Play Again
    "Alt+M", // Mark > Delete Marker
    "Alt+P", // Set Poster Frame / Clear Poster Frame (Media Pool)
    "Alt+Q", // Source Timeline view on/off
    "Alt+R", // Playback > Render Cache: cycle Off/Smart/User
    "Alt+S", // Color > Nodes > Add Serial
    "Alt+Space", // Play Around In
    "Alt+T", // Timeline > Add Video Only Transition
    "Alt+U", // Toggle selection V+A / V / A
    "Alt+W", // Invert Wipe
    "Alt+Z", // Primaries / Log wheels mode
    "Alt+]", // Mark > Delete Keyframe
    "Ctrl+1", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+2", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+3", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+4", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+5", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+6", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+7", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+8", // Color > Memories > Load Memory A-H
    "Ctrl+9", // Workspace > Active Panel Selection (Edit page: Media Folders, Media Clips, Source Viewer, Timeline, Timeline Viewer, Effects, Edit Index, -, Inspector)
    "Ctrl+Alt+'", // Playback > Next Gap
    "Ctrl+Alt+,", // Trim > Slip Eye One Frame Reverse / Forward
    "Ctrl+Alt+-", // Clip > Audio > Increase / Decrease Audio Level 1dB
    "Ctrl+Alt+.", // Trim > Slip Eye One Frame Reverse / Forward
    "Ctrl+Alt+/", // Play to Out
    "Ctrl+Alt+1", // Audio destination to track 1-8
    "Ctrl+Alt+2", // Audio destination to track 1-8
    "Ctrl+Alt+3", // Audio destination to track 1-8
    "Ctrl+Alt+4", // Audio destination to track 1-8
    "Ctrl+Alt+5", // Audio destination to track 1-8
    "Ctrl+Alt+6", // Audio destination to track 1-8
    "Ctrl+Alt+7", // Audio destination to track 1-8
    "Ctrl+Alt+8", // Audio destination to track 1-8
    "Ctrl+Alt+;", // Playback > Previous Gap
    "Ctrl+Alt+=", // Clip > Audio > Increase / Decrease Audio Level 1dB
    "Ctrl+Alt+ArrowDown", // Move audio destination control up / down
    "Ctrl+Alt+ArrowUp", // Move audio destination control up / down
    "Ctrl+Alt+B", // Color > Stills > Next Still / Previous Still
    "Ctrl+Alt+F1", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F2", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F3", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F4", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F5", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F6", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F7", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F8", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+F9", // Auto Select audio track 1-8 / all audio
    "Ctrl+Alt+G", // Color > Stills > Grab Still
    "Ctrl+Alt+I", // Mark Split > Mark Audio In / Mark Audio Out
    "Ctrl+Alt+L", // Clip > Linked Clips (link/unlink)
    "Ctrl+Alt+N", // Color > Stills > Next Still / Previous Still
    "Ctrl+Alt+O", // Mark Split > Mark Audio In / Mark Audio Out
    "Ctrl+Alt+R", // Color > Resolve Live / Live Freeze / Live Snapshot
    "Ctrl+Alt+S", // File > Create Timeline Backup
    "Ctrl+Alt+T", // Color > Tracker > Track Forward / Track Reverse / Track Stop
    "Ctrl+Alt+W", // Split Screen on/off
    "Ctrl+Alt+X", // Clear Audio In and Out (split)
    "Ctrl+Alt+`", // Color > Printer Light Hotkeys
    "Ctrl+ArrowDown", // Move clip selection with arrows
    "Ctrl+ArrowLeft", // Move clip selection with arrows
    "Ctrl+ArrowRight", // Move clip selection with arrows
    "Ctrl+ArrowUp", // Move clip selection with arrows
    "Ctrl+Home", // Reset All Grades and Nodes
    "Ctrl+J", // Dynamic trim reverse / forward (Quick Trim)
    "Ctrl+L", // Dynamic trim reverse / forward (Quick Trim)
    "Ctrl+M", // Add Marker and open Modify Marker
    "Ctrl+N", // File > New Timeline
    "Ctrl+P", // Set Poster Frame / Clear Poster Frame (Media Pool)
    "Ctrl+PageDown", // Timeline Thumbnail Mode: C mode sorting
    "Ctrl+PageUp", // Timeline > Swap Timeline and Source Viewer
    "Ctrl+R", // Clip > Retime Controls
    "Ctrl+Shift+1", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+2", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+3", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+4", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+5", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+6", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+7", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+8", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+9", // Timeline > Video Track Enable 1-9
    "Ctrl+Shift+ArrowDown", // Move video destination control up / down
    "Ctrl+Shift+ArrowLeft", // Edit > Multicam > Previous Angle / Next Angle
    "Ctrl+Shift+ArrowRight", // Edit > Multicam > Previous Angle / Next Angle
    "Ctrl+Shift+ArrowUp", // Move video destination control up / down
    "Ctrl+Shift+Home", // Reset Grades and Keep Nodes
    "Ctrl+Shift+I", // File > Import Timeline > Import AAF, EDL, XML, DRT
    "Ctrl+Shift+L", // Timeline > Linked Selection
    "Ctrl+Shift+N", // File > New Bin
    "Ctrl+Shift+O", // File > Export > Export AAF, XML, DRT
    "Ctrl+Shift+R", // Color > Resolve Live / Live Freeze / Live Snapshot
    "Ctrl+U", // Color > Grade Version > Add / Default
    "Ctrl+W", // Color > Stills > Play Still / Wipe On/Off
    "Ctrl+X", // Edit > Cut (leave gap)
    "Ctrl+]", // Mark > Make Static Keyframe
    "D", // Clip Enable (disable/enable selected clips)
    "Q", // Toggle focus Source / Timeline Viewer
    "S", // Toggle Slip / Slide (Trim mode, clip selected)
    "Shift+1", // Project Manager
    "Shift+2", // Switch to Page (pages in order)
    "Shift+3", // Switch to Page (pages in order)
    "Shift+4", // Switch to Page (pages in order)
    "Shift+5", // Switch to Page (pages in order)
    "Shift+6", // Switch to Page (pages in order)
    "Shift+7", // Switch to Page (pages in order)
    "Shift+8", // Switch to Page (pages in order)
    "Shift+9", // Project Settings
    "Shift+A", // Mark Selection (In/Out to selected clips)
    "Shift+Alt+-", // Clip > Audio > Increase / Decrease Audio Level 3dB
    "Shift+Alt+1", // Lock video track 1-8 / all video
    "Shift+Alt+2", // Lock video track 1-8 / all video
    "Shift+Alt+3", // Lock video track 1-8 / all video
    "Shift+Alt+4", // Lock video track 1-8 / all video
    "Shift+Alt+5", // Lock video track 1-8 / all video
    "Shift+Alt+6", // Lock video track 1-8 / all video
    "Shift+Alt+7", // Lock video track 1-8 / all video
    "Shift+Alt+8", // Lock video track 1-8 / all video
    "Shift+Alt+9", // Lock video track 1-8 / all video
    "Shift+Alt+=", // Clip > Audio > Increase / Decrease Audio Level 3dB
    "Shift+Alt+ArrowLeft", // Edit > Multicam > Previous Page / Next Page
    "Shift+Alt+ArrowRight", // Edit > Multicam > Previous Page / Next Page
    "Shift+Alt+C", // Color > Auto Color
    "Shift+Alt+F1", // Lock audio track 1-8 / all audio
    "Shift+Alt+F2", // Lock audio track 1-8 / all audio
    "Shift+Alt+F3", // Lock audio track 1-8 / all audio
    "Shift+Alt+F4", // Lock audio track 1-8 / all audio
    "Shift+Alt+F5", // Lock audio track 1-8 / all audio
    "Shift+Alt+F6", // Lock audio track 1-8 / all audio
    "Shift+Alt+F7", // Lock audio track 1-8 / all audio
    "Shift+Alt+F8", // Lock audio track 1-8 / all audio
    "Shift+Alt+F9", // Lock audio track 1-8 / all audio
    "Shift+Alt+H", // Highlight BW (high-contrast highlight)
    "Shift+Alt+I", // Mark Split > Mark Video In / Mark Video Out
    "Shift+Alt+O", // Mark Split > Mark Video In / Mark Video Out
    "Shift+Alt+P", // Color > Preview Memory / Original Memory
    "Shift+Alt+Q", // Edit > Switch focus to timeline after edit
    "Shift+Alt+V", // Edit > Paste Value
    "Shift+Alt+X", // Clear Video In and Out (split)
    "Shift+Alt+Z", // Viewer 100% (Actual Size)
    "Shift+Alt+[", // Edit > Multicam > Video and Audio / Video Only / Audio Only
    "Shift+Alt+\\", // Edit > Multicam > Video and Audio / Video Only / Audio Only
    "Shift+Alt+]", // Edit > Multicam > Video and Audio / Video Only / Audio Only
    "Shift+ArrowLeft", // Step One Second Reverse / Forward
    "Shift+ArrowRight", // Step One Second Reverse / Forward
    "Shift+F", // Workspace > Viewer Mode > Full Screen Viewer
    "Shift+F10", // Ripple Overwrite
    "Shift+F12", // Append to End of Timeline
    "Shift+I", // Go to In
    "Shift+J", // Fast Reverse
    "Shift+K", // Play Slow (1/2, 1/4, 1/8)
    "Shift+L", // Fast Forward
    "Shift+M", // Mark > Modify Marker
    "Shift+O", // Go to Out
    "Shift+Q", // Source Tape view
    "Shift+Space", // Play Around Out
    "Shift+V", // Timeline > Select Nearest Clip/Gap
    "Shift+`", // View > Viewer Overlay > Toggle On/Off
    "W", // Dynamic Trim mode
];

/// Final Cut Pro, Final Cut Pro 12.4 (User Guide for Mac, page version 12.4 / macOS 26.6, fetched 2026-10-28): `keymap_fixtures/final_cut.json`.
pub(crate) const FINAL_CUT: &[(&str, &[&str])] = &[
    ("add_audio_crossfade", &["Alt+T"]), // Crossfade
    ("add_default_transition", &["Ctrl+T"]), // Add Default Transition
    ("add_marker", &["M"]), // Add Marker
    ("blade_tool", &["B"]), // Blade Tool
    ("clear_in_out", &["Alt+X"]), // Clear Selected Ranges
    ("clear_in_point", &["Alt+I"]), // Clear Range Start
    ("clear_out_point", &["Alt+O"]), // Clear Range End
    ("copy_clip", &["Ctrl+C"]), // Copy
    ("create_title", &[]), // Connect Default Title
    ("cut_clip", &["Ctrl+X"]), // Cut
    ("delete_clip", &["Backspace"]), // Delete
    ("deselect_all", &["Ctrl+Shift+A"]), // Deselect All
    ("export", &["Ctrl+E"]), // Share to Default Destination
    ("extend_edit", &["Shift+X"]), // Extend Edit
    ("extract_edit", &["Backspace"]), // Delete
    ("fit_to_window", &["Shift+Z"]), // Zoom to Fit
    ("focus_panel_search", &["Ctrl+F"]), // Find
    ("freeze_frame", &["Alt+F"]), // Insert/Connect Freeze Frame
    ("go_to_end", &["End"]), // Go to End
    ("go_to_start", &["Home"]), // Go to Beginning
    ("import_media", &["Ctrl+I"]), // Import Media
    ("insert_edit", &["W"]), // Insert
    ("leave_compound", &["Ctrl+["]), // Timeline History Back
    ("lift_clip", &["Shift+Backspace"]), // Replace with Gap
    ("lift_edit", &["Shift+Backspace"]), // Replace with Gap
    ("match_frame_program_to_source", &["Shift+F"]), // Reveal Source in Browser
    ("move_clips_down_a_track", &["Alt+ArrowDown"]), // Move Down
    ("move_clips_up_a_track", &["Alt+ArrowUp"]), // Move Up
    ("multicam_angle_1", &["1"]), // Cut and Switch to Viewer Angle 1
    ("multicam_angle_2", &["2"]), // Cut and Switch to Viewer Angle 2
    ("multicam_angle_3", &["3"]), // Cut and Switch to Viewer Angle 3
    ("multicam_angle_4", &["4"]), // Cut and Switch to Viewer Angle 4
    ("multicam_angle_5", &["5"]), // Cut and Switch to Viewer Angle 5
    ("multicam_angle_6", &["6"]), // Cut and Switch to Viewer Angle 6
    ("multicam_angle_7", &["7"]), // Cut and Switch to Viewer Angle 7
    ("multicam_angle_8", &["8"]), // Cut and Switch to Viewer Angle 8
    ("multicam_angle_9", &["9"]), // Cut and Switch to Viewer Angle 9
    ("new_project", &["Ctrl+N"]), // New Project
    ("next_edit_point", &["ArrowDown", "'"]), // Down / Go to Next Edit
    ("next_marker", &[]), // Next Marker
    ("nudge_clips_back", &[","]), // Nudge Left
    ("nudge_clips_back_many", &["Shift+,"]), // Nudge Left Many
    ("nudge_clips_forward", &["."]), // Nudge Right
    ("nudge_clips_forward_many", &["Shift+."]), // Nudge Right Many
    ("open_project", &["Ctrl+O"]), // Open Library
    ("overwrite_edit", &["D"]), // Overwrite
    ("paste_attributes", &["Ctrl+Shift+V"]), // Paste Attributes
    ("paste_insert", &["Ctrl+V"]), // Paste Insert at Playhead
    ("place_on_top_edit", &["Q"]), // Connect to Primary Storyline
    ("play_in_to_out", &["/"]), // Play Selection
    ("play_pause", &["Space"]), // Play/Pause
    ("preferences", &["Ctrl+,"]), // Settings
    ("prev_edit_point", &["ArrowUp", ";"]), // Up / Go to Previous Edit
    ("prev_marker", &[]), // Previous Marker
    ("quit", &["Ctrl+Q"]), // Quit
    ("range_tool", &["R"]), // Range Selection Tool
    ("redo", &["Ctrl+Shift+Z"]), // Redo Change
    ("replace_edit", &["Shift+R"]), // Replace
    ("ripple_trim_head", &["Alt+["]), // Trim Start
    ("ripple_trim_tail", &["Alt+]"]), // Trim End
    ("select_all", &["Ctrl+A"]), // Select All
    ("select_edit_point", &["\\"]), // Select Left and Right Edit Edges
    ("select_tool", &["A"]), // Select (Arrow) Tool
    ("set_in_point", &["I"]), // Set Range Start
    ("set_out_point", &["O"]), // Set Range End
    ("set_range_to_clip", &["X"]), // Select Clip Range
    ("shuttle_forward", &["L"]), // Play Forward
    ("shuttle_pause", &["K"]), // Stop
    ("shuttle_reverse", &["J"]), // Play Reverse
    ("split_at_playhead", &["Ctrl+B"]), // Blade
    ("step_backward", &["ArrowLeft"]), // Go to Previous Frame
    ("step_forward", &["ArrowRight"]), // Go to Next Frame
    ("toggle_audio_skim", &["Shift+S"]), // Audio Skimming
    ("toggle_browser_panel", &[]), // Show/Hide Browser
    ("toggle_inspector_panel", &["Ctrl+4"]), // Show/Hide Inspector
    ("toggle_keyframe_editor", &[]), // Show/Hide Video Animation
    ("toggle_loop", &["Ctrl+L"]), // Loop Playback
    ("toggle_multicam_mode", &["Ctrl+Shift+7"]), // Show/Hide Angles
    ("toggle_scopes", &["Ctrl+7"]), // Show/Hide Video Scopes
    ("toggle_skimming", &["S"]), // Skimming
    ("toggle_snap", &["N"]), // Snapping
    ("trim_edit_back", &[","]), // Nudge Left
    ("trim_edit_back_many", &["Shift+,"]), // Nudge Left Many
    ("trim_edit_forward", &["."]), // Nudge Right
    ("trim_edit_forward_many", &["Shift+."]), // Nudge Right Many
    ("trim_tool", &["T"]), // Trim Tool
    ("undo", &["Ctrl+Z"]), // Undo Change
    ("zoom_in", &["Ctrl+="]), // Zoom In
    ("zoom_out", &["Ctrl+-"]), // Zoom Out
];

/// Final Cut Pro's keys for commands Klipp lacks.
pub(crate) const FINAL_CUT_TAKEN: &[&str] = &[
    "-", // Negative Timecode Entry
    "=", // Positive Timecode Entry
    "Alt+,", // Nudge Audio Subframe Left
    "Alt+.", // Nudge Audio Subframe Right
    "Alt+1", // Switch to Viewer Angle 1
    "Alt+2", // Switch to Viewer Angle 2
    "Alt+3", // Switch to Viewer Angle 3
    "Alt+4", // Switch to Viewer Angle 4
    "Alt+5", // Switch to Viewer Angle 5
    "Alt+6", // Switch to Viewer Angle 6
    "Alt+7", // Switch to Viewer Angle 7
    "Alt+8", // Switch to Viewer Angle 8
    "Alt+9", // Switch to Viewer Angle 9
    "Alt+A", // Add Adjustment Clip
    "Alt+ArrowLeft", // Move Left
    "Alt+ArrowRight", // Move Right
    "Alt+B", // Enable Beat Detection
    "Alt+Backspace", // Color Board: Reset Current Board Controls
    "Alt+C", // Add Caption
    "Alt+D", // Distort Tool
    "Alt+E", // Add Default Video Effect
    "Alt+G", // New Compound Clip
    "Alt+H", // Hide Browser Clips
    "Alt+J", // Play Rate -.5
    "Alt+L", // Play Rate .5
    "Alt+M", // Add Marker and Modify
    "Alt+N", // New Event
    "Alt+R", // Replace from Start
    "Alt+S", // Solo
    "Alt+Space", // Play from Playhead
    "Alt+Tab", // Next Text
    "Alt+U", // Show/Hide 2-Up When Trimming
    "Alt+V", // Paste as Connected
    "Alt+W", // Insert Gap
    "Alt+Y", // Audition: Duplicate as Audition
    "Alt+\\", // Trim to Selection
    "C", // Select Clip
    "Ctrl+1", // Go to Browser
    "Ctrl+2", // Go to Timeline
    "Ctrl+3", // Go to Viewer
    "Ctrl+5", // Show/Hide Effects Browser
    "Ctrl+8", // Go to Audio Enhancements
    "Ctrl+9", // Background Tasks
    "Ctrl+Alt+1", // Show/Hide Titles and Generators
    "Ctrl+Alt+2", // Toggle Filmstrip/List View
    "Ctrl+Alt+3", // Go to Event Viewer
    "Ctrl+Alt+4", // Go to Inspector
    "Ctrl+Alt+7", // Show/Hide 360° Viewer
    "Ctrl+Alt+8", // Record Voiceover
    "Ctrl+Alt+ArrowDown", // Overwrite to Primary Storyline
    "Ctrl+Alt+ArrowLeft", // Duplicate Range: Go to Previous Related Clip
    "Ctrl+Alt+ArrowRight", // Duplicate Range: Go to Next Related Clip
    "Ctrl+Alt+ArrowUp", // Lift from Storyline
    "Ctrl+Alt+B", // Enable/Disable Balance Color
    "Ctrl+Alt+Backspace", // Delete Selection Only
    "Ctrl+Alt+C", // Copy Effects
    "Ctrl+Alt+E", // Add Default Audio Effect
    "Ctrl+Alt+G", // Synchronize Clips
    "Ctrl+Alt+H", // Hide Other Applications
    "Ctrl+Alt+K", // Keyboard Customization
    "Ctrl+Alt+M", // Match Color
    "Ctrl+Alt+N", // New Smart Collection
    "Ctrl+Alt+O", // Add Color Adjustment
    "Ctrl+Alt+R", // Retime: Reset
    "Ctrl+Alt+S", // Clip Skimming
    "Ctrl+Alt+V", // Paste Effects
    "Ctrl+Alt+W", // Insert Default Generator
    "Ctrl+Alt+X", // Remove Effects
    "Ctrl+ArrowDown", // Select Clip Below
    "Ctrl+ArrowLeft", // Select Previous Clip
    "Ctrl+ArrowRight", // Select Next Clip
    "Ctrl+ArrowUp", // Select Clip Above
    "Ctrl+Backspace", // Move to Trash
    "Ctrl+G", // Create Storyline
    "Ctrl+H", // Hide Application
    "Ctrl+J", // Project Properties
    "Ctrl+K", // Show/Hide Keyword Editor
    "Ctrl+M", // Minimize
    "Ctrl+R", // Show/Hide Retime Editor
    "Ctrl+Shift+,", // Show Fewer Filmstrip Frames
    "Ctrl+Shift+-", // Decrease Clip Height
    "Ctrl+Shift+.", // Show More Filmstrip Frames
    "Ctrl+Shift+1", // Show/Hide Photos, Videos, and Audio
    "Ctrl+Shift+2", // Show/Hide Timeline Index
    "Ctrl+Shift+8", // Show/Hide Audio Meters
    "Ctrl+Shift+=", // Increase Clip Height
    "Ctrl+Shift+Alt+,", // Show One Frame per Filmstrip
    "Ctrl+Shift+Alt+ArrowLeft", // Go to Previous Focus Point
    "Ctrl+Shift+Alt+ArrowRight", // Go to Next Focus Point
    "Ctrl+Shift+Alt+C", // Consolidate Motion Content
    "Ctrl+Shift+Alt+F", // Reveal Project in Browser
    "Ctrl+Shift+Alt+S", // Show/Hide Audio Lanes
    "Ctrl+Shift+Alt+V", // Paste Timecode
    "Ctrl+Shift+ArrowDown", // Collapse to Connected Storyline
    "Ctrl+Shift+ArrowLeft", // Previous Video Angle
    "Ctrl+Shift+ArrowRight", // Select Next Video Angle
    "Ctrl+Shift+B", // Blade All
    "Ctrl+Shift+C", // Closed Captions
    "Ctrl+Shift+H", // Show Horizon
    "Ctrl+Shift+I", // Set Additional Selection Start
    "Ctrl+Shift+K", // New Keyword Collection
    "Ctrl+Shift+M", // Match Audio
    "Ctrl+Shift+N", // New Folder
    "Ctrl+Shift+O", // Set Additional Selection End
    "Ctrl+Shift+R", // Reveal in Finder
    "Ctrl+Shift+S", // Subtitles
    "Ctrl+Shift+X", // Remove Attributes
    "Ctrl+Shift+Y", // Audition: Duplicate from Original
    "Ctrl+Y", // Create Audition
    "Ctrl+]", // Timeline History Forward
    "Ctrl+`", // Show/Hide Sidebar
    "E", // Append to Storyline
    "F", // Favorite
    "G", // Toggle Storyline Mode
    "H", // Hand Tool
    "P", // Position Tool
    "Shift+1", // Source Media: Audio & Video
    "Shift+2", // Source Media: Video Only
    "Shift+3", // Source Media: Audio Only
    "Shift+A", // Monitor Audio
    "Shift+Alt+'", // Go to Next Bank
    "Shift+Alt+,", // Nudge Audio Subframe Left Many
    "Shift+Alt+.", // Nudge Audio Subframe Right Many
    "Shift+Alt+1", // Cut/Switch Multicam Audio and Video
    "Shift+Alt+2", // Cut/Switch Multicam Video Only
    "Shift+Alt+3", // Cut/Switch Multicam Audio Only
    "Shift+Alt+;", // Go to Previous Bank
    "Shift+Alt+A", // Start/Stop Voiceover Recording
    "Shift+Alt+ArrowLeft", // Previous Audio Angle
    "Shift+Alt+ArrowRight", // Select Next Audio Angle
    "Shift+Alt+C", // Copy Keyframes
    "Shift+Alt+H", // Show/Hide Hidden Clips
    "Shift+Alt+N", // View Clip Names
    "Shift+Alt+P", // Show Image Playground
    "Shift+Alt+S", // Timeline Scrolling
    "Shift+Alt+Tab", // Previous Text
    "Shift+Alt+V", // Paste Keyframes
    "Shift+Alt+X", // Cut Keyframes
    "Shift+Alt+Y", // Finalize Audition
    "Shift+Alt+Z", // Vertical Zoom to Fit
    "Shift+ArrowDown", // Extend Selection Down
    "Shift+ArrowLeft", // Go Back 10 Frames
    "Shift+ArrowRight", // Go Forward 10 Frames
    "Shift+ArrowUp", // Extend Selection Up
    "Shift+C", // Crop Tool
    "Shift+D", // Overwrite - Backtimed
    "Shift+E", // Detect Edits
    "Shift+H", // Retime: Hold
    "Shift+I", // Go to Range Start
    "Shift+N", // Retime: Create Normal Speed Segment
    "Shift+O", // Go to Range End
    "Shift+Q", // Connect to Primary Storyline - Backtimed
    "Shift+Space", // Play Reverse
    "Shift+T", // Transform Tool
    "Shift+V", // Set Monitoring Angle
    "Shift+Y", // Audition: Replace and Add to Audition
    "Shift+[", // Select Left Audio Edge
    "Shift+\\", // Select Left and Right Audio Edit Edges
    "Shift+]", // Select Right Audio Edge
    "U", // Unrate
    "V", // Enable/Disable Clip
    "Y", // Open Audition
    "Z", // Zoom Tool
    "[", // Select Left Edge
    "]", // Select Right Edge
    "`", // Override Connections
];
