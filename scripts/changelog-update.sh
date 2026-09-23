#!/usr/bin/env bash
# Add a section to CHANGELOG.md for a new version, drafted from the commit
# subjects since the last tag. Older sections are left alone, since they have
# been reviewed; this one is a draft to edit before release.sh goes on.
#
#   scripts/changelog-update.sh 0.5.0
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:?usage: $0 <version>}"
if grep -q "^## $version " CHANGELOG.md; then
    echo "CHANGELOG.md already has $version" >&2
    exit 1
fi

last=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
subjects=$(git log --no-merges --format='- %s' "${last:+$last..}HEAD" | grep -v '^- Release v' || true)
section="## $version - $(date +%F)"$'\n\n'"${subjects:-- No changes besides the version.}"$'\n'

# The new section goes under the title, above the previous release.
awk -v section="$section" 'NR == 2 { print ""; printf "%s", section } { print }' CHANGELOG.md > CHANGELOG.md.new
mv CHANGELOG.md.new CHANGELOG.md
