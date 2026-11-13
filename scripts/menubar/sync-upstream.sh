#!/bin/zsh
# Brings the latest Handy (cjpais/handy) into this fork.
#
#   scripts/menubar/sync-upstream.sh          merge upstream/main into this branch
#   scripts/menubar/sync-upstream.sh --check  only report how far behind we are
#
# The fork keeps upstream merges cheap by living almost entirely in new files
# (src-tauri/src/menubar/, docs/menubar/, scripts/menubar/). Only three
# upstream files carry small hooks, so those are the only likely conflicts:
#
#   src-tauri/src/lib.rs      tray click → popover, skip WebView windows
#   src-tauri/src/overlay.rs  recording overlay → native HUD
#   src-tauri/src/tray.rs     tray state shared with the popover
#
# Every hook is behind `#[cfg(target_os = "macos")]` and `menubar::enabled()`,
# so when resolving a conflict keep upstream's code and re-add the hook.
#
# We merge (not rebase) because the branch is public: rebasing would rewrite
# history other people may have pulled.
set -euo pipefail
cd "$(dirname "$0")/../.."

UPSTREAM_URL=https://github.com/cjpais/handy.git
git remote get-url upstream >/dev/null 2>&1 || git remote add upstream "$UPSTREAM_URL"
git fetch --quiet upstream main

behind=$(git rev-list --count HEAD..upstream/main)
echo "This branch is $behind commit(s) behind upstream/main."
[[ ${1:-} == --check || $behind == 0 ]] && exit 0

if [[ -n $(git status --porcelain --untracked-files=no) ]]; then
  echo "Commit or stash your changes first." >&2
  exit 1
fi

if ! git merge --no-edit upstream/main; then
  echo
  echo "Merge conflicts in:"
  git diff --name-only --diff-filter=U | sed 's/^/  /'
  echo
  echo "Resolve them (keep upstream's code, re-add the menubar hook), then:"
  echo "  git add -A && git commit --no-edit && scripts/menubar/sync-upstream.sh"
  exit 1
fi

echo "Merged. Running the menu bar tests…"
(cd src-tauri && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4} cargo test --lib menubar)
echo "Done. Review with 'git log --oneline -n $behind' and push when happy."
