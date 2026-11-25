#!/bin/sh
# Builds release/ScreenKnob-<version>.dmg (a disk image with the app and an
# Applications shortcut to drag it onto) and release/ScreenKnob-<version>.zip.
set -eu
cd "$(dirname "$0")/.."

./scripts/make-app.sh

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=release
STAGE=$(mktemp -d)
mkdir -p "$OUT"

APP=target/bundle.noindex/ScreenKnob.app
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

rm -f "$OUT/ScreenKnob-$VERSION.dmg" "$OUT/ScreenKnob-$VERSION.zip"
# A zip rather than a loose app, so the project folder doesn't add another
# ScreenKnob to the Apps list.
ditto -c -k --keepParent "$APP" "$OUT/ScreenKnob-$VERSION.zip"
hdiutil create -volname "ScreenKnob" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$OUT/ScreenKnob-$VERSION.dmg" >/dev/null
rm -rf "$STAGE"

echo "Built $OUT/ScreenKnob-$VERSION.dmg and $OUT/ScreenKnob-$VERSION.zip"
