#!/bin/zsh
# Builds "Handy Bar.app" (debug). The name, bundle id and icon come from
# src-tauri/tauri.macos.conf.json, so it runs beside an installed upstream
# Handy with its own settings and history.
#
# Set HANDY_SIGNING_IDENTITY to a local code-signing identity to keep macOS
# permissions (Microphone, Accessibility) across rebuilds; ad-hoc builds
# lose them every time because each build is a new identity to macOS.
set -e
cd "$(dirname "$0")/../.."
SIGN=${HANDY_SIGNING_IDENTITY:--}
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build --debug --bundles app --config \
  "{\"bundle\":{\"macOS\":{\"signingIdentity\":\"$SIGN\"}}}"
echo "$(pwd)/src-tauri/target/debug/bundle/macos/Handy Bar.app"
