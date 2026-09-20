#!/usr/bin/env bash
# Times the three numbers the plan moves. Needs bench/seed.sh to have run.
set -euo pipefail
K=${KPDRIVE:-target/release/kpdrive}; T=$(mktemp -d)
t() { local s; s=$(date +%s.%N); "$@" > /dev/null 2>&1; printf '%.1f s' "$(echo "$(date +%s.%N) - $s" | bc)"; }
echo "walk, forced full pass:  $(t "$K" sync --force)"
echo "download, 20 MiB:        $(t "$K" get kpdrive-bench/big.bin "$T/big.bin")"
cmp -s "$T/big.bin" "$(python3 -c 'import json,os;print(json.load(open(os.path.expanduser("~/.config/kpdrive/config.json")))["sync_folder"])')/kpdrive-bench/big.bin" && echo "  (download byte-identical)"
head -c $((20 * 1024 * 1024)) /dev/urandom > "$T/up-$$.bin"
echo "upload, 20 MiB:          $(t "$K" put "$T/up-$$.bin" kpdrive-bench)"
"$K" rm "kpdrive-bench/up-$$.bin" > /dev/null 2>&1 || echo "  (could not trash the upload; remove kpdrive-bench/up-$$.bin by hand)"
rm -rf "$T"
