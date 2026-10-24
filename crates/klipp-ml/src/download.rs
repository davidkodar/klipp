//! HTTPS model downloader with streaming SHA-256 verification.
//!
//! ## Design
//!
//! The verification core ([`write_and_verify`]) is generic over any
//! [`std::io::Read`], so unit tests exercise it with `Cursor<Vec<u8>>`
//! and integration tests with a real `TcpStream` against a localhost
//! server. The HTTP layer ([`download_to_cache`]) is a thin wrapper
//! that opens the URL with `ureq`, hands the body reader to
//! `write_and_verify`, and falls through to the next source on
//! failure.
//!
//! ## Pre-flight HEAD request
//!
//! Before committing to the body, [`download_to_cache`] sends a HEAD
//! request and compares `Content-Length` against the manifest's
//! `size_bytes` — within ±5% tolerance, since some CDNs return
//! slightly different sizes for transcoded responses, but a 100×
//! mismatch means something is very wrong (DoS via huge file, wrong
//! URL, etc.) and we abort before downloading a single byte.
//!
//! ## Atomic rename
//!
//! Bytes stream into `<id>.bin.partial`. On a clean SHA-256 match,
//! the partial file is renamed to `<id>.bin` (atomic on the same
//! filesystem). On any failure, the partial is left in place — a
//! retry can resume manually if the user wants, and the obvious-
//! debris filename means the cache directory tells the truth.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::cache::ModelCache;
use crate::manifest::ModelEntry;

/// Tolerance for the pre-flight Content-Length vs manifest size_bytes
/// comparison + the final actual-bytes comparison. 5% catches "they
/// served us a 1 GB file when we expected 200 MB" without rejecting
/// CDNs that round or recompress slightly.
const SIZE_TOLERANCE_FRACTION: f64 = 0.05;

