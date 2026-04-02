# Audio Mixing

Klipp provides per-clip and per-track audio controls with professional loudness metering.

## Volume

Adjust clip volume in the inspector's Audio tab. Volume is measured in decibels (dB):

- **0 dB** -- Unity gain (original level)
- **Positive values** -- Amplify the signal
- **Negative values** -- Attenuate the signal
- **-inf** -- Complete silence

Use the scrubby slider to adjust: click and drag horizontally on the dB value for smooth, precise control.

## Pan

The pan control positions audio in the stereo field:

- **-100** -- Hard left
- **0** -- Center (default)
- **+100** -- Hard right

Pan is applied after volume in the signal chain.

## Gain

Per-clip gain adjustment applies before all other processing. Use gain to normalize levels between clips from different sources before applying volume and effects.

## Pan Law

Pan law determines how volume is compensated when panning audio to one side. Configure in **Edit > Preferences > Audio**:

- **0 dB** -- No compensation. Center signal plays at full volume in both channels.
- **-3 dB (Constant Power)** -- Center signal attenuated by 3 dB. Maintains perceived loudness across the stereo field. This is the most common choice.
- **-4.5 dB** -- Compromise between -3 dB and -6 dB.
- **-6 dB (Linear)** -- Center signal attenuated by 6 dB. Mathematically precise linear crossfade.

## Audio Keyframes

All audio parameters (volume, pan, EQ bands) can be keyframed for automation:

1. Select a clip on the timeline
2. Move the playhead to the desired position
3. Click the keyframe diamond icon next to any audio parameter
4. Move the playhead and set a new value

Keyframes interpolate between values using the selected curve type (linear, ease-in, ease-out, bezier).

## Loudness Metering (EBU R128)

Klipp includes loudness metering following the EBU R128 standard:

- **Momentary loudness** -- Short-term (400ms window), shown on real-time meters
- **Short-term loudness** -- 3-second integration window
- **Integrated loudness** -- Program loudness over the full timeline
- **Loudness range** -- Dynamic range measurement
- **True peak** -- Maximum inter-sample peak level

Target levels can be configured in preferences. Common targets:
- **-23 LUFS** -- EBU R128 broadcast standard
- **-14 LUFS** -- YouTube/streaming target
- **-16 LUFS** -- Podcast standard
