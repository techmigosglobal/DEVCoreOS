#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
workd_binary="${DEVCORE_WORKD_BIN:-$repo_root/target/debug/devcore-workd}"

for command_name in dbus-run-session gdbus; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        printf 'error: required command not found: %s\n' "$command_name" >&2
        exit 1
    fi
done
[[ -x "$workd_binary" ]] || {
    printf 'error: devcore-workd binary is not executable: %s\n' "$workd_binary" >&2
    exit 1
}

smoke_root="$(mktemp -d "${TMPDIR:-/tmp}/devcore-workd-verify.XXXXXX")"
case "$smoke_root" in
    "${TMPDIR:-/tmp}/devcore-workd-verify."*) ;;
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
export DEVCORE_WORKD_BIN="$workd_binary"
dbus-run-session -- bash -s <<'DBUS_SCRIPT'
    set -Eeuo pipefail
    checkpoint='service startup'
    report_failure() {
        status=$?
        printf 'Work D-Bus verification failed (status %s)\n' "$status" >&2
        printf 'checkpoint: %s\n' "$checkpoint" >&2
        cat "$DEVCORE_VERIFY_ROOT/workd.log" >&2 || true
        cat "$DEVCORE_VERIFY_ROOT/introspection.err" >&2 || true
        [[ -z "${build_output:-}" ]] || printf 'build output: %s\n' "$build_output" >&2
        [[ -z "${test_output:-}" ]] || printf 'test output: %s\n' "$test_output" >&2
        [[ -z "${status_output:-}" ]] || printf 'status output: %s\n' "$status_output" >&2
        [[ -z "${cancellation_status:-}" ]] || printf 'cancellation status: %s\n' "$cancellation_status" >&2
        [[ -z "${build_log:-}" ]] || printf 'build log: %s\n' "$build_log" >&2
        [[ -z "${test_log:-}" ]] || printf 'test log: %s\n' "$test_log" >&2
        [[ -z "${workspace_read:-}" ]] || printf 'workspace read: %s\n' "$workspace_read" >&2
        [[ -z "${workspace_write:-}" ]] || printf 'workspace write: %s\n' "$workspace_write" >&2
        [[ -z "${workspace_list:-}" ]] || printf 'workspace list: %s\n' "$workspace_list" >&2
        [[ -z "${git_output:-}" ]] || printf 'git output: %s\n' "$git_output" >&2
        [[ -z "${git_diff_output:-}" ]] || printf 'git diff output: %s\n' "$git_diff_output" >&2
        [[ -z "${git_history_output:-}" ]] || printf 'git history output: %s\n' "$git_history_output" >&2
        [[ -z "${git_log:-}" ]] || printf 'git log: %s\n' "$git_log" >&2
        [[ -z "${git_diff_log:-}" ]] || printf 'git diff log: %s\n' "$git_diff_log" >&2
        [[ -z "${git_history_log:-}" ]] || printf 'git history log: %s\n' "$git_history_log" >&2
        exit "$status"
    }
    trap report_failure ERR

    stub_bin="$DEVCORE_VERIFY_ROOT/bin"
    mkdir -p "$stub_bin"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -Eeuo pipefail' \
        'while [[ $# -gt 0 && "$1" == --* ]]; do shift; done' \
        '[[ $# -gt 0 ]]' \
        'exec "$@"' >"$stub_bin/systemd-run"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -Eeuo pipefail' \
        'printf "stub podman argv:"' \
        'printf " <%s>" "$@"' \
        'printf "\\n"' \
        'if [[ " $* " == *" git status --short --branch "* ]]; then printf "GIT_STATUS_ARGV_OK\\n"; fi' \
        'if [[ " $* " == *" git diff --no-color --no-ext-diff "* ]]; then printf "GIT_DIFF_ARGV_OK\\n"; fi' \
        'if [[ " $* " == *" git log --oneline --decorate --no-color -n 20 "* ]]; then printf "GIT_HISTORY_ARGV_OK\\n"; fi' \
        'if [[ " $* " == *" --devcore-cancel-smoke "* ]]; then exec sleep 5; fi' >"$stub_bin/podman"
    chmod 700 "$stub_bin/systemd-run" "$stub_bin/podman"
    export PATH="$stub_bin:$PATH"

    checkpoint='starting work service'
    XDG_STATE_HOME="$DEVCORE_VERIFY_ROOT/state" "$DEVCORE_WORKD_BIN" \
        >"$DEVCORE_VERIFY_ROOT/workd.log" 2>&1 &
    workd_pid=$!
    stop_workd() {
        kill "$workd_pid" 2>/dev/null || true
        wait "$workd_pid" 2>/dev/null || true
    }
    trap stop_workd EXIT

    ready=0
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        if gdbus introspect --session --dest org.devcore.Work1 \
            --object-path /org/devcore/Work \
            >"$DEVCORE_VERIFY_ROOT/introspection.txt" \
            2>"$DEVCORE_VERIFY_ROOT/introspection.err"; then
            ready=1
            break
        fi
        sleep 0.1
    done
    if [[ "$ready" != 1 ]]; then
        cat "$DEVCORE_VERIFY_ROOT/workd.log" >&2 || true
        cat "$DEVCORE_VERIFY_ROOT/introspection.err" >&2 || true
        exit 1
    fi

    checkpoint='checking D-Bus interfaces'
    for interface_name in Environment1 Build1 Test1 Workspace1 Git1 Job1; do
        grep -Fq "interface org.devcore.$interface_name" \
            "$DEVCORE_VERIFY_ROOT/introspection.txt"
    done
    grep -Fq "ReadLog" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "Cancel" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "Ensure" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "ReadText" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "WriteText" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "StartStatus" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "StartDiff" "$DEVCORE_VERIFY_ROOT/introspection.txt"
    grep -Fq "StartHistory" "$DEVCORE_VERIFY_ROOT/introspection.txt"

    checkpoint='ensuring environment'
    environment_image="quay.io/devcore/rust@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    workspace_dir="$DEVCORE_VERIFY_ROOT/workspace"
    mkdir -p "$workspace_dir/src"
    printf 'before workspace service\n' >"$workspace_dir/README.md"
    gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Environment1.Ensure \
        devcore-rust "$environment_image" "$workspace_dir" "['rust']" false >/dev/null
    gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Environment1.Ensure \
        devcore-rust "$environment_image" "$workspace_dir" "['rust']" false >/dev/null

    checkpoint='listing, reading, and writing workspace text'
    workspace_list=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Workspace1.List \
        devcore-rust "")
    grep -Fq "README.md" <<<"$workspace_list"
    grep -Fq "src" <<<"$workspace_list"
    workspace_read=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Workspace1.ReadText \
        devcore-rust README.md)
    grep -Fq "before workspace service" <<<"$workspace_read"
    workspace_write=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Workspace1.WriteText \
        devcore-rust README.md "after workspace service")
    grep -Fq "README.md" <<<"$workspace_write"
    grep -Fxq "after workspace service" "$workspace_dir/README.md"
    if traversal_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Workspace1.ReadText \
        devcore-rust ../outside.txt 2>&1); then
        traversal_code=0
    else
        traversal_code=$?
    fi
    [[ "$traversal_code" -ne 0 ]]
    grep -Fq "workspace file path" <<<"$traversal_output"

    checkpoint='submitting Git status'
    git_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Git1.StartStatus \
        devcore-rust dbus-git-smoke "$workspace_dir")
    grep -Fq "dbus-git-smoke" <<<"$git_output"

    checkpoint='submitting Git diff and history'
    git_diff_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Git1.StartDiff \
        devcore-rust dbus-git-diff-smoke "$workspace_dir")
    grep -Fq "dbus-git-diff-smoke" <<<"$git_diff_output"
    git_history_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Git1.StartHistory \
        devcore-rust dbus-git-history-smoke "$workspace_dir")
    grep -Fq "dbus-git-history-smoke" <<<"$git_history_output"

    checkpoint='submitting build'
    build_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Build1.StartBuild \
        devcore-rust dbus-build-smoke cargo-build cargo "$workspace_dir")
    grep -Fq "dbus-build-smoke" <<<"$build_output"

    checkpoint='submitting test'
    test_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Test1.StartTest \
        devcore-rust dbus-test-smoke unit-test unit printf "['stub-test']" "$workspace_dir")
    grep -Fq "dbus-test-smoke" <<<"$test_output"

    checkpoint='submitting cancellation test'
    cancel_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Test1.StartTest \
        devcore-rust dbus-cancel-smoke cancel-test unit printf "['--devcore-cancel-smoke']" "$workspace_dir")
    grep -Fq "dbus-cancel-smoke" <<<"$cancel_output"
    sleep 0.1
    gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.Cancel dbus-cancel-smoke >/dev/null

    wait_for_success() {
        local job_id="$1"
        local status_output=''
        for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
            status_output=$(gdbus call --session --dest org.devcore.Work1 \
                --object-path /org/devcore/Work \
                --method org.devcore.Job1.Status "$job_id")
            if grep -Fq "'succeeded'" <<<"$status_output"; then
                return 0
            fi
            if grep -Eq "'failed'|'cancelled'" <<<"$status_output"; then
                printf 'job did not succeed: %s\n' "$status_output" >&2
                return 1
            fi
            sleep 0.1
        done
        printf 'job did not finish in time: %s\n' "$status_output" >&2
        return 1
    }

    checkpoint='waiting for build'
    wait_for_success dbus-build-smoke
    checkpoint='waiting for test'
    wait_for_success dbus-test-smoke
    checkpoint='waiting for Git status'
    wait_for_success dbus-git-smoke
    checkpoint='waiting for Git diff'
    wait_for_success dbus-git-diff-smoke
    checkpoint='waiting for Git history'
    wait_for_success dbus-git-history-smoke
    checkpoint='waiting for cancellation'
    cancellation_status=''
    for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
        cancellation_status=$(gdbus call --session --dest org.devcore.Work1 \
            --object-path /org/devcore/Work \
            --method org.devcore.Job1.Status dbus-cancel-smoke)
        if grep -Fq "'cancelled'" <<<"$cancellation_status"; then
            break
        fi
        if grep -Fq "'succeeded'" <<<"$cancellation_status"; then
            printf 'cancellation request did not cancel job: %s\n' "$cancellation_status" >&2
            exit 1
        fi
        sleep 0.1
    done
    grep -Fq "'cancelled'" <<<"$cancellation_status"

    checkpoint='reading build log'
    build_log=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.ReadLog dbus-build-smoke stdout 0 4096)
    grep -Fq "byte 0x73" <<<"$build_log"
    grep -Fq "0x70" <<<"$build_log"

    checkpoint='reading test log'
    test_log=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.ReadLog dbus-test-smoke stdout 0 4096)
    grep -Fq "byte 0x73" <<<"$test_log"
    grep -Fq "0x70" <<<"$test_log"

    checkpoint='reading Git status log'
    git_log=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.ReadLog dbus-git-smoke stdout 0 4096)
    grep -Fq "0x47" <<<"$git_log"
    grep -Fq "0x49" <<<"$git_log"

    checkpoint='reading Git diff log'
    git_diff_log=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.ReadLog dbus-git-diff-smoke stdout 0 4096)
    grep -Fq "0x44" <<<"$git_diff_log"
    grep -Fq "0x49" <<<"$git_diff_log"

    checkpoint='reading Git history log'
    git_history_log=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.ReadLog dbus-git-history-smoke stdout 0 4096)
    grep -Fq "0x48" <<<"$git_history_log"
    grep -Fq "0x49" <<<"$git_history_log"

    checkpoint='checking unknown job errors'
    if status_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.Status missing-job 2>&1); then
        status_code=0
    else
        status_code=$?
    fi
    [[ "$status_code" -ne 0 ]]
    grep -Fq "unknown job" <<<"$status_output"

    if log_output=$(gdbus call --session --dest org.devcore.Work1 \
        --object-path /org/devcore/Work \
        --method org.devcore.Job1.ReadLog missing-job stdout 0 4096 2>&1); then
        log_code=0
    else
        log_code=$?
    fi
    [[ "$log_code" -ne 0 ]]
    grep -Fq "unknown job" <<<"$log_output"
DBUS_SCRIPT

printf 'user-session Work D-Bus contract verified\n'
