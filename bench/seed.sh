#!/usr/bin/env bash
# Seeds a reproducible tree into the sync folder and pushes it with one sync:
# 20 folders x 10 small files, one folder of 160 files (so listing needs two
# 150-entry chunks), and one 20 MiB file. Tear down with bench/teardown.sh.
set -euo pipefail
K=${KPDRIVE:-target/release/kpdrive}
ROOT=$(python3 -c 'import json,os;print(json.load(open(os.path.expanduser("~/.config/kpdrive/config.json")))["sync_folder"])')
B="$ROOT/kpdrive-bench"
mkdir -p "$B"
for d in $(seq -w 1 20); do
  mkdir -p "$B/d$d"
  for f in $(seq -w 1 10); do head -c $((1024 + RANDOM % 3072)) /dev/urandom > "$B/d$d/f$f.bin"; done
done
mkdir -p "$B/wide"; for f in $(seq -w 1 160); do head -c 1024 /dev/urandom > "$B/wide/f$f.bin"; done
head -c $((20 * 1024 * 1024)) /dev/urandom > "$B/big.bin"
echo "seeded $(find "$B" -type f | wc -l) files under $B; pushing (every small file is ~5 round trips)"
start=$(date +%s); "$K" sync > /dev/null; echo "pushed in $(( $(date +%s) - start )) s"
