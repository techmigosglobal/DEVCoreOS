#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_ref="${1:-${DEVCORE_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos:alpha}}"
output_dir="${DEVCORE_OUTPUT_DIR:-$repo_root/artifacts/qcow2}"
image_builder_bin="${IMAGE_BUILDER_BIN:-image-builder}"
build_root_ref="$(sed -n 's/^build_root_image = "\(.*\)"$/\1/p' "$repo_root/platform/image/base-images.lock")"

if [[ "${DEVCORE_IMAGE_BUILDER_CONTAINER:-0}" == "1" ]]; then
    exec "$repo_root/platform/image/build-qcow2-container.sh" "$image_ref"
fi

if [[ "${EUID}" -ne 0 ]]; then
    printf 'error: image-builder QCOW2 creation requires a privileged build environment; rerun in an isolated builder VM or container\n' >&2
    exit 1
fi
if ! command -v "$image_builder_bin" >/dev/null 2>&1; then
    printf 'error: unified image-builder CLI not found: %s\n' "$image_builder_bin" >&2
    exit 1
fi
if [[ "$build_root_ref" != *@sha256:* ]]; then
    printf 'error: image-builder build root must be digest pinned\n' >&2
    exit 1
fi

mkdir -p "$output_dir"
"$image_builder_bin" bootc inspect --ref "$image_ref"
"$image_builder_bin" build \
    --bootc-ref "$image_ref" \
    --bootc-build-ref "$build_root_ref" \
    --bootc-default-fs ext4 \
    --output-dir "$output_dir" \
    qcow2
