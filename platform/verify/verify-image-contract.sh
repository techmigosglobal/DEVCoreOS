#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_root="$repo_root/platform/image"
system_root="$repo_root/platform/system"

for file in \
    "$repo_root/Cargo.toml" \
    "$image_root/Containerfile" \
    "$image_root/base-images.lock" \
    "$image_root/build-oci.sh" \
    "$image_root/build-qcow2.sh" \
    "$image_root/build-iso.sh" \
    "$system_root/greetd/config.toml" \
    "$system_root/systemd/devcored.service" \
    "$repo_root/features/desktop/compositor/src/main.rs" \
    "$repo_root/features/desktop/session/src/lib.rs" \
    "$repo_root/features/shell/app/src/main.rs" \
    "$repo_root/features/onboarding/greeter/src/main.rs" \
    "$repo_root/features/studio/workd/src/main.rs" \
    "$repo_root/features/provision/daemon/src/main.rs"; do
    [[ -f "$file" ]] || { printf 'error: missing feature-owned image input: %s\n' "$file" >&2; exit 1; }
done

for script in "$image_root"/*.sh "$repo_root/platform/vm"/*.sh "$repo_root/platform/verify"/*.sh; do
    bash -n "$script"
done

grep -Fq '"features/desktop/compositor"' "$repo_root/Cargo.toml"
grep -Fq '"features/shell/app"' "$repo_root/Cargo.toml"
grep -Fq '"features/studio/workd"' "$repo_root/Cargo.toml"
grep -Fq '"shared/execution"' "$repo_root/Cargo.toml"
base_ref="$(sed -n 's/^base_image = "\(.*\)"$/\1/p' "$image_root/base-images.lock")"
grep -Fqx "ARG DEVCORE_BASE_IMAGE=$base_ref" "$image_root/Containerfile" || {
    printf 'error: BaseOS Containerfile differs from the locked Fedora input\n' >&2
    exit 1
}
grep -Fq 'FROM ${DEVCORE_BASE_IMAGE}' "$image_root/Containerfile"
grep -Fq 'COPY usr/bin/devcore-shell /usr/bin/devcore-shell' "$image_root/Containerfile"
grep -Fq 'COPY usr/bin/devcore-compositor /usr/bin/devcore-compositor' "$image_root/Containerfile"
grep -Fq 'systemctl enable devcored.service' "$image_root/Containerfile"
grep -Fq 'platform/system/greetd/config.toml' "$image_root/build-oci.sh"
grep -Fq 'platform/image/Containerfile' "$image_root/build-oci.sh"
grep -Fq 'platform/image/base-images.lock' "$image_root/build-qcow2.sh"
grep -Fq 'platform/image/base-images.lock' "$image_root/build-iso.sh"
grep -Fq 'devcore-compositor --backend drm --shell /usr/bin/devcore-greeter' "$system_root/greetd/config.toml"
grep -Fq 'devcore.live=1' "$repo_root/platform/image/live-iso.yaml"
grep -Fq 'devcore.installer=auto' "$repo_root/features/shell/app/src/main.rs"

if grep -R -Eqi '(anaconda|kickstart|registry:localhost)' \
    "$image_root/Containerfile" "$image_root/build-oci.sh" "$system_root"; then
    printf 'error: active BaseOS image paths must not include legacy installer components\n' >&2
    exit 1
fi

printf 'BaseOS image contract checks passed\n'
