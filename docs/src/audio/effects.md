# Audio Effects

Klipp includes built-in audio effects for common post-production needs.

## Equalizer (EQ)

A parametric equalizer available in the inspector's Audio tab:

- Multiple adjustable frequency bands
- Each band has frequency, gain, and Q (bandwidth) controls
- Band types: low shelf, high shelf, bell/peak, low pass, high pass
- Visual frequency response curve

Use EQ to shape the tonal balance of your audio -- reduce muddiness, add presence, or roll off unwanted low-frequency rumble.

## Compressor

Dynamic range compression reduces the difference between loud and quiet parts of the audio:

- **Threshold** -- Level above which compression begins (dB)
- **Ratio** -- Compression ratio (e.g., 4:1 means 4 dB of input above threshold produces 1 dB of output)
- **Attack** -- Time for compression to engage (ms)
- **Release** -- Time for compression to disengage (ms)
- **Makeup Gain** -- Compensate for volume reduction after compression

## Multiband Compressor

Splits the audio into frequency bands and applies independent compression to each:

- Low band (below ~200 Hz)
- Mid band (~200 Hz to ~2 kHz)
- High band (above ~2 kHz)

Each band has its own threshold, ratio, attack, and release controls. Useful for taming specific frequency ranges without affecting the whole signal.

## Anti-Click Fade

Klipp automatically applies very short fades at clip boundaries to prevent audible clicks and pops. This happens transparently and does not affect the visible clip length. The anti-click fade duration is configurable in **Edit > Preferences > Audio** (default: 3ms).

## Fade In / Fade Out

Apply fades at clip start and end by dragging the fade handles in the clip's top corners on the timeline. Available fade curve types:

- **Linear** -- Constant rate of change. Simple and predictable.
- **Exponential** -- Slow start, fast finish (fade in) or fast start, slow finish (fade out). Sounds more natural for music.
- **S-Curve** -- Slow start and finish with faster change in the middle. Smooth and subtle.
- **Logarithmic** -- Follows perceived loudness more closely than linear. Good for dialogue fades.

Select the curve type by right-clicking on a fade handle or from the inspector when a fade is selected.

## Effect Order

Audio effects are processed in the order they appear in the inspector. Drag effects to reorder them. The typical signal chain is:

1. Gain (input level)
2. EQ
3. Compressor
4. Pan
5. Volume (output level)
