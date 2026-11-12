# Audio Hardware

Configure audio output devices and backend settings in **Preferences → Audio Hardware**.

## Output Device

Select the audio output device from the dropdown in preferences. Klipp lists all output devices detected by the system audio backend. The default device is used if no specific device is selected.

To change devices during a session, open preferences and select the new device. Audio output switches immediately without restarting.

## Sample Rate

Playback runs at 48 kHz when the output device takes it, the rate Klipp's sound caches and effects are made at; a device that doesn't gets its own rate, and the sound is converted to it. The page shows the rate in use. It is not a setting: any other rate would convert every sample.

## Buffer Size

**Device default** (the default) leaves the buffer to the device and PipeWire, as they set it. A size chosen is asked of PipeWire for Klipp's output, and applies after Klipp is restarted. It works through PipeWire only; on plain ALSA or PulseAudio the device's own buffer is used.

| Buffer | Latency at 48 kHz | Use when |
|---|---|---|
| Device default | PipeWire's quantum | Most of the time |
| 64 / 128 samples | ~1.3 / ~2.7 ms | Lowest latency, on a fast machine |
| 256 samples | ~5 ms | Low-latency monitoring |
| 512 samples | ~10 ms | A good balance |
| 1024 samples | ~21 ms | Stable playback |
| 2048 samples | ~42 ms | Use if experiencing dropouts |

PipeWire runs every program on a device at the smallest buffer any of them asks for, so a small buffer chosen in Klipp applies to the others on that device too while Klipp plays. A `PIPEWIRE_LATENCY` set in the environment Klipp is started from is kept, and wins over the setting.

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
| Dropouts / crackling | Buffer too small for system load | Choose a larger buffer size, or Device default, and restart Klipp; close other audio apps |
| High latency | Buffer too large or PipeWire quantum mismatch | Choose a smaller buffer size and restart Klipp |
| Device not listed | USB device not enumerated | Reconnect device; reopen preferences; some interfaces need firmware |

For PipeWire-specific tuning (per-application sample rate, quantum), use `pw-metadata` and `pw-cli` outside Klipp; Klipp respects whatever cpal exposes.
