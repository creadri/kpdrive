#!/usr/bin/env bash
# Removes the seeded tree locally and lets sync trash it remotely.
set -euo pipefail
K=${KPDRIVE:-target/release/kpdrive}
ROOT=$(python3 -c 'import json,os;print(json.load(open(os.path.expanduser("~/.config/kpdrive/config.json")))["sync_folder"])')
rm -rf "$ROOT/kpdrive-bench"
"$K" sync | tail -1
