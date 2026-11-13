#!/bin/zsh
# Memory of the native menu bar UI vs the upstream WebView UI, same binary.
#
# For each mode: launch, wait for it to settle, open the UI (popover or
# settings window), then add up the memory of Handy and of every WebKit
# process that appeared after launch (a WebView's content, networking and GPU
# helpers run as separate processes).
#
# Usage: scripts/menubar/bench-memory.sh ["/path/Handy Bar.app"]
set -e
APP=${1:-"$(cd "$(dirname "$0")/../.." && pwd)/src-tauri/target/debug/bundle/macos/Handy Bar.app"}
BIN="$APP/Contents/MacOS/handy"
cd "$(dirname "$0")/../../src-tauri"

webkit_pids() { pgrep -f 'com.apple.WebKit' | sort; }
footprint_mb() { footprint -p "$1" 2>/dev/null | awk '/phys_footprint:/ {gsub(/[^0-9.]/,"",$2); v=$2; u=$3; if (u ~ /KB/) v/=1024; if (u ~ /GB/) v*=1024; print v; exit}'; }
rss_mb() { ps -o rss= -p "$1" | awk '{printf "%.1f", $1/1024}'; }

measure() {
  local mode=$1
  pkill -f 'Handy Bar.app/Contents/MacOS/handy' || true
  sleep 1
  local before=$(webkit_pids)
  if [[ $mode == webview ]]; then
    HANDY_WEBVIEW_UI=1 "$BIN" >/dev/null 2>&1 &
  else
    "$BIN" >/dev/null 2>&1 &
  fi
  sleep 8
  local pid=$(pgrep -f 'Handy Bar.app/Contents/MacOS/handy' | head -1)
  # Open the UI the way a person would (second launch = "show the app").
  "$BIN" >/dev/null 2>&1 || true
  sleep 4
  local main_rss=$(rss_mb $pid)
  local main_fp=$(footprint_mb $pid)
  local extra_rss=0 extra_n=0
  for p in $(comm -13 <(echo "$before") <(webkit_pids)); do
    extra_rss=$(( extra_rss + $(ps -o rss= -p $p 2>/dev/null || echo 0) ))
    extra_n=$(( extra_n + 1 ))
  done
  printf '%-8s handy RSS %7s MB  footprint %7s MB  + %d WebKit procs %6.1f MB  = total RSS %7.1f MB\n' \
    $mode $main_rss ${main_fp:-?} $extra_n $(( extra_rss / 1024.0 )) $(( main_rss + extra_rss / 1024.0 ))
  pkill -f 'Handy Bar.app/Contents/MacOS/handy' || true
  sleep 1
}

measure native
measure webview
