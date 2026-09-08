#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_ref="${1:-${DEVCORE_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos:alpha}}"
output_dir="${DEVCORE_OUTPUT_DIR:-$repo_root/artifacts/iso}"
image_builder_bin="${IMAGE_BUILDER_BIN:-image-builder}"
build_root_ref="$(sed -n 's/^build_root_image = \"\(.*\)\"$/\1/p' "$repo_root/platform/image/base-images.lock")"
qcow2_evidence="${DEVCORE_QCOW2_EVIDENCE:-$repo_root/artifacts/evidence/qcow2-uefi.json}"
provisioning_evidence="${DEVCORE_PROVISIONING_EVIDENCE:-$repo_root/artifacts/evidence/post-install-provisioning.json}"

if [[ "${EUID}" -ne 0 ]]; then
    printf 'error: image-builder ISO creation requires a privileged build environment; rerun in an isolated builder VM or container\n' >&2
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
if [[ "${DEVCORE_QCOW2_VERIFIED:-0}" != "1" ]]; then
    printf 'error: ISO generation requires DEVCORE_QCOW2_VERIFIED=1 after the disposable UEFI/KVM gate\n' >&2
    exit 1
fi
if [[ ! -f "$qcow2_evidence" ]]; then
    printf 'error: QCOW2 evidence file is required before ISO generation: %s\n' "$qcow2_evidence" >&2
    exit 1
fi
grep -Fq '"boot_status":"passed"' "$qcow2_evidence" \
    || grep -Fq '"boot_status": "passed"' "$qcow2_evidence" || {
    printf 'error: QCOW2 evidence does not record a passed boot status\n' >&2
    exit 1
}
grep -Fq "\"image_ref\":\"$image_ref\"" "$qcow2_evidence" \
    || grep -Fq "\"image_ref\": \"$image_ref\"" "$qcow2_evidence" || {
    printf 'error: QCOW2 evidence does not match image reference: %s\n' "$image_ref" >&2
    exit 1
}
if [[ "${DEVCORE_PROVISIONING_VERIFIED:-0}" != "1" ]]; then
    printf 'error: ISO generation requires DEVCORE_PROVISIONING_VERIFIED=1 after the post-install guest gate\n' >&2
    exit 1
fi
if [[ ! -f "$provisioning_evidence" ]]; then
    printf 'error: post-install provisioning evidence is required before ISO generation: %s\n' \
        "$provisioning_evidence" >&2
    exit 1
fi
grep -Fq '"status":"passed"' "$provisioning_evidence" \
    || grep -Fq '"status": "passed"' "$provisioning_evidence" || {
    printf 'error: post-install evidence does not record a passed status\n' >&2
    exit 1
}
grep -Fq '"network":"connected"' "$provisioning_evidence" \
    || grep -Fq '"network": "connected"' "$provisioning_evidence" || {
    printf 'error: post-install evidence does not record connected network readiness\n' >&2
    exit 1
}
grep -Fq "\"image_ref\":\"$image_ref\"" "$provisioning_evidence" \
    || grep -Fq "\"image_ref\": \"$image_ref\"" "$provisioning_evidence" || {
    printf 'error: post-install evidence does not match image reference: %s\n' "$image_ref" >&2
    exit 1
}

mkdir -p "$output_dir"
"$image_builder_bin" bootc inspect --ref "$image_ref"
"$image_builder_bin" build \
    --bootc-ref "$image_ref" \
    --bootc-build-ref "$build_root_ref" \
    --bootc-default-fs ext4 \
    --output-dir "$output_dir" \
    bootc-generic-iso
