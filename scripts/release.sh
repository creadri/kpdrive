#!/usr/bin/env bash
# Bump both crate versions and tag the result. The tag is what publishes a
# release, so the workflow refuses a tag that disagrees with the manifests.
#
#   scripts/release.sh 0.4.0      # then: git push origin dev v0.4.0
set -euo pipefail
cd "$(dirname "$0")/.."

version="${1:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.]+)?$ ]] || {
    echo "usage: $0 <version>   e.g. $0 0.4.0" >&2
    exit 1
}
[ -z "$(git status --porcelain)" ] || { echo "working tree is dirty" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/v$version" >/dev/null && {
    echo "tag v$version already exists" >&2
    exit 1
}

sed -i "0,/^version = \".*\"/s//version = \"$version\"/" Cargo.toml ui/Cargo.toml
cargo update --workspace --offline --quiet

# The draft comes from commit subjects; say what changed for users instead.
scripts/changelog-update.sh "$version"
echo "Review the $version section of CHANGELOG.md, then press Enter."
echo "Ctrl-C aborts; git checkout . undoes the version bump."
read -r
scripts/changelog.sh

git commit -qam "Release v$version"
git tag -a "v$version" -m "kpdrive $version"
echo "committed and tagged v$version. Push it: git push origin $(git branch --show-current) v$version"
