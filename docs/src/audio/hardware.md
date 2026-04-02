# Audio Hardware

Configure audio output devices and backend settings in **Edit > Preferences > Audio**.

## Output Device

Select the audio output device from the dropdown in preferences. Klipp lists all available output devices detected by the system audio backend. The default device is used if no specific device is selected.

To change devices during a session, open preferences and select the new device. Audio output switches immediately without restarting.

## Sample Rate

Configure the output sample rate:

- **44,100 Hz** -- CD quality, suitable for most music-focused work
- **48,000 Hz** -- Standard for video production (default and recommended)
- **96,000 Hz** -- High-resolution audio

Klipp resamples source audio to match the output sample rate automatically. The resampler is recreated on seek to avoid artifacts.

## Buffer Size

The audio buffer size controls the trade-off between latency and reliability:

- **Small buffer (256 samples)** -- Lower latency (~5ms at 48kHz), but may cause audio dropouts on slower systems
- **Medium buffer (512 samples)** -- Good balance of latency and stability (default)
- **Large buffer (1024 samples)** -- Higher latency (~21ms at 48kHz), but very stable playback
- **Extra large buffer (2048 samples)** -- Use if experiencing dropouts

Klipp uses a ~2-second ring buffer internally to absorb timing jitter between the decode pipeline and audio output.

## Audio Backend

Klipp uses cpal for audio output, which automatically selects the best available backend:

### PipeWire (Recommended)
The default on most modern Linux distributions (Fedora 34+, Ubuntu 22.10+, Arch/CachyOS). PipeWire provides low-latency audio with good application routing and compatibility.

### ALSA
Direct hardware access, available on all Linux distributions. Used as a fallback when PipeWire is not available. ALSA provides the lowest possible latency but lacks the routing flexibility of PipeWire.

## Troubleshooting

**No audio output**: Verify the correct device is selected in preferences. Check that PipeWire or ALSA services are running. Try selecting the default device.

**Audio dropouts or crackling**: Increase the buffer size in preferences. Close other audio-intensive applications. Ensure your system is not under heavy CPU load.

**High latency**: Decrease the buffer size. Switch to ALSA if PipeWire adds unacceptable latency for your use case.

**Device not listed**: Reconnect the audio device and reopen the preferences panel. Some USB audio interfaces require specific drivers or firmware.
