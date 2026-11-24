#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

if [[ ! -x "${REPO_ROOT}/ui/node_modules/.bin/tauri" ]]; then
  echo "Installing frontend dependencies in ui/..."
  npm ci --prefix "${REPO_ROOT}/ui"
fi

echo "==> Building Coral&Kai Companion .app bundle (aarch64-apple-darwin)..."
CI=true "${REPO_ROOT}/ui/node_modules/.bin/tauri" build --target aarch64-apple-darwin --bundles app

TARGET_DIR="$(python3 -c 'import json, subprocess; print(json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"]))["target_directory"])')"
VERSION="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "${REPO_ROOT}/src-tauri/tauri.conf.json")"

APP_PATH="${TARGET_DIR}/aarch64-apple-darwin/release/bundle/macos/Coral&Kai Companion.app"
DMG_DIR="${TARGET_DIR}/aarch64-apple-darwin/release/bundle/dmg"
DMG_PATH="${DMG_DIR}/Coral&Kai Companion_${VERSION}_aarch64.dmg"

if [[ ! -d "${APP_PATH}" ]]; then
  echo "ERROR: Built .app bundle not found at ${APP_PATH}" >&2
  exit 1
fi

echo "==> Verifying zero bundled model weights (BYOM)..."
FOUND_WEIGHTS="$(find "${APP_PATH}" -type f \( -name '*.gguf' -o -name '*.bin' -o -name '*.onnx' \))"
if [[ -n "${FOUND_WEIGHTS}" ]]; then
  echo "ERROR: Found bundled model weights inside .app bundle:" >&2
  echo "${FOUND_WEIGHTS}" >&2
  exit 1
fi

echo "==> Stripping extended attributes & .gitkeep placeholders before sealing signature..."
xattr -cr "${APP_PATH}"
find "${APP_PATH}/Contents/Resources" -name ".gitkeep" -delete

echo "==> Ad-hoc signing nested runtimes and sealing .app bundle with entitlements..."
DYLIB_PATH="${APP_PATH}/Contents/Resources/resources/runtime/libwhisper_bridge.dylib"
if [[ -f "${DYLIB_PATH}" ]]; then
  install_name_tool -id "@rpath/libwhisper_bridge.dylib" "${DYLIB_PATH}"
  codesign --force --sign - "${DYLIB_PATH}"
fi

while IFS= read -r -d '' bin_file; do
  if file -b "${bin_file}" | grep -q "Mach-O"; then
    codesign --force --sign - "${bin_file}"
  fi
done < <(find "${APP_PATH}/Contents/Resources" -type f \( -name "*.dylib" -o -name "*.so" \) -print0)

codesign --force --sign - --entitlements "${REPO_ROOT}/src-tauri/entitlements.plist" "${APP_PATH}"

echo "==> Verifying sealed .app signature (strict + deep)..."
codesign --verify --deep --strict --verbose=4 "${APP_PATH}"
codesign -dvvv --entitlements - "${APP_PATH}" 2>&1

echo "==> Packaging sealed .app into compressed .dmg..."
mkdir -p "${DMG_DIR}"
rm -f "${DMG_DIR}"/*.dmg "${DMG_DIR}"/SHA256SUMS.txt

STAGING_DIR="$(mktemp -d)"
trap 'rm -rf "${STAGING_DIR}"' EXIT

ditto "${APP_PATH}" "${STAGING_DIR}/Coral&Kai Companion.app"
ln -s /Applications "${STAGING_DIR}/Applications"

hdiutil create -volname "Coral&Kai Companion" \
  -srcfolder "${STAGING_DIR}" \
  -ov -format UDZO \
  "${DMG_PATH}"

hdiutil verify "${DMG_PATH}"

(cd "${DMG_DIR}" && shasum -a 256 "$(basename "${DMG_PATH}")" > SHA256SUMS.txt)
cat "${DMG_DIR}/SHA256SUMS.txt"

echo "==> Packaged successfully:"
echo "    APP: ${APP_PATH}"
echo "    DMG: ${DMG_PATH}"
echo "    SHA: ${DMG_DIR}/SHA256SUMS.txt"
