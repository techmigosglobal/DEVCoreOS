#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
containerfile="$repo_root/platform/image/Containerfile"
provision_lib="$repo_root/features/provision/domain/src/lib.rs"
provisiond="$repo_root/features/provision/daemon/src/main.rs"
dbus_service="$repo_root/platform/system/dbus-1/services/org.devcore.Provision1.service"
iso_script="$repo_root/platform/image/build-iso.sh"

for file in "$containerfile" "$provision_lib" "$provisiond" "$dbus_service" "$iso_script"; do
    [[ -f "$file" ]] || { printf 'error: missing provisioning asset: %s\n' "$file" >&2; exit 1; }
done

grep -Fq 'devcore-provisiond' "$repo_root/platform/image/build-oci.sh"
grep -Fq 'devcore-provisiond' "$containerfile"
grep -Fq 'org.devcore.Provision1' "$dbus_service"
grep -Fq 'digest-pinned' "$provision_lib"
grep -Fq 'DEVCORE_PROVISIONING_VERIFIED=1' "$iso_script"
printf 'Provisioning contract checks passed\n'
