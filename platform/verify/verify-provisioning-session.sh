#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
provisiond_binary="${DEVCORE_PROVISIOND_BIN:-$repo_root/target/debug/devcore-provisiond}"

for command_name in dbus-run-session gdbus; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'error: required command not found: %s\n' "$command_name" >&2
        exit 1
    fi
done
[[ -x "$provisiond_binary" ]] || {
    printf 'error: devcore-provisiond binary is not executable: %s\n' "$provisiond_binary" >&2
    exit 1
}

smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/devcore-provision-verify.XXXXXX")"
case "$smoke_root" in
    "${TMPDIR:-/tmp}/devcore-provision-verify."*) ;;
    *)
        printf 'error: temporary verification directory is outside the expected scope\n' >&2
        exit 1
        ;;
esac
cleanup() {
    rm -rf -- "$smoke_root"
}
trap cleanup EXIT

export DEVCORE_VERIFY_ROOT="$smoke_root"
export DEVCORE_PROVISIOND_BIN="$provisiond_binary"
dbus-run-session -- bash -s <<'DBUS_SCRIPT'
    set -Eeuo pipefail
    checkpoint='service startup'
    report_failure() {
        status=$?
        printf 'Provisioning D-Bus verification failed (status %s)\n' "$status" >&2
        printf 'checkpoint: %s\n' "$checkpoint" >&2
        cat "$DEVCORE_VERIFY_ROOT/provisiond.log" >&2 || true
        cat "$DEVCORE_VERIFY_ROOT/introspection.err" >&2 || true
        exit "$status"
    }
    trap report_failure ERR
    XDG_STATE_HOME="$DEVCORE_VERIFY_ROOT/state" HOME="$DEVCORE_VERIFY_ROOT/home" \
        "$DEVCORE_PROVISIOND_BIN" >"$DEVCORE_VERIFY_ROOT/provisiond.log" 2>&1 &
    provisiond_pid=$!
    cleanup_service() {
        kill "$provisiond_pid" 2>/dev/null || true
        wait "$provisiond_pid" 2>/dev/null || true
    }
    trap cleanup_service EXIT

    checkpoint='service startup'
    ready=0
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        if gdbus introspect --session --dest org.devcore.Provision1 \
            --object-path /org/devcore/Provision \
            >"$DEVCORE_VERIFY_ROOT/introspection.txt" 2>"$DEVCORE_VERIFY_ROOT/introspection.err"; then
            ready=1
            break
        fi
        sleep 0.1
    done
    [[ "$ready" == 1 ]]
    checkpoint='checking D-Bus interfaces'
    grep -Fq "interface org.devcore.Provision1" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "CatalogJson" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "StatusJson" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "Plan" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "Start" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "Cancel" "$DEVCORE_VERIFY_ROOT/introspection.txt"

    checkpoint='reading catalog and status'
    catalog_output=$(gdbus call --session --dest org.devcore.Provision1 \
        --object-path /org/devcore/Provision \
        --method org.devcore.Provision1.CatalogJson)
    grep -Fq "android-studio" <<<"$catalog_output"
    grep -Fq "flutter" <<<"$catalog_output"

    status_output=$(gdbus call --session --dest org.devcore.Provision1 \
        --object-path /org/devcore/Provision \
        --method org.devcore.Provision1.StatusJson)
    grep -Fq '"phase":"idle"' <<<"$status_output"

    checkpoint='planning a Flatpak bundle'
    plan_output=$(gdbus call --session --dest org.devcore.Provision1 \
        --object-path /org/devcore/Provision \
        --method org.devcore.Provision1.Plan \
        "['chrome']" balanced "uint64 68719476736" false "")
    grep -Fq "com.google.Chrome" <<<"$plan_output"
    grep -Fq "flatpak" <<<"$plan_output"

    checkpoint='rejecting start without consent'
    if start_output=$(gdbus call --session --dest org.devcore.Provision1 \
        --object-path /org/devcore/Provision \
        --method org.devcore.Provision1.Start \
        "['chrome']" balanced "uint64 68719476736" false "" \
        2>"$DEVCORE_VERIFY_ROOT/consent.err"); then
        printf 'start without consent unexpectedly succeeded: %s\n' "$start_output" >&2
        exit 1
    fi
    grep -Fq 'explicit user consent is required' "$DEVCORE_VERIFY_ROOT/consent.err"

    checkpoint='checking private atomic state'
    state_file="$DEVCORE_VERIFY_ROOT/state/devcore/provisioning.json"
    [[ -f "$state_file" ]]
    [[ "$(stat -c '%a' "$state_file")" == "600" ]]
DBUS_SCRIPT
printf 'Post-install provisioning D-Bus verification passed\n'
