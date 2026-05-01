# Audio Hardware

Configure audio output devices and backend settings in **Edit → Preferences → Audio**.

## Output Device

Select the audio output device from the dropdown in preferences. Klipp lists all output devices detected by the system audio backend. The default device is used if no specific device is selected.

To change devices during a session, open preferences and select the new device. Audio output switches immediately without restarting.

## Sample Rate

Configure the output sample rate:

- **44 100 Hz** — CD quality, suitable for music-focused work
- **48 000 Hz** — Standard for video production (default and recommended)
- **96 000 Hz** — High-resolution audio

Klipp resamples source audio to match the output sample rate automatically. The resampler is recreated on seek to avoid stale-buffer artefacts.

## Buffer Size

Trade-off between latency and reliability:

| Buffer | Latency at 48 kHz | Use when |
|---|---|---|
| 256 samples | ~5 ms | Low-latency monitoring on a fast machine |
| 512 samples | ~10 ms | Default — good balance |
| 1024 samples | ~21 ms | Stable playback |
| 2048 samples | ~42 ms | Use if experiencing dropouts |

Klipp uses a ~2-second ring buffer internally to absorb timing jitter between the decode pipeline and audio output, plus an overflow buffer to catch samples that don't fit on a ring-full.

## Audio Backend

Klipp uses cpal for audio output, which automatically selects the best available backend:

### PipeWire (recommended)
The default on most modern Linux distributions (Fedora 34+, Ubuntu 22.10+, Arch / CachyOS). PipeWire provides low-latency audio with excellent application routing and JACK compatibility.

### ALSA
Direct hardware access. Available on all Linux distributions; used as a fallback when PipeWire is not available. Lower latency ceiling but no routing flexibility.

### JACK
Available through PipeWire's JACK compatibility layer. Use when integrating with Ardour or other JACK-aware DAWs.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| No audio output | Wrong device selected; PipeWire/ALSA not running | Verify device in preferences; check `systemctl --user status pipewire` |
| Dropouts / crackling | Buffer too small for system load | Increase buffer size; close other audio apps |
| High latency | Buffer too large or PipeWire quantum mismatch | Decrease buffer size; consider switching to ALSA |
| Device not listed | USB device not enumerated | Reconnect device; reopen preferences; some interfaces need firmware |
| Audio glitches on seek | Resampler-state staleness | Should auto-recover; if persistent, file a bug — recreate-on-seek path is supposed to handle this |

For PipeWire-specific tuning (per-application sample rate, quantum), use `pw-metadata` and `pw-cli` outside Klipp; Klipp respects whatever cpal exposes.
