# Audio Preferences

Access audio preferences from **Edit > Preferences > Audio**.

## Pan Law

Select the pan law used for stereo panning. This determines how volume is compensated when audio is panned away from center:

- **0 dB** -- No compensation. Center signal at full volume in both channels.
- **-3 dB (Constant Power)** -- Center attenuated by 3 dB. Maintains perceived loudness as audio moves across the stereo field. This is the default and most common setting.
- **-4.5 dB** -- Compromise between -3 dB and -6 dB laws.
- **-6 dB (Linear)** -- Center attenuated by 6 dB. Linear crossfade between channels.

Pan law applies globally to all clips. Change it before beginning your audio mix, as switching pan law mid-project alters the balance of all panned clips.

## Anti-Click Fade

Klipp applies a short crossfade at every clip boundary to prevent audible clicks from discontinuities in the audio waveform.

- **Duration** -- Length of the anti-click fade in milliseconds (default: 3ms)
- **Enable/Disable** -- Toggle anti-click fades on or off

The anti-click fade is extremely short and inaudible in practice. Disabling it may cause clicks at cut points, especially with music or continuous audio.

## Loudness Standard

Select the loudness metering standard for the audio meters:

- **EBU R128** -- European broadcast standard, target -23 LUFS (default)
- **ATSC A/85** -- North American broadcast standard, target -24 LKFS
- **Custom** -- Set a custom target loudness in LUFS

The selected standard determines the target indicator on the loudness meters and the reference level for integrated loudness measurement.

## Output Device

Select the audio output device from detected system devices. See [Audio Hardware](../audio/hardware.md) for details.

## Sample Rate

Configure the output sample rate:

- **44,100 Hz** -- CD quality
- **48,000 Hz** -- Video production standard (default)
- **96,000 Hz** -- High-resolution audio

Source audio is automatically resampled to match the output sample rate.

## Buffer Size

Set the audio buffer size to balance latency and stability:

- **256 samples** -- Low latency (~5ms at 48kHz)
- **512 samples** -- Balanced (default)
- **1024 samples** -- Stable, higher latency
- **2048 samples** -- Maximum stability

Increase the buffer size if you experience audio dropouts or crackling. Decrease it if audio latency is noticeable during scrubbing.
