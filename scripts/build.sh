#!/usr/bin/env bash
# Build the Rust core, generate its Swift bindings, build the app and assemble Hermacos.app.
#
#   scripts/build.sh               release build for this Mac's architecture
#   scripts/build.sh --debug       debug Swift build (faster to iterate)
#   scripts/build.sh --universal   release build for Apple silicon and Intel (what CI ships)
#
# Environment:
#   VERSION            marketing version; defaults to the latest git tag, else 0.0.0
#   BUILD_NUMBER       build number; defaults to the commit count
#   CODESIGN_IDENTITY  signing identity; defaults to "-" (ad-hoc). A "Developer ID
#                      Application: …" identity also turns on the hardened runtime.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
config=release
universal=false
for arg in "$@"; do
  case "$arg" in
    --debug) config=debug ;;
    --universal) universal=true ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

version="${VERSION:-$(git describe --tags --abbrev=0 2>/dev/null || true)}"
version="${version#v}"
version="${version:-0.0.0}"
build_number="${BUILD_NUMBER:-$(git rev-list --count HEAD 2>/dev/null || echo 1)}"
identity="${CODESIGN_IDENTITY:--}"

echo "==> Rust core"
# The generated bindings are not checked in, so a fresh clone lacks their folders.
mkdir -p app/Vendor/lib build/bindings app/Sources/HermesCore app/Sources/hermes_coreFFI/include
if $universal; then
  cargo build --release -p hermes-core --target aarch64-apple-darwin --target x86_64-apple-darwin
  lipo -create \
    target/aarch64-apple-darwin/release/libhermes_core.a \
    target/x86_64-apple-darwin/release/libhermes_core.a \
    -output app/Vendor/lib/libhermes_core.a
  bindings_library=target/aarch64-apple-darwin/release/libhermes_core.dylib
else
  cargo build --release -p hermes-core
  install -m 644 target/release/libhermes_core.a app/Vendor/lib/libhermes_core.a
  bindings_library=target/release/libhermes_core.dylib
fi
cargo run --quiet --release -p uniffi-bindgen -- generate \
  --library "$bindings_library" --language swift --out-dir build/bindings
install -m 644 build/bindings/hermes_core.swift app/Sources/HermesCore/hermes_core.swift
install -m 644 build/bindings/hermes_coreFFI.h app/Sources/hermes_coreFFI/include/hermes_coreFFI.h

echo "==> Swift app ($config$($universal && echo ", universal"))"
swift_args=(--package-path app -c "$config")
$universal && swift_args+=(--arch arm64 --arch x86_64)
# The macOS 27 SDK turns @State into a macro whose plugin ships only with full Xcode. With just
# the Command Line Tools installed, build against the macOS 26 SDK they also carry.
developer_dir="$(xcode-select -p)"
if [[ ! -f "$developer_dir/Platforms/MacOSX.platform/Developer/usr/lib/swift/host/plugins/libSwiftUIMacros.dylib" ]]; then
  for sdk in "$developer_dir"/SDKs/MacOSX26*.sdk; do
    [[ -d "$sdk" ]] && swift_args+=(--sdk "$sdk")
  done
fi
swift build "${swift_args[@]}"
binary="$(swift build "${swift_args[@]}" --show-bin-path)/Hermacos"

echo "==> Bundle (version $version, build $build_number)"
bundle="build/Hermacos.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$binary" "$bundle/Contents/MacOS/Hermacos"
cp app/Resources/Info.plist "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$bundle/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_number" "$bundle/Contents/Info.plist"
[[ -f app/Resources/AppIcon.icns ]] && cp app/Resources/AppIcon.icns "$bundle/Contents/Resources/AppIcon.icns"

if [[ "$identity" == "-" ]]; then
  codesign --force --sign - --timestamp=none "$bundle" >/dev/null
else
  # Notarization requires the hardened runtime and a secure timestamp.
  codesign --force --options runtime --timestamp --sign "$identity" "$bundle"
fi
echo "Built $bundle ($(lipo -archs "$bundle/Contents/MacOS/Hermacos"))"
