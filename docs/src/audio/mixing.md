# Audio Mixing

Klipp provides per-clip and per-track audio controls with EBU R128 loudness metering. Playback is heard at the loudness the export will deliver, and playback and export mix with the same pan law.

## The Audio Mixer

**Window → Audio Mixer** (Ctrl+Shift+M) shows a channel strip per audio track — fader, pan, mute, solo, meters and the track's automation mode — and the master.

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

How volume is compensated when panning to one side, set in **Preferences → Audio**:

- **Linear (balance)** — the default: the side panned away from is turned down, the other left at full level.
- **Constant power** — keeps the perceived loudness constant across the field.
- **−3 dB**, **−4.5 dB**, **−6 dB** — the centre attenuated by that much.

## Track Automation

An audio track's volume and pan can be keyed over time and are played and exported with the mix. The automation lane under a track (its header menu, **Show automation lane**) draws the keys on a fader law in dB, down to silence. In the mixer, each track's automation mode is **Off**, **Read**, **Touch**, **Latch** or **Write**; Touch, Latch and Write record the controls' moves while playing, each pass one undo step.

## Roles and Ducking

Each clip has a role — dialogue, music, effects and the like — which the mix, the delivery check and the assistant use. Klipp can classify clips' sound and propose their roles. **Auto Duck** in the Audio tab lowers a clip under a sidechain, such as music under dialogue.

## Fitting Music

**Fit Music to Length…** in a music clip's menu fits it to a length by whole bars, cutting or repeating them. The plan is shown before **Apply** replaces the clip with the fitted stretches, their joins crossfaded, in one undo step.

## Audio Keyframes

Clip volume, pan and audio effect parameters can be keyed. Click the diamond icon next to any audio parameter to add a keyframe at the playhead. See [Keyframes](../color/keyframes.md) for the full keyframe model.

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

Set the project target in preferences; the meter highlights when the integrated loudness drifts outside the target's tolerance band. An export can be fitted to a loudness target, measured on its own output; see [Export Quality](../export/quality.md).

## Audio Output Chain

From clip to speaker:

1. Decode (codec → PCM via FFmpeg), read on its own thread
2. Resample to the project sample rate
3. Per-clip gain, audio effects, volume and pan
4. Track mix, with its automation
5. Master mix → cpal output → PipeWire / ALSA

At a cut, playback swaps a track's sound on the cut's own sample.
