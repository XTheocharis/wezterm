#!/bin/bash
# Download OpenConsole.exe, OpenConsoleProxy.dll, and conpty.dll from a
# microsoft/terminal GitHub release. These replace the inbox Windows
# console host with an updated version that supports mouse reporting
# and the Default Terminal protocol.
# Defaults to the latest stable release; pass a tag to pin the version.
set -x
set -e

cd "$(git rev-parse --show-toplevel)"

TAG="${1:-latest}"
DEST=assets/windows/conhost
WORK=/tmp/wt-conhost-update

# Resolve the release tag and find download URLs. The ConPTY NuGet package
# uses a date-based version (like 1.24.260512001) that isn't in the tag,
# so we parse the release's asset list to find the right files.
if [[ "$TAG" == "latest" ]] ; then
  API_URL=https://api.github.com/repos/microsoft/terminal/releases/latest
else
  API_URL=https://api.github.com/repos/microsoft/terminal/releases/tags/$TAG
fi

RELEASE_JSON=$(curl -sSL "$API_URL")
TAG=$(printf '%s' "$RELEASE_JSON" \
  | sed -nE 's/.*"tag_name":[[:space:]]*"([^"]+)".*/\1/p' \
  | head -1)
X64_URL=$(printf '%s' "$RELEASE_JSON" \
  | grep -oE 'https://[^"]*releases/download/[^"]*_x64\.zip' \
  | head -1)
NUPKG_URL=$(printf '%s' "$RELEASE_JSON" \
  | grep -oE 'https://[^"]*releases/download/[^"]*ConPTY[^"]*\.nupkg' \
  | head -1)

test -n "$TAG"
test -n "$X64_URL"
test -n "$NUPKG_URL"

rm -rf "$WORK"
mkdir -p "$WORK"
curl -sSL -o "$WORK/wt_x64.zip" "$X64_URL"
curl -sSL -o "$WORK/conpty.nupkg" "$NUPKG_URL"

# The x64 zip contains the unpacked MSIX layout. Files sit under a
# terminal-<version>/ directory. Extract OpenConsole.exe and its COM
# proxy stub from here.
unzip -j -o "$WORK/wt_x64.zip" \
  'terminal-*/OpenConsole.exe' \
  'terminal-*/OpenConsoleProxy.dll' \
  -d "$DEST"

# conpty.dll is the ConPTY client library. It ships only in the separate
# ConPTY NuGet package and must match the OpenConsole.exe version because
# it spawns OpenConsole.exe as the ConPTY host at runtime.
unzip -j -o "$WORK/conpty.nupkg" \
  'runtimes/win-x64/native/conpty.dll' \
  -d "$DEST"

echo "Updated $DEST to $TAG"
