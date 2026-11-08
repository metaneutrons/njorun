# Njörun identity

The northern-window mark combines an angular N, aurora colors and a guiding
star. The wordmark is original geometric lettering, including the ö; no font
files or third-party illustration are embedded. Original artwork is licensed
GPL-3.0-or-later with the project.

- `mark.svg`: transparent square icon, viewBox 320 × 320.
- `wordmark-light.svg`: transparent horizontal lockup for light backgrounds.
- `wordmark-dark.svg`: transparent horizontal lockup for dark backgrounds.
- `boot.svg`: centered startup composition on midnight blue, 480 × 480.

The Home Assistant integration shows the mark and wordmarks as PNGs from
`custom_components/njorun/brand/` (Home Assistant 2026.3 or newer). They are
rendered from these SVGs with `scripts/brand/render-ha.sh` (librsvg) and
committed; rerun it after changing the artwork.

The boot image is intended to be centered with aspect ratio preserved on either
orientation. Match the surrounding surface to `#0c1d27`; do not stretch the image
or render the README tagline on the device. Startup progress/status belongs to
the application, outside the artwork. Using the asset does not imply that board
boot/display support is implemented.

Canonical geometry and compositions live in
[generate.py](../../scripts/brand/generate.py). These four SVGs are deliberately
committed generated assets so GitHub and the firmware build can consume them
without running a generator. Regenerate with:

    python3 scripts/brand/generate.py

CI regenerates them and rejects drift. All graphics use simple paths, circles,
rectangles and transforms, with no external resources, SVG text, filters or
scripts. They were rendered and visually checked with librsvg on macOS; the
embedded rendering path remains to be qualified.
