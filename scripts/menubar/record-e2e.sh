#!/bin/zsh
# Records the whole screen while the end-to-end test runs, then saves an
# H.264 MP4 next to the test results.
#
# Recording uses the macOS screenshot toolbar (⌘⇧5), the same tool a person
# would use, so it needs no extra permissions beyond Accessibility for the
# process that runs this script.
#
# Usage: scripts/menubar/record-e2e.sh [phase] [output.mp4]
set -e
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
PHASE=${1:-all}
OUT=${2:-$ROOT/docs/menubar/e2e-recording.mp4}
WORK=$(mktemp -d)

swiftc -O -o "$WORK/e2e" "$ROOT/scripts/menubar/e2e/HandyMenubarE2E.swift"

ui() { osascript -e "tell application \"System Events\" to tell process \"screencaptureui\" to $1"; }
before=$(ls -t ~/Desktop | grep -E '^Screen Recording' | head -1 || true)

# Start a full-screen recording.
osascript -e 'tell application "System Events" to key code 23 using {command down, shift down}'
for i in {1..30}; do ui 'exists window 1' 2>/dev/null | grep -q true && break; sleep 0.1; done
ui 'click button 5 of group 1 of window 1'   # Record Entire Screen
sleep 0.3
ui 'click button 8 of group 1 of window 1'   # Record
sleep 2

set +e
cd "$ROOT"
"$WORK/e2e" --phase "$PHASE" --results "$ROOT/docs/menubar/e2e-results.json" | tee "$ROOT/docs/menubar/e2e-log.txt"
status=${pipestatus[1]}
set -e

# Stop recording (⌃⌘⎋) and wait for the file.
osascript -e 'tell application "System Events" to key code 53 using {command down, control down}'
for i in {1..100}; do
  f=$(ls -t ~/Desktop | grep -E '^Screen Recording' | head -1 || true)
  [[ -n $f && $f != $before ]] && break
  sleep 0.3
done
sleep 3
ffmpeg -v error -y -i ~/Desktop/"$f" -vf "scale=1920:-2" -c:v libx264 -preset medium -crf 26 -pix_fmt yuv420p -movflags +faststart "$OUT"
mv ~/Desktop/"$f" "$WORK/raw.mov"
echo "Recording: $OUT (raw: $WORK/raw.mov)"
exit $status
