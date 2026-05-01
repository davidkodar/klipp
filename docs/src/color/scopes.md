# Video Scopes

Video scopes provide objective measurement of your image's colour and luminance values, essential for accurate colour grading. Access scopes from **View → Scopes** or the Color Grading workspace.

## Waveform

The waveform monitor displays luminance values from left to right, matching the horizontal position of pixels in the frame:

- **Y-axis** — Brightness level (0 = black, 100 = white / 100 IRE)
- **X-axis** — Horizontal position in the frame
- Bright areas in the waveform indicate many pixels at that luminance

Use the waveform to check exposure. Keep important content between 0 and 100 IRE. Clipping at the top or bottom indicates lost detail.

## Vectorscope

The vectorscope shows colour (hue and saturation) as a circular plot:

- **Angle** — Hue (red, yellow, green, cyan, blue, magenta around the circle)
- **Distance from centre** — Saturation (centre = no colour, edge = maximum saturation)
- Coloured target boxes mark standard colour-bar positions (R, G, B, Cy, Mg, Yl)

### Skin Tone Line

A diagonal line at approximately 11 o'clock (~123°) represents the universal skin-tone hue. Healthy skin tones cluster along this line regardless of ethnicity. If faces deviate significantly, consider adjusting hue or white balance.

## Histogram

The histogram shows the distribution of pixel brightness values:

- **X-axis** — Brightness (black on left, white on right)
- **Y-axis** — Number of pixels at each brightness level
- Separate RGB channels can be displayed overlaid or individually

A well-exposed image typically has a histogram spread across the full range without hard clipping.

## Parade

The parade scope displays separate waveforms for red, green, and blue channels side by side:

- Compare channel balance at a glance
- Matching shapes across all three channels indicates neutral (grey) tones
- Mismatched channels indicate a colour cast

Particularly useful for white-balance correction: adjust temperature and tint until highlight areas show equal levels across all three channels.

## Working Colourspace

Scopes display values in the working colour space (default ACEScg, linear). For viewers used to display-referred scopes, the bottom of the scope panel offers a **Display-referred** toggle that re-maps the scope output through the current ODT before displaying — closer to what an SDI monitor would show.

## Scope Quality Settings

Adjust scope rendering quality in preferences:

- **High** — full resolution analysis; most accurate, higher CPU usage
- **Medium** — samples every other pixel; good balance for real-time grading
- **Low** — samples every fourth pixel; best performance for smooth playback

Scope quality does not affect the video output, only the precision of the scope displays.
