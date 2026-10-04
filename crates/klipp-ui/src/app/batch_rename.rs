//! Batch rename: new file names worked out and previewed before anything is
//! renamed (#890).

#[allow(unused_imports)]
use super::*;

/// Rename mode for the batch-rename dialog. `FindReplace` substitutes a
/// literal substring in the clip's stem; `Sequential` rebuilds the stem as
/// `{prefix}{counter}{suffix}` with zero padding. File extensions are
/// always preserved.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum BatchRenameMode {
    #[default]
    FindReplace,
    Sequential,
}

/// Split `"name.ext"` into `("name", ".ext")` at the LAST dot. Returns
/// `(full, "")` for names without an extension — so `"noext"` survives
/// intact and filenames like `"my.file.name.mp4"` keep the full stem.
pub(super) fn split_stem_ext(filename: &str) -> (&str, &str) {
    if let Some(pos) = filename.rfind('.') {
        (&filename[..pos], &filename[pos..])
    } else {
        (filename, "")
    }
}

/// Compute the new filename for one item given the current dialog state.
/// Pure — no egui, no AppState access. Tested directly.
#[allow(clippy::too_many_arguments)]
pub fn compute_new_filename(
    old: &str,
    mode: &BatchRenameMode,
    find: &str,
    replace: &str,
    match_case: bool,
    prefix: &str,
    suffix: &str,
    counter: u32,
    padding: u8,
) -> String {
    let (stem, ext) = split_stem_ext(old);
    match mode {
        BatchRenameMode::FindReplace => {
            if find.is_empty() {
                return old.to_string();
            }
            let new_stem = if match_case {
                stem.replace(find, replace)
            } else {
                // Case-insensitive literal replace — find spans in the
                // lowercased copy, splice `replace` into the original to
                // preserve the untouched portions verbatim.
                let lower_stem = stem.to_lowercase();
                let lower_find = find.to_lowercase();
                let mut result = stem.to_string();
                let mut search_start = 0;
                while let Some(pos) = lower_stem[search_start..].find(&lower_find) {
                    let abs_pos = search_start + pos;
                    result.replace_range(abs_pos..abs_pos + find.len(), replace);
                    search_start = abs_pos + replace.len();
                    if search_start >= result.len() {
                        break;
                    }
                }
                result
            };
            format!("{}{}", new_stem, ext)
        }
        BatchRenameMode::Sequential => {
            let pad = padding as usize;
            format!(
                "{}{:0>width$}{}{}",
                prefix,
                counter,
                suffix,
                ext,
                width = pad,
            )
        }
    }
}

/// Build the full preview for the batch-rename dialog. Returns one
/// `(MediaId, old, new)` per input item, counter starts at `start` and
/// increments by 1 per item (in iterator order).
#[allow(clippy::too_many_arguments)]
pub fn preview_batch_rename(
    items: &[(klipp_core::MediaId, String)],
    mode: &BatchRenameMode,
    find: &str,
    replace: &str,
    match_case: bool,
    prefix: &str,
    suffix: &str,
    start: u32,
    padding: u8,
) -> Vec<(klipp_core::MediaId, String, String)> {
    items
        .iter()
        .enumerate()
        .map(|(i, (id, old))| {
            let new = compute_new_filename(
                old,
                mode,
                find,
                replace,
                match_case,
                prefix,
                suffix,
                start + i as u32,
                padding,
            );
            (*id, old.clone(), new)
        })
        .collect()
}

#[cfg(test)]
mod batch_rename_tests {
    use super::*;
    use klipp_core::MediaId;

    #[test]
    fn find_replace_basic() {
        let result = compute_new_filename(
            "Interview_Raw.mp4",
            &BatchRenameMode::FindReplace,
            "Raw", "Edit", true, "", "", 1, 2,
        );
        assert_eq!(result, "Interview_Edit.mp4");
    }

    #[test]
    fn find_replace_preserves_extension() {
        let result = compute_new_filename(
            "clip.mov",
            &BatchRenameMode::FindReplace,
            "clip", "scene", true, "", "", 1, 2,
        );
        assert_eq!(result, "scene.mov");
    }

    #[test]
    fn find_replace_empty_find_is_noop() {
        let result = compute_new_filename(
            "clip.mov",
            &BatchRenameMode::FindReplace,
            "", "anything", true, "", "", 1, 2,
        );
        assert_eq!(result, "clip.mov");
    }

    #[test]
    fn find_replace_case_insensitive() {
        let result = compute_new_filename(
            "Interview_RAW.mp4",
            &BatchRenameMode::FindReplace,
            "raw", "edit", false, "", "", 1, 2,
        );
        // Case-insensitive match on "RAW" with replacement "edit".
        assert_eq!(result, "Interview_edit.mp4");
    }

    #[test]
    fn sequential_numbering_with_padding() {
        let result = compute_new_filename(
            "clip.mov",
            &BatchRenameMode::Sequential,
            "", "", true, "Scene_", "", 1, 3,
        );
        assert_eq!(result, "Scene_001.mov");
    }

    #[test]
    fn sequential_numbering_counter_increments() {
        let items = vec![
            (MediaId::new(), "a.mp4".to_string()),
            (MediaId::new(), "b.mp4".to_string()),
            (MediaId::new(), "c.mp4".to_string()),
        ];
        let result = preview_batch_rename(
            &items, &BatchRenameMode::Sequential,
            "", "", true, "Clip_", "", 1, 2,
        );
        assert_eq!(result[0].2, "Clip_01.mp4");
        assert_eq!(result[1].2, "Clip_02.mp4");
        assert_eq!(result[2].2, "Clip_03.mp4");
    }

    #[test]
    fn split_stem_ext_last_dot() {
        assert_eq!(split_stem_ext("my.file.name.mp4"), ("my.file.name", ".mp4"));
        assert_eq!(split_stem_ext("noext"), ("noext", ""));
        assert_eq!(split_stem_ext(""), ("", ""));
    }

    #[test]
    fn sequential_with_suffix() {
        let result = compute_new_filename(
            "src.avi",
            &BatchRenameMode::Sequential,
            "", "", true, "Take_", "_final", 7, 2,
        );
        assert_eq!(result, "Take_07_final.avi");
    }
}
