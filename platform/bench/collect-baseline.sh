#!/usr/bin/env bash
set -Eeuo pipefail

output_dir="${1:-./artifacts/bench/$(date -u +%Y%m%dT%H%M%SZ)}"
mkdir -p "$output_dir"

command -v devcored >/dev/null 2>&1 || {
    printf 'error: devcored must be installed in the system being measured\n' >&2
    exit 1
}

devcored --json >"$output_dir/devcored.json"
free -k >"$output_dir/memory.txt"
ps -eo pid=,ppid=,comm=,rss=,stat= --sort=-rss >"$output_dir/processes.txt"
cat /proc/pressure/cpu >"$output_dir/psi-cpu.txt" 2>/dev/null || true
cat /proc/pressure/memory >"$output_dir/psi-memory.txt" 2>/dev/null || true
cat /proc/pressure/io >"$output_dir/psi-io.txt" 2>/dev/null || true
du -sx /usr /var >"$output_dir/storage-kib.txt" 2>/dev/null || true
systemd-analyze time >"$output_dir/boot-time.txt" 2>/dev/null || true

printf 'baseline collected in %s\n' "$output_dir"
