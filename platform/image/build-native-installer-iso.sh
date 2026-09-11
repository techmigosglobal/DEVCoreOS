#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_builder_bin="${IMAGE_BUILDER_BIN:-image-builder}"
installer_image="${DEVCORE_INSTALLER_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos-installer:alpha}"
output_dir="${DEVCORE_INSTALLER_OUTPUT_DIR:-$repo_root/artifacts/native-installer-iso}"
builder_image="${DEVCORE_IMAGE_BUILDER_IMAGE:-docker.io/library/image-builder-cli:latest}"

[[ "$(uname -m)" == "x86_64" ]] || {
    printf 'error: native live media is supported only on an x86-64 builder\n' >&2
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
if [[ "${EUID}" -ne 0 ]] || ! command -v "$image_builder_bin" >/dev/null 2>&1; then
    # Docker Desktop provides the privileged Linux builder. Stream only the
    # installer image into private container storage; do not mount host disks.
    command -v podman >/dev/null
    command -v docker >/dev/null
    docker info >/dev/null
    docker image inspect "$builder_image" >/dev/null || {
        printf 'error: import the Image Builder archive with docker load --input <archive>\n' >&2
        exit 1
    }
    podman image inspect "$installer_image" >/dev/null
    output_dir="$(realpath "$output_dir")"
    podman save --format docker-archive "$installer_image" |
        docker run --rm --privileged -i \
            --env DEVCORE_INSTALLER_IMAGE_REF="$installer_image" \
            --volume "$output_dir:/output" \
            --entrypoint /bin/bash "$builder_image" -c '
                set -Eeuo pipefail
                podman load
                image-builder bootc inspect --ref "$DEVCORE_INSTALLER_IMAGE_REF"
                image-builder build --bootc-ref "$DEVCORE_INSTALLER_IMAGE_REF" \
                    --bootc-default-fs ext4 --output-dir /output \
                    --with-manifest --with-buildlog bootc-generic-iso
            '
    printf 'Native live installer ISO built in: %s\n' "$output_dir"
    exit 0
fi
"$image_builder_bin" bootc inspect --ref "$installer_image"
"$image_builder_bin" build \
    --bootc-ref "$installer_image" \
    --bootc-default-fs ext4 \
    --output-dir "$output_dir" \
    bootc-generic-iso

printf 'Native live installer ISO built in: %s\n' "$output_dir"
