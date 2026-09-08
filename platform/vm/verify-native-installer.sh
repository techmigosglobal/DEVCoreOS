#!/usr/bin/env bash
set -Eeuo pipefail

# Safe live-media smoke harness. It creates a disposable target image and never
# accepts a block device. Full destructive install evidence is recorded by the
# operator after interacting with the native Installer window in the VM.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
iso="${1:-}"
qemu_img_bin="${QEMU_IMG_BIN:-qemu-img}"
qemu_bin="${QEMU_BIN:-qemu-system-x86_64}"
timeout_seconds="${DEVCORE_VM_TIMEOUT_SECONDS:-90}"
output_dir="${DEVCORE_INSTALLER_VM_OUTPUT_DIR:-$repo_root/artifacts/evidence/native-installer-vm}"

[[ -n "$iso" && -f "$iso" && ! -b "$iso" ]] || {
    printf 'usage: %s <native-installer.iso>\n' "${0##*/}" >&2
    exit 2
}
command -v "$qemu_img_bin" >/dev/null 2>&1 || { printf 'error: qemu-img is required\n' >&2; exit 1; }
command -v "$qemu_bin" >/dev/null 2>&1 || { printf 'error: qemu-system-x86_64 is required\n' >&2; exit 1; }
[[ "$timeout_seconds" =~ ^[1-9][0-9]*$ ]] || { printf 'error: timeout must be positive\n' >&2; exit 1; }

mkdir -p "$output_dir"
target_disk="$(mktemp --tmpdir="${TMPDIR:-/tmp}" devcore-installer-target.XXXXXX.qcow2)"
vars_file="$(mktemp --tmpdir="${TMPDIR:-/tmp}" devcore-installer-vars.XXXXXX.fd)"
log_file="$output_dir/live-boot.serial.log"
trap 'rm -f -- "$target_disk" "$vars_file"' EXIT

"$qemu_img_bin" create -f qcow2 "$target_disk" 40G >/dev/null
ovmf_code="${OVMF_CODE:-/usr/share/edk2/ovmf/OVMF_CODE.fd}"
ovmf_vars="${OVMF_VARS:-/usr/share/edk2/ovmf/OVMF_VARS.fd}"
[[ -r "$ovmf_code" && -r "$ovmf_vars" ]] || { printf 'error: OVMF firmware is required\n' >&2; exit 1; }
cp --preserve=mode,timestamps "$ovmf_vars" "$vars_file"

set +e
timeout --foreground "$timeout_seconds" "$qemu_bin" \
    -machine q35,accel=kvm -cpu host -smp 2 -m 4096 \
    -drive "if=pflash,format=raw,readonly=on,file=$ovmf_code" \
    -drive "if=pflash,format=raw,file=$vars_file" \
    -drive "file=$target_disk,format=qcow2,if=virtio" \
    -cdrom "$iso" -serial "file:$log_file" -display none -no-reboot
qemu_status=$?
set -e
case "$qemu_status" in 0|124) ;; *) printf 'error: QEMU exited %s\n' "$qemu_status" >&2; exit 1 ;; esac
grep -Eqi 'Linux version|Booting Linux' "$log_file" || { printf 'error: live ISO did not reach Linux\n' >&2; exit 1; }
grep -Eqi 'systemd(\[| |$)' "$log_file" || { printf 'error: live ISO did not reach systemd\n' >&2; exit 1; }
printf '{"live_boot":"passed","target":"disposable-qcow2","install_interaction":"operator-required","serial_log":"%s"}\n' "$log_file" > "$output_dir/live-boot.json"
printf 'safe native-installer live boot evidence recorded: %s\n' "$output_dir/live-boot.json"
