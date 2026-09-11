#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
container_engine="${CONTAINER_ENGINE:-podman}"
base_image="${DEVCORE_BASE_IMAGE:?set DEVCORE_BASE_IMAGE to the immutable DevCore BaseOS digest}"
installer_image="${DEVCORE_INSTALLER_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos-installer:alpha}"
installer_base_image="${DEVCORE_INSTALLER_BASE_IMAGE_REF:-$base_image}"
installer_bin="${DEVCORE_INSTALLER_BIN:-$repo_root/target/release/devcore-installer}"
installer_daemon="${DEVCORE_INSTALLER_DAEMON:-$repo_root/target/release/devcore-installerd}"

case "$container_engine" in
    podman) ;;
    *)
        printf 'error: native installer builds require podman for OCI archive output\n' >&2
        exit 1
        ;;
esac
command -v "$container_engine" >/dev/null 2>&1 || {
    printf 'error: required command not found: %s\n' "$container_engine" >&2
    exit 1
}
for tool in jq tar sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || {
        printf 'error: required payload verification command not found: %s\n' "$tool" >&2
        exit 1
    }
done
if [[ ! "$base_image" =~ ^ghcr\.io/techmigosglobal/devcoreos@sha256:[0-9a-f]{64}$ ]]; then
    printf 'error: DEVCORE_BASE_IMAGE must be the immutable DevCore GHCR BaseOS digest\n' >&2
    exit 1
fi
if [[ "$base_image" == registry:*localhost* ]]; then
    printf 'error: local registry payload references are not supported\n' >&2
    exit 1
