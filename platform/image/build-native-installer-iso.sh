#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_builder_bin="${IMAGE_BUILDER_BIN:-image-builder}"
installer_image="${DEVCORE_INSTALLER_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos-installer:alpha}"
output_dir="${DEVCORE_INSTALLER_OUTPUT_DIR:-$repo_root/artifacts/native-installer-iso}"

[[ "${EUID}" -eq 0 ]] || {
    printf 'error: ISO assembly must run inside a privileged isolated builder\n' >&2
    exit 1
}
[[ "$(uname -m)" == "x86_64" ]] || {
    printf 'error: native live media is supported only on an x86-64 builder\n' >&2
    exit 1
}
command -v "$image_builder_bin" >/dev/null 2>&1 || {
    printf 'error: unified image builder not found: %s\n' "$image_builder_bin" >&2
    exit 1
}
[[ -f "$repo_root/platform/image/live-iso.yaml" ]] || {
    printf 'error: missing live boot menu configuration\n' >&2
    exit 1
}
if [[ "$installer_image" == registry:*localhost* ]]; then
    printf 'error: local registry installer references are not supported\n' >&2
    exit 1
fi

mkdir -p "$output_dir"
"$image_builder_bin" bootc inspect --ref "$installer_image"
"$image_builder_bin" build \
    --bootc-ref "$installer_image" \
    --bootc-default-fs ext4 \
    --output-dir "$output_dir" \
    bootc-generic-iso

printf 'Native live installer ISO built in: %s\n' "$output_dir"
