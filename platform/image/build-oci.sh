#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cargo_bin="${CARGO_BIN:-cargo}"
image_ref="${DEVCORE_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos:alpha}"
base_lock="$repo_root/platform/image/base-images.lock"
container_engine="${CONTAINER_ENGINE:-podman}"
# Set DEVCORE_COMPOSITOR_FEATURES empty when the native compositor was already
# built in a Linux toolchain and the image build should avoid another package
# download. The default still builds the reviewed native-drm feature.
compositor_features="${DEVCORE_COMPOSITOR_FEATURES-native-drm}"

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        printf 'error: required command not found: %s\n' "$1" >&2
        exit 1
    fi
}

require_command "$cargo_bin"
case "$container_engine" in
    podman | docker) ;;
    *)
        printf 'error: CONTAINER_ENGINE must be podman or docker\n' >&2
        exit 1
        ;;
esac
require_command "$container_engine"

base_image="$(sed -n 's/^base_image = "\(.*\)"$/\1/p' "$base_lock")"
if [[ "$base_image" != *@sha256:* ]]; then
    printf 'error: base_image must be a digest-pinned OCI reference\n' >&2
    exit 1
fi
build_base_image="${DEVCORE_BASE_IMAGE_REF:-$base_image}"
if [[ -n "${DEVCORE_BASE_IMAGE_REF:-}" && "$build_base_image" != localhost/* && "$build_base_image" != *@sha256:* ]]; then
    printf 'error: DEVCORE_BASE_IMAGE_REF must be a local image or digest-pinned reference\n' >&2
    exit 1
fi

cd "$repo_root"
if [[ -n "$compositor_features" ]] && command -v pkg-config >/dev/null 2>&1 \
    && pkg-config --exists libudev libseat libinput gbm; then
    "$cargo_bin" build --locked --release --package devcore-compositor \
        --features "$compositor_features"
elif [[ -n "$compositor_features" ]]; then
    # Build native-seat support in an isolated Linux toolchain when the host
    # intentionally lacks DRM development headers. Use the selected engine so
    # a rootless Podman build does not depend on a Docker daemon.
    require_command "$container_engine"
    host_uid="$(id -u)"
    host_gid="$(id -g)"
    "$container_engine" run --rm \
        --security-opt label=disable \
        --env DEVCORE_COMPOSITOR_FEATURES="$compositor_features" \
        --env DEVCORE_BUILD_UID="$host_uid" \
        --env DEVCORE_BUILD_GID="$host_gid" \
        --env DEVCORE_CONTAINER_ENGINE="$container_engine" \
        --volume "$repo_root:/workspace" \
        --workdir /workspace \
        rust:1.90-bookworm \
        bash -c '
            set -Eeuo pipefail
            export PATH=/usr/local/cargo/bin:$PATH
            apt-get update >/dev/null
            DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
                pkg-config libudev-dev libseat-dev libinput-dev libgbm-dev libdrm-dev \
                libegl1-mesa-dev libxkbcommon-dev >/dev/null
            cargo build --locked --release --package devcore-compositor \
                --features "$DEVCORE_COMPOSITOR_FEATURES"
            if [ "$DEVCORE_CONTAINER_ENGINE" = docker ]; then
                chown -R "$DEVCORE_BUILD_UID:$DEVCORE_BUILD_GID" /workspace/target
            fi
        '
fi
"$cargo_bin" build --locked --release --package devcored --package devcore-firstbootd \
    --package devcore-greeter --package devcore-hardwared --package devcore-shell \
    --package devcore-updated --package devcore-workd --package devcore-provisiond
if ! "$container_engine" image inspect "$build_base_image" >/dev/null 2>&1; then
    if [[ -n "${DEVCORE_BASE_IMAGE_REF:-}" ]]; then
        printf 'error: selected BaseOS image is not available locally: %s\n' "$build_base_image" >&2
        exit 1
    fi
    "$container_engine" pull "$build_base_image"
fi

build_context="$(mktemp -d "${TMPDIR:-/tmp}/devcore-oci.XXXXXX")"
trap 'rm -rf "$build_context"' EXIT

install -D -m 0755 "$repo_root/target/release/devcored" "$build_context/usr/libexec/devcore/devcored"
install -D -m 0755 "$repo_root/target/release/devcore-firstbootd" \
    "$build_context/usr/libexec/devcore/devcore-firstbootd"
install -D -m 0755 "$repo_root/target/release/devcore-hardwared" \
    "$build_context/usr/libexec/devcore/devcore-hardwared"
install -D -m 0755 "$repo_root/target/release/devcore-updated" \
    "$build_context/usr/libexec/devcore/devcore-updated"
install -D -m 0755 "$repo_root/target/release/devcore-workd" "$build_context/usr/libexec/devcore/devcore-workd"
install -D -m 0755 "$repo_root/target/release/devcore-provisiond" \
    "$build_context/usr/libexec/devcore/devcore-provisiond"
install -D -m 0755 "$repo_root/target/release/devcore-shell" "$build_context/usr/bin/devcore-shell"
install -D -m 0755 "$repo_root/target/release/devcore-greeter" "$build_context/usr/bin/devcore-greeter"
install -D -m 0755 "$repo_root/target/release/devcore-compositor" \
    "$build_context/usr/bin/devcore-compositor"
install -D -m 0644 "$repo_root/platform/system/systemd/devcored.service" \
    "$build_context/usr/lib/systemd/system/devcored.service"
install -D -m 0644 "$repo_root/platform/system/systemd/devcore-firstbootd.service" \
    "$build_context/usr/lib/systemd/system/devcore-firstbootd.service"
install -D -m 0644 "$repo_root/platform/system/systemd/devcore-hardwared.service" \
    "$build_context/usr/lib/systemd/system/devcore-hardwared.service"
install -D -m 0644 "$repo_root/platform/system/systemd/devcore-updated.service" \
    "$build_context/usr/lib/systemd/system/devcore-updated.service"
install -D -m 0644 "$repo_root/platform/system/sysusers.d/devcore.conf" \
    "$build_context/usr/lib/sysusers.d/devcore.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system.d/org.devcore.Resource1.conf" \
    "$build_context/usr/share/dbus-1/system.d/org.devcore.Resource1.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system.d/org.devcore.FirstBoot1.conf" \
    "$build_context/usr/share/dbus-1/system.d/org.devcore.FirstBoot1.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system.d/org.devcore.Hardware1.conf" \
    "$build_context/usr/share/dbus-1/system.d/org.devcore.Hardware1.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system.d/org.devcore.Update1.conf" \
    "$build_context/usr/share/dbus-1/system.d/org.devcore.Update1.conf"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system-services/org.devcore.Resource1.service" \
    "$build_context/usr/share/dbus-1/system-services/org.devcore.Resource1.service"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system-services/org.devcore.FirstBoot1.service" \
    "$build_context/usr/share/dbus-1/system-services/org.devcore.FirstBoot1.service"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system-services/org.devcore.Hardware1.service" \
    "$build_context/usr/share/dbus-1/system-services/org.devcore.Hardware1.service"
install -D -m 0644 "$repo_root/platform/system/dbus-1/system-services/org.devcore.Update1.service" \
    "$build_context/usr/share/dbus-1/system-services/org.devcore.Update1.service"
install -D -m 0644 "$repo_root/platform/system/dbus-1/services/org.devcore.Work1.service" \
    "$build_context/usr/share/dbus-1/services/org.devcore.Work1.service"
install -D -m 0644 "$repo_root/platform/system/dbus-1/services/org.devcore.Provision1.service" \
    "$build_context/usr/share/dbus-1/services/org.devcore.Provision1.service"
install -D -m 0644 "$repo_root/platform/system/greetd/config.toml" \
    "$build_context/etc/greetd/config.toml"

if [[ "$container_engine" == "podman" ]]; then
    podman build --pull=never --format=oci --build-arg "DEVCORE_BASE_IMAGE=$build_base_image" \
        --build-arg "DEVCORE_SKIP_RUNTIME_PACKAGES=${DEVCORE_SKIP_RUNTIME_PACKAGES:-false}" \
        --tag "$image_ref" --file "$repo_root/platform/image/Containerfile" "$build_context"
else
    # Docker is a local build fallback only. image-builder consumes Podman
    # storage later, so release builders continue to use Podman.
    docker build --pull=false --build-arg "DEVCORE_BASE_IMAGE=$build_base_image" \
        --build-arg "DEVCORE_SKIP_RUNTIME_PACKAGES=${DEVCORE_SKIP_RUNTIME_PACKAGES:-false}" \
        --tag "$image_ref" --file "$repo_root/platform/image/Containerfile" "$build_context"
fi
printf 'OCI image built: %s\n' "$image_ref"
