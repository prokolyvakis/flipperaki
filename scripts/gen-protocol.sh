#!/usr/bin/env bash
# Regenerate the C (nanopb) Buddy bindings from protocol/buddy.proto.
# Static fields only (bounds come from protocol/buddy.options); no callbacks.
# Uses the project-local ./.venv; never installs system software.
set -euo pipefail
cd "$(dirname "$0")/.."

NANOPB_VERSION=0.4.8
[[ -x .venv/bin/python ]] || uv venv .venv
# nanopb 0.4.8's generator imports pkg_resources, removed in setuptools 81.
uv pip install --quiet --python .venv "nanopb==${NANOPB_VERSION}" "setuptools==80.10.2"
export PYTHONWARNINGS=ignore::UserWarning

out=protocol/generated
mkdir -p "$out"
.venv/bin/nanopb_generator \
    --strip-path \
    --error-on-unmatched \
    -I protocol \
    -f protocol/buddy.options \
    -D "$out" \
    protocol/buddy.proto

if grep -q "pb_callback_t" "$out/buddy.pb.h"; then
    echo "error: generated bindings contain callback fields" >&2
    exit 1
fi

size=$(sed -n 's/^#define flipperaki_buddy_Envelope_size[[:space:]]*\([0-9]*\).*/\1/p' "$out/buddy.pb.h")
if [[ -z "$size" || "$size" -gt 512 ]]; then
    echo "error: flipperaki_buddy_Envelope max encoded size '${size}' exceeds 512" >&2
    exit 1
fi
echo "flipperaki_buddy_Envelope max encoded size: ${size} bytes (limit 512)"
