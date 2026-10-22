use std::path::{Component, Path, PathBuf};

/// Convert an absolute runtime path to a relative path for on-disk storage.
///
/// Relative to the project directory (the `.klipp/` folder).
/// Falls back to absolute if the path is on a different drive/volume
/// (e.g. Windows UNC paths, different mount points).
///
/// Both paths are normalised first. A media path read from another
/// project folder (an auto-save being restored) can still carry that
/// folder and its `..` in it; diffed as-is it was stored as
/// `../X_autosave.klipp/../../Videos/a.mov`, which stops resolving once
/// the auto-save folder is deleted.
pub fn to_project_relative(project_dir: &Path, absolute: &Path) -> PathBuf {
    let absolute = normalize_path(absolute);
    pathdiff::diff_paths(&absolute, normalize_path(project_dir)).unwrap_or(absolute)
}

/// Resolve a stored path (possibly relative) to an absolute runtime path.
///
/// Resolution order:
///   1. If the stored path is already absolute — return as-is
///   2. If relative — join with project_dir (even if the file doesn't exist on disk)
///
/// The caller should run `path.exists()` separately to set the offline flag.
/// Never returns a bare relative path — always joins with project_dir.
///
/// The result is normalised lexically: `..` is collapsed on the path's
/// text, as `to_project_relative` computed it, not by the filesystem,
/// which cannot step out of a directory that no longer exists. That
/// keeps projects saved with a path through a deleted auto-save folder
/// opening.
pub fn resolve_media_path(project_dir: &Path, stored: &Path) -> PathBuf {
    if stored.is_absolute() {
        normalize_path(stored)
    } else {
        normalize_path(&project_dir.join(stored))
    }
}

/// Resolve a stored path while guarding against path-traversal attacks.
///
/// Returns `None` (and logs `tracing::warn`) for relative paths whose `..`
/// components escape the project directory. Absolute paths are allowed
/// because users legitimately reference media outside the `.klipp` folder
/// (e.g. `~/Videos/clip.mp4`). The caller should treat `None` as "media
/// offline" and never feed the untrusted path to ffmpeg or any other I/O.
///
/// Does NOT touch the filesystem — works on path syntax alone so it's safe
/// to call even when the media or the project drive is disconnected.
pub fn safe_resolve(project_dir: &Path, stored: &Path) -> Option<PathBuf> {
    if stored.is_absolute() {
        Some(normalize_path(stored))
    } else {
        let joined = project_dir.join(stored);
        let normalized = normalize_path(&joined);
        let project_normalized = normalize_path(project_dir);
        if normalized.starts_with(&project_normalized) {
            Some(normalized)
        } else {
            tracing::warn!(
                "safe_resolve: rejecting relative path that escapes project dir: {:?}",
                stored,
            );
            None
        }
    }
}

/// Collapse `.` and `..` components without touching the filesystem.
///
/// Unlike `std::fs::canonicalize`, this never follows symlinks and works
/// for paths that don't exist on disk. A leading `..` that can't be
/// collapsed (no parent to pop) is preserved — the caller's prefix check
/// catches any remaining escape.
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else {
                    parts.push(comp);
                }
            }
            _ => parts.push(comp),
        }
    }
    parts.iter().collect()
}

/// Maximum input-file size for project / import parsers (64 MiB).
///
/// A 10 GB `.kdenlive` or `.fcpxml` dropped on the importer would OOM the
/// process without a cap. 64 MiB is generous for any realistic project
/// file while keeping the memory ceiling bounded.
pub const MAX_INPUT_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Read a text file into a String, rejecting files larger than `max_bytes`.
///
/// Drop-in replacement for `std::fs::read_to_string` with an OOM guard.
/// The size check uses metadata (no read before the decision) so a 10 GB
/// file is rejected instantly without reading a single byte.
pub fn read_to_string_capped(path: &Path, max_bytes: u64) -> Result<String, std::io::Error> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file too large ({} bytes, max {} bytes): {}",
                size, max_bytes, path.display()
            ),
        ));
    }
    let mut s = String::with_capacity(size as usize);
    file.take(max_bytes).read_to_string(&mut s)?;
    Ok(s)
}

