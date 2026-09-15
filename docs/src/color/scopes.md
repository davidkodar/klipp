# Video Scopes

Video scopes provide objective measurement of your image's colour and luminance values, essential for accurate colour grading. Access scopes from **View → Scopes** or the Color Grading workspace.

## Waveform

The waveform monitor displays levels from left to right, matching the horizontal position of pixels in the frame:

- **Y-axis** — Level, in the units of the scope's scale (see [Graticule units](#graticule-units))
- **X-axis** — Horizontal position in the frame
- Brighter trace means more pixels at that level

The waveform and parade are drawn on the GPU from the frame the viewer shows, at the size of their panel on screen in physical pixels, one measured column per pixel of trace, so they stay sharp in a large or fullscreen panel. Every pixel of the frame is measured (or every second or fourth, by scope quality) into 1024 columns by 1024 levels, so the trace has 10-bit vertical resolution, and the trace brightness follows the density of samples on a saturating curve: a lone sample is a dim dot, the gaps between traces stay dark, and a dense trace goes bright without clipping into a solid blob, so the picture's structure stands out against black. The optional glow (**Bloom**, off by default) is the light a dense trace throws.

**Low Pass Filter** (on by default) measures a slightly smoothed copy of the picture, a Gaussian 4 pixels wide in a picture 3840 pixels on its long side and narrower in a smaller one, so fine texture, noise and the steps between 8-bit codes draw clean lines rather than a haze. Turn it off to see every pixel's own level. The docked scopes and the floating scope window share the setting, as they share **Bloom**: changing either in one changes it in the other. It applies to the waveform and parade only: the histogram and vectorscope always measure the picture itself.

Levels are those of the delivery: the working space's primaries encoded with the SDR viewer transfer chosen in preferences (sRGB or BT.1886), PQ in BT.2020 while HDR monitoring shows display light, and the display codes themselves in an ACES project.

Use the waveform to check exposure. Keep important content between 0 and 100 IRE. Clipping at the top or bottom indicates lost detail. The **Luma only** option shows one trace of Y′ built from the encoded channels with the working space's luminance weights; otherwise the red, green and blue traces are drawn over each other and add towards white where they agree. **Envelope** draws each column's lowest and highest level as a line, and **Intensity** sets the trace brightness: 0.5 draws the density as it is, 1 is six times brighter and 0 six times dimmer.

## Graticule units

The trace always spans the full 10-bit code range, 0 to 1023. The scale, chosen in the scope settings or in the scope window's ··· menu, says what each height is called:

- **10-bit** — code values, 0 to 1023, marked every 128
- **IRE** — 0 IRE at code 64 and 100 IRE at code 940, the legal range of a 10-bit signal. A full-range picture reaches from −7.3 to 109.5 IRE, so the 0 and 100 lines sit inside the trace, not at its edges, and anything outside them is outside legal range.
- **%** — percent of the full range: 0 at code 0, 100 at code 1023

**Show legal range** marks codes 64 and 940 with two cool lines across the waveform, parade and histogram, labelled in the scale's units, so illegal levels are seen at a glance whatever the scale.

While HDR monitoring is on, the scopes measure PQ display light and the graticule is in **nits**: 0, 1, 10, 100, 203 (reference white), 1 000 and 10 000 cd/m² at their positions on the PQ curve, with minor marks between. The legal range has no meaning there and is not drawn.

## Vectorscope

The vectorscope shows colour (hue and saturation) as a circular plot:

- **Angle** — Hue (red, yellow, green, cyan, blue, magenta around the circle)
- **Distance from centre** — Saturation (centre = no colour, edge = maximum saturation)
- Target boxes mark the 75 % colour-bar positions (R, G, B, Cy, Mg, Yl) in the project's working space

It is drawn on the GPU as a density plot of Y′CbCr chroma: every measured pixel is placed at its Cb and Cr in the project's working space, and the trace brightness follows the density on a logarithmic curve, so a lone pixel is a faint dot and a skin tone a bright streak. With **Colour** on the cloud is tinted by the hue at each point; off draws it mono. **Zoom** (1×, 2×, 4×) magnifies the plane about the centre and the graticule follows, and **Luma low** and **Luma high** keep only pixels whose Y′ lies between them, to look at the chroma of shadows or highlights alone.

### Skin Tone Line

A diagonal line at approximately 11 o'clock (~123°) represents the universal skin-tone hue. Healthy skin tones cluster along this line regardless of ethnicity. If faces deviate significantly, consider adjusting hue or white balance.

## Histogram

The histogram shows how many pixels sit at each level, across the whole frame:

- **X-axis** — Level (black on the left, white on the right), marked in the scope's units (see [Graticule units](#graticule-units))
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

## Refresh rate

The scopes are drawn as often as they can afford: each pass is timed, and the next may start twice that long after it, never later than 100 ms. A pass that costs a few milliseconds, as a docked scope of an HD frame does, is drawn every frame; a 4K frame into four fullscreen scopes at Full quality costs more and is drawn every second or third frame. During playback the scopes measure whatever reaches the screen, including frames the fast playback paths hand over as display bytes, so a scope open never slows playback down beyond its own cost. Paused, the scopes measure every pixel of the frame as soon as anything about them changes: the frame, a setting, or the size of their pane.
