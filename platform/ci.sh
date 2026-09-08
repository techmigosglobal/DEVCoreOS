#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_bin="${CARGO_BIN:-cargo}"

if ! command -v "$cargo_bin" >/dev/null 2>&1; then
    printf 'error: cargo is required; set CARGO_BIN or use the repository-local toolchain\n' >&2
    exit 1
fi

cd "$repo_root"
"$cargo_bin" fmt --check
"$cargo_bin" test --workspace
"$cargo_bin" clippy --workspace --all-targets -- -D warnings
"$cargo_bin" build --locked \
    -p devcore-installer -p devcore-installerd -p devcore-workd \
    -p devcore-hardwared -p devcore-updated -p devcore-provisiond
"$repo_root/platform/verify/verify-workd-session.sh"
"$repo_root/platform/verify/verify-firstboot.sh"
"$repo_root/platform/verify/verify-hardware-session.sh"
"$repo_root/platform/verify/verify-update-session.sh"
"$repo_root/platform/verify/verify-provisioning-contract.sh"
"$repo_root/platform/verify/verify-provisioning-session.sh"
"$repo_root/platform/verify/verify-image-contract.sh"
"$repo_root/platform/verify/verify-installer-contract.sh"
