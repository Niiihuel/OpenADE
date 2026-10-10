#!/bin/bash
set -euo pipefail
# Executed by EAS on macOS, or manually from apps/ios on a Mac.
cd "$(dirname "$0")/.."
mkdir -p build/reports
xcodebuild -version > build/reports/xcode-version.txt
python3 scripts/generate_project.py
xcodebuild -project ios/OpenADERemote.xcodeproj -scheme OpenADERemote \
  -configuration Debug -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build/DerivedData CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- build \
  2>&1 | tee build/reports/build.log

# Validate the actual production Swift branches too, without modifying any
# Apple signing capability/profile while the previous TestFlight upload waits.
# This compiles Release for the simulator; XCTest and launch remain Debug.
xcodebuild -project ios/OpenADERemote.xcodeproj -scheme OpenADERemote \
  -configuration Release -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' \
  -derivedDataPath build/ReleaseDerivedData CODE_SIGNING_ALLOWED=NO build \
  2>&1 | tee build/reports/release-build.log

# Choose an available iPhone from the runner's actual simulator inventory.
xcrun simctl list devices available -j > build/reports/simulators.json
OPENADE_SIMULATOR_ID=$(python3 - <<'PY'
import json
from pathlib import Path
devices = json.loads(Path('build/reports/simulators.json').read_text())['devices']
for runtime in sorted(devices, reverse=True):
    if 'iOS' not in runtime:
        continue
    for device in devices[runtime]:
        if device['name'].startswith('iPhone'):
            print(device['udid'])
            raise SystemExit(0)
raise SystemExit('No iPhone simulator available on this runner')
PY
)
xcodebuild -project ios/OpenADERemote.xcodeproj -scheme OpenADERemote \
  -configuration Debug -destination "platform=iOS Simulator,id=$OPENADE_SIMULATOR_ID" \
  -derivedDataPath build/DerivedData -resultBundlePath build/reports/Tests.xcresult \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_IDENTITY=- test 2>&1 | tee build/reports/tests.log

OPENADE_APP=build/DerivedData/Build/Products/Debug-iphonesimulator/OpenADERemote.app
codesign -d --entitlements :- "$OPENADE_APP" > build/reports/simulator-entitlements.plist 2> build/reports/codesign.log
xcrun simctl bootstatus "$OPENADE_SIMULATOR_ID" -b
xcrun simctl status_bar "$OPENADE_SIMULATOR_ID" override --time '9:41' --dataNetwork wifi --wifiMode active --wifiBars 3 --batteryState charged --batteryLevel 100
xcrun simctl install "$OPENADE_SIMULATOR_ID" "$OPENADE_APP"
defaults write com.apple.iphonesimulator ConnectHardwareKeyboard -bool NO
xcrun simctl ui "$OPENADE_SIMULATOR_ID" appearance dark
xcrun simctl launch "$OPENADE_SIMULATOR_ID" dev.openade.remote
sleep 3
xcrun simctl io "$OPENADE_SIMULATOR_ID" screenshot build/reports/screenshot.png
# Explicit DEBUG-only fixtures exercise the internal layouts without a live
# host or credentials. These screenshots are labeled fixture-* and are never
# evidence of a real PC session; Release builds exclude the fixture code.
# Warm up the first-use keyboard before the composer captures. Its one-time
# QuickPath introduction otherwise covers the first focused fixture.
xcrun simctl terminate "$OPENADE_SIMULATOR_ID" dev.openade.remote || true
xcrun simctl launch "$OPENADE_SIMULATOR_ID" dev.openade.remote --ui-fixture=permissions
sleep 3
for OPENADE_FIXTURE_SCENE in projects empty new-task composer attachments permissions models advanced chat files changes git-diff git-commit git-comment source connections large permissions-large models-large; do
  xcrun simctl terminate "$OPENADE_SIMULATOR_ID" dev.openade.remote || true
  xcrun simctl launch "$OPENADE_SIMULATOR_ID" dev.openade.remote "--ui-fixture=$OPENADE_FIXTURE_SCENE"
  sleep 3
  xcrun simctl io "$OPENADE_SIMULATOR_ID" screenshot "build/reports/fixture-$OPENADE_FIXTURE_SCENE.png"
done
xcrun simctl ui "$OPENADE_SIMULATOR_ID" appearance light
xcrun simctl terminate "$OPENADE_SIMULATOR_ID" dev.openade.remote || true
xcrun simctl launch "$OPENADE_SIMULATOR_ID" dev.openade.remote --ui-fixture=chat
sleep 2
xcrun simctl io "$OPENADE_SIMULATOR_ID" screenshot build/reports/fixture-chat-light.png
for OPENADE_FIXTURE_SCENE in git-diff permissions; do
  xcrun simctl terminate "$OPENADE_SIMULATOR_ID" dev.openade.remote || true
  xcrun simctl launch "$OPENADE_SIMULATOR_ID" dev.openade.remote "--ui-fixture=$OPENADE_FIXTURE_SCENE"
  sleep 3
  xcrun simctl io "$OPENADE_SIMULATOR_ID" screenshot "build/reports/fixture-$OPENADE_FIXTURE_SCENE-light.png"
done
xcrun simctl spawn "$OPENADE_SIMULATOR_ID" log show --last 2m --predicate 'process == "OpenADERemote"' --style compact > build/reports/simulator-console.log
tar -czf build/OpenADERemote-simulator.tar.gz -C build/DerivedData/Build/Products/Debug-iphonesimulator OpenADERemote.app
