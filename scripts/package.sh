#!/usr/bin/env bash
# Turn build/Hermacos.app into the files a release ships:
#
#   dist/Hermacos-<version>.dmg   drag-to-Applications disk image
#   dist/Hermacos-<version>.zip   the app alone (what scripts/install.sh downloads)
#   dist/SHA256SUMS.txt
#
# Run scripts/build.sh first. When the app was signed with a Developer ID and notary
# credentials are present, the disk image is notarized and stapled so it opens without a
# Gatekeeper warning:
#
#   NOTARY_APPLE_ID, NOTARY_TEAM_ID, NOTARY_PASSWORD   Apple ID, team, app-specific password
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
app="build/Hermacos.app"
[[ -d "$app" ]] || { echo "build/Hermacos.app not found; run scripts/build.sh first" >&2; exit 1; }

version="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$app/Contents/Info.plist")"
identity="${CODESIGN_IDENTITY:--}"
dmg="dist/Hermacos-$version.dmg"
zip="dist/Hermacos-$version.zip"
mkdir -p dist
rm -f "$dmg" "$zip" dist/SHA256SUMS.txt

echo "==> Disk image"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
ditto "$app" "$staging/Hermacos.app"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname "Hermacos" -srcfolder "$staging" -fs HFS+ -format UDZO -ov "$dmg"

if [[ "$identity" != "-" ]]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
  if [[ -n "${NOTARY_APPLE_ID:-}" && -n "${NOTARY_TEAM_ID:-}" && -n "${NOTARY_PASSWORD:-}" ]]; then
    echo "==> Notarize"
    xcrun notarytool submit "$dmg" --wait \
      --apple-id "$NOTARY_APPLE_ID" --team-id "$NOTARY_TEAM_ID" --password "$NOTARY_PASSWORD"
    xcrun stapler staple "$dmg"
    # The ticket covers the app inside the image too; staple it so the zip is covered.
    xcrun stapler staple "$app"
  else
    echo "Signed but not notarized: NOTARY_APPLE_ID / NOTARY_TEAM_ID / NOTARY_PASSWORD not set."
  fi
fi

echo "==> Zip"
ditto -c -k --keepParent "$app" "$zip"

(cd dist && shasum -a 256 "$(basename "$dmg")" "$(basename "$zip")" > SHA256SUMS.txt)
echo "Packaged:"
ls -lh "$dmg" "$zip" | awk '{print "  " $9 "  " $5}'
