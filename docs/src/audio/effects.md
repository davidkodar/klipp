# Audio Effects

Audio effects go on a clip from **Library → Audio FX**: drag one onto a clip, or double-click it with the clip selected. They then appear in the clip's Audio tab, in the order they run.

## Built-in Effects

| Effect | What it does |
|---|---|
| **EQ** | 6-band parametric equaliser |
| **Compressor** | Dynamic range control: threshold, ratio, attack, release, makeup gain |
| **Multiband Compressor** | Per-band compression |
| **Limiter** | Peak protection |
| **Gate** | Silence low-level noise |
| **High Pass** | Remove low-frequency rumble and wind |
| **De-esser** | Reduce sibilance in vocals |
| **Denoiser** | Learn and reduce steady background noise |
| **Noise Reduction** | Per-band noise gating |
| **Voice Isolation** | Separate speech from wind, traffic and room noise (AI, model downloaded on request) |
| **Declick** | Remove pops and clicks |
| **Normalize** | Set a target loudness (LUFS) |
| **Reverb** | Room and space simulation |
| **Delay** | Time-based repeat |
| **Chorus** | Modulation and width |
| **Pitch Shift** | Transpose the pitch |

The command palette also offers one-step clean-ups: **Hum Removal** at 50 Hz or 60 Hz, and **Camera Wind** rumble reduction.

## Plug-ins

The **Installed** filter lists the LV2 plug-ins found on the system, which run as audio effects; plug-ins Klipp cannot run are marked, and **Hide unsupported plugins** hides them. **Tools → Audio Plugins…** scans and manages them; **Re-enable all Audio Plugins** in the command palette turns back on any that were disabled.

## Fade In / Fade Out

Drag the fade handles in a clip's top corners on the timeline, or set the fades in the Audio tab. Curve types:

- **Linear** — constant rate of change
- **Logarithmic** — follows perceived loudness more closely; good for dialogue
- **Exponential** — slow start, fast finish (in) or the reverse (out)
- **S-Curve** — slow start and finish, faster middle
- **Constant Power** — for crossfades between two clips

**Alt+T** adds an audio crossfade at a cut.

## Anti-Click Fade

Klipp applies very short fades at clip boundaries to prevent clicks. They do not change the visible clip length; their length is set in **Preferences → Audio**.

## Effect Order

Audio effects run in the order they appear in the Audio tab; reorder them there. Each can be bypassed on its own, to compare without losing its settings.
