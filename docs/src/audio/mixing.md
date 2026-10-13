# Audio Mixing

Klipp provides per-clip and per-track audio controls with EBU R128 loudness metering.

## Volume

Adjust clip volume in the inspector's Audio tab. Volume is in decibels:

- **0 dB** — Unity gain (original level)
- **Positive values** — Amplify the signal
- **Negative values** — Attenuate
- **−∞** — Complete silence

Use the scrubby slider — click and drag horizontally on the dB value for smooth, precise control.

## Pan

Position audio in the stereo field:

- **−100** — Hard left
- **0** — Centre (default)
- **+100** — Hard right

Pan is applied after volume in the signal chain.

## Gain

Per-clip gain adjustment applies before all other processing. Use gain to normalise levels between clips from different sources before applying volume and effects.

## Normalizing Clips

**Normalize…** in a clip's menu, or **Clip → Normalize Audio…**, brings the selected clips to a loudness or peak target by setting their gain. The media files are not changed, and one undo puts every clip back.

- **Mode:** integrated loudness (ITU-R BS.1770, in LUFS) or peak (the highest sample, in dBFS).
- **Target:** −14 LUFS (web), −16 LUFS (podcast), −23 LUFS (EBU R128 broadcast), −24 LUFS (ATSC A/85), or any value.
- **Clips:** each on its own, or the selection as one group. A group takes one gain measured from all its clips together, so the clips keep their levels against each other.
- **True-peak ceiling:** the gain never raises a clip's true peak above it (−1 dBTP by default). A clip held below its target by the ceiling is counted in the message afterwards.

Each clip is measured over the part of its file it plays. A silent clip is left as it was, and so is a clip with no sound. The dialog opens with the last mode, target and ceiling used. Over the control socket the same operation is `normalize_clips`, and it answers each clip's measured loudness, peaks and the gain it set.

## Pan Law

Determines how volume is compensated when panning to one side. Configure in **Edit → Preferences → Audio**:

- **0 dB** — No compensation. Centre at full volume in both channels.
- **−3 dB (Constant Power)** — Centre attenuated by 3 dB. Maintains perceived loudness across the stereo field. Most common.
- **−4.5 dB** — Compromise.
- **−6 dB (Linear)** — Centre attenuated by 6 dB. Mathematically precise linear crossfade.

## Audio Keyframes

All audio parameters (volume, pan, EQ bands) can be keyframed for automation. Click the diamond icon next to any audio parameter to add a keyframe at the playhead. See [Keyframes](../color/keyframes.md) for the full keyframe model.

## Loudness Metering (EBU R128)

Klipp includes loudness metering following EBU R128:

- **Momentary loudness** — short-term (400 ms window)
- **Short-term loudness** — 3-second integration window
- **Integrated loudness** — program loudness over the full timeline
- **Loudness range (LRA)** — dynamic range measurement
- **True peak** — maximum inter-sample peak level

Common targets:

| Target | LUFS |
|---|---|
| EBU R128 broadcast | −23 |
| YouTube / streaming | −14 |
| Podcast | −16 |
| Apple Music | −16 |
| Spotify | −14 |

Set the project target in preferences; the meter highlights when the integrated loudness drifts outside the target's tolerance band.

## Audio Output Chain

The full chain from clip to speaker:

1. Decode (codec → PCM via FFmpeg)
2. Resample to project sample rate (recreated on seek to avoid stale-buffer artefacts)
3. Per-clip gain → EQ → compressor (if enabled)
4. Per-clip volume + pan
5. Track-level mix
6. Master mix → cpal output → PipeWire/ALSA

A ~2-second ring buffer absorbs timing jitter between decode and output. An overflow buffer catches samples that don't fit in the ring buffer (prevents truncation that produced loud audio artefacts in early builds).
