#!/usr/bin/env bash
# Copy CHANGELOG.md into the files that carry their own changelog: Debian's,
# the spec's %changelog and the metainfo's <releases>. CHANGELOG.md is the one
# to edit; these are rewritten whole from it every time.
#
# It reads "## <version> - <YYYY-MM-DD>" headings and "- " items; a line
# indented under an item continues it.
set -euo pipefail
cd "$(dirname "$0")/.."

spec=packaging/kpdrive.spec
metainfo=packaging/be.otterit.kpdrive.metainfo.xml
# Entries are signed by the packager, whoever the commits came from.
who=$(sed -n 's/^Maintainer: //p' debian/control)

versions=() dates=() items=()
while IFS= read -r line; do
    if [[ $line =~ ^##\ ([^ ]+)\ -\ ([0-9-]+)$ ]]; then
        versions+=("${BASH_REMATCH[1]}")
        dates+=("${BASH_REMATCH[2]}")
        items+=("")
    elif [[ $line =~ ^-\ (.*) ]]; then
        items[-1]+="${BASH_REMATCH[1]}"$'\n'
    elif [[ $line =~ ^\ +([^ ].*) && -n ${items[-1]:-} ]]; then
        items[-1]="${items[-1]%$'\n'} ${BASH_REMATCH[1]}"$'\n'
    fi
done < CHANGELOG.md
[ ${#versions[@]} -gt 0 ] || { echo "no releases found in CHANGELOG.md" >&2; exit 1; }

deb="" rpm="" releases=""
for i in "${!versions[@]}"; do
    version=${versions[$i]} day=${dates[$i]} list=${items[$i]%$'\n'}
    [ -n "$list" ] || list="No changes besides the version."

    deb+="kpdrive ($version) unstable; urgency=medium"$'\n\n'
    while IFS= read -r item; do
        # Debian wants lines under 80 columns.
        deb+=$(fold -s -w 74 <<<"$item" | sed 's/ *$//; 1s/^/  * /; 2,$s/^/    /')$'\n'
    done <<<"$list"
    deb+=$'\n'" -- $who  $(LC_ALL=C date -R -d "$day 12:00 UTC")"$'\n\n'

    rpm+=$'\n'"* $(LC_ALL=C date -d "$day" '+%a %b %d %Y') $who - $version-1"$'\n'
    # A bare % in a spec starts a macro.
    rpm+=$(sed 's/%/%%/g; s/^/- /' <<<"$list")$'\n'

    releases+="    <release version=\"$version\" date=\"$day\"/>"$'\n'
done

printf '%s' "${deb%$'\n'}" > debian/changelog
sed -i '/^%changelog$/q' "$spec"
printf '%s' "${rpm#$'\n'}" >> "$spec"
awk -v releases="$releases" '
    /^  <\/releases>$/ { skip = 0 }
    !skip { print }
    /^  <releases>$/ { printf "%s", releases; skip = 1 }
' "$metainfo" > "$metainfo.new"
mv "$metainfo.new" "$metainfo"
