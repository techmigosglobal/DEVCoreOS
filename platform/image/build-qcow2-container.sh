#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_ref="${1:-${DEVCORE_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos:alpha}}"
output_dir="${DEVCORE_OUTPUT_DIR:-$repo_root/artifacts/qcow2}"
builder_image="${DEVCORE_IMAGE_BUILDER_IMAGE:-ghcr.io/osbuild/image-builder-cli:latest}"
build_root_ref="$(sed -n 's/^build_root_image = \"\(.*\)\"$/\1/p' "$repo_root/platform/image/base-images.lock")"

if ! command -v docker >/dev/null 2>&1; then
    printf 'error: Docker is required for the containerized Image Builder route\n' >&2
    exit 1
fi
if [[ "$image_ref" != *@sha256:* && "$image_ref" != *:* ]]; then
    printf 'error: image reference must include a tag or digest: %s\n' "$image_ref" >&2
    exit 1
fi
if [[ "$build_root_ref" != *@sha256:* ]]; then
    printf 'error: image-builder build root must be digest pinned\n' >&2
    exit 1
fi

mkdir -p "$output_dir"
if ! docker image inspect "$builder_image" >/dev/null 2>&1; then
    docker pull "$builder_image"
fi

build_context="$(mktemp -d "$repo_root/.devcore-image-builder.XXXXXX")"
cleanup() {
    find "$build_context" -depth -delete
}
trap cleanup EXIT
mkdir -p "$build_context/output"

# Docker Desktop and nested Podman do not share an image store. Stream the
# consumed export directly into the isolated builder so the host filesystem
# never receives a second full copy of the OCI image.
docker save "$image_ref" |
docker run --rm --privileged -i \
    --env DEVCORE_BUILDER_IMAGE_REF="$image_ref" \
    --env DEVCORE_BUILD_ROOT_REF="$build_root_ref" \
    --volume "$build_context:/workspace" \
    --workdir /workspace \
    --entrypoint /bin/sh \
    "$builder_image" \
    -c 'set -Eeuo pipefail
        podman load
        image-builder bootc inspect --ref "$DEVCORE_BUILDER_IMAGE_REF"
        image-builder build \
            --bootc-ref "$DEVCORE_BUILDER_IMAGE_REF" \
            --bootc-build-ref "$DEVCORE_BUILD_ROOT_REF" \
            --bootc-default-fs ext4 \
            --output-dir /workspace/output \
            qcow2'

# Image Builder names the artifact from the selected bootc image (for example
# bootc-fedora-43-qcow2-x86_64.qcow2) rather than using a fixed disk.qcow2
# filename.  Select the single QCOW2 artifact and normalize it below so the
# rest of the pipeline has a stable path.
qcow2="$(find "$build_context/output" -type f -name '*.qcow2' -print -quit)"
if [[ -z "$qcow2" ]]; then
    printf 'error: Image Builder completed without producing a QCOW2 artifact\n' >&2
    exit 1
fi
install -D -m 0644 "$qcow2" "$output_dir/disk.qcow2"
printf 'QCOW2 image built: %s\n' "$output_dir/disk.qcow2"
