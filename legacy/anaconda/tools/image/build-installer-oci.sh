#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
payload_ref="${1:-${DEVCORE_IMAGE_REF:-localhost/devcore-os:alpha}}"
installer_ref="${DEVCORE_INSTALLER_IMAGE_REF:-localhost/devcore-os-installer:alpha}"
container_engine="${CONTAINER_ENGINE:-podman}"

case "$container_engine" in
    podman | docker) ;;
    *)
        printf 'error: CONTAINER_ENGINE must be podman or docker\n' >&2
        exit 1
        ;;
esac
command -v "$container_engine" >/dev/null 2>&1 || {
    printf 'error: required container engine not found: %s\n' "$container_engine" >&2
    exit 1
}

# Require a local payload image. The installer must embed the exact BaseOS
# image being released rather than silently pulling a mutable substitute.
"$container_engine" image inspect "$payload_ref" >/dev/null

build_context="$(mktemp -d "${TMPDIR:-/tmp}/devcore-installer.XXXXXX")"
cleanup() {
    rm -rf "$build_context"
}
trap cleanup EXIT

install -D -m 0644 "$repo_root/packaging/installer/interactive-defaults.ks" \
    "$build_context/etc/anaconda/interactive-defaults.ks"
install -D -m 0644 "$repo_root/packaging/installer/iso.yaml" \
    "$build_context/usr/lib/image-builder/bootc/iso.yaml"
install -m 0644 "$repo_root/images/Installer.Containerfile" "$build_context/Containerfile"

"$container_engine" build --pull=never \
    --build-arg "DEVCORE_BASE_IMAGE=$payload_ref" \
    --build-arg "DEVCORE_PAYLOAD_REF=$payload_ref" \
    --tag "$installer_ref" \
    --file "$build_context/Containerfile" \
    "$build_context"

printf 'DevCore graphical installer environment built: %s\n' "$installer_ref"