/// IO buffer size for streaming. 64 KiB is a sweet spot — small
/// enough that progress updates feel responsive, large enough that
/// per-chunk overhead is negligible.
const STREAM_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download cancelled by user")]
    Cancelled,
    #[error("manifest entry {id:?} has size_bytes = 0 (placeholder, not yet populated by release CI)")]
    UnfilledPlaceholder { id: String },
    #[error("manifest entry has no sources")]
    NoSources,
    #[error("downloaded {actual} bytes; manifest expects {expected} (±{tolerance}%)")]
    SizeMismatch { expected: u64, actual: u64, tolerance: f64 },
    #[error("HEAD pre-flight: server reports Content-Length {actual}; manifest expects {expected} (±{tolerance}%)")]
    PreflightSizeMismatch { expected: u64, actual: u64, tolerance: f64 },
    #[error("downloaded SHA-256 {actual:?} does not match manifest's {expected:?}")]
    HashMismatch { expected: String, actual: String },
    #[error("all {count} download sources failed; last error: {last}")]
    AllSourcesFailed { count: usize, last: String },
    #[error("http error: {0}")]
    Http(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Progress callback hook. The downloader calls these as bytes flow
/// through; the UI implements the trait to drive a progress bar.
/// Unit tests can use [`NoOpProgress`] to ignore.
pub trait ProgressSink {
    /// Called once at the start of a download with the total expected
    /// byte count from the manifest.
    fn started(&mut self, total_bytes: u64);
    /// Called repeatedly with byte deltas (NOT cumulative). The UI
    /// adds these to its own counter.
    fn advance(&mut self, bytes_delta: u64);
    /// Called once at the end of a successful download.
    fn finished(&mut self);
}

/// `ProgressSink` impl that drops every event. Used by tests + headless
/// callers (CLI, future scripting hook) that don't want UI feedback.
pub struct NoOpProgress;
impl ProgressSink for NoOpProgress {
    fn started(&mut self, _: u64) {}
    fn advance(&mut self, _: u64) {}
    fn finished(&mut self) {}
}

/// Bounds-check `actual` against `expected` with the configured
/// tolerance. Returns true when `actual` is within
/// `expected ± expected*tolerance`.
fn within_size_tolerance(expected: u64, actual: u64) -> bool {
    if expected == 0 {
        return false;
    }
    let band = (expected as f64 * SIZE_TOLERANCE_FRACTION).ceil() as u64;
    let lo = expected.saturating_sub(band);
    let hi = expected.saturating_add(band);
    actual >= lo && actual <= hi
}

/// Stream bytes from `reader` to `target_partial`, computing SHA-256
/// on the fly. On success: rename `target_partial` → `target_final`.
/// On any failure: leave `target_partial` in place for inspection,
/// return an error. Honors `cancel` between chunks.
///
/// `expected_size_bytes` may be 0, which means "size unknown / sentinel
/// placeholder" and triggers [`DownloadError::UnfilledPlaceholder`] —
/// callers using a freshly-shipped manifest before release CI fills
/// it in get a clear error rather than a confusing partial download.
///
/// This is the testable core. Integration tests pass a `Cursor<Vec<u8>>`
/// or a real `TcpStream`; production code passes a ureq response body.
pub fn write_and_verify<R: Read>(
    reader: &mut R,
    expected_sha256: &str,
    expected_size_bytes: u64,
    target_partial: &Path,
    target_final: &Path,
    progress: &mut dyn ProgressSink,
    cancel: &AtomicBool,
    entry_id_for_errors: &str,
) -> Result<(), DownloadError> {
    if expected_size_bytes == 0 {
        return Err(DownloadError::UnfilledPlaceholder {
            id: entry_id_for_errors.to_string(),
        });
    }

    progress.started(expected_size_bytes);

    let mut file = std::fs::File::create(target_partial)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
    let mut total: u64 = 0;
    // 5% tolerance band → cap the absolute total we'll accept BEFORE
    // bailing. Protects against a malicious server streaming forever
    // with chunked encoding even if HEAD said something reasonable.
    let band = (expected_size_bytes as f64 * SIZE_TOLERANCE_FRACTION).ceil() as u64;
    let cap = expected_size_bytes.saturating_add(band);

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > cap {
            return Err(DownloadError::SizeMismatch {
                expected: expected_size_bytes,
                actual: total,
                tolerance: SIZE_TOLERANCE_FRACTION * 100.0,
            });
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        progress.advance(n as u64);
    }

    file.sync_all()?;
    drop(file); // close before rename so Windows is happy

    if !within_size_tolerance(expected_size_bytes, total) {
        return Err(DownloadError::SizeMismatch {
            expected: expected_size_bytes,
            actual: total,
            tolerance: SIZE_TOLERANCE_FRACTION * 100.0,
        });
    }

    let actual_hash = hex::encode(hasher.finalize());
    if !actual_hash.eq_ignore_ascii_case(expected_sha256) {
        return Err(DownloadError::HashMismatch {
            expected: expected_sha256.to_string(),
            actual: actual_hash,
        });
    }

    std::fs::rename(target_partial, target_final)?;
    progress.finished();
    Ok(())
}

/// Stream bytes from `reader` to `target_partial`, hashing on the
/// fly, with NO expected size or hash. Used by the custom-URL paste
/// flow where neither value is known up front — the user gets the
/// computed values back and confirms before Klipp registers the
/// file as a custom model. The renamed-on-success behaviour matches
/// [`write_and_verify`].
///
/// `max_bytes_cap` is a defensive bound: if the URL serves an
/// arbitrarily long stream, we abort once the response crosses
/// this. Caller picks something generous (a few GB) — the goal is
/// to catch hostile servers, not legitimate large models.
///
/// On success, returns `(actual_sha256_hex_lowercase, total_bytes)`.
/// On any failure: leaves `target_partial` in place for inspection.
pub fn write_and_compute<R: Read>(
    reader: &mut R,
    target_partial: &Path,
    target_final: &Path,
    progress: &mut dyn ProgressSink,
    cancel: &AtomicBool,
    max_bytes_cap: u64,
) -> Result<(String, u64), DownloadError> {
    progress.started(0);

    let mut file = std::fs::File::create(target_partial)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
    let mut total: u64 = 0;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > max_bytes_cap {
            return Err(DownloadError::SizeMismatch {
                expected: max_bytes_cap,
                actual: total,
                tolerance: 0.0,
            });
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        progress.advance(n as u64);
    }

    file.sync_all()?;
    drop(file);

    let actual_hash = hex::encode(hasher.finalize());
    std::fs::rename(target_partial, target_final)?;
    progress.finished();
    Ok((actual_hash, total))
}

/// Try each source in `entry.sources` until one produces bytes that
/// pass [`write_and_verify`]. Returns the resolved path on success.
///
/// HEAD pre-flight runs against each source before the body GET; a
/// source whose HEAD fails or whose Content-Length is wildly off
/// the manifest is skipped silently and we move on to the next URL.
/// (HEAD failures don't poison the source — some CDNs reject HEAD
/// outright. We treat them as "couldn't pre-flight, try the GET
/// anyway"; the streaming size guard catches a mismatched body.)
pub fn download_to_cache(
    cache: &ModelCache,
    entry: &ModelEntry,
    progress: &mut dyn ProgressSink,
    cancel: &AtomicBool,
) -> Result<PathBuf, DownloadError> {
    if entry.sources.is_empty() {
        return Err(DownloadError::NoSources);
    }
    if entry.size_bytes == 0 {
        return Err(DownloadError::UnfilledPlaceholder {
            id: entry.id.clone(),
        });
    }

    let target_final = cache.path_for(entry);
    let target_partial = cache.partial_path_for(entry);

    let mut last_err: Option<String> = None;

    for source in &entry.sources {
        if cancel.load(Ordering::Relaxed) {
            return Err(DownloadError::Cancelled);
        }

        // HEAD pre-flight — best-effort. A failed HEAD doesn't poison
        // the source; we just proceed to the GET and rely on the
        // streaming size guard.
        if let Some(content_length) = head_content_length(&source.url) {
            if !within_size_tolerance(entry.size_bytes, content_length) {
                last_err = Some(format!(
                    "preflight size mismatch from {}: expected {} got {}",
                    source.url, entry.size_bytes, content_length,
                ));
                tracing::warn!(
                    "klipp-ml: skipping source {} due to size mismatch (expected {}, got {})",
                    source.url, entry.size_bytes, content_length,
                );
                continue;
            }
        }

        // GET + stream + verify.
        let result = ureq::get(&source.url).call();
        match result {
            Ok(resp) => {
                let mut reader = resp.into_reader();
                match write_and_verify(
                    &mut reader,
                    &entry.sha256,
                    entry.size_bytes,
                    &target_partial,
                    &target_final,
                    progress,
                    cancel,
                    &entry.id,
                ) {
                    Ok(()) => return Ok(target_final),
                    Err(DownloadError::Cancelled) => return Err(DownloadError::Cancelled),
                    Err(e) => {
                        last_err = Some(format!("{} from {}: {e}", entry.id, source.url));
                        // Wipe the partial so the next source starts
                        // clean. Best-effort; a leftover partial is
                        // self-healing on the next attempt.
                        let _ = std::fs::remove_file(&target_partial);
                        tracing::warn!(
                            "klipp-ml: source {} failed for {}: {e}",
                            source.url, entry.id,
                        );
                    }
                }
            }
            Err(e) => {
                last_err = Some(format!("http error from {}: {e}", source.url));
                tracing::warn!("klipp-ml: source {} failed: {e}", source.url);
            }
        }
    }

    Err(DownloadError::AllSourcesFailed {
        count: entry.sources.len(),
        last: last_err.unwrap_or_else(|| "no error captured".into()),
    })
}

/// Fetch a single URL into the cache directly, without an existing
/// manifest entry. Computes SHA + size on the fly via
/// [`write_and_compute`] and returns them plus the resolved path so
/// the caller can show a confirmation dialog before registering the
/// file as a custom model.
///
/// `target_partial` and `target_final` are caller-supplied so the
/// custom-URL flow can place files under a `custom/` subdirectory
/// of the cache without needing a `ModelEntry`. `max_bytes_cap`
/// guards against hostile servers; pick something generous (a few
/// GB) — the goal is "no infinite stream", not enforcement of any
/// real expected size.
pub fn download_url_uncertified(
    url: &str,
    target_partial: &Path,
    target_final: &Path,
    progress: &mut dyn ProgressSink,
    cancel: &AtomicBool,
    max_bytes_cap: u64,
) -> Result<(String, u64), DownloadError> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| DownloadError::Http(format!("GET {url}: {e}")))?;
    let mut reader = resp.into_reader();
    write_and_compute(
        &mut reader,
        target_partial,
        target_final,
        progress,
        cancel,
        max_bytes_cap,
    )
    .inspect_err(|_| {
        let _ = std::fs::remove_file(target_partial);
    })
}

