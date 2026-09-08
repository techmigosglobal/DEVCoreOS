#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
hardware_binary="${DEVCORE_HARDWARE_BIN:-$repo_root/target/debug/devcore-hardwared}"

for command_name in dbus-run-session gdbus; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'error: required command not found: %s\n' "$command_name" >&2
        exit 1
    fi
done
[[ -x "$hardware_binary" ]] || {
    printf 'error: devcore-hardwared binary is not executable: %s\n' "$hardware_binary" >&2
    exit 1
}

smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/devcore-hardware-verify.XXXXXX")"
case "$smoke_root" in
    "${TMPDIR:-/tmp}/devcore-hardware-verify."*) ;;
    *)
        printf 'error: temporary verification directory is outside the expected scope\n' >&2
        exit 1
        ;;
esac
cleanup() {
    rm -rf -- "$smoke_root"
}
trap cleanup EXIT

export DEVCORE_HARDWARE_BIN="$hardware_binary"
export DEVCORE_HARDWARE_ROOT="$smoke_root"
dbus-run-session -- bash -s <<'DBUS_SCRIPT'
    set -Eeuo pipefail
    export DEVCORE_HARDWARE_BUS=session
    "$DEVCORE_HARDWARE_BIN" --daemon >"$DEVCORE_HARDWARE_ROOT/hardwared.log" 2>&1 &
    hardwared_pid=$!
    stop_hardwared() {
        kill "$hardwared_pid" 2>/dev/null || true
        wait "$hardwared_pid" 2>/dev/null || true
    }
    trap stop_hardwared EXIT

    ready=0
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        if gdbus introspect --session --dest org.devcore.Hardware1 \
            --object-path /org/devcore/Hardware \
            >"$DEVCORE_HARDWARE_ROOT/introspection.txt" 2>"$DEVCORE_HARDWARE_ROOT/introspection.err"; then
            ready=1
            break
        fi
        sleep 0.1
    done
    if [[ "$ready" != 1 ]]; then
        cat "$DEVCORE_HARDWARE_ROOT/hardwared.log" >&2 || true
        cat "$DEVCORE_HARDWARE_ROOT/introspection.err" >&2 || true
        exit 1
    fi

    grep -Fq 'interface org.devcore.Hardware1' "$DEVCORE_HARDWARE_ROOT/introspection.txt"
    grep -Fq 'SnapshotJson' "$DEVCORE_HARDWARE_ROOT/introspection.txt"
    grep -Fq 'Refresh' "$DEVCORE_HARDWARE_ROOT/introspection.txt"

    snapshot=$(gdbus call --session --dest org.devcore.Hardware1 \
        --object-path /org/devcore/Hardware \
        --method org.devcore.Hardware1.SnapshotJson)
    grep -Fq 'cpu_model' <<<"$snapshot"
    grep -Fq 'network_interfaces' <<<"$snapshot"

    refreshed=$(gdbus call --session --dest org.devcore.Hardware1 \
        --object-path /org/devcore/Hardware \
        --method org.devcore.Hardware1.Refresh)
    grep -Fq 'memory_total_kib' <<<"$refreshed"

    printf 'hardware D-Bus session verification passed\n'
DBUS_SCRIPT
