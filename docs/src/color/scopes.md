# Video Scopes

Video scopes provide objective measurement of your image's color and luminance values, essential for accurate color grading. Access scopes from **View > Scopes** or the Color Grading workspace.

## Waveform

The waveform monitor displays luminance values from left to right, matching the horizontal position of pixels in the frame:

- **Y-axis** -- Brightness level (0 = black, 100 = white / 100 IRE)
- **X-axis** -- Horizontal position in the frame
- Bright areas in the waveform indicate many pixels at that luminance level

Use the waveform to check exposure. Keep important content between 0 and 100 IRE. Clipping at the top or bottom indicates lost detail.

## Vectorscope

The vectorscope shows color (hue and saturation) as a circular plot:

- **Angle** -- Hue (red, yellow, green, cyan, blue, magenta around the circle)
- **Distance from center** -- Saturation (center = no color, edge = maximum saturation)
- Colored target boxes mark standard color bar positions (R, G, B, Cy, Mg, Yl)

### Skin Tone Line

A diagonal line (approximately 11 o'clock position, ~123 degrees) represents the universal skin tone hue. Regardless of ethnicity, healthy skin tones cluster along this line. If faces in your footage deviate from this line, consider adjusting hue or white balance.

## Histogram

The histogram shows the distribution of pixel brightness values:

- **X-axis** -- Brightness (black on left, white on right)
- **Y-axis** -- Number of pixels at each brightness level
- Separate RGB channels can be displayed overlaid or individually

A well-exposed image typically has a histogram spread across the full range without hard clipping at either end.

## Parade

The parade scope displays separate waveforms for red, green, and blue channels side by side:

- Compare channel balance at a glance
- Matching shapes across all three channels indicates neutral (gray) tones
- Mismatched channels indicate a color cast

The parade is particularly useful for white balance correction: adjust temperature and tint until highlight areas show equal levels across all three channels.

## Scope Quality Settings

Adjust scope rendering quality in preferences:

- **High** -- Full resolution analysis. Most accurate but higher CPU usage.
- **Medium** -- Samples every other pixel. Good balance for real-time grading.
- **Low** -- Samples every fourth pixel. Best performance for smooth playback.

Scope quality does not affect the video output, only the precision of the scope displays.
