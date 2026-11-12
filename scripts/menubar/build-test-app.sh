#!/bin/zsh
# Builds "Handy Menubar.app" (debug) under its own bundle id, so it can run
# next to an installed Handy without the single-instance plugin handing
# launches to it, and with its own settings/history.
#
# Set HANDY_SIGNING_IDENTITY to a local code-signing identity to keep macOS
# permissions (Microphone, Accessibility) across rebuilds; ad-hoc builds
# lose them every time because each build is a new identity to macOS.
set -e
cd "$(dirname "$0")/../.."
SIGN=${HANDY_SIGNING_IDENTITY:--}
CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build --debug --bundles app --config \
  "{\"identifier\":\"com.pais.handy.menubar\",\"productName\":\"Handy Menubar\",\"bundle\":{\"createUpdaterArtifacts\":false,\"macOS\":{\"signingIdentity\":\"$SIGN\"}}}"
echo "$(pwd)/src-tauri/target/debug/bundle/macos/Handy Menubar.app"
