#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
    printf 'usage: %s <disk.qcow2> [seconds]\n' "${0##*/}" >&2
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
    usage
    exit 2
fi

image="$1"
timeout_seconds="${2:-180}"
qemu_bin="${QEMU_BIN:-qemu-system-x86_64}"
ovmf_code="${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}"
ovmf_vars="${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}"

if [[ ! -f "$image" ]]; then
    printf 'error: QCOW2 image must be a regular file: %s\n' "$image" >&2
    exit 1
fi
if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
    printf 'error: KVM access is required for this non-destructive VM test\n' >&2
    exit 1
fi
if [[ ! -r "$ovmf_code" ]]; then
    printf 'error: UEFI firmware is unavailable: %s\n' "$ovmf_code" >&2
    exit 1
fi
if [[ ! -r "$ovmf_vars" ]]; then
    printf 'error: UEFI variable template is unavailable: %s\n' "$ovmf_vars" >&2
    exit 1
fi
if ! command -v "$qemu_bin" >/dev/null 2>&1; then
    printf 'error: qemu-system-x86_64 is required\n' >&2
    exit 1
fi

log_file="${DEVCORE_QEMU_LOG:-$(mktemp "${TMPDIR:-/tmp}/devcore-qemu.XXXXXX.log")}"
vars_file="$(mktemp "${TMPDIR:-/tmp}/devcore-ovmf-vars.XXXXXX.fd")"
cp --preserve=mode,timestamps "$ovmf_vars" "$vars_file"
trap 'rm -f "$vars_file"; printf "QEMU serial log: %s\\n" "$log_file"' EXIT

# -snapshot discards all guest writes. This script never accepts block devices.
timeout --foreground "$timeout_seconds" "$qemu_bin" \
    -machine q35,accel=kvm \
    -cpu host \
    -smp "${DEVCORE_VM_CPUS:-2}" \
    -m "${DEVCORE_VM_MEMORY_MIB:-4096}" \
    -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code" \
    -drive "if=pflash,format=raw,file=$vars_file" \
    -drive "file=$image,format=qcow2,if=virtio" \
    -snapshot \
    -display none \
    -serial "file:$log_file" \
    -no-reboot
