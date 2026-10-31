//! What this machine's ffmpeg can encode, for tests that need an encoder
//! a distribution may build without.

use std::sync::OnceLock;

/// Whether this ffmpeg's libx265 encodes 10-bit. Arch Linux ARM builds
/// x265 8-bit only, so no 10-bit HEVC (an HDR10 or Main10 export) can be
/// made there; Arch's x86_64 x265 carries 8, 10 and 12-bit. Asked of the
/// encoder's pixel formats, not by encoding a frame: the ffmpeg program
/// turns a 10-bit request into 8-bit where the encoder has no 10-bit,
/// and succeeds.
pub fn libx265_encodes_10_bit() -> bool {
    static ANSWER: OnceLock<bool> = OnceLock::new();
    *ANSWER.get_or_init(|| {
        std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-h", "encoder=libx265"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.contains("pixel formats") && l.contains("yuv420p10le")))
    })
}

/// Whether an export's error is the 10-bit HEVC encoder failing to open
/// on a machine whose libx265 has no 10-bit: the one failure a test of a
/// 10-bit HEVC export passes over. Anywhere 10-bit can be made, the
/// failure is the export's.
pub fn no_10_bit_hevc_here(error: &str) -> bool {
    error.contains("open video encoder") && !libx265_encodes_10_bit()
}
