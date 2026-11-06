//! ONNX Runtime's WebGPU provider as a plug-in, for the GPUs CUDA
//! doesn't serve: AMD and Intel, through Vulkan.
//!
//! No distribution's onnxruntime has WebGPU built in, so on those GPUs
//! every model ran on the CPU. Microsoft ships the provider on its own,
//! as a library ONNX Runtime loads at run time (`onnxruntime-ep-webgpu`
//! on PyPI). Klipp downloads it only when asked, pinned by its checksum
//! in the model manifest like every other download, and takes the one
//! library out of the package into Klipp's data folder.
//!
//! The package is read as untrusted, checksum and all: it is checked
//! against the manifest before anything is read from it, only the one
//! named file is taken out, with a cap on its size, and what comes out
//! must have the library's own checksum too, or nothing is kept. It is
//! written beside the final path and moved into place whole.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The manifest entry of the plug-in package for this machine.
pub const ENTRY: &str = "onnxruntime_ep_webgpu_linux_x86_64";

/// The library's name inside the package.
const MEMBER: &str = "onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so";

/// The library's SHA-256, taken from the package of version 0.5.0 on
/// 2026-11-07: what comes out of the package must be exactly this.
const LIBRARY_SHA256: &str = "fc98b10c7d03537694ff4ea6ebca79ed3b41ca71e316dac3292257ee5f85c95d";

/// The most the library may unpack to: it is 16.6 MB.
const MAX_LIBRARY_BYTES: u64 = 64 * 1024 * 1024;

/// The folder the library is kept in, beside the model cache.
pub fn folder(cache: &klipp_ml::cache::ModelCache) -> PathBuf {
    cache.root().with_file_name("onnxruntime-webgpu")
}

/// Where the library is kept.
pub fn library_path(cache: &klipp_ml::cache::ModelCache) -> PathBuf {
    folder(cache).join("libonnxruntime_providers_webgpu.so")
}

/// Whether to offer the plug-in here: a GPU that none of the providers
/// this onnxruntime has can run on. Asked once: it looks at the hardware.
pub fn offered() -> bool {
    use crate::runtime_resolve::{detect_gpu_vendor, GpuVendor};
    static OFFERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // Only the x86-64 package is in the manifest.
    if !cfg!(target_arch = "x86_64") {
        return false;
    }
    *OFFERED.get_or_init(|| match detect_gpu_vendor() {
        GpuVendor::Amd | GpuVendor::Intel => true,
        GpuVendor::Nvidia => crate::provider::gpu_provider().is_none(),
        GpuVendor::None | GpuVendor::Unknown => false,
    })
}

/// Whether the package has been downloaded (it is taken apart when first
/// used).
pub fn downloaded() -> bool {
    let (Ok(manifest), Ok(cache)) = (klipp_ml::manifest::load_effective(), klipp_ml::cache::ModelCache::open()) else {
        return false;
    };
    library_path(&cache).is_file() || manifest.get(ENTRY).is_some_and(|e| cache.path_for(e).is_file())
}

/// The library, taken out of the downloaded package the first time:
/// nothing when the package isn't downloaded, an error when it, or what
/// came out of it, isn't the one Klipp knows.
pub fn library() -> Result<Option<PathBuf>, String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let into = library_path(&cache);
    if into.is_file() {
        let got = klipp_ml::cache::sha256_of_file(&into).map_err(|e| format!("{}: {e}", into.display()))?;
        if got.eq_ignore_ascii_case(LIBRARY_SHA256) {
            return Ok(Some(into));
        }
        // Not ours any more: taken out again from the package below.
        let _ = std::fs::remove_file(&into);
    }
    let Some(entry) = manifest.get(ENTRY) else { return Ok(None) };
    let package = cache.path_for(entry);
    if !package.is_file() {
        return Ok(None);
    }
    unpack(&package, &entry.sha256, LIBRARY_SHA256, &into).map(Some)
}

