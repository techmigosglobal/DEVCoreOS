#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
update_binary="${DEVCORE_UPDATED_BIN:-$repo_root/target/debug/devcore-updated}"

for command_name in dbus-run-session gdbus; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'error: required command not found: %s\n' "$command_name" >&2
        exit 1
    fi
done
[[ -x "$update_binary" ]] || {
    printf 'error: devcore-updated binary is not executable: %s\n' "$update_binary" >&2
    exit 1
}

smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/devcore-update-verify.XXXXXX")"
case "$smoke_root" in
    "${TMPDIR:-/tmp}/devcore-update-verify."*) ;;
    *)
        printf 'error: temporary verification directory is outside the expected scope\n' >&2
        exit 1
        ;;
esac
cleanup() {
    rm -rf -- "$smoke_root"
}
trap cleanup EXIT

export DEVCORE_UPDATED_BIN="$update_binary"
export DEVCORE_UPDATE_ROOT="$smoke_root"
dbus-run-session -- bash -s <<'DBUS_SCRIPT'
    set -Eeuo pipefail
    export DEVCORE_UPDATE_BUS=session
    export DEVCORE_BOOTC_BIN=/usr/bin/echo
    export DEVCORE_UPDATE_CHANNEL=beta
    "$DEVCORE_UPDATED_BIN" >"$DEVCORE_UPDATE_ROOT/updated.log" 2>&1 &
    updated_pid=$!
    stop_updated() {
        kill "$updated_pid" 2>/dev/null || true
        wait "$updated_pid" 2>/dev/null || true
    }
    trap stop_updated EXIT

    ready=0
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        if gdbus introspect --session --dest org.devcore.Update1 \
            --object-path /org/devcore/Update \
            >"$DEVCORE_UPDATE_ROOT/introspection.txt" 2>"$DEVCORE_UPDATE_ROOT/introspection.err"; then
            ready=1
            break
        fi
        sleep 0.1
    done
    if [[ "$ready" != 1 ]]; then
        cat "$DEVCORE_UPDATE_ROOT/updated.log" >&2 || true
        cat "$DEVCORE_UPDATE_ROOT/introspection.err" >&2 || true
        exit 1
    fi

    grep -Fq 'interface org.devcore.Update1' "$DEVCORE_UPDATE_ROOT/introspection.txt"
    grep -Fq 'StatusJson' "$DEVCORE_UPDATE_ROOT/introspection.txt"
    grep -Fq 'Plan' "$DEVCORE_UPDATE_ROOT/introspection.txt"

    status=$(gdbus call --session --dest org.devcore.Update1 \
        --object-path /org/devcore/Update \
        --method org.devcore.Update1.StatusJson)
    grep -Fq 'status --json' <<<"$status"

    image="quay.io/devcore/os@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    plan=$(gdbus call --session --dest org.devcore.Update1 \
        --object-path /org/devcore/Update \
        --method org.devcore.Update1.Plan apply beta "$image")
    grep -Fq '"operation":"apply"' <<<"$plan"
    grep -Fq '"channel":"beta"' <<<"$plan"
    grep -Fq 'ostree-container' <<<"$plan"

    if invalid=$(gdbus call --session --dest org.devcore.Update1 \
        --object-path /org/devcore/Update \
        --method org.devcore.Update1.Plan apply beta quay.io/devcore/os:latest 2>&1); then
        exit 1
    fi
    grep -Fq 'invalid update image' <<<"$invalid"

    printf 'update D-Bus session verification passed\n'
DBUS_SCRIPT
