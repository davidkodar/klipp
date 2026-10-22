# Audio Preferences

Two pages: **Audio** for the mix, and **Audio Hardware** for devices.

## Audio

### Mixing

- **Pan law** — the gain change when panning from centre to one side: **Linear (balance)** (the default), **Constant power**, **−3 dB**, **−4.5 dB** or **−6 dB**. Playback and export use the same law. See [Audio Mixing](../audio/mixing.md#pan-law).
- **Anti-click fade** — the length of the micro-fade at clip edges that prevents clicks.
- **Default fade duration** and **Default fade curve** — what a clip's **Add Fade In** and **Add Fade Out** use.

### Loudness

- **Loudness standard** — off, EBU R128 (−23 LUFS) or ATSC A/85 (−24 LKFS): the target the meters and the Loudness History scope show.
- **Peak hold time** — how long the meters hold a peak.

### Auto-Duck Defaults

The duck level, attack, release and threshold a new Auto Duck starts with.

## Audio Hardware

- **Output device** — **Default (auto)** or a specific device; **Refresh** after plugging one in.
- **Sample rate** — 44.1, 48 (default), 96 or 192 kHz.
- **Buffer size** — 64 to 2048 samples; larger is more stable, with more latency. See [Audio Hardware](../audio/hardware.md#buffer-size).
- **Recording input** — the input device used when a track is armed and recording, and a reminder when the system input is muted.
- **Detected hardware** — the audio backend and the output devices found.