/// Take the library out of the package at `package`, whose SHA-256 must be
/// `sha256`, into `into`, where its own SHA-256 must be `library_sha256`.
pub fn unpack(package: &Path, sha256: &str, library_sha256: &str, into: &Path) -> Result<PathBuf, String> {
    let got = klipp_ml::cache::sha256_of_file(package).map_err(|e| format!("{}: {e}", package.display()))?;
    if !got.eq_ignore_ascii_case(sha256) {
        return Err(format!("the WebGPU package is not the one Klipp knows (sha256 {got}); nothing was taken out of it"));
    }
    let bytes = std::fs::read(package).map_err(|e| format!("{}: {e}", package.display()))?;
    let library = member(&bytes, MEMBER, MAX_LIBRARY_BYTES)?;
    let got = sha256_hex(&library);
    if !got.eq_ignore_ascii_case(library_sha256) {
        return Err(format!("the library in the WebGPU package is not the one Klipp knows (sha256 {got}); it was not kept"));
    }
    let parent = into.parent().ok_or("the library's folder has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let staging = into.with_extension("partial");
    std::fs::write(&staging, &library).map_err(|e| format!("{}: {e}", staging.display()))?;
    std::fs::rename(&staging, into).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        format!("{}: {e}", into.display())
    })?;
    Ok(into.to_path_buf())
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn u16_at(b: &[u8], at: usize) -> Result<usize, String> {
    b.get(at..at + 2).map(|s| u16::from_le_bytes([s[0], s[1]]) as usize).ok_or_else(|| "the package ends early".to_string())
}

fn u32_at(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as u64).ok_or_else(|| "the package ends early".to_string())
}

