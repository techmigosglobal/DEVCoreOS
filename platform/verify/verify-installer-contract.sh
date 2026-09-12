#!/usr/bin/env bash
set -Eeuo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
image_root="$repo_root/platform/image"
system_root="$repo_root/platform/system"
contract="$repo_root/contracts/dbus/org.devcore.Installer1.xml"
containerfile="$image_root/Installer.Containerfile"
boot_config="$image_root/live-iso.yaml"
oci_script="$image_root/build-native-installer-oci.sh"
iso_script="$image_root/build-native-installer-iso.sh"
service="$system_root/systemd/devcore-installer.service"
live_greetd="$system_root/greetd/live-config.toml"
live_tmpfiles="$system_root/tmpfiles.d/devcore-live.conf"
installer_app="$repo_root/features/installer/app/src/main.rs"
shell_app="$repo_root/features/shell/app/src/main.rs"
policy="$system_root/dbus-1/system.d/org.devcore.Installer1.conf"
activation="$system_root/dbus-1/system-services/org.devcore.Installer1.service"
polkit_action="$system_root/polkit-1/actions/org.devcore.installer.policy"
polkit_rule="$system_root/polkit-1/rules.d/org.devcore.installer.rules"
installer_config="$system_root/installer/config.json"
installer_containerfile="$containerfile"
ci_workflow="$repo_root/.github/workflows/ci.yml"

for file in "$contract" "$containerfile" "$boot_config" "$oci_script" "$iso_script" \
    "$service" "$live_greetd" "$live_tmpfiles" "$policy" "$activation" "$polkit_action" "$polkit_rule" \
    "$installer_config" "$system_root/sysusers.d/devcore-live.conf" "$installer_app" "$shell_app" \
    "$ci_workflow"; do
    [[ -f "$file" ]] || {
        printf 'error: missing native installer asset: %s\n' "$file" >&2
        exit 1
    }
done

for script in "$oci_script" "$iso_script" "$repo_root/platform/verify/verify-installer-contract.sh"; do
    bash -n "$script"
done

# Limit the scan to replacement assets, so legacy files can coexist during the
# migration without letting them become inputs to the new installer.
if grep -R -Eqi '(anaconda|kickstart)' "$image_root" "$system_root" \
    --exclude='Installer.Containerfile'; then
    printf 'error: replacement installer assets must not depend on the legacy installer stack\n' >&2
    exit 1
fi
if grep -Eqi '(FROM .*anaconda|dnf.*anaconda|anaconda\.target|kickstart)' "$containerfile"; then
    printf 'error: installer image must not use the legacy installer stack\n' >&2
    exit 1
fi
forbidden_ref="$(printf '%s%s' 'registry:' 'localhost')"
if grep -R -Fq "$forbidden_ref" "$image_root" "$system_root"; then
    printf 'error: replacement installer assets must not use a local registry payload\n' >&2
    exit 1
fi

grep -Fqx 'label: DEVCOREOS_LIVE' "$boot_config"
grep -Fq 'name: Try DevCore OS' "$boot_config"
grep -Fq 'name: Install DevCore OS' "$boot_config"
grep -Fq 'devcore.live=1' "$boot_config"
grep -Fq 'devcore.installer=manual' "$boot_config"
grep -Fq 'devcore.installer=auto' "$boot_config"
[[ "$(grep -c 'root=live:CDLABEL=DEVCOREOS_LIVE' "$boot_config")" -eq 2 ]]
[[ "$(grep -c 'systemd.unit=graphical.target' "$boot_config")" -eq 2 ]]
grep -Fq 'rd.live.overlay.overlayfs=1' "$boot_config"
grep -Fq 'dmsquash-live' "$containerfile"
grep -Fqx '[default_session]' "$live_greetd"
grep -Fq 'user = "devcore-live"' "$live_greetd"
grep -Fq 'devcore-compositor --backend drm --shell /usr/bin/devcore-shell' "$live_greetd"
grep -Fq 'devcore.installer=auto' "$shell_app"
grep -Fq 'Command::new("/usr/bin/devcore-installer")' "$shell_app"

grep -Fqx 'COPY usr/bin/devcore-installer /usr/bin/devcore-installer' "$containerfile"
grep -Fq 'ARG DEVCORE_INSTALLER_BASE_IMAGE=' "$containerfile"
grep -Fq 'FROM ${DEVCORE_INSTALLER_BASE_IMAGE}' "$containerfile"
grep -Fq 'ARG DEVCORE_SKIP_INSTALLER_PACKAGES=false' "$containerfile"
grep -Fq 'gdisk' "$containerfile"
if grep -Fq 'gptfdisk' "$containerfile"; then
    printf 'error: Fedora installer image must use the gdisk package name\n' >&2
    exit 1