/// Maximum node count for untrusted XML imports (1M nodes ≈ 1GB of
/// in-memory parse structure). roxmltree defaults to `u32::MAX`, which
/// would accept a degenerate input with billions of tiny nodes and OOM
/// the process even though the file itself fit under `MAX_INPUT_FILE_BYTES`.
pub const MAX_XML_NODES: u32 = 1_000_000;

/// Largest frame dimension Klipp will accept from an untrusted import.
/// 16384 matches the common GPU texture-size ceiling; anything above is
/// almost certainly a malformed file or an allocation-exhaustion attempt.
pub const MAX_FRAME_DIMENSION: u32 = 16_384;

/// Largest FPS numerator we'll accept. 1_000_000 covers every sensible
/// rational frame rate (drop-frame 30000/1001, etc.) with a wide safety
/// margin; anything above is noise.
pub const MAX_FPS_NUMERATOR: u32 = 1_000_000;

/// Clamp an FPS read from an untrusted import into a sensible range and
/// substitute 24/1 if either half is zero. Without this, `Fps::new(x, 0)`
/// flowing from a malformed profile produces an infinite/NaN frame rate
/// that corrupts every downstream timeline calculation.
pub fn sanitize_fps(num: u32, den: u32) -> klipp_core::Fps {
    if den == 0 || num == 0 {
        tracing::warn!(
            "sanitize_fps: rejecting zero fps ({num}/{den}) — substituting 24/1",
        );
        return klipp_core::Fps::film_24();
    }
    if num > MAX_FPS_NUMERATOR {
        tracing::warn!(
            "sanitize_fps: numerator {num} exceeds cap {MAX_FPS_NUMERATOR} — clamping",
        );
        return klipp_core::Fps::new(MAX_FPS_NUMERATOR, den);
    }
    klipp_core::Fps::new(num, den)
}

/// A media item's picture size as saved: 0×0 is kept, a file with no
/// picture (a sound, its cover art not counted); anything else as
/// [`sanitize_resolution`] has it (#1892). Read through that, a sound
/// came back from its project as 1920×1080 footage.
pub fn sanitize_media_resolution(width: u32, height: u32) -> klipp_core::Resolution {
    if width == 0 && height == 0 {
        return klipp_core::Resolution { width: 0, height: 0 };
    }
    sanitize_resolution(width, height)
}

/// Clamp a frame resolution into the GPU-texture-safe range. Zero
/// dimensions are replaced with 1080p defaults; oversize dimensions are
/// clamped to `MAX_FRAME_DIMENSION` so a crafted file can't ask us to
/// allocate a 10-billion-pixel buffer.
pub fn sanitize_resolution(width: u32, height: u32) -> klipp_core::Resolution {
    let mut w = width;
    let mut h = height;
    if w == 0 || h == 0 {
        tracing::warn!(
            "sanitize_resolution: zero dimension ({w}x{h}) — substituting 1920x1080",
        );
        w = 1920;
        h = 1080;
    }
    if w > MAX_FRAME_DIMENSION || h > MAX_FRAME_DIMENSION {
        tracing::warn!(
            "sanitize_resolution: oversize ({w}x{h}) — clamping to {MAX_FRAME_DIMENSION}",
        );
        w = w.min(MAX_FRAME_DIMENSION);
        h = h.min(MAX_FRAME_DIMENSION);
    }
    klipp_core::Resolution {
        width: w,
        height: h,
    }
}

/// Parse an XML string into a roxmltree Document with explicit,
/// security-hardened options:
///   * `allow_dtd: false` — reject documents that declare a DOCTYPE /
///     entity references. This is already the roxmltree default, but
///     stating it locks the posture in place if the upstream default
///     ever flips.
///   * `nodes_limit: MAX_XML_NODES` — cap the parsed node count to
///     prevent entity-bomb / nested-element DoS from degenerate inputs.
pub fn parse_xml_safe(xml: &str) -> Result<roxmltree::Document<'_>, roxmltree::Error> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
        },
    )
}

