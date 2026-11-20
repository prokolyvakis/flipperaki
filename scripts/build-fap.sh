#!/usr/bin/env bash
# Build the Buddy FAP with uFBT against the pinned Momentum SDK into flipper/dist
# and print its sha256. Never flashes, connects to a device or installs system
# software; all uFBT state stays in ./.ufbt and ./.venv.
#
# SDK pin is PROVISIONAL until the user's device firmware is recorded
# (docs/compatibility.md): Momentum release mntm-012, API 87.1.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD

UFBT_VERSION=0.2.6
SDK_URL=https://up.momentum-fw.dev/builds/firmware/mntm-012/flipper-z-f7-sdk-mntm-012.zip
SDK_SHA256=c90530c3e0aa7bc4356b8521df20d23d7b4a5435ab58a2530ffb696200408cc2
export UFBT_HOME="$root/.ufbt"

[[ -x .venv/bin/python ]] || uv venv .venv
uv pip install --quiet --python .venv "ufbt==${UFBT_VERSION}"

sdk_zip="$UFBT_HOME/download/$(basename "$SDK_URL")"
mkdir -p "$(dirname "$sdk_zip")"
if ! echo "$SDK_SHA256  $sdk_zip" | shasum -a 256 -c --status 2>/dev/null; then
    curl -fsSL -o "$sdk_zip.part" "$SDK_URL"
    if ! echo "$SDK_SHA256  $sdk_zip.part" | shasum -a 256 -c --status; then
        rm -f "$sdk_zip.part"
        echo "error: SDK archive sha256 mismatch for $SDK_URL" >&2
        exit 1
    fi
    mv "$sdk_zip.part" "$sdk_zip"
fi
.venv/bin/ufbt update --hw-target f7 --local "$sdk_zip"

# Stage the app with the generated bindings so uFBT never globs flipper/tests.
stage=flipper/build/fap/flipperaki
rm -rf flipper/build/fap
mkdir -p "$stage" flipper/dist
cp flipper/*.c flipper/*.h flipper/application.fam protocol/generated/buddy.pb.[ch] "$stage/"
(cd "$stage" && "$root/.venv/bin/ufbt" fap_flipperaki)
cp "$UFBT_HOME/build/flipperaki.fap" flipper/dist/flipperaki.fap
shasum -a 256 flipper/dist/flipperaki.fap
