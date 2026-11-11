#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Fabian Schmieder
# SPDX-License-Identifier: GPL-3.0-or-later
# Install packages on a GitHub Ubuntu runner and survive a broken package mirror.
#
# The runner image reads its package sources through /etc/apt/apt-mirrors.txt, which points at
# azure.archive.ubuntu.com. When that host does not resolve, every apt call of a job fails after
# many minutes. This script switches the list to the main Ubuntu archive first and then repeats
# update and install a few times. Usage: scripts/ci/apt-install.sh PACKAGE...
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "Usage: $0 PACKAGE..." >&2
    exit 2
fi

MIRRORS=/etc/apt/apt-mirrors.txt
if [[ -f "$MIRRORS" ]] && grep -q 'azure.archive.ubuntu.com' "$MIRRORS"; then
    echo "Using archive.ubuntu.com instead of azure.archive.ubuntu.com"
    echo "http://archive.ubuntu.com/ubuntu/ priority:1" | sudo tee "$MIRRORS" >/dev/null
fi

APT_OPTIONS=(-o Acquire::Retries=3 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20)
attempt=1
until sudo apt-get "${APT_OPTIONS[@]}" update &&
    sudo apt-get "${APT_OPTIONS[@]}" install --yes --no-install-recommends "$@"; do
    if [[ $attempt -ge 3 ]]; then
        echo "apt failed after $attempt attempts" >&2
        exit 1
    fi
    attempt=$((attempt + 1))
    echo "apt failed, attempt $attempt of 3 in 30 seconds" >&2
    sleep 30
done
