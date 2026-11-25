#!/bin/sh
# Builds ScreenKnob.app into dist/ (optionally installs it into /Applications).
#   ./scripts/make-app.sh            build dist/ScreenKnob.app
#   ./scripts/make-app.sh --install  also copy it to /Applications
set -eu
cd "$(dirname "$0")/.."

cargo build --release

APP=dist/ScreenKnob.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"
cp target/release/screenknob "$APP/Contents/MacOS/screenknob"

# Local (ad-hoc) signature with a fixed identity, so the permissions you grant
# survive updates. No Apple developer account is needed.
codesign --force --sign - --identifier craft.monitor-volume \
  --requirements '=designated => identifier "craft.monitor-volume"' "$APP"

echo "Built $APP"

if [ "${1:-}" = "--install" ]; then
  # Quit a running copy first; it hands the sound back to your screen as it closes.
  osascript -e 'quit app "ScreenKnob"' >/dev/null 2>&1 || true
  pkill -x screenknob >/dev/null 2>&1 || true
  rm -rf /Applications/ScreenKnob.app
  cp -R "$APP" /Applications/
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f /Applications/ScreenKnob.app
  echo "Installed /Applications/ScreenKnob.app"
fi
