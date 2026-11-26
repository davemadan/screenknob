#!/bin/sh
# Builds the ScreenKnob Audio driver bundle (universal) into
# target/driver.noindex/ScreenKnobAudio.driver.
set -eu
cd "$(dirname "$0")/.."

OUT=target/driver.noindex/ScreenKnobAudio.driver
mkdir -p "$OUT/Contents/MacOS"
cp driver/Info.plist "$OUT/Contents/Info.plist"
clang -arch arm64 -arch x86_64 -mmacosx-version-min=14.2 -O2 -Wall -Wextra -Wno-unused-parameter \
  -bundle -o "$OUT/Contents/MacOS/ScreenKnobAudio" driver/ScreenKnobAudio.c \
  -framework CoreAudio -framework CoreFoundation
codesign --force --sign - "$OUT" >/dev/null
echo "Built $OUT"