/// Issue a HEAD against `url`. Returns the Content-Length on success;
/// `None` on any failure (network, non-2xx, missing header). Callers
/// treat None as "couldn't pre-flight, proceed cautiously" rather
/// than as a fatal error.
///
/// Public so the custom-URL paste flow can sniff the size before
/// streaming the body — fast-fails an oversized download without
/// committing to even one byte of the actual file, and lets the UI
/// show a determinate progress bar from the first chunk.
pub fn head_content_length(url: &str) -> Option<u64> {
    match ureq::head(url).call() {
        Ok(resp) => resp.header("Content-Length").and_then(|s| s.parse().ok()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Source;
    use std::io::Cursor;
    use std::sync::Arc;

    fn make_entry(id: &str, sha256: &str, size: u64, sources: Vec<&str>) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            display_name: id.into(),
            size_bytes: size,
            sha256: sha256.into(),
            license: "Apache-2.0".into(),
            attribution: "test".into(),
            sources: sources.into_iter().map(|s| Source { url: s.into() }).collect(),
            kind: crate::manifest::ModelKind::default(),
            runtime: crate::manifest::ModelRuntime::default(),
            prompts: crate::manifest::PromptKinds::empty(),
            with: Vec::new(),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        hex::encode(h.finalize())
    }

    /// `CountingProgress` records the calls made into it. Used to
    /// pin that the lifecycle is correct: started → advance* → finished.
    #[derive(Default)]
    struct CountingProgress {
        started_total: Option<u64>,
        advanced: u64,
        finished: bool,
    }
    impl ProgressSink for CountingProgress {
        fn started(&mut self, total_bytes: u64) {
            self.started_total = Some(total_bytes);
        }
        fn advance(&mut self, bytes_delta: u64) {
            self.advanced = self.advanced.saturating_add(bytes_delta);
        }
        fn finished(&mut self) {
            self.finished = true;
        }
    }

    // ─── write_and_verify unit tests ──────────────────────────────

    #[test]
    fn happy_path_writes_renames_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"klipp test bytes for the model file";
        let hash = sha256_hex(bytes);
        let partial = dir.path().join("m.partial");
        let final_ = dir.path().join("m.bin");
        let mut reader = Cursor::new(bytes.to_vec());
        let mut prog = CountingProgress::default();
        let cancel = AtomicBool::new(false);

        write_and_verify(&mut reader, &hash, bytes.len() as u64,
            &partial, &final_, &mut prog, &cancel, "happy_path",
        ).expect("ok");

        assert!(final_.exists());
        assert!(!partial.exists(), "partial must be gone after rename");
        assert_eq!(std::fs::read(&final_).unwrap(), bytes);
        assert_eq!(prog.started_total, Some(bytes.len() as u64));
        assert_eq!(prog.advanced, bytes.len() as u64);
        assert!(prog.finished);
    }

    #[test]
    fn hash_mismatch_leaves_partial_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"actual bytes";
        let wrong_hash = sha256_hex(b"different bytes");
        let partial = dir.path().join("m.partial");
        let final_ = dir.path().join("m.bin");
        let mut reader = Cursor::new(bytes.to_vec());
        let cancel = AtomicBool::new(false);

        let err = write_and_verify(&mut reader, &wrong_hash, bytes.len() as u64,
            &partial, &final_, &mut NoOpProgress, &cancel, "hash_mismatch",
        ).expect_err("should reject");
        assert!(matches!(err, DownloadError::HashMismatch { .. }));
        assert!(!final_.exists(), "no final file on hash failure");
        assert!(partial.exists(), "partial preserved for inspection");
    }

    #[test]
    fn size_overshoot_aborts_streaming() {
        // Stream more bytes than expected — the inline cap should
        // abort mid-stream, NOT after writing the entire payload.
        // Tests the DoS guard against a malicious server sending
        // forever.
        let dir = tempfile::tempdir().unwrap();
        // expected = 100 bytes; tolerance = 5% → cap at 105 bytes.
        // We feed 200 bytes; the stream should bail before completion.
        let bytes = vec![0xABu8; 200];
        let any_hash = "0".repeat(64);
        let partial = dir.path().join("m.partial");
        let final_ = dir.path().join("m.bin");
        let mut reader = Cursor::new(bytes);
        let cancel = AtomicBool::new(false);

        let err = write_and_verify(&mut reader, &any_hash, 100,
            &partial, &final_, &mut NoOpProgress, &cancel, "oversize",
        ).expect_err("should reject");
        assert!(matches!(err, DownloadError::SizeMismatch { .. }),
            "expected SizeMismatch, got {err:?}");
        assert!(!final_.exists());
    }

    #[test]
    fn size_undershoot_after_complete_read_errors() {
        // EOF reached but total < expected - tolerance. Should
        // surface as SizeMismatch (not HashMismatch — it's a
        // different bug class).
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![0u8; 50]; // expected 100, got 50
        let any_hash = sha256_hex(&bytes); // hash matches what we sent
        let partial = dir.path().join("m.partial");
        let final_ = dir.path().join("m.bin");
        let mut reader = Cursor::new(bytes);
        let cancel = AtomicBool::new(false);

        let err = write_and_verify(&mut reader, &any_hash, 100,
            &partial, &final_, &mut NoOpProgress, &cancel, "undersize",
        ).expect_err("should reject");
        assert!(matches!(err, DownloadError::SizeMismatch { .. }));
    }

    #[test]
    fn cancel_mid_stream_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![0u8; STREAM_CHUNK_BYTES * 4]; // 4 chunks worth
        let any_hash = "0".repeat(64);
        let partial = dir.path().join("m.partial");
        let final_ = dir.path().join("m.bin");
        let mut reader = Cursor::new(bytes);
        let cancel = Arc::new(AtomicBool::new(true)); // cancel BEFORE first read
        let cancel_ref: &AtomicBool = &cancel;

        let err = write_and_verify(&mut reader, &any_hash, 256_000,
            &partial, &final_, &mut NoOpProgress, cancel_ref, "cancelled",
        ).expect_err("should cancel");
        assert!(matches!(err, DownloadError::Cancelled));
        assert!(!final_.exists());
    }

    #[test]
    fn placeholder_size_zero_rejects_with_clear_error() {
        // Catches the shipping manifest case where release CI hasn't
        // populated real size_bytes yet. User clicks "Use AI Mask",
        // sees a clear "manifest not yet populated" error rather
        // than a confusing partial download.
        let dir = tempfile::tempdir().unwrap();
        let mut reader: Cursor<Vec<u8>> = Cursor::new(vec![]);
        let cancel = AtomicBool::new(false);

        let err = write_and_verify(&mut reader,
            "0000000000000000000000000000000000000000000000000000000000000000",
            0,
            &dir.path().join("p"), &dir.path().join("f"),
            &mut NoOpProgress, &cancel, "placeholder",
        ).expect_err("should reject");
        assert!(matches!(err, DownloadError::UnfilledPlaceholder { .. }));
    }

    #[test]
    fn within_size_tolerance_accepts_5_percent_drift() {
        assert!(within_size_tolerance(1000, 1000));
        assert!(within_size_tolerance(1000, 950));   // 5% under: ok
        assert!(within_size_tolerance(1000, 1050));  // 5% over: ok
        assert!(!within_size_tolerance(1000, 800));  // 20% under: reject
        assert!(!within_size_tolerance(1000, 1500)); // 50% over: reject
        assert!(!within_size_tolerance(0, 100));     // expected=0: always reject
    }

    // ─── download_to_cache integration test (localhost) ───────────

    /// One-shot HTTP server: accepts a single TCP connection on a
    /// random port, swallows the request, returns `body` with a
    /// proper Content-Length header. Returns the bound port.
    /// Spawns a thread; the server thread exits after one request.
    fn spawn_oneshot_server(body: Vec<u8>) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // Handle 2 requests max (HEAD pre-flight + GET).
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else { break };
                stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok();
                let mut req_buf = [0u8; 4096];
                let _ = stream.read(&mut req_buf);
                let req_str = String::from_utf8_lossy(&req_buf);
                let is_head = req_str.starts_with("HEAD ");
                let resp_headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    body.len(),
                );
                let _ = stream.write_all(resp_headers.as_bytes());
                if !is_head {
                    let _ = stream.write_all(&body);
                }
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        });
        port
    }

    #[test]
    fn integration_download_to_cache_happy_path_localhost() {
        // End-to-end: spin up a localhost HTTP server, point a
        // ModelEntry at it, run download_to_cache. File lands in
        // the cache dir, hash matches, partial is gone.
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(cache_dir.path());
        let body = b"klipp integration-test model bytes that are at least a few KiB long for the streaming path. \
                     keep going to make sure the chunked read loop runs more than once even at the 64 KiB chunk size."
            .repeat(1024); // ~200 KB
        let hash = sha256_hex(&body);
        let port = spawn_oneshot_server(body.clone());

        let url = format!("http://127.0.0.1:{port}/sam2.onnx");
        let entry = make_entry(
            "integration_test_model",
            &hash,
            body.len() as u64,
            vec![&url],
        );

        let cancel = AtomicBool::new(false);
        let mut prog = CountingProgress::default();
        let path = download_to_cache(&cache, &entry, &mut prog, &cancel)
            .expect("integration download should succeed");

        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(prog.finished);
        assert_eq!(prog.started_total, Some(body.len() as u64));

        // The cache helper agrees: re-asking is_cached now reports
        // Verified — closes the loop on the air-gap path.
        let status = cache.is_cached(&entry).expect("cache check");
        assert_eq!(status, crate::cache::CachedStatus::Verified);
    }

    #[test]
    fn integration_falls_back_to_second_source_when_first_fails() {
        // First source: nothing listening on a random port (instant
        // refusal). Second source: a working oneshot server.
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(cache_dir.path());
        let body = b"second-source bytes".repeat(4096);
        let hash = sha256_hex(&body);

        // Bind + drop so the port number is one nothing's listening on.
        let dead_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let live_port = spawn_oneshot_server(body.clone());

        let entry = make_entry(
            "fallback_test",
            &hash,
            body.len() as u64,
            vec![
                &format!("http://127.0.0.1:{dead_port}/dead"),
                &format!("http://127.0.0.1:{live_port}/live"),
            ],
        );

        let cancel = AtomicBool::new(false);
        let path = download_to_cache(&cache, &entry, &mut NoOpProgress, &cancel)
            .expect("fallback should succeed via second source");
        assert!(path.exists());
        assert_eq!(std::fs::read(&path).unwrap(), body);
    }

    #[test]
    fn integration_all_sources_fail_returns_aggregate_error() {
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::open_at(cache_dir.path());
        let dead_port_1 = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let dead_port_2 = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };

        let entry = make_entry(
            "all_dead",
            &sha256_hex(b"whatever"),
            8,
            vec![
                &format!("http://127.0.0.1:{dead_port_1}/x"),
                &format!("http://127.0.0.1:{dead_port_2}/y"),
            ],
        );

        let cancel = AtomicBool::new(false);
        let err = download_to_cache(&cache, &entry, &mut NoOpProgress, &cancel)
            .expect_err("should fail when all sources fail");
        assert!(matches!(err, DownloadError::AllSourcesFailed { count: 2, .. }),
            "expected AllSourcesFailed with count=2, got {err:?}");
    }

    // ─── write_and_compute (custom URL paste) tests ──────────────

    #[test]
    fn compute_writes_renames_and_returns_sha_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"some custom-model bytes a user pasted";
        let expected_hash = sha256_hex(bytes);
        let partial = dir.path().join("c.partial");
        let final_ = dir.path().join("c.bin");
        let mut reader = Cursor::new(bytes.to_vec());
        let mut prog = CountingProgress::default();
        let cancel = AtomicBool::new(false);

        let (hash, size) = write_and_compute(
            &mut reader,
            &partial,
            &final_,
            &mut prog,
            &cancel,
            10_000_000,
        )
        .expect("ok");

        assert_eq!(hash, expected_hash);
        assert_eq!(size, bytes.len() as u64);
        assert!(final_.exists());
        assert!(!partial.exists());
        assert_eq!(std::fs::read(&final_).unwrap(), bytes);
        assert!(prog.finished);
    }

    #[test]
    fn compute_aborts_when_stream_exceeds_cap() {
        let dir = tempfile::tempdir().unwrap();
        // 1 KB of input, cap of 100 bytes — should bail.
        let bytes = vec![0u8; 1024];
        let partial = dir.path().join("c.partial");
        let final_ = dir.path().join("c.bin");
        let mut reader = Cursor::new(bytes);
        let mut prog = CountingProgress::default();
        let cancel = AtomicBool::new(false);

        let err = write_and_compute(
            &mut reader,
            &partial,
            &final_,
            &mut prog,
            &cancel,
            100,
        )
        .expect_err("must reject oversize input");
        assert!(matches!(err, DownloadError::SizeMismatch { .. }));
        assert!(!final_.exists(), "rename must not happen on failure");
    }

    #[test]
    fn compute_honors_cancellation_between_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![0u8; 4 * 1024 * 1024]; // 4 MB, multiple chunks
        let partial = dir.path().join("c.partial");
        let final_ = dir.path().join("c.bin");
        let mut reader = Cursor::new(bytes);
        let mut prog = CountingProgress::default();
        let cancel = Arc::new(AtomicBool::new(true)); // already cancelled

        let err = write_and_compute(
            &mut reader,
            &partial,
            &final_,
            &mut prog,
            &cancel,
            100_000_000,
        )
        .expect_err("cancelled before any chunk read");
        assert!(matches!(err, DownloadError::Cancelled));
    }
}
