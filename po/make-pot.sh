#!/usr/bin/env bash
# Rebuilds po/kpdrive.pot from both languages the sources are written in.
#
# Rust literals parse closely enough as C; QML's expression syntax is
# JavaScript, so xgettext reaches the strings inside a nested block.
set -euo pipefail
cd "$(dirname "$0")/.."

xgettext --language=C --from-code=UTF-8 --add-comments=TRANSLATORS \
  --keyword=t --keyword=tn:1,2 --keyword=lookup --keyword=lookup_plural:1,2 \
  --package-name=kpdrive --copyright-holder="kpdrive contributors" \
  --msgid-bugs-address="https://github.com/creadri/kpdrive/issues" \
  -o po/kpdrive.pot src/*.rs ui/src/*.rs

xgettext --language=JavaScript --from-code=UTF-8 --add-comments=TRANSLATORS \
  --keyword=i18n --keyword=i18np:1,2 \
  --join-existing -o po/kpdrive.pot ui/qml/*.qml

echo "po/kpdrive.pot: $(grep -c '^msgid "' po/kpdrive.pot) entries"
for po in po/*.po; do
  [ -e "$po" ] || continue
  msgmerge --quiet --update --backup=none "$po" po/kpdrive.pot
  printf '%s: ' "$po"; msgfmt --statistics -o /dev/null "$po"
done
