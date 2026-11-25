#!/bin/sh
# Builds release/ScreenKnob.app and release/ScreenKnob-<version>.dmg: a disk image
# with the app and an Applications shortcut to drag it onto.
set -eu
cd "$(dirname "$0")/.."

./scripts/make-app.sh

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=release
STAGE=$(mktemp -d)
mkdir -p "$OUT"

cp -R dist/ScreenKnob.app "$STAGE/"
ln -s /Applications "$STAGE/Applications"

rm -rf "$OUT/ScreenKnob.app" "$OUT/ScreenKnob-$VERSION.dmg"
cp -R dist/ScreenKnob.app "$OUT/"
hdiutil create -volname "ScreenKnob" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$OUT/ScreenKnob-$VERSION.dmg" >/dev/null
rm -rf "$STAGE"

echo "Built $OUT/ScreenKnob.app and $OUT/ScreenKnob-$VERSION.dmg"
