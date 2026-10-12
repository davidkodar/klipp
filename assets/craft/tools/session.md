# Session, jobs, projects and export

What each of these tools does in full: the details its short description leaves out. Read one tool's part with klipp_guide and its name as the topic (find_shots), or this whole page as session-tools.

## klipp_batch

Several write tools applied together in one frame and recorded as ONE undo step: use it for anything that is one action in your plan (an append and its grade, a set of parameter changes). Every call is checked before any is applied; a bad one refuses the whole batch and nothing changes. Reads (status, lists, screenshots) are refused inside a batch. klipp_look calls batch too, but only with other looks: one look on many clips is one call and one undo step. Replies with the document revision after the batch.

## klipp_cancel_job

Cancel the named render or one queued analysis job. Running analysis, import and sync cannot be interrupted safely. A queued UI cancellation has its own operation_id; confirm its outcome before saying the job stopped.

## klipp_export

Render the active sequence to an absolute path: defaults, or an export preset by name, codec (h264, hevc, prores...), bit_depth 10, bitrate_kbps; the extension gives the container (mp4, mov, mkv, webm). Refused while a clip's media is offline or onto a project media file. Asynchronous: klipp_export_status with wait true returns when the file is done.

## klipp_export_status

Every render job Klipp knows: status (queued, rendering, complete, failed, cancelled), progress 0 to 1, frames, stage, error, loudness result. With wait true, waits for job_id or all currently known jobs. Returns finished, succeeded and timed_out; failed or cancelled is not success. Timeout does not cancel renders.

## klipp_follow_job

Wait for one render or analysis job to end, then say how it ended: completed with the file and its loudness, failed with why, or cancelled, and the way there (each 10% with the time). Waits up to max_wait_ms (10 minutes unless given, at most an hour); if the job is still running then, says so — call again. Your client may show progress meanwhile, but you see only this reply. For live progress you can tell the person as it happens, run `klipp-mcp follow <job_id>` as a background task instead: it prints a line per step and a last line when the job ends.

## klipp_job_status

Read live render and analysis jobs in one shape, including phase, progress, result and error. Give job_id to follow one job. With wait true, return on a progress change or after a bounded long poll; call again until completed, failed or cancelled.

## klipp_new_project

Start an empty project: name, and location an existing folder (the project is <location>/<name>.klipp, saved at once). width, height and fps set its first sequence; left out, both follow the first clip placed. Refused when the open project has unsaved changes unless discard_changes is true: klipp_save_project first.

## klipp_operation_status

Read the latest known outcome of a queued write by its operation_id. Reports waiting_for_ui, ui_applied or ui_refused, the final document revision and refusal reason. Outcomes are kept only while Klipp runs; an ID from before a restart says so. Progress is null when the UI has no measurable total; this tracks UI command completion, not any background render or import probe.

## klipp_perf

Frame timings: decode, grade, composite, total, drops, plus GPU->CPU readback counters, and presented_fps, the pictures shown per second over the last 2 s: it falls to null within 2 s of a pause, and stepping or scrubbing reads as a small rate. Use it to tell whether a change actually made playback faster.

## klipp_record

Record the screen for secs with the recorder on the machine (gpu-screen-recorder, else wf-recorder): target "focused" (the focused window, the default), "screen", a monitor's name, or a region "WxH+X+Y"; fps 60. Imported into the pool unless import is false, so the footage of the product can be made from here: ask the user to bring the window up, record, analyze, cut.

## klipp_relink_media

Point a media item at a file: an offline item found again, or a better copy. The file is judged against what the project knew of the item (reel, start timecode, length, rate, size) and relinked; the answer's check says how sure that is. A file that is probably not the clip is refused with the reason; force: true relinks it anyway (a relink is not undone by undo). klipp_list_media marks items offline.

## klipp_save_project

Save the project: to an absolute path (a folder ending in .klipp, made if needed), or where it was last saved when no path is given. Save before a long piece of work and when you are done, so the user can carry on by hand. klipp_status says where the project is saved and whether it has unsaved changes.

## klipp_status

Whether Klipp is there and what is open, read live without changing anything: first a plain summary to tell a person who asks whether Klipp is connected (version, project and whether it is saved, timeline, playhead, media), then the details: the build (commit, dirty, when it was built, and whether a newer build is on disk so a restart is due), transport state, timeline summary, viewer resolution and the latest performance sample. Start here.

## klipp_window_screenshot

The whole Klipp window as the person sees it, panels and dialogs included, as a PNG (1600 wide by default): for checking a layout, a panel or a dialog. The viewer in it is drawn through its monitor LUT and overlays, so measure colour with klipp_screenshot or klipp_frame_stats, never from this.
