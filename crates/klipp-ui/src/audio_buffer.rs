//! The output buffer asked of PipeWire (#173). PipeWire's ALSA plugin
//! reads `PIPEWIRE_LATENCY` when an output opens, so the app sets it from
//! the preferences before it starts any thread, and a change applies after
//! a restart. Measured on PipeWire 1.6: the output then runs at the buffer
//! chosen; cpal's fixed buffer size came out at a quarter of it, and once
//! flooded the stream with errors.
//!
//! Only this one setting is read from `~/.config/klipp/preferences.json`,
//! as untrusted: a size cap, and no file, a file that doesn't read, or a
//! buffer not offered is the device's own buffer, nothing set. A
//! `PIPEWIRE_LATENCY` already in the environment is the person's own and
//! is left as it is.

use std::path::{Path, PathBuf};

/// More than any preferences file holds; a larger file is not read.
const MAX_BYTES: u64 = 4 << 20;

/// The buffers offered, in samples at 48 kHz.
pub const CHOICES: [u32; 6] = [64, 128, 256, 512, 1024, 2048];

#[derive(serde::Deserialize, Default)]
struct OnDisk {
    #[serde(default)]
    audio_hardware: Hardware,
}

#[derive(serde::Deserialize, Default)]
struct Hardware {
    #[serde(default)]
    output_buffer: Option<u32>,
}

/// The buffer the preferences in `file` choose, when they choose one
/// offered.
pub fn chosen(file: &Path) -> Option<u32> {
    if std::fs::metadata(file).ok()?.len() > MAX_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(file).ok()?;
    let n = serde_json::from_str::<OnDisk>(&text).ok()?.audio_hardware.output_buffer?;
    CHOICES.contains(&n).then_some(n)
}

/// `PIPEWIRE_LATENCY` for the buffer `file` chooses; `None` when it
/// chooses none, or `existing` is set already.
pub fn latency_for(file: &Path, existing: Option<&std::ffi::OsStr>) -> Option<String> {
    if existing.is_some() {
        return None;
    }
    chosen(file).map(|n| format!("{n}/48000"))
}

fn preferences_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("klipp").join("preferences.json"))
}

/// `PIPEWIRE_LATENCY` to set for the buffer the preferences choose, with
/// this process's environment as it is.
pub fn pipewire_latency() -> Option<String> {
    latency_for(&preferences_file()?, std::env::var_os("PIPEWIRE_LATENCY").as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(test: &str, text: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-audio-buffer-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preferences.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A chosen 512 is asked of PipeWire as 512 samples at 48 kHz, and a
    /// `PIPEWIRE_LATENCY` already set is left alone.
    #[test]
    fn a_chosen_buffer_is_asked_for_unless_one_is_set() {
        let chosen = file("chosen", br#"{"audio_hardware": {"output_buffer": 512, "output_device": null}, "editing": {}}"#);
        assert_eq!(latency_for(&chosen, None).as_deref(), Some("512/48000"));
        assert_eq!(latency_for(&chosen, Some(std::ffi::OsStr::new("1024/48000"))), None, "the person's own is kept");
    }

    /// The device's own buffer for anything else: no file, garbage, a file
    /// too large to be preferences, a buffer not offered, and an older
    /// file's `buffer_size`, which was never used.
    #[test]
    fn anything_else_is_the_devices_own_buffer() {
        assert_eq!(latency_for(Path::new("/definitely/not/here/preferences.json"), None), None);
        assert_eq!(latency_for(&file("garbage", b"\x00not json{{"), None), None);
        let mut big = br#"{"audio_hardware": {"output_buffer": 512}, "pad": ""#.to_vec();
        big.extend(std::iter::repeat_n(b'x', MAX_BYTES as usize));
        big.extend(br#""}"#);
        assert_eq!(latency_for(&file("big", &big), None), None);
        assert_eq!(latency_for(&file("odd", br#"{"audio_hardware": {"output_buffer": 300}}"#), None), None);
        assert_eq!(latency_for(&file("old", br#"{"audio_hardware": {"sample_rate": 44100, "buffer_size": 256}}"#), None), None);
        assert_eq!(latency_for(&file("none", br#"{"audio_hardware": {"output_buffer": null}}"#), None), None);
    }
}
