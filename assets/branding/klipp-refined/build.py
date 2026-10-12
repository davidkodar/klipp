"""Build the Klipp cut-K identity: icon, wordmark lockups and PNG exports.

Requires rsvg-convert. Run from anywhere; output lands next to this file.
"""
from pathlib import Path
import subprocess

OUT = Path(__file__).resolve().parent
AMBER, TILE, WHITE, PAPER = '#D99A38', '#242629', '#EAE7E0', '#F4F1EA'


def cut_k(x, y, h, stem, arm, slip, gap, main, leg):
    """A K whose leg is cut off at the waist and slipped right, like a clip on a timeline.

    The stem and arm are one piece in `main`; the leg is the moved piece in `leg`.
    """
    mid, g = y + h / 2, gap / 2
    sx = x + stem
    reach = sx + h / 2 * 0.70
    joint = sx + h * 2 / 176
    lean = (h / 2 - g) * 0.82
    lx = joint + slip
    return (f'<path fill="{main}" d="M{x} {y}H{sx}V{mid - g}H{joint}L{reach} {y}H{reach + arm}'
            f'L{joint + arm} {mid - g}H{sx}V{y + h}H{x}Z"/>'
            f'<path fill="{leg}" d="M{lx} {mid + g}H{lx + arm}L{lx + arm + lean} {y + h}H{lx + lean}Z"/>')


def icon(small=False):
    tile = f'<rect width="256" height="256" rx="56" fill="{TILE}"/>'
    if small:
        # 16-32 px: a heavier stem and a wider cut so the slip survives a few pixels.
        return tile + cut_k(48, 40, 176, 48, 50, 18, 22, WHITE, AMBER)
    return tile + cut_k(58, 40, 176, 42, 50, 12, 10, WHITE, AMBER)


def wordmark(main):
    """'Klipp' drawn from rectangles and arcs: cap 140, x-height 100, stroke 28."""
    st, xh, base, sp = 28, 100, 140, 26
    out = cut_k(0, 0, 140, 34, 40, 10, 8, main, AMBER)
    x = 152
    out += f'<rect x="{x}" y="0" width="{st}" height="{base}" fill="{main}"/>'
    x += st + sp
    out += (f'<rect x="{x}" y="{base - xh}" width="{st}" height="{xh}" fill="{main}"/>'
            f'<rect x="{x}" y="0" width="{st}" height="{st}" fill="{AMBER}"/>')
    x += st + sp
    r = xh / 2
    for _ in range(2):
        cx = x + st + r - 14
        out += (f'<rect x="{x}" y="{base - xh}" width="{st}" height="{xh + 44}" fill="{main}"/>'
                f'<path fill="{main}" fill-rule="evenodd" d="M{x + st - 1} {base - xh}H{cx}'
                f'A{r} {r} 0 0 1 {cx} {base}H{x + st - 1}Z M{x + st - 1} {base - xh + st - 2}'
                f'V{base - st + 2}H{cx}A{r - st + 2} {r - st + 2} 0 0 0 {cx} {base - xh + st - 2}Z"/>')
        x = cx + r + sp
    return out, x - sp


def svg(w, h, body, title):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
            f'viewBox="0 0 {w} {h}" role="img"><title>{title}</title>{body}</svg>\n')


def save(name, w, h, body, title):
    p = OUT / name
    p.write_text(svg(w, h, body, title))
    return p


def render(source, dest, w, h=None):
    args = ['rsvg-convert', '-w', str(w)] + (['-h', str(h)] if h else []) + ['-o', str(dest), str(source)]
    subprocess.run(args, check=True)


def lockup(main):
    """Horizontal lockup: icon tile then wordmark, transparent background."""
    word, width = wordmark(main)
    w = 200 + 60 + int(width) + 4
    body = (f'<g transform="scale(0.78125)">{icon()}</g>'
            f'<g transform="translate(260 18)">{word}</g>')
    return w, 200, body


master = save('klipp-icon.svg', 256, 256, icon(), 'Klipp cut K')
small = save('klipp-icon-small.svg', 256, 256, icon(True), 'Klipp cut K, small sizes')
for size in [16, 22, 24, 32, 48, 64, 128, 256, 512, 1024]:
    render(small if size <= 32 else master, OUT / f'klipp-icon-{size}.png', size, size)

w, h, body = lockup(WHITE)
dark = save('klipp-logo.svg', w, h, body, 'Klipp logo, for dark backgrounds')
render(dark, OUT / 'klipp-logo.png', w * 3)
w, h, body = lockup(TILE)
light = save('klipp-logo-light.svg', w, h, body, 'Klipp logo, for light backgrounds')
render(light, OUT / 'klipp-logo-light.png', w * 3)

# Wordmark alone, as the About screens draw it: 184 units tall (cap to descender).
for name, main in [('klipp-wordmark', WHITE), ('klipp-wordmark-light', TILE)]:
    word, width = wordmark(main)
    src = save(f'{name}.svg', int(width) + 1, 184, word, 'Klipp wordmark')
    render(src, OUT / f'{name}.png', (int(width) + 1) * 192 // 184, 192)

# Review sheet: the lockup on dark and light, and the icon at the sizes it ships.
W = 1120
body = f'<rect width="{W}" height="900" fill="#151619"/>'
body += f'<g transform="translate(80 80)">{lockup(WHITE)[2]}</g>'
body += f'<rect y="370" width="{W}" height="300" fill="{PAPER}"/>'
body += f'<g transform="translate(80 400)">{lockup(TILE)[2]}</g>'
body += '<g font-family="sans-serif"><text x="80" y="730" fill="#EAE7E0" font-size="18">Actual icon sizes</text>'
body += '<text x="80" y="756" fill="#A4A6AA" font-size="14">Klipp means cut: the K\'s leg is cut off and slipped, like a clip on a timeline. Wider cut at 16-32 px.</text></g>'
x = 80
for size in [16, 24, 32, 48, 64, 128]:
    body += f'<g transform="translate({x} {780 + 100 - size * 100 / 128}) scale({size / 256})">{icon(size <= 32)}</g>'
    x += size + 24
for i, (color, label) in enumerate([(AMBER, 'Amber'), (TILE, 'Charcoal'), (WHITE, 'Soft white')]):
    bx = 640 + i * 150
    body += f'<rect x="{bx}" y="790" width="120" height="44" rx="6" fill="{color}" stroke="#3a3c40"/>'
    body += f'<text x="{bx}" y="858" fill="#A4A6AA" font-family="sans-serif" font-size="13">{label} {color}</text>'
preview = save('preview.svg', W, 900, body, 'Klipp identity review sheet')
render(preview, OUT / 'preview.png', W, 900)
print(OUT / 'preview.png')

# The website serves its own copies; keep them in step with the masters.
SITE = OUT.parents[2] / 'website' / 'assets'
if SITE.is_dir():
    for name in ('klipp-icon.svg', 'klipp-wordmark.svg'):
        (SITE / name).write_text((OUT / name).read_text())
