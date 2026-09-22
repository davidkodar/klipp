"""Build the refined Klipp vector concept and PNG exports. Requires rsvg-convert."""
from pathlib import Path
import subprocess

OUT = Path(__file__).resolve().parent
AMBER, TILE, WHITE = '#D99A38', '#242629', '#EAE7E0'

def rect(x,y,w,h,r,fill):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}"/>'

def icon(small=False):
    s = rect(0,0,256,256,52,TILE)
    if small:
        s += rect(24,88,208,88,4,AMBER)
        for x,w in [(36,52),(100,48),(176,44)]:
            s += rect(x,100,w,64,0,TILE)
        s += rect(150,84,22,96,0,TILE)
        s += f'<path fill="{WHITE}" d="M148 68H176L168 80V192H156V80Z"/>'
        return s
    s += rect(28,91,200,80,5,AMBER)
    for x,w in [(35,55),(98,55),(167,54)]:
        s += rect(x,107 if not small else 103,w,48 if not small else 56,3,TILE)
    if not small:
        for x in [38,57,76,101,120,139,172,191,210]:
            for y in [95,159]:
                s += rect(x,y,7,8,1.5,TILE)
    # The edit seam doubles as a playhead, aligned with the frame boundary.
    s += rect(156,88,8,87,0,TILE)
    s += f'<path fill="{WHITE}" d="M152 76H168L162 84V188H158V84Z"/>'
    return s

# A compact geometric wordmark drawn as paths: no font installation required.
def wordmark():
    return f'''<g fill="{WHITE}">
    <path d="M0 0H8V30L34 0H44L17 31L46 68H36L12 37L8 42V68H0Z"/>
    <path d="M56 0H64V56Q64 61 69 61H72V68H68Q56 68 56 56Z"/>
    <path d="M83 21H91V68H83Z"/><circle cx="87" cy="6" r="4.5"/>
    <path fill-rule="evenodd" d="M105 21H113V27Q120 20 129 20C144 20 152 30 152 44.5S144 69 130 69Q120 69 113 62V86H105ZM113 44.5C113 55 119 62 129 62S144 55 144 44.5S139 27 129 27S113 34 113 44.5Z"/>
    <path fill-rule="evenodd" d="M165 21H173V27Q180 20 189 20C204 20 212 30 212 44.5S204 69 190 69Q180 69 173 62V86H165ZM173 44.5C173 55 179 62 189 62S204 55 204 44.5S199 27 189 27S173 34 173 44.5Z"/>
    </g>'''

def svg(w,h,body,title):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img"><title>{title}</title>{body}</svg>\n'

def save(name,w,h,body,title):
    p=OUT/name
    p.write_text(svg(w,h,body,title))
    return p

def render(source,dest,w,h=None):
    subprocess.run(['rsvg-convert','-w',str(w),'-h',str(h or w),'-o',str(dest),str(source)],check=True)

master=save('klipp-icon.svg',256,256,icon(),'Klipp filmstrip and editing playhead')
small=save('klipp-icon-small.svg',256,256,icon(True),'Klipp simplified small icon')
lockup=save('klipp-logo.svg',320,398,
    '<g transform="translate(32 8)">'+icon()+'</g><g transform="translate(54 298)">'+wordmark()+'</g>',
    'Klipp logo')
for size in [16,22,24,32,48,64,128,256,512,1024]:
    render(small if size <= 32 else master,OUT/f'klipp-icon-{size}.png',size)
render(lockup,OUT/'klipp-logo.png',640,796)

body=rect(0,0,1120,800,0,'#151619')
body+='<g transform="translate(90 82) scale(1.5)">'+icon()+'</g>'
body+='<g transform="translate(123 524) scale(1.5)">'+wordmark()+'</g>'
body+='<g fill="#EAE7E0" font-family="sans-serif"><text x="604" y="118" font-size="24">Klipp / refined identity</text><text x="604" y="156" fill="#A4A6AA" font-size="16">Film frames. One precise edit.</text></g>'
for i,(color,label) in enumerate([(AMBER,'Muted amber'),(TILE,'Charcoal'),(WHITE,'Soft white')]):
    x=604+i*142
    body+=rect(x,207,112,64,8,color)
    body+=f'<g font-family="sans-serif" font-size="14" fill="#A4A6AA"><text x="{x}" y="300">{label}</text><text x="{x}" y="324">{color}</text></g>'
body+='<text x="604" y="402" fill="#EAE7E0" font-family="sans-serif" font-size="18">Actual icon sizes</text>'
x=604
for size in [16,24,32,48,64,128]:
    body+=f'<g transform="translate({x} {452+128-size}) scale({size/256})">'+icon(size<=32)+'</g>'
    body+=f'<text x="{x+size/2}" y="609" text-anchor="middle" fill="#A4A6AA" font-family="sans-serif" font-size="12">{size}</text>'
    x+=size+15
body+='<text x="604" y="673" fill="#A4A6AA" font-family="sans-serif" font-size="14">Simplified geometry at 16–32 px</text>'
preview=save('preview.svg',1120,800,body,'Klipp refined logo and icon size review')
render(preview,OUT/'preview.png',1120,800)
print(OUT/'preview.png')
