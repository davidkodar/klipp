//! Single source of truth for media file extensions Klipp can handle.
//!
//! Before this module existed, every place in the codebase that needed to
//! decide "is this file media?" had its own hardcoded list. The lists drifted
//! apart over time — one was missing `ts`, another was missing `heic`, a third
//! had `tga` that no other list had, etc. Adding a new format like `avif`
//! required touching 9 different files and inevitably missing a few.
//!
//! This module replaces all of that with three canonical constants and a
//! handful of case-insensitive predicates. Every consumer in the workspace
//! now imports from here. To add a new format, change the relevant constant
//! and every call site picks it up automatically.
//!
//! ## Scope
//!
//! Only **media** extensions live here: video containers, audio formats, and
//! still-image formats that Klipp decodes via ffmpeg / image. **Not** included:
//!
//! - Subtitles (`srt`, `vtt`) — separate domain
//! - LUTs (`cube`, `3dl`, `csp`) — separate domain
//! - Project files (`klipp`) — separate domain
//! - Texture-only formats (`tga`) — used by the inspector texture-fill picker
//!   only, kept hardcoded there with a comment
//!
//! ## Case sensitivity
//!
//! All predicates are case-insensitive: `is_video("mp4")`, `is_video("MP4")`,
//! and `is_video("Mp4")` all return `true`. The constants themselves are
//! lowercase by convention; comparisons internally lowercase the input.

use std::path::Path;

/// Video container/codec extensions Klipp can decode and play.
///
/// Order is intentional (most common first) but not load-bearing — consumers
/// should treat the slice as an unordered set.
///
/// `mxf` is the professional container: Sony XDCAM and XAVC, Canon's cinema
/// bodies, Panasonic P2, and Avid's DNxHD/DNxHR masters all arrive in it, as
/// does most broadcast delivery. ffmpeg demuxes it and decodes what is inside,
/// so it needs no separate path here — but note it is a *container*, and one
/// that can legitimately hold essence Klipp has no decoder for. An MXF that
/// wraps JPEG 2000 (a DCP or IMF reel) will be offered for import and then
/// fail to decode, which is the same behaviour as any other container holding
/// an unsupported codec.
///
/// Not `r3d`, `ari` or `braw`: those are camera RAW and need licensed vendor
/// SDKs. See `RAW_EXTENSIONS` below for the stills story, and the project's
/// recorded position on camera RAW as a non-goal.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "mov", "avi", "webm", "m4v", "mts", "ts", "mxf",
];

/// Audio file extensions Klipp can decode and play.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "aac", "flac", "ogg", "m4a", "aiff",
];

/// Still-image extensions Klipp can decode (via ffmpeg or the `image` crate).
///
/// Includes the modern formats `heic`, `heif`, `avif`, and `jxl` because
/// modern phone cameras and the web routinely produce them.
///
/// Also includes the camera RAW formats from [`RAW_EXTENSIONS`] — they're
/// treated as still images by Klipp because we extract their embedded JPEG
/// preview rather than demosaicing the actual raw bayer data. Callers that
/// need to know "is this a raw file that needs special decoding?" should
/// use [`is_raw`] instead of `is_image`.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp", "exr",
    "heic", "heif", "avif", "jxl",
    // Camera RAW formats — actual decode goes through the embedded-JPEG
    // extractor (klipp_engine::extract_raw_preview) instead of ffmpeg.
    "arw", "cr2", "cr3", "nef", "raf", "orf", "rw2", "dng", "pef", "srw", "rwl",
];

/// Camera RAW image extensions. These are a subset of [`IMAGE_EXTENSIONS`] —
/// every entry here is also in `IMAGE_EXTENSIONS` so they get treated as
/// stills throughout the app, but they decode through a different path
/// (embedded JPEG preview extraction, not ffmpeg) because none of the
/// proprietary RAW codecs are supported by stock ffmpeg.
///
/// Callers that build a thumbnail / poster decode pipeline should check
/// `is_raw(ext)` first and dispatch to `klipp_engine::extract_raw_preview`,
/// then fall back to the regular ffmpeg image path for everything else.
///
/// **Important**: the current implementation extracts the camera-baked
/// JPEG preview from these files, NOT the demosaiced raw bayer data. This
/// is fine for thumbnails / browsing / drag-into-timeline workflows but
/// it means there's no "raw editing" capability — white balance, exposure
/// recovery, etc. are baked in. See `klipp_engine::extract_raw_preview`'s
/// doc comment for the full design rationale and the upgrade path to real
/// RAW decoding via `rawler` or `libraw` if Klipp ever needs that.
pub const RAW_EXTENSIONS: &[&str] = &[
    "arw",  // Sony Alpha
    "cr2",  // Canon (older)
    "cr3",  // Canon (newer)
    "nef",  // Nikon
    "raf",  // Fujifilm
    "orf",  // Olympus
    "rw2",  // Panasonic
    "dng",  // Adobe Digital Negative (also produced by some phones)
    "pef",  // Pentax
    "srw",  // Samsung
    "rwl",  // Leica
];