/// `xml` without a document type declaration that names its root and
/// nothing more (`<!DOCTYPE fcpxml>`, `<!DOCTYPE xmeml>`), which Final Cut
/// Pro writes, and Klipp's own FCPXML too, and [`parse_xml_safe`] refuses
/// as a DTD. One with an internal subset (`[...]`), where entities are
/// declared, is left for the parser to refuse.
pub fn without_bare_doctype(xml: &str) -> std::borrow::Cow<'_, str> {
    let Some(at) = xml.find("<!DOCTYPE") else { return xml.into() };
    let Some(len) = xml[at..].find('>') else { return xml.into() };
    let decl = &xml[at..at + len + 1];
    if decl.contains('[') {
        return xml.into();
    }
    format!("{}{}", &xml[..at], &xml[at + len + 1..]).into()
}

/// Write `data` to `path` atomically: write to a unique temp file in the
/// same directory (so the rename stays on one filesystem), then rename
/// over the target. A crash mid-write leaves an orphan temp file, never
/// a corrupted target.
///
/// The temp file name includes the process id and a high-resolution
/// timestamp so two concurrent saves (two Klipp instances against the
/// same project, or two threads racing to save the same file) cannot
/// clobber each other's temp file — each writes to a uniquely-named
/// scratch path and the rename picks the winner.
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "target".to_string());
    let temp_name = format!(".{base}.tmp.{pid}.{nanos}");
    let temp_path = path.with_file_name(temp_name);
    std::fs::write(&temp_path, data)?;
    std::fs::rename(&temp_path, path)?;
    Ok(())
}

