#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cargo_bin="${CARGO_BIN:-cargo}"
daemon_binary="$repo_root/target/debug/devcore-firstbootd"

if ! command -v "$cargo_bin" >/dev/null 2>&1; then
    printf 'error: cargo is required\n' >&2
    exit 1
fi
for command_name in dbus-run-session busctl; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'error: required command not found: %s\n' "$command_name" >&2
        exit 1
    fi
done

cd "$repo_root"
"$cargo_bin" build --locked -p devcore-firstbootd
state_root="$(mktemp -d "${TMPDIR:-/tmp}/devcore-firstboot-check.XXXXXX")"
trap 'rm -rf "$state_root"' EXIT

dbus-run-session -- env \
    DEVCORE_FIRSTBOOT_BUS=session \
    DEVCORE_FIRSTBOOT_STATE_DIR="$state_root" \
    DEVCORE_FIRSTBOOT_BINARY="$daemon_binary" \
    bash -c '
        set -Eeuo pipefail
        "$DEVCORE_FIRSTBOOT_BINARY" >/dev/null 2>&1 &
        daemon_pid=$!
        cleanup() {
            kill "$daemon_pid" 2>/dev/null || true
            wait "$daemon_pid" 2>/dev/null || true
        }
        trap cleanup EXIT

        status=""
        for _ in $(seq 1 50); do
            if status="$(busctl --user call \
                org.devcore.FirstBoot1 \
                /org/devcore/FirstBoot \
                org.devcore.FirstBoot1 Status 2>/dev/null)"; then
                break
            fi
            status=""
            sleep 0.1
        done
        [[ "$status" == *"true"* ]]
        busctl --user introspect \
            org.devcore.FirstBoot1 \
            /org/devcore/FirstBoot \
            org.devcore.FirstBoot1 >/dev/null
    '

printf 'first-boot disposable D-Bus status check passed\n'