/// The file `name` in the zip archive `zip` (a wheel is one), at most
/// `cap` bytes once unpacked: found through the archive's directory,
/// stored or deflated, and nothing else.
pub fn member(zip: &[u8], name: &str, cap: u64) -> Result<Vec<u8>, String> {
    // The directory's end record: in the last 22 bytes plus at most a
    // 64 KB comment.
    const END: &[u8] = b"PK\x05\x06";
    let from = zip.len().saturating_sub(22 + 65_535);
    let end = (from..zip.len().saturating_sub(21)).rev().find(|&i| zip[i..].starts_with(END)).ok_or("the package is not a zip archive")?;
    let entries = u16_at(zip, end + 10)?;
    let mut at = u32_at(zip, end + 16)? as usize;
    for _ in 0..entries {
        if !zip.get(at..).is_some_and(|s| s.starts_with(b"PK\x01\x02")) {
            return Err("the package's directory is damaged".into());
        }
        let method = u16_at(zip, at + 10)?;
        let packed = u32_at(zip, at + 20)?;
        let size = u32_at(zip, at + 24)?;
        let (name_len, extra_len, comment_len) = (u16_at(zip, at + 28)?, u16_at(zip, at + 30)?, u16_at(zip, at + 32)?);
        let local = u32_at(zip, at + 42)? as usize;
        let here = zip.get(at + 46..at + 46 + name_len).ok_or("the package ends early")?;
        at += 46 + name_len + extra_len + comment_len;
        if here != name.as_bytes() {
            continue;
        }
        if size > cap || packed > cap {
            return Err(format!("{name} in the package is too big ({size} bytes)"));
        }
        if !zip.get(local..).is_some_and(|s| s.starts_with(b"PK\x03\x04")) {
            return Err("the package's file header is damaged".into());
        }
        let start = local + 30 + u16_at(zip, local + 26)? + u16_at(zip, local + 28)?;
        let data = zip.get(start..start + packed as usize).ok_or("the package ends early")?;
        let mut out = Vec::with_capacity(size as usize);
        match method {
            0 => out.extend_from_slice(data),
            8 => {
                flate2::read::DeflateDecoder::new(data).take(cap + 1).read_to_end(&mut out).map_err(|e| format!("{name} in the package: {e}"))?;
            }
            m => return Err(format!("{name} in the package is packed in a way Klipp doesn't read ({m})")),
        }
        if out.len() as u64 != size {
            return Err(format!("{name} in the package unpacked to {} bytes, not the {size} it says", out.len()));
        }
        return Ok(out);
    }
    Err(format!("no {name} in the package"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip archive of `files` (name, data, deflated or stored), the way
    /// a wheel is made.
    fn zip_of(files: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut dir) = (Vec::new(), Vec::new());
        for (name, data, deflate) in files {
            let packed = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                std::io::Write::write_all(&mut e, data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            dir.extend_from_slice(b"PK\x01\x02");
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            dir.extend_from_slice(&method.to_le_bytes());
            dir.extend_from_slice(&[0; 8]);
            dir.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&offset.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-webgpu-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_the_named_file_comes_out_stored_or_deflated() {
        let lib = vec![7u8; 100_000];
        let zip = zip_of(&[("README.md", b"hello", false), (MEMBER, &lib, true), ("x/other.so", b"no", false)]);
        assert_eq!(member(&zip, MEMBER, 1 << 20).unwrap(), lib);
        assert_eq!(member(&zip, "README.md", 1 << 20).unwrap(), b"hello");
        assert!(member(&zip, "missing", 1 << 20).unwrap_err().contains("no missing"));
    }

    /// The package is read as hostile: too big, not a zip, cut short, or
    /// with a name that only looks like the library: nothing comes out.
    #[test]
    fn a_hostile_package_gives_nothing() {
        let lib = vec![0u8; 10_000];
        let zip = zip_of(&[(MEMBER, &lib, true)]);
        assert!(member(&zip, MEMBER, 9_999).unwrap_err().contains("too big"), "over the cap");
        assert!(member(b"not a zip at all, just bytes and more bytes", MEMBER, 1 << 20).is_err());
        assert!(member(&zip[..zip.len() / 2], MEMBER, 1 << 20).is_err(), "cut short");
        let lookalike = zip_of(&[("../onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so", &lib, false), ("onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so.evil", &lib, false)]);
        assert!(member(&lookalike, MEMBER, 1 << 20).is_err(), "only the exact name");
        // A deflated member that says it is smaller than it unpacks to.
        let mut lying = zip_of(&[(MEMBER, &lib, true)]);
        let dir = lying.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
        lying[dir + 24..dir + 28].copy_from_slice(&100u32.to_le_bytes());
        assert!(member(&lying, MEMBER, 1 << 20).is_err());
    }

    /// The package and the library each have to be the ones Klipp knows,
    /// and a refused one leaves nothing behind.
    #[test]
    fn both_checksums_must_match_or_nothing_is_kept() {
        let dir = scratch("checksums");
        let lib = b"the library".to_vec();
        let package = dir.join("p.whl");
        std::fs::write(&package, zip_of(&[(MEMBER, &lib, true)])).unwrap();
        let package_sha = klipp_ml::cache::sha256_of_file(&package).unwrap();
        let into = dir.join("out/lib.so");
        assert!(unpack(&package, &"0".repeat(64), &sha256_hex(&lib), &into).unwrap_err().contains("not the one Klipp knows"));
        assert!(unpack(&package, &package_sha, &"0".repeat(64), &into).unwrap_err().contains("library in the WebGPU package"));
        assert!(!into.exists() && !into.with_extension("partial").exists(), "nothing kept");
        assert_eq!(unpack(&package, &package_sha, &sha256_hex(&lib), &into).unwrap(), into);
        assert_eq!(std::fs::read(&into).unwrap(), lib);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real package, where it has been fetched for the test: the
    /// library in it is the one pinned here.
    #[test]
    fn the_pinned_package_holds_the_pinned_library() {
        let Some(package) = std::env::var_os("KLIPP_TEST_WEBGPU_WHEEL").map(PathBuf::from) else {
            eprintln!("skipping: KLIPP_TEST_WEBGPU_WHEEL not set");
            return;
        };
        let bytes = std::fs::read(&package).unwrap();
        let lib = member(&bytes, MEMBER, MAX_LIBRARY_BYTES).unwrap();
        assert_eq!(sha256_hex(&lib), LIBRARY_SHA256);
    }
}
