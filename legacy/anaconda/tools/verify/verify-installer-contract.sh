#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
containerfile="$repo_root/images/Installer.Containerfile"
kickstart="$repo_root/packaging/installer/interactive-defaults.ks"
iso_config="$repo_root/packaging/installer/iso.yaml"
oci_script="$repo_root/tools/image/build-installer-oci.sh"
iso_script="$repo_root/tools/image/build-installer-iso.sh"
firstboot_ui="$repo_root/apps/devcore-greeter/src/main.rs"

for file in "$containerfile" "$kickstart" "$iso_config" "$oci_script" "$iso_script"; do
    [[ -f "$file" ]] || { printf 'error: missing installer file: %s\n' "$file" >&2; exit 1; }
done

grep -Fqx 'graphical' "$kickstart"
grep -Fqx 'eula --agreed' "$kickstart"
grep -Fq 'inst.graphical' "$iso_config"
grep -Fq 'anaconda' "$containerfile"
grep -Fq 'bootc-installer-payload-ref' "$iso_script"
grep -Fq 'bootc-generic-iso' "$iso_script"
grep -Fq 'DEVCORE_QCOW2_VERIFIED' "$iso_script"
grep -Fq 'DEVCORE_PROVISIONING_VERIFIED' "$iso_script"
grep -Fq '(root.step + 1) + " of 8"' "$firstboot_ui"
grep -Fq 'root.step == 7' "$firstboot_ui"
grep -Fq '.min(7)' "$firstboot_ui"

for forbidden in clearpart autopart user rootpw package; do
    if grep -Eqi "^[[:space:]]*${forbidden}( |$)" "$kickstart"; then
        printf 'error: installer defaults must not silently run destructive or account/package commands: %s\n' "$forbidden" >&2
        exit 1
    fi
done

bash -n "$oci_script"
bash -n "$iso_script"
printf 'installer contract checks passed\n'