fi
grep -Fq 'rm -rf /usr/share/anaconda /usr/libexec/anaconda' "$containerfile"
grep -Fq 'graphical.target /etc/systemd/system/default.target' "$containerfile"
grep -Fq 'multi-user.target.wants/greetd.service' "$containerfile"
grep -Fqx 'COPY usr/libexec/devcore/devcore-installerd /usr/libexec/devcore/devcore-installerd' "$containerfile"
grep -Fq 'COPY usr/share/devcore-installer/payload/devcore-baseos.oci.tar' "$containerfile"
grep -Fq 'COPY usr/share/devcore-installer/payload/SHA256SUMS' "$containerfile"
grep -Fq 'COPY usr/lib/image-builder/bootc/iso.yaml /usr/lib/image-builder/bootc/iso.yaml' "$containerfile"
grep -Fq 'sha256sum --strict --check SHA256SUMS' "$containerfile"
grep -Fq 'COPY usr/lib/systemd/system/devcore-installer.service' "$containerfile"
grep -Fq 'COPY etc/greetd/config.toml /etc/greetd/config.toml' "$containerfile"
grep -Fq 'COPY etc/pam.d/greetd /etc/pam.d/greetd' "$containerfile"
grep -Fq 'COPY etc/systemd/system/greetd.service.d/live.conf /etc/systemd/system/greetd.service.d/live.conf' "$containerfile"
grep -Fq 'COPY usr/lib/tmpfiles.d/devcore-live.conf' "$containerfile"
grep -Fq 'COPY usr/share/dbus-1/system.d/org.devcore.Installer1.conf' "$containerfile"
grep -Fq 'COPY usr/share/polkit-1/actions/org.devcore.installer.policy' "$containerfile"
grep -Fq 'systemctl enable devcore-installer.service' "$containerfile"

grep -Fq '<interface name="org.devcore.Installer1">' "$contract"
for method in ListDisks Preflight Start Status Cancel; do
    grep -Fq "<method name=\"$method\">" "$contract"
done
grep -Fq '<arg name="password_fd" type="h" direction="in"/>' "$contract"
grep -Fq 'transient-only; never stored, logged, or passed through argv' "$contract"
grep -Fq 'canonical /dev/disk/by-id path' "$contract"
grep -Fq 'disks below 32 GiB' "$contract"
grep -Fq 'ERASE &lt;disk suffix&gt;' "$installer_config" || grep -Fq 'ERASE <disk suffix>' "$installer_config"
grep -Fq 'bootc install to-filesystem' "$installer_config"
grep -Fq '<signal name="Progress">' "$contract"
grep -Fq '<signal name="StateChanged">' "$contract"

grep -Fqx 'Type=dbus' "$service"
grep -Fqx 'BusName=org.devcore.Installer1' "$service"
grep -Fq 'ExecStart=/usr/libexec/devcore/devcore-installerd --config /usr/share/devcore-installer/config.json' "$service"
grep -Fqx 'User=root' "$service"
grep -Fqx 'NoNewPrivileges=yes' "$service"
grep -Fqx 'PrivateDevices=no' "$service"
grep -Fqx 'Name=org.devcore.Installer1' "$activation"
grep -Fqx 'SystemdService=devcore-installer.service' "$activation"
grep -Fq '<allow own="org.devcore.Installer1"/>' "$policy"
grep -Fq '<allow send_destination="org.devcore.Installer1"/>' "$policy"
grep -Fq '<policy group="devcore-live">' "$policy"
grep -Fq '<deny send_destination="org.devcore.Installer1"/>' "$policy"
grep -Fq 'action id="org.devcore.installer.manage"' "$polkit_action"
grep -Fq 'subject.isInGroup("devcore-live")' "$polkit_rule"

grep -Fq 'minimum_disk_bytes' "$installer_config"
grep -Fq '34359738368' "$installer_config"
grep -Fq 'efi_system_partition_bytes' "$installer_config"
grep -Fq '1073741824' "$installer_config"
grep -Fq '"root_filesystem": "ext4"' "$installer_config"
grep -Fq '"disk_id_prefix": "/dev/disk/by-id/"' "$installer_config"
grep -Fq '"cancellation": "before-destructive-storage-only"' "$installer_config"
grep -Fq 'save --format oci-archive' "$oci_script"
grep -Fq 'sha256sum devcore-baseos.oci.tar payload-image.ref payload-config.digest > SHA256SUMS' "$oci_script"
grep -Fq 'payload-config.digest' "$installer_containerfile"
grep -Fq 'podman load --input' "$installer_config"
grep -Fq '"$(uname -m)" == "x86_64"' "$iso_script"
builder_image="$(sed -n 's/^image_builder_image = "\(.*\)"$/\1/p' "$repo_root/platform/image/base-images.lock")"
[[ "$builder_image" =~ ^ghcr\.io/osbuild/image-builder-cli@sha256:[0-9a-f]{64}$ ]]
grep -Fq 'Image Builder container must be digest pinned' "$iso_script"
grep -Fq 'build_iso:' "$ci_workflow"
grep -Fq 'libxkbcommon-dev' "$ci_workflow"
grep -Fq 'platform/image/build-native-installer-iso.sh' "$ci_workflow"
grep -Fq 'if-no-files-found: error' "$ci_workflow"
grep -Fq "find \"\$DEVCORE_INSTALLER_OUTPUT_DIR\" -type f -name '*.iso'" "$ci_workflow"
grep -Fq 'OwnedFd::from' "$installer_app"
grep -Fq 'source.set_password("".into())' "$installer_app"

printf 'native installer contract checks passed\n'
