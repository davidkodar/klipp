# Video Scopes

Video scopes provide objective measurement of your image's colour and luminance values, essential for accurate colour grading. Access scopes from **View → Scopes** or the Color Grading workspace.

## Waveform

The waveform monitor displays levels from left to right, matching the horizontal position of pixels in the frame:

- **Y-axis** — Level (0 = black, 100 = white / 100 IRE, or 0–1023 in 10-bit code values)
- **X-axis** — Horizontal position in the frame
- Brighter trace means more pixels at that level

The waveform and parade are drawn on the GPU from the frame the viewer shows, at the size of their panel on screen in physical pixels, one measured column per pixel of trace, so they stay sharp in a large or fullscreen panel. Every pixel of the frame is measured (or every second or fourth, by scope quality) into 1024 columns by 1024 levels, so the trace has 10-bit vertical resolution, and the trace brightness follows the density of samples on a saturating curve: a lone sample is a dim dot, the gaps between traces stay dark, and a dense trace goes bright without clipping into a solid blob, so the picture's structure stands out against black. The optional glow (**Bloom**, off by default) is the light a dense trace throws.

Levels are those of the delivery: the working space's primaries encoded with the SDR viewer transfer chosen in preferences (sRGB or BT.1886), PQ in BT.2020 while HDR monitoring shows display light, and the display codes themselves in an ACES project.

Use the waveform to check exposure. Keep important content between 0 and 100 IRE. Clipping at the top or bottom indicates lost detail. The **Luma only** option shows one trace of Y′ built from the encoded channels with the working space's luminance weights; otherwise the red, green and blue traces are drawn over each other and add towards white where they agree.

## Vectorscope

The vectorscope shows colour (hue and saturation) as a circular plot:

- **Angle** — Hue (red, yellow, green, cyan, blue, magenta around the circle)
- **Distance from centre** — Saturation (centre = no colour, edge = maximum saturation)
- Target boxes mark the 75 % colour-bar positions (R, G, B, Cy, Mg, Yl) in the project's working space

It is drawn on the GPU as a density plot of Y′CbCr chroma: every measured pixel is placed at its Cb and Cr in the project's working space, and the trace brightness follows the density on a logarithmic curve, so a lone pixel is a faint dot and a skin tone a bright streak. In the colour modes the cloud is tinted by the hue at each point; Gray draws it mono. **Zoom** (1×, 2×, 4×) magnifies the plane about the centre and the graticule follows, and the low and high range settings keep only pixels whose Y′ lies between them, to look at the chroma of shadows or highlights alone.

### Skin Tone Line

A diagonal line at approximately 11 o'clock (~123°) represents the universal skin-tone hue. Healthy skin tones cluster along this line regardless of ethnicity. If faces deviate significantly, consider adjusting hue or white balance.

## Histogram

The histogram shows how many pixels sit at each level, across the whole frame:

- **X-axis** — Level (black on the left, white on the right), marked in the scope's units: 10-bit codes, IRE or percent
- **Y-axis** — How many pixels sit at that level

It is drawn on the GPU from the same measurement as the waveform, at 10-bit resolution, as a filled curve per channel with a bright line along its top. Four layouts are offered: **RGB overlay** draws red, green and blue over each other, adding towards white where they agree; **RGB parade** puts them side by side and **RGB stacked** in rows; **Luma** shows Y′ alone, built with the working space's luminance weights. **Logarithmic count** sets the height by the logarithm of the count, so a small population beside a large one still shows, and **Fill** sets the brightness of the area under each curve.

The tallest level fills nine tenths of the height. Pixels clipped at black or white pile up at the very ends; they are drawn as a spike to the top, but do not set the scale, so a clipped sky does not flatten the rest of the picture. The curve is smoothed over four 10-bit levels, the spacing of 8-bit codes: an 8-bit source reads as the smooth distribution it is rather than a comb, while a grade that stretches the codes further apart still shows its gaps.

A well-exposed image typically has a histogram spread across the full range without hard clipping.

## Parade

The parade scope displays separate waveforms for red, green, and blue channels side by side:

- Compare channel balance at a glance
- Matching shapes across all three channels indicates neutral (grey) tones
- Mismatched channels indicate a colour cast

Particularly useful for white-balance correction: adjust temperature and tint until highlight areas show equal levels across all three channels.

## Working Colourspace

The vectorscope measures Y′CbCr in the project's working space, with that space's own luminance weights, so its targets sit where the space's primaries land. The waveform, parade and histogram measure the delivered levels described above.

## Scope Quality Settings

Adjust how many pixels the scopes measure during playback in preferences. While playback is paused, every pixel is measured whatever the setting:

- **High** — every pixel; most accurate
- **Medium** — every other pixel; the default
- **Low** — every fourth pixel; least work per refresh

A frame is never sampled so coarsely that fewer than about two million of its pixels are measured: a small frame, such as a skim frame or a reduced playback frame, is measured whole at every quality.

When a scope is drawn larger than its samples can fill, as when a small frame is shown in a fullscreen scope, the trace is blurred just enough that each pixel's brightness is measured rather than chance. With plenty of samples, as with a full frame at High quality, it is not blurred at all.

A sampled column is spread over every scope column it stands for, so a lower quality changes how many samples are counted, not where the trace is.

Scope quality does not affect the video output, only the precision of the scope displays.
