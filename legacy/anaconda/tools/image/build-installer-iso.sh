#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
payload_ref="${1:-${DEVCORE_IMAGE_REF:-localhost/devcore-os:alpha}}"
installer_ref="${DEVCORE_INSTALLER_IMAGE_REF:-localhost/devcore-os-installer:alpha}"
output_dir="${DEVCORE_INSTALLER_OUTPUT_DIR:-$repo_root/artifacts/installer-iso}"
image_builder_bin="${IMAGE_BUILDER_BIN:-image-builder}"
qcow2_evidence="${DEVCORE_QCOW2_EVIDENCE:-$repo_root/artifacts/evidence/qcow2-uefi.json}"
provisioning_evidence="${DEVCORE_PROVISIONING_EVIDENCE:-$repo_root/artifacts/evidence/post-install-provisioning.json}"
release_gate="${DEVCORE_RELEASE_GATE:-0}"

if [[ "${EUID}" -ne 0 ]]; then
    printf 'error: installer ISO creation requires a privileged isolated builder\n' >&2
    exit 1
fi
command -v "$image_builder_bin" >/dev/null 2>&1 || {
    printf 'error: unified image-builder CLI not found: %s\n' "$image_builder_bin" >&2
    exit 1
}
[[ "${DEVCORE_QCOW2_VERIFIED:-0}" == "1" && -f "$qcow2_evidence" ]] || {
    printf 'error: installer ISO requires disposable QCOW2/UEFI boot evidence\n' >&2
    exit 1
}
grep -Eq '"boot_status"[[:space:]]*:[[:space:]]*"passed"' "$qcow2_evidence" || {
    printf 'error: QCOW2 evidence does not record a passed boot\n' >&2
    exit 1
}
grep -Fq "$payload_ref" "$qcow2_evidence" || {
    printf 'error: QCOW2 evidence does not match payload image: %s\n' "$payload_ref" >&2
    exit 1
}
if [[ "$release_gate" == "1" ]]; then
    [[ "${DEVCORE_PROVISIONING_VERIFIED:-0}" == "1" && -f "$provisioning_evidence" ]] || {
        printf 'error: release-gated installer ISO requires target-PC provisioning evidence\n' >&2
        exit 1
    }
    grep -Eq '"status"[[:space:]]*:[[:space:]]*"passed"' "$provisioning_evidence" || {
        printf 'error: provisioning evidence does not record a passed status\n' >&2
        exit 1
    }
    grep -Fq "$payload_ref" "$provisioning_evidence" || {
        printf 'error: provisioning evidence does not match payload image: %s\n' "$payload_ref" >&2
        exit 1
    }
fi

mkdir -p "$output_dir"
"$image_builder_bin" bootc inspect --ref "$installer_ref"
"$image_builder_bin" build \
    --bootc-ref "$installer_ref" \
    --bootc-installer-payload-ref "$payload_ref" \
    --bootc-default-fs ext4 \
    --output-dir "$output_dir" \
    bootc-generic-iso

if [[ "$release_gate" == "1" ]]; then
    printf 'DevCore installer ISO built and release-gated in: %s\n' "$output_dir"
else
    printf 'DevCore installer ISO built (boot/QCOW2 verified; target-PC install validation pending): %s\n' "$output_dir"
fi
