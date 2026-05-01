# Audio Effects

Klipp includes built-in audio effects for common post-production needs.

## Equaliser (EQ)

A parametric equaliser available in the inspector's Audio tab:

- Multiple adjustable bands
- Each band has frequency, gain, and Q (bandwidth) controls
- Band types — low shelf, high shelf, bell/peak, low pass, high pass
- Visual frequency-response curve

Use EQ to shape the tonal balance — reduce muddiness, add presence, roll off unwanted low-frequency rumble.

## Compressor

Reduces the difference between loud and quiet parts of the audio:

- **Threshold** — Level above which compression begins (dB)
- **Ratio** — e.g. 4:1 means 4 dB of input above threshold produces 1 dB of output
- **Attack** — Time for compression to engage (ms)
- **Release** — Time for compression to disengage (ms)
- **Makeup Gain** — Compensate for level reduction after compression

## Multiband Compressor

Splits the audio into frequency bands and applies independent compression to each:

- Low band (below ~200 Hz)
- Mid band (~200 Hz – 2 kHz)
- High band (above ~2 kHz)

Each band has its own threshold, ratio, attack, release. Useful for taming specific ranges without affecting the whole signal.

## Anti-Click Fade

Klipp automatically applies very short fades at clip boundaries to prevent audible clicks and pops. This happens transparently and does not affect the visible clip length. Default 3 ms; configurable in **Edit → Preferences → Audio**.

## Fade In / Fade Out

Apply fades at clip start and end by dragging the fade handles in the clip's top corners on the timeline. Curve types:

- **Linear** — constant rate of change
- **Exponential** — slow start, fast finish (in) or vice versa (out); natural for music
- **S-Curve** — slow start and finish, faster middle; smooth and subtle
- **Logarithmic** — follows perceived loudness more closely than linear; good for dialogue

Right-click on a fade handle to change curve type, or select the fade and edit in the inspector.

## Effect Order

Audio effects are processed in the order they appear in the inspector. Drag to reorder. Typical signal chain:

1. Gain (input level)
2. EQ
3. Compressor
4. Pan
5. Volume (output level)

Effects can be bypassed individually with the bypass toggle on each row, useful for A/B comparison without losing your settings.
