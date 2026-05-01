# Audio Preferences

Access audio preferences from **Edit → Preferences → Audio**.

## Pan Law

Determines how volume is compensated when audio is panned away from centre:

- **0 dB** — No compensation. Centre signal at full volume in both channels.
- **−3 dB (Constant Power)** — Centre attenuated by 3 dB. Maintains perceived loudness across the stereo field. Default and most common.
- **−4.5 dB** — Compromise between −3 dB and −6 dB.
- **−6 dB (Linear)** — Centre attenuated by 6 dB. Linear crossfade between channels.

Pan law applies globally to all clips. Change before beginning your audio mix — switching pan law mid-project alters the balance of all panned clips.

## Anti-Click Fade

Klipp applies a short crossfade at every clip boundary to prevent audible clicks from waveform discontinuities.

- **Duration** — Length in milliseconds (default: 3 ms)
- **Enable / Disable**

The anti-click fade is extremely short and inaudible in practice. Disabling may cause clicks at cut points, especially on continuous music.

## Loudness Standard

Select the loudness metering standard:

- **EBU R128** — European broadcast standard, target −23 LUFS (default)
- **ATSC A/85** — North American broadcast standard, target −24 LKFS
- **Custom** — set a custom target loudness in LUFS

The selected standard determines the meter's target indicator and the reference level for integrated loudness measurement.

## Output Device

Select the audio output device from detected system devices. See [Audio Hardware](../audio/hardware.md) for backend details.

## Sample Rate

Configure the output sample rate:

- **44 100 Hz** — CD quality
- **48 000 Hz** — Video production standard (default)
- **96 000 Hz** — High-resolution audio

Source audio is automatically resampled to match the output. The resampler is recreated on seek to avoid stale-buffer artefacts.

## Buffer Size

Trade-off between latency and stability:

| Buffer | Latency at 48 kHz |
|---|---|
| 256 samples | ~5 ms |
| 512 samples | ~10 ms (default) |
| 1024 samples | ~21 ms |
| 2048 samples | ~42 ms |

Increase if you experience dropouts or crackling. Decrease if audio latency is noticeable during scrubbing.

## Audio Backend

Klipp uses cpal, which selects the best available backend:

- **PipeWire** (recommended; default on most modern distros)
- **ALSA** (fallback)
- **JACK** via PipeWire's JACK compatibility layer
