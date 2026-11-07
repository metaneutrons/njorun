#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
# Flash only the D1001 application slot (ota_0) after build.sh; partition layout unchanged.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${1:-/dev/cu.usbmodem1101}"
R="$ROOT/target/build/p4-build/cargo/riscv32imafc-esp-espidf/release"
OUT="$ROOT/target/flash"
mkdir -p "$OUT"
espflash save-image --chip esp32p4 --flash-size 32mb --merge --skip-padding \
  --bootloader "$R/build/bootloader.bin" \
  --partition-table "$ROOT/experiments/p4-build/partitions.d1001.csv" \
  "$R/njorun-p4-smoke" "$OUT/d1001-merged.bin" >/dev/null
tail -c +$((0x20000 + 1)) "$OUT/d1001-merged.bin" > "$OUT/d1001-app.bin"
espflash write-bin --chip esp32p4 --port "$PORT" 0x20000 "$OUT/d1001-app.bin"
espflash reset --port "$PORT"
