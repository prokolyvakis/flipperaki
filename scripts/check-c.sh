#!/usr/bin/env bash
# Portable C checks for the Buddy core: clang, -Wall -Wextra -Werror, ASan+UBSan,
# CTest, then clang-tidy. Builds into flipper/tests/build. Never flashes,
# connects to a device or installs software.
set -euo pipefail
cd "$(dirname "$0")/.."

LLVM_BIN=${LLVM_BIN:-/opt/homebrew/opt/llvm/bin}
build=flipper/tests/build
sanitize="-fsanitize=address,undefined -fno-sanitize-recover=all -fno-omit-frame-pointer"

cmake -S flipper/tests -B "$build" \
    -DCMAKE_C_COMPILER=clang \
    -DCMAKE_BUILD_TYPE=Debug \
    -DCMAKE_C_FLAGS="$sanitize" \
    -DCMAKE_EXE_LINKER_FLAGS="$sanitize" \
    -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build "$build" --parallel
ctest --test-dir "$build" --output-on-failure

# DeprecatedOrUnsafeBufferHandling only suggests Annex K (memset_s), which the
# firmware libc does not provide.
"$LLVM_BIN/clang-tidy" --quiet -p "$build" \
    --warnings-as-errors='*' --header-filter='flipper/buddy_.*' \
    --checks='-*,clang-analyzer-*,-clang-analyzer-security.insecureAPI.DeprecatedOrUnsafeBufferHandling,bugprone-*,-bugprone-easily-swappable-parameters,cert-*,misc-*,-misc-include-cleaner' \
    flipper/buddy_core.c flipper/buddy_protocol.c