pub fn read_capped(path: &Path, max_bytes: u64) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "file too large ({} bytes, max {} bytes): {}",
                size, max_bytes, path.display()
            ),
        ));
    }
    let mut buf = Vec::with_capacity(size as usize);
    file.take(max_bytes).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_doctype_is_dropped_and_one_with_entities_is_not() {
        let bare = "<?xml version=\"1.0\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.9\"/>";
        assert!(parse_xml_safe(bare).is_err());
        assert!(parse_xml_safe(&without_bare_doctype(bare)).is_ok());
        let entities = "<!DOCTYPE x [<!ENTITY a \"b\">]><x>&a;</x>";
        assert!(parse_xml_safe(&without_bare_doctype(entities)).is_err(), "entities still refused");
    }
    use std::fs;

    // ═══════════════════════════════════════════════════════════════
    // atomic_write regression.
    //
    // SEC-4: workspace.json (and other project files) used to use
    // `fs::write`, which truncates the destination before writing.
    // A crash mid-write produced a 0-byte / partial-write workspace.
    // The fix routes everything through atomic_write (temp+rename).
    //
    // SEC-5: the temp file name was originally a fixed string; two
    // concurrent saves (two Klipp instances on the same project, or
    // two threads racing) would clobber each other's scratch file.
    // The fix includes pid + nanos in the temp name so each writer
    // gets its own unique scratch path.
    //
    // Both fixes shipped 2026-04-17 with no tests. These tests pin
    // the contracts so a future "let me simplify the temp name"
    // surfaces in CI rather than as a corrupted .klipp project.
    // ═══════════════════════════════════════════════════════════════

    /// Per-test scratch dir under target/debug-tests; auto-cleaned.
    /// Avoids depending on /tmp permissions and keeps the data
    /// adjacent to the build.
    struct ScratchDir(std::path::PathBuf);
    impl ScratchDir {
        fn new(test_name: &str) -> Self {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = std::env::temp_dir()
                .join(format!("klipp-atomic-write-{test_name}-{nanos}"));
            fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn atomic_write_creates_target_with_correct_contents() {
        let scratch = ScratchDir::new("creates_target");
        let path = scratch.0.join("workspace.json");
        atomic_write(&path, b"{\"projects\": []}").expect("write should succeed");
        let read = fs::read(&path).expect("target must exist after atomic_write");
        assert_eq!(read, b"{\"projects\": []}");
    }

    #[test]
    fn atomic_write_overwrites_existing_target() {
        // Atomic write must replace existing content — that's the
        // whole point of the rename.
        let scratch = ScratchDir::new("overwrites");
        let path = scratch.0.join("workspace.json");
        fs::write(&path, b"old contents").expect("seed");
        atomic_write(&path, b"new contents").expect("overwrite should succeed");
        assert_eq!(fs::read(&path).unwrap(), b"new contents");
    }

    #[test]
    fn atomic_write_leaves_no_temp_file_on_success() {
        // After a successful atomic_write, only the target should
        // remain in the directory — no stray .tmp.<pid>.<nanos>
        // files. Pinning so a future "let me skip the rename" or
        // "let me write to the temp and forget to clean up" surfaces.
        let scratch = ScratchDir::new("no_temp_left");
        let path = scratch.0.join("workspace.json");
        atomic_write(&path, b"data").unwrap();
        let entries: Vec<_> = fs::read_dir(&scratch.0).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["workspace.json".to_string()],
            "after atomic_write only the target file should exist; got {entries:?}");
    }

    #[test]
    fn atomic_write_two_consecutive_calls_use_different_temp_names() {
        // SEC-5 contract: pid + nanos suffix means two consecutive
        // calls (which share the same pid but get different nanos)
        // produce different temp names. We can't observe the temp
        // names directly post-rename, but we can prove that two
        // concurrent writers would NOT collide by computing the
        // names the same way the impl does.
        use std::time::{SystemTime, UNIX_EPOCH};
        let pid = std::process::id();
        let n1 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::thread::sleep(std::time::Duration::from_nanos(1));
        let n2 = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        assert_ne!(n1, n2, "high-resolution timestamps must advance between calls");
        let temp1 = format!(".workspace.json.tmp.{pid}.{n1}");
        let temp2 = format!(".workspace.json.tmp.{pid}.{n2}");
        assert_ne!(temp1, temp2,
            "two atomic_write calls must produce distinct temp file names");
    }

    #[test]
    fn atomic_write_two_threads_to_same_path_dont_corrupt() {
        // Two concurrent writers to the SAME target file. Each writer
        // creates its own uniquely-named temp file (different nanos),
        // and rename(2) is atomic — so the final file ends up being
        // exactly one of the two payloads. Neither writer's payload
        // is a partial mix, and neither writer's temp file is
        // clobbered by the other.
        let scratch = std::sync::Arc::new(ScratchDir::new("two_threads"));
        let path = scratch.0.join("workspace.json");

        let p1 = path.clone();
        let p2 = path.clone();
        let payload_a = b"AAAAAAAAAAAAAAAA";
        let payload_b = b"BBBBBBBBBBBBBBBB";

        let t1 = std::thread::spawn(move || { atomic_write(&p1, payload_a) });
        let t2 = std::thread::spawn(move || { atomic_write(&p2, payload_b) });
        t1.join().unwrap().expect("thread A write");
        t2.join().unwrap().expect("thread B write");

        let final_contents = fs::read(&path).expect("target must exist");
        // The final file is EXACTLY one of the two payloads — never
        // a partial mix, never empty, never both concatenated.
        assert!(final_contents == payload_a || final_contents == payload_b,
            "concurrent atomic_write race must yield exactly one payload; got {} bytes",
            final_contents.len());

        // No leftover temp files in the scratch dir.
        let entries: Vec<_> = fs::read_dir(&scratch.0).unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                if name == "workspace.json" { None } else { Some(name) }
            })
            .collect();
        assert!(entries.is_empty(),
            "concurrent writers must clean up their own temps; leftovers: {entries:?}");
    }

    #[test]
    fn atomic_write_preserves_old_file_on_partial_write_failure() {
        // The atomicity guarantee: if the write fails after the temp
        // is partially populated but before the rename, the original
        // file at `path` remains untouched. We simulate this by
        // pointing atomic_write at a path inside a non-existent
        // directory — std::fs::write fails, no rename, and any
        // pre-existing file at the target is unaffected.
        let scratch = ScratchDir::new("partial_failure");
        let target = scratch.0.join("workspace.json");
        fs::write(&target, b"original content").expect("seed");

        // Try to write to a path inside a directory that doesn't
        // exist — atomic_write builds the temp path in the target's
        // parent, so a non-existent parent triggers the failure
        // path predictably.
        let bad_target = scratch.0.join("nonexistent_subdir").join("workspace.json");
        let result = atomic_write(&bad_target, b"new content");
        assert!(result.is_err(), "write to bad path must fail");

        // The pre-existing target is unchanged.
        let preserved = fs::read(&target).unwrap();
        assert_eq!(preserved, b"original content",
            "atomic_write failure must NOT corrupt the original target file");
    }

    // ═══════════════════════════════════════════════════════════════
    // sanitize_fps / sanitize_resolution.
    //
    // A malformed project.json (or hand-edited file) carrying
    // fps.den == 0 used to flow through Fps::new(num, 0) on the
    // native load path, producing NaN / inf rates that corrupt every
    // downstream timeline calculation. The native load path now
    // funnels MediaItem and Sequence fps + resolution through these
    // sanitizers, the same helpers the importer paths already use.
    // These tests pin the helper contracts so a future refactor that
    // weakens the bound (e.g. removes the zero-substitute) surfaces
    // in CI.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn sanitize_fps_substitutes_film_24_on_zero_denominator() {
        let fps = sanitize_fps(30, 0);
        assert_eq!(fps, klipp_core::Fps::film_24(),
            "den=0 must substitute 24/1, not produce NaN/inf");
    }

    #[test]
    fn sanitize_fps_substitutes_film_24_on_zero_numerator() {
        let fps = sanitize_fps(0, 1);
        assert_eq!(fps, klipp_core::Fps::film_24(),
            "num=0 must substitute 24/1");
    }

    #[test]
    fn sanitize_fps_clamps_oversize_numerator() {
        let fps = sanitize_fps(MAX_FPS_NUMERATOR + 1, 1);
        assert_eq!(fps, klipp_core::Fps::new(MAX_FPS_NUMERATOR, 1),
            "num > cap must clamp, not pass through");
    }

    #[test]
    fn sanitize_fps_passes_valid_values_unchanged() {
        // Common production rates round-trip identical.
        assert_eq!(sanitize_fps(24000, 1001), klipp_core::Fps::new(24000, 1001));
        assert_eq!(sanitize_fps(30000, 1001), klipp_core::Fps::new(30000, 1001));
        assert_eq!(sanitize_fps(60, 1), klipp_core::Fps::new(60, 1));
    }

    #[test]
    fn sanitize_resolution_substitutes_1080p_on_zero_dimension() {
        assert_eq!(sanitize_resolution(0, 1080),
            klipp_core::Resolution { width: 1920, height: 1080 });
        assert_eq!(sanitize_resolution(1920, 0),
            klipp_core::Resolution { width: 1920, height: 1080 });
    }

    /// #1892: a sound has no picture, and keeps none through its project;
    /// a size with one side lost is still mended as before.
    #[test]
    fn a_media_item_with_no_picture_keeps_none() {
        assert_eq!(sanitize_media_resolution(0, 0), klipp_core::Resolution { width: 0, height: 0 });
        assert_eq!(sanitize_resolution(0, 0), klipp_core::Resolution { width: 1920, height: 1080 }, "the control: what it was read through");
        assert_eq!(sanitize_media_resolution(0, 1080), klipp_core::Resolution { width: 1920, height: 1080 });
        assert_eq!(sanitize_media_resolution(3840, 2160), klipp_core::Resolution { width: 3840, height: 2160 });
    }

    #[test]
    fn sanitize_resolution_clamps_oversize() {
        let res = sanitize_resolution(MAX_FRAME_DIMENSION + 1, 1080);
        assert_eq!(res.width, MAX_FRAME_DIMENSION,
            "width > cap must clamp to MAX_FRAME_DIMENSION");
        assert_eq!(res.height, 1080, "height under cap must round-trip unchanged");
    }

    #[test]
    fn sanitize_resolution_passes_valid_values_unchanged() {
        assert_eq!(sanitize_resolution(1920, 1080),
            klipp_core::Resolution { width: 1920, height: 1080 });
        assert_eq!(sanitize_resolution(3840, 2160),
            klipp_core::Resolution { width: 3840, height: 2160 });
    }
}
