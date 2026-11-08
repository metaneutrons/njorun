#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
# Render the Home Assistant brand images of the njorun integration from the canonical SVGs.
# HA 2026.3+ shows custom_components/<domain>/brand/*.png in the UI. Needs librsvg
# (rsvg-convert). The PNGs are committed; rerun after changing assets/brand.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SRC="$ROOT/assets/brand"
OUT="$ROOT/custom_components/njorun/brand"
mkdir -p "$OUT"
render() { rsvg-convert "$@"; }
render -w 256 -h 256 -o "$OUT/icon.png" "$SRC/mark.svg"
render -w 512 -h 512 -o "$OUT/icon@2x.png" "$SRC/mark.svg"
render -h 128 -o "$OUT/logo.png" "$SRC/wordmark-light.svg"
render -h 256 -o "$OUT/logo@2x.png" "$SRC/wordmark-light.svg"
render -h 128 -o "$OUT/dark_logo.png" "$SRC/wordmark-dark.svg"
render -h 256 -o "$OUT/dark_logo@2x.png" "$SRC/wordmark-dark.svg"
