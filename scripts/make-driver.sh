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

# A copy for tests that shares sound under its own name, so tests never touch
# the real driver's sound. Run: target/test_driver target/driver-test.noindex/ScreenKnobAudio
TEST=target/driver-test.noindex
mkdir -p "$TEST"
clang -mmacosx-version-min=14.2 -O2 -bundle -DSK_SHM_NAME='"/screenknob.test.1"' -o "$TEST/ScreenKnobAudio" \
  driver/ScreenKnobAudio.c -framework CoreAudio -framework CoreFoundation
clang -O2 -DSK_SHM_NAME='"/screenknob.test.1"' driver/test_driver.c -o target/test_driver \
  -framework CoreAudio -framework CoreFoundation
echo "Built test copy (shares sound as /screenknob.test.1) and target/test_driver"