fi
if [[ -n "${DEVCORE_INSTALLER_BASE_IMAGE_REF:-}" && "$installer_base_image" != localhost/* && "$installer_base_image" != *@sha256:* ]]; then
    printf 'error: DEVCORE_INSTALLER_BASE_IMAGE_REF must be a local image or digest-pinned reference\n' >&2
    exit 1
fi

# Rebuild the default binaries so installer fixes cannot be omitted from media.
if [[ -z "${DEVCORE_INSTALLER_BIN:-}" || -z "${DEVCORE_INSTALLER_DAEMON:-}" ]]; then
    (cd "$repo_root" && "${CARGO_BIN:-cargo}" build --locked --release \
        --package devcore-installer --package devcore-installerd)
fi

for file in "$installer_bin" "$installer_daemon" \
    "$repo_root/platform/image/Installer.Containerfile" \
    "$repo_root/platform/image/live-iso.yaml" \
    "$repo_root/contracts/dbus/org.devcore.Installer1.xml"; do
    [[ -f "$file" ]] || {
        printf 'error: required installer input is missing: %s\n' "$file" >&2
        exit 1
    }
done

"$container_engine" image inspect "$base_image" >/dev/null || {
    printf 'error: BaseOS digest is not available locally: %s\n' "$base_image" >&2
    exit 1
}
"$container_engine" image inspect "$installer_base_image" >/dev/null || {
    printf 'error: installer base image is not available locally: %s\n' "$installer_base_image" >&2
    exit 1
}

build_context="$(mktemp -d "${TMPDIR:-/tmp}/devcore-native-installer.XXXXXX")"
cleanup() {
    rm -rf "$build_context"
}
trap cleanup EXIT

payload_dir="$build_context/usr/share/devcore-installer/payload"
mkdir -p "$payload_dir"
# OCI export can change compressed layer descriptors and the manifest digest.
# The OCI configuration digest binds the original config and uncompressed
# rootfs diff IDs, so it remains the local identity across archive conversion.
config_id="$("$container_engine" image inspect --format '{{.Id}}' "$base_image")"
config_digest="sha256:${config_id#sha256:}"
[[ "$config_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    printf 'error: BaseOS image configuration digest is invalid\n' >&2
    exit 1
}
"$container_engine" save --format oci-archive --output "$payload_dir/devcore-baseos.oci.tar" "$base_image"
archive_manifest="$(tar -xOf "$payload_dir/devcore-baseos.oci.tar" index.json \
    | jq -er 'if (.manifests | length) == 1 then .manifests[0].digest else error("expected one payload image") end')"
[[ "$archive_manifest" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    printf 'error: payload archive manifest digest is invalid\n' >&2
    exit 1
}
archive_config="$(tar -xOf "$payload_dir/devcore-baseos.oci.tar" "blobs/sha256/${archive_manifest#sha256:}" \
    | jq -er '.config.digest')"
[[ "$archive_config" == "$config_digest" ]] || {
    printf 'error: OCI export changed the BaseOS configuration; rebuild BaseOS with --format=oci\n' >&2
    exit 1
}
archive_config_hash="$(tar -xOf "$payload_dir/devcore-baseos.oci.tar" "blobs/sha256/${config_digest#sha256:}" \
    | sha256sum | cut -d ' ' -f 1)"
[[ "sha256:$archive_config_hash" == "$config_digest" ]] || {
    printf 'error: payload archive configuration content does not match its digest\n' >&2
    exit 1
}
(
    cd "$payload_dir"
    printf '%s\n' "$base_image" > payload-image.ref
    printf '%s\n' "$config_digest" > payload-config.digest
    sha256sum devcore-baseos.oci.tar payload-image.ref payload-config.digest > SHA256SUMS
)

install -D -m 0755 "$installer_bin" "$build_context/usr/bin/devcore-installer"
install -D -m 0755 "$installer_daemon" "$build_context/usr/libexec/devcore/devcore-installerd"
install -D -m 0644 "$repo_root/platform/system/systemd/devcore-installer.service" \
    "$build_context/usr/lib/systemd/system/devcore-installer.service"
install -D -m 0644 "$repo_root/platform/system/sysusers.d/devcore-live.conf" \
    "$build_context/usr/lib/sysusers.d/devcore-live.conf"
install -D -m 0644 "$repo_root/platform/system/tmpfiles.d/devcore-live.conf" \
    "$build_context/usr/lib/tmpfiles.d/devcore-live.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system.d/org.devcore.Installer1.conf" \
    "$build_context/usr/share/dbus-1/system.d/org.devcore.Installer1.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system-services/org.devcore.Installer1.service" \
    "$build_context/usr/share/dbus-1/system-services/org.devcore.Installer1.service"
install -D -m 0644 "$repo_root/platform/system/polkit-1/actions/org.devcore.installer.policy" \
    "$build_context/usr/share/polkit-1/actions/org.devcore.installer.policy"
install -D -m 0644 "$repo_root/platform/system/polkit-1/rules.d/org.devcore.installer.rules" \
    "$build_context/etc/polkit-1/rules.d/org.devcore.installer.rules"
install -D -m 0644 "$repo_root/platform/system/installer/config.json" \
    "$build_context/usr/share/devcore-installer/config.json"
install -D -m 0644 "$repo_root/platform/image/live-iso.yaml" \
    "$build_context/usr/lib/image-builder/bootc/iso.yaml"
install -D -m 0644 "$repo_root/platform/system/greetd/live-config.toml" \
    "$build_context/etc/greetd/config.toml"
install -D -m 0644 "$repo_root/platform/system/greetd/live-pam.conf" \
    "$build_context/etc/pam.d/greetd"
install -D -m 0644 "$repo_root/platform/system/greetd/live-dropin.conf" \
    "$build_context/etc/systemd/system/greetd.service.d/live.conf"
install -D -m 0644 "$repo_root/platform/image/Installer.Containerfile" "$build_context/Containerfile"

"$container_engine" build --pull=never --format=oci \
    --build-arg "DEVCORE_BASE_IMAGE=$base_image" \
    --build-arg "DEVCORE_INSTALLER_BASE_IMAGE=$installer_base_image" \
    --build-arg "DEVCORE_SKIP_INSTALLER_PACKAGES=${DEVCORE_SKIP_INSTALLER_PACKAGES:-false}" \
    --tag "$installer_image" \
    --file "$build_context/Containerfile" \
    "$build_context"

printf 'Native installer OCI image built: %s\n' "$installer_image"
