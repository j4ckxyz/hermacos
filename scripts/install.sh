#!/bin/sh
# Install or update Hermacos from the latest GitHub release:
#
#   curl -fsSL https://raw.githubusercontent.com/j4ckxyz/hermacos/main/scripts/install.sh | sh
#
# Downloads the release zip, puts Hermacos.app in /Applications (or ~/Applications when that
# isn't writable) and opens it. Environment, mostly for testing:
#
#   HERMACOS_REPO   owner/name of the GitHub repository
#   HERMACOS_ZIP    URL of a zip to install instead of the latest release
#   HERMACOS_DEST   folder to install into
#   HERMACOS_OPEN   set to 0 to not open the app afterwards
set -eu

repo="${HERMACOS_REPO:-j4ckxyz/hermacos}"
minimum_macos=26

[ "$(uname -s)" = "Darwin" ] || { echo "Hermacos runs on macOS only." >&2; exit 1; }
major="$(sw_vers -productVersion | cut -d. -f1)"
if [ "$major" -lt "$minimum_macos" ]; then
  echo "Hermacos needs macOS $minimum_macos or later; this Mac runs $(sw_vers -productVersion)." >&2
  exit 1
fi

url="${HERMACOS_ZIP:-}"
if [ -z "$url" ]; then
  url="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" \
    | grep '"browser_download_url"' | grep '\.zip"' | head -n 1 | cut -d '"' -f 4)"
  [ -n "$url" ] || { echo "No release found for $repo." >&2; exit 1; }
fi

dest="${HERMACOS_DEST:-/Applications}"
if [ ! -w "$dest" ]; then
  dest="$HOME/Applications"
  mkdir -p "$dest"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
echo "Downloading $url"
curl -fL --progress-bar "$url" -o "$work/Hermacos.zip"
ditto -x -k "$work/Hermacos.zip" "$work/unpacked"
[ -d "$work/unpacked/Hermacos.app" ] || { echo "The download did not contain Hermacos.app." >&2; exit 1; }

# Replace a running copy cleanly.
if pgrep -x Hermacos >/dev/null 2>&1; then
  osascript -e 'tell application id "app.hermacos.Hermacos" to quit' >/dev/null 2>&1 || true
  sleep 1
fi
rm -rf "${dest:?}/Hermacos.app"
mv "$work/unpacked/Hermacos.app" "$dest/Hermacos.app"

version="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$dest/Hermacos.app/Contents/Info.plist")"
echo "Installed Hermacos $version in $dest"
[ "${HERMACOS_OPEN:-1}" = "0" ] || open "$dest/Hermacos.app"
