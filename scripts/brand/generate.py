#!/usr/bin/env python3
"""Regenerate the original, font-independent Njörun SVG identity."""
from pathlib import Path
import os

os.chdir(Path(__file__).resolve().parents[2])
# Original vector geometry. No font or third-party illustration is embedded.
mark = '''<g id="northern-window">
  <rect x="28" y="28" width="264" height="264" rx="72" fill="#142d3a"/>
  <g id="folded-n" transform="translate(-7.5 -4.5)">
    <path d="M82 222V121L131 82V183Z" fill="#75e0c2"/>
    <path d="M131 82L222 184V237L131 135Z" fill="#46bfae"/>
    <path d="M222 184V109L253 84V212L222 237Z" fill="#a8eee1"/>
    <path d="M82 222L131 183L167 223L139 247H107Z" fill="#c5f3e8"/>
  </g>
  <path d="M199 63L205 80L222 86L205 92L199 109L193 92L176 86L193 80Z" fill="#f0cb8d"/>
  <path d="M58 257Q152 303 261 253" fill="none" stroke="#294b57" stroke-width="3" stroke-linecap="round"/>
</g>'''
word = '''<g id="wordmark" fill="none" stroke="{ink}" stroke-width="7.5" stroke-linecap="round" stroke-linejoin="round">
  <path d="M4 62V4L47 62V4"/>
  <path d="M72 25V68Q72 80 59 80"/>
  <path d="M98 43Q98 23 116 23Q134 23 134 43Q134 63 116 63Q98 63 98 43Z"/>
  <path d="M158 62V25M158 38Q164 23 179 25"/>
  <path d="M199 25V47Q199 63 214 63Q230 63 230 47V25M230 47V62"/>
  <path d="M255 62V25M255 39Q255 23 272 23Q288 23 288 39V62"/>
</g>
<g fill="{ink}"><circle cx="72" cy="7" r="4.5"/><circle cx="106" cy="7" r="4"/><circle cx="126" cy="7" r="4"/></g>'''
Path('assets/brand/mark.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 320" role="img" aria-labelledby="title"><title id="title">Njörun — northern window</title>'+mark+'</svg>\n')
for variant, ink in [('dark','#edf8f5'),('light','#17323d')]:
 svg=f'''<svg xmlns="http://www.w3.org/2000/svg" width="760" height="280" viewBox="0 0 760 280" role="img" aria-labelledby="title description">
<title id="title">Njörun</title><desc id="description">An aurora-green folded N in a rounded northern window, with a warm guiding star.</desc>
<g transform="translate(10 10) scale(.8125)">{mark}</g>
<g transform="translate(326 89) scale(1.23)">{word.format(ink=ink)}</g>
</svg>\n'''
 Path(f'assets/brand/wordmark-{variant}.svg').write_text(svg)
boot=f'''<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480" viewBox="0 0 480 480" role="img" aria-labelledby="title description">
<title id="title">Njörun</title><desc id="description">Njörun startup artwork. Northern window and outlined wordmark on midnight blue.</desc>
<rect width="480" height="480" fill="#0c1d27"/>
<g transform="translate(112 53) scale(.8)">{mark}</g>
<g transform="translate(123 332) scale(.8)">{word.format(ink='#edf8f5')}</g>
</svg>\n'''
Path('assets/brand/boot.svg').write_text(boot)
