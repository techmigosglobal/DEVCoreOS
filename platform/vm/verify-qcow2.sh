#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ $# -lt 1 || $# -gt 2 ]]; then
    printf 'usage: %s <disk.qcow2> [image-reference]\n' "${0##*/}" >&2
    exit 2
fi

disk="$1"
image_ref="${2:-${DEVCORE_IMAGE_REF:-ghcr.io/techmigosglobal/devcoreos:alpha}}"
timeout_seconds="${DEVCORE_VM_TIMEOUT_SECONDS:-60}"
evidence_path="${DEVCORE_QCOW2_EVIDENCE:-$repo_root/artifacts/evidence/qcow2-uefi.json}"
serial_log="$(mktemp "${TMPDIR:-/tmp}/devcore-qcow2-verify.XXXXXX.log")"

if [[ ! -f "$disk" ]]; then
    printf 'error: QCOW2 input must be a regular file: %s\n' "$disk" >&2
    exit 1
fi
if [[ -b "$disk" ]]; then
    printf 'error: block devices are never valid QCOW2 verification inputs\n' >&2
    exit 1
fi
if [[ ! "$timeout_seconds" =~ ^[1-9][0-9]*$ ]]; then
    printf 'error: DEVCORE_VM_TIMEOUT_SECONDS must be a positive integer\n' >&2
    exit 1
fi
if [[ ! "$image_ref" =~ ^[A-Za-z0-9._:/@+-]+$ ]]; then
    printf 'error: image reference contains unsupported evidence characters\n' >&2
    exit 1
fi
if [[ -e "$evidence_path" && "${DEVCORE_OVERWRITE_EVIDENCE:-0}" != "1" ]]; then
    printf 'error: evidence file already exists; set DEVCORE_OVERWRITE_EVIDENCE=1 to replace it\n' >&2
    exit 1
fi

set +e
DEVCORE_QEMU_LOG="$serial_log" \
    "$repo_root/platform/vm/boot-qcow2.sh" "$disk" "$timeout_seconds"
qemu_status=$?
set -e

# A timeout is expected for a guest that remains running. A clean QEMU exit is
# also acceptable. Any other status indicates a QEMU or firmware failure.
case "$qemu_status" in
    0 | 124) ;;
    *)
        printf 'error: QEMU exited with status %s; serial log: %s\n' "$qemu_status" "$serial_log" >&2
        exit 1
        ;;
esac

grep -Eqi 'Linux version|Booting Linux' "$serial_log" || {
    printf 'error: serial log has no Linux kernel boot marker: %s\n' "$serial_log" >&2
    exit 1
}
grep -Eqi 'systemd(\[| |$)' "$serial_log" || {
    printf 'error: serial log has no systemd startup marker: %s\n' "$serial_log" >&2
    exit 1
}

mkdir -p "$(dirname "$evidence_path")"
printf '{"image_ref":"%s","boot_status":"passed","qemu_exit_status":%s,"serial_log":"%s"}\n' \
    "$image_ref" "$qemu_status" "$serial_log" >"$evidence_path"
printf 'QCOW2 UEFI boot evidence recorded: %s\n' "$evidence_path"