/// Build a `Vec` containing every media extension from all three categories.
///
/// Used by file picker dialogs that show one combined "All media" filter.
/// Returns a `Vec` (not a slice) because the union is computed at call time —
/// dialog APIs (e.g. `rfd::FileDialog::add_filter`) accept `&[&str]`, so
/// callers can borrow the vec.
///
/// The order is video, then audio, then image (matching the order most pro
/// NLEs show in their import dialogs). Length = sum of the three constants
/// with no duplicates (verified by `no_cross_domain_duplicates` test).
pub fn all_media_extensions() -> Vec<&'static str> {
    let mut out = Vec::with_capacity(
        VIDEO_EXTENSIONS.len() + AUDIO_EXTENSIONS.len() + IMAGE_EXTENSIONS.len(),
    );
    out.extend_from_slice(VIDEO_EXTENSIONS);
    out.extend_from_slice(AUDIO_EXTENSIONS);
    out.extend_from_slice(IMAGE_EXTENSIONS);
    out
}

/// Returns `true` if the extension (without leading dot) is a video container.
/// Case-insensitive — `is_video("mp4")` and `is_video("MP4")` both return true.
pub fn is_video(ext: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    let lower = ext.to_lowercase();
    VIDEO_EXTENSIONS.contains(&lower.as_str())
}

/// Returns `true` if the extension is a known audio format. Case-insensitive.
pub fn is_audio(ext: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    let lower = ext.to_lowercase();
    AUDIO_EXTENSIONS.contains(&lower.as_str())
}

/// Returns `true` if the extension is a known still-image format. Case-insensitive.
/// Includes RAW formats — see [`is_raw`] if you need to distinguish them.
pub fn is_image(ext: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    let lower = ext.to_lowercase();
    IMAGE_EXTENSIONS.contains(&lower.as_str())
}

/// Returns `true` if the extension is a camera RAW format that needs to be
/// decoded via embedded-JPEG preview extraction rather than ffmpeg.
/// Case-insensitive.
///
/// Every RAW extension is also a member of [`IMAGE_EXTENSIONS`], so any
/// caller that just wants to know "should this file appear in the image
/// filter / pool / browser?" should use `is_image` instead. Use `is_raw`
/// only when you're about to decode and need to choose the right code path.
pub fn is_raw(ext: &str) -> bool {
    if ext.is_empty() {
        return false;
    }
    let lower = ext.to_lowercase();
    RAW_EXTENSIONS.contains(&lower.as_str())
}

/// Returns `true` if the extension belongs to any of the three media categories.
pub fn is_media(ext: &str) -> bool {
    is_video(ext) || is_audio(ext) || is_image(ext)
}

/// Path-aware variant of [`is_video`]. Extracts the file's extension and
/// checks it case-insensitively. Returns `false` for paths with no extension
/// (e.g. `Makefile`, `README`).
pub fn path_is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_video)
        .unwrap_or(false)
}

/// Path-aware variant of [`is_audio`].
pub fn path_is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_audio)
        .unwrap_or(false)
}

/// Path-aware variant of [`is_image`].
pub fn path_is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_image)
        .unwrap_or(false)
}

/// Path-aware variant of [`is_raw`].
pub fn path_is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_raw)
        .unwrap_or(false)
}

