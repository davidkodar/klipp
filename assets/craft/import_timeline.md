# Import a timeline and inspect its report

Call `klipp_import_timeline` with an absolute `path` ending in `.otio`, `.fcpxml`,
`.kdenlive`, `.mlt` or `.edl`. It appends a sequence to the current project and
switches to it. It does not replace or save the current project. Other formats
and converter installation are not supported by this operation.

The response includes the same `report` shown in the app: imported sequence,
track, clip, transition, marker and title counts; warnings grouped by kind with
up to three original examples; all warning details; and missing media ids,
names and paths. Warning counts count notices, not individual effects: one
notice may describe several dropped items. Preserve this distinction when
summarising the import.

If the UI has not finished before the reply, read `klipp_import_timeline` without
`path` until its `request_id` matches the returned id. It then contains `report`
or `error`. Do not submit the import again. A read without `path` never imports.

The report opens after import. File → Last Import Report reopens it, and its
Relink… button opens the existing relink flow for offline media. It records the
state at import; subsequent relinking does not rewrite that historical report.
Saving the project keeps the report for reopening, including Save As.