/// Path-aware variant of [`is_media`].
pub fn path_is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(is_media)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Constants baseline ────────────────────────────────────────

    #[test]
    fn video_extensions_baseline() {
        // Lock in the expected video set. If this test fails, either the
        // baseline was intentionally changed (update the test) or someone
        // accidentally removed an extension.
        let expected: &[&str] = &[
            "mp4", "mkv", "mov", "avi", "webm", "m4v", "mts", "ts", "mxf",
        ];
        assert_eq!(VIDEO_EXTENSIONS, expected);
    }

    #[test]
    fn mxf_is_video_and_not_a_still() {
        // The professional container. Every predicate has to agree about it,
        // because the file browser gates on `is_media` and the decode path
        // dispatches on `is_raw` — an MXF landing in the stills path would
        // try the embedded-JPEG extractor and find nothing.
        for spelling in ["mxf", "MXF", "Mxf"] {
            assert!(is_video(spelling), "{spelling} should be video");
            assert!(is_media(spelling), "{spelling} should be media");
            assert!(!is_image(spelling), "{spelling} is not a still");
            assert!(!is_raw(spelling), "{spelling} is not camera RAW");
            assert!(!is_audio(spelling), "{spelling} is not audio-only");
        }
        // A Canon/Sony card path, as the sidecar reader already expects to see
        // one (`klipp-media/src/sidecar.rs` tests exactly this filename).
        let card = PathBuf::from("/cards/A001/CONTENTS/A001C001_240315_XYZ.MXF");
        assert!(path_is_media(&card));
        assert!(path_is_video(&card));
        assert!(!path_is_raw(&card));
    }

    #[test]
    fn audio_extensions_baseline() {
        let expected: &[&str] = &[
            "wav", "mp3", "aac", "flac", "ogg", "m4a", "aiff",
        ];
        assert_eq!(AUDIO_EXTENSIONS, expected);
    }

    #[test]
    fn image_extensions_baseline() {
        // Includes the modern phone/web formats AND camera RAW formats.
        // The RAW formats are decoded via embedded-JPEG preview extraction,
        // not ffmpeg — see klipp_engine::extract_raw_preview.
        let expected: &[&str] = &[
            "png", "jpg", "jpeg", "tiff", "tif", "bmp", "webp", "exr",
            "heic", "heif", "avif", "jxl",
            "arw", "cr2", "cr3", "nef", "raf", "orf", "rw2", "dng", "pef", "srw", "rwl",
        ];
        assert_eq!(IMAGE_EXTENSIONS, expected);
    }

    #[test]
    fn raw_extensions_baseline() {
        let expected: &[&str] = &[
            "arw", "cr2", "cr3", "nef", "raf", "orf", "rw2", "dng", "pef", "srw", "rwl",
        ];
        assert_eq!(RAW_EXTENSIONS, expected);
    }

    #[test]
    fn every_raw_is_also_an_image() {
        // Critical invariant: RAW_EXTENSIONS must be a subset of
        // IMAGE_EXTENSIONS so RAW files are treated as still images
        // throughout the app. If this fails, the file browser will
        // fail to detect RAW files as media.
        for &raw in RAW_EXTENSIONS {
            assert!(
                IMAGE_EXTENSIONS.contains(&raw),
                "RAW extension '{}' is not in IMAGE_EXTENSIONS",
                raw
            );
        }
    }

    #[test]
    fn is_raw_accepts_known_formats() {
        for &ext in RAW_EXTENSIONS {
            assert!(is_raw(ext), "is_raw should accept {}", ext);
        }
    }

    #[test]
    fn is_raw_case_insensitive() {
        assert!(is_raw("arw"));
        assert!(is_raw("ARW"));
        assert!(is_raw("Arw"));
        assert!(is_raw("CR2"));
        assert!(is_raw("dng"));
        assert!(is_raw("DNG"));
    }

    #[test]
    fn is_raw_rejects_non_raw() {
        // Standard images are not RAW.
        assert!(!is_raw("png"));
        assert!(!is_raw("jpg"));
        assert!(!is_raw("heic"));
        // Other categories.
        assert!(!is_raw("mp4"));
        assert!(!is_raw("wav"));
        // Unrelated.
        assert!(!is_raw("txt"));
        assert!(!is_raw(""));
    }

    #[test]
    fn is_image_accepts_raw_formats() {
        // Sanity: every RAW format must also pass is_image.
        for &raw in RAW_EXTENSIONS {
            assert!(is_image(raw), "is_image should accept RAW '{}'", raw);
        }
    }

    #[test]
    fn path_is_raw_handles_real_paths() {
        use std::path::PathBuf;
        assert!(path_is_raw(&PathBuf::from("/photos/DSC01393.ARW")));
        assert!(path_is_raw(&PathBuf::from("DSC_0001.NEF")));
        assert!(path_is_raw(&PathBuf::from("IMG_1234.cr3")));
        assert!(!path_is_raw(&PathBuf::from("/photos/IMG.jpg")));
        assert!(!path_is_raw(&PathBuf::from("/tmp/Makefile")));
    }

    // ── Cross-domain integrity ────────────────────────────────────

    #[test]
    fn no_cross_domain_duplicates() {
        // Critical invariant: no extension may belong to more than one
        // category. If this fails, predicates become ambiguous.
        use std::collections::HashSet;
        let mut seen: HashSet<&str> = HashSet::new();
        for &ext in VIDEO_EXTENSIONS.iter()
            .chain(AUDIO_EXTENSIONS.iter())
            .chain(IMAGE_EXTENSIONS.iter())
        {
            assert!(
                seen.insert(ext),
                "extension '{}' appears in more than one category",
                ext
            );
        }
    }

    #[test]
    fn no_intra_list_duplicates() {
        // Each list itself must not have duplicates (regression for
        // accidental copy/paste mistakes when adding new formats).
        for (name, list) in [
            ("VIDEO_EXTENSIONS", VIDEO_EXTENSIONS),
            ("AUDIO_EXTENSIONS", AUDIO_EXTENSIONS),
            ("IMAGE_EXTENSIONS", IMAGE_EXTENSIONS),
        ] {
            use std::collections::HashSet;
            let unique: HashSet<&&str> = list.iter().collect();
            assert_eq!(
                unique.len(),
                list.len(),
                "{name} contains duplicate entries"
            );
        }
    }

    #[test]
    fn all_lowercase_in_constants() {
        // The case-insensitive predicates assume the constants are lowercase.
        // Defend that assumption.
        for &ext in VIDEO_EXTENSIONS.iter()
            .chain(AUDIO_EXTENSIONS.iter())
            .chain(IMAGE_EXTENSIONS.iter())
        {
            assert_eq!(
                ext, ext.to_lowercase(),
                "extension '{}' is not lowercase",
                ext
            );
        }
    }

    // ── all_media_extensions ──────────────────────────────────────

    #[test]
    fn all_media_extensions_is_complete_union() {
        let all = all_media_extensions();
        // Length must equal the sum of the three lists with no duplicates.
        assert_eq!(
            all.len(),
            VIDEO_EXTENSIONS.len() + AUDIO_EXTENSIONS.len() + IMAGE_EXTENSIONS.len()
        );
        // Every entry from each list must appear.
        for &ext in VIDEO_EXTENSIONS {
            assert!(all.contains(&ext), "missing video ext: {}", ext);
        }
        for &ext in AUDIO_EXTENSIONS {
            assert!(all.contains(&ext), "missing audio ext: {}", ext);
        }
        for &ext in IMAGE_EXTENSIONS {
            assert!(all.contains(&ext), "missing image ext: {}", ext);
        }
    }

    // ── is_video / is_audio / is_image (string predicates) ────────

    #[test]
    fn is_video_accepts_known_formats() {
        for &ext in VIDEO_EXTENSIONS {
            assert!(is_video(ext), "is_video should accept {}", ext);
        }
    }

    #[test]
    fn is_video_case_insensitive() {
        assert!(is_video("mp4"));
        assert!(is_video("MP4"));
        assert!(is_video("Mp4"));
        assert!(is_video("mP4"));
    }

    #[test]
    fn is_video_rejects_other_categories() {
        assert!(!is_video("wav"));
        assert!(!is_video("png"));
        assert!(!is_video("heic"));
    }

    #[test]
    fn is_video_rejects_non_media() {
        assert!(!is_video("txt"));
        assert!(!is_video("json"));
        assert!(!is_video("srt"));
        assert!(!is_video("klipp"));
    }

    #[test]
    fn is_audio_accepts_known_formats() {
        for &ext in AUDIO_EXTENSIONS {
            assert!(is_audio(ext), "is_audio should accept {}", ext);
        }
    }

    #[test]
    fn is_audio_case_insensitive() {
        assert!(is_audio("wav"));
        assert!(is_audio("WAV"));
        assert!(is_audio("FLAC"));
    }

    #[test]
    fn is_audio_rejects_other_categories() {
        assert!(!is_audio("mp4"));
        assert!(!is_audio("png"));
        assert!(!is_audio("heic"));
    }

    #[test]
    fn is_image_accepts_known_formats() {
        for &ext in IMAGE_EXTENSIONS {
            assert!(is_image(ext), "is_image should accept {}", ext);
        }
    }

    #[test]
    fn is_image_accepts_modern_formats_regression() {
        // The original HEIC bug — make sure these never silently disappear.
        assert!(is_image("heic"));
        assert!(is_image("heif"));
        assert!(is_image("avif"));
        assert!(is_image("jxl"));
        assert!(is_image("HEIC"));
        assert!(is_image("Heic"));
        assert!(is_image("HEIF"));
        assert!(is_image("AVIF"));
    }

    #[test]
    fn is_image_rejects_other_categories() {
        assert!(!is_image("mp4"));
        assert!(!is_image("wav"));
    }

    // ── is_media (combined) ───────────────────────────────────────

    #[test]
    fn is_media_accepts_all_three_categories() {
        assert!(is_media("mp4"));
        assert!(is_media("wav"));
        assert!(is_media("png"));
        assert!(is_media("heic"));
    }

    #[test]
    fn is_media_case_insensitive() {
        assert!(is_media("MP4"));
        assert!(is_media("WAV"));
        assert!(is_media("PNG"));
        assert!(is_media("HEIC"));
    }

    #[test]
    fn is_media_rejects_unrelated() {
        assert!(!is_media("txt"));
        assert!(!is_media("json"));
        assert!(!is_media("srt"));    // subtitle, separate domain
        assert!(!is_media("klipp"));  // project file, separate domain
        assert!(!is_media("cube"));   // LUT, separate domain
        assert!(!is_media("tga"));    // texture-only, intentionally excluded
    }

    #[test]
    fn empty_extension_returns_false() {
        assert!(!is_video(""));
        assert!(!is_audio(""));
        assert!(!is_image(""));
        assert!(!is_media(""));
    }

    // ── Path-aware predicates ─────────────────────────────────────

    #[test]
    fn path_is_video_basic() {
        assert!(path_is_video(&PathBuf::from("/tmp/video.mp4")));
        assert!(path_is_video(&PathBuf::from("clip.MOV")));
        assert!(!path_is_video(&PathBuf::from("/tmp/audio.wav")));
    }

    #[test]
    fn path_is_video_no_extension() {
        // Files with no extension (Makefile, README, etc.) must return false,
        // not panic.
        assert!(!path_is_video(&PathBuf::from("Makefile")));
        assert!(!path_is_video(&PathBuf::from("/tmp/Makefile")));
        assert!(!path_is_video(&PathBuf::from("README")));
    }

    #[test]
    fn path_is_image_uppercase_extension() {
        // The HEIC files from the user's audit had .HEIC uppercase variants.
        assert!(path_is_image(&PathBuf::from("/photos/IMG_5147.HEIC")));
        assert!(path_is_image(&PathBuf::from("photo.HEIC")));
        assert!(path_is_image(&PathBuf::from("/tmp/image.PNG")));
    }

    #[test]
    fn path_is_image_dotfile_with_no_real_extension() {
        // ".heic" as a filename — Path::extension returns None for dotfiles
        // (no stem before the dot). Must not panic.
        let p = PathBuf::from(".heic");
        let _ = path_is_image(&p);
    }

    #[test]
    fn path_with_unicode_filename_does_not_panic() {
        // The user has Swedish-named files like "Pappa röker pipa.mp4".
        // Make sure unicode in the path doesn't break extension extraction.
        assert!(path_is_video(&PathBuf::from("/home/david/Pappa röker pipa!.mp4")));
        assert!(path_is_image(&PathBuf::from("/photos/Tāves mörmor.JPG")));
        assert!(path_is_audio(&PathBuf::from("/musik/röst.wav")));
    }

    #[test]
    fn path_is_media_returns_true_for_any_category() {
        assert!(path_is_media(&PathBuf::from("video.mp4")));
        assert!(path_is_media(&PathBuf::from("song.flac")));
        assert!(path_is_media(&PathBuf::from("photo.heic")));
        assert!(!path_is_media(&PathBuf::from("notes.txt")));
        assert!(!path_is_media(&PathBuf::from("Makefile")));
    }

    #[test]
    fn path_with_double_extension_uses_last_segment() {
        // Path::extension returns the part after the LAST dot. Verify the
        // predicate matches that behavior.
        assert!(path_is_video(&PathBuf::from("video.tar.mp4")));
        assert!(!path_is_video(&PathBuf::from("video.mp4.bak")));
    }

    #[test]
    fn path_in_directory_with_dot_in_name() {
        // Edge case: directory with a dot in its name.
        assert!(path_is_video(&PathBuf::from("/home/user/My.Movies/clip.mp4")));
        assert!(!path_is_video(&PathBuf::from("/home/user/foo.mp4/notes")));
    }
}
