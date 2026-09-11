# Native, offline-capable DevCore OS live-installer image.
# The build script supplies a digest-pinned DevCore BaseOS image and an OCI
# archive of that exact local image. No network registry is used at install time.
ARG DEVCORE_BASE_IMAGE
ARG DEVCORE_INSTALLER_BASE_IMAGE=${DEVCORE_BASE_IMAGE}
FROM ${DEVCORE_INSTALLER_BASE_IMAGE}
ARG DEVCORE_SKIP_INSTALLER_PACKAGES=false

LABEL org.opencontainers.image.title="DevCore OS Native Installer" \
      org.opencontainers.image.description="DevCore live desktop with an offline native installer" \
      org.opencontainers.image.vendor="DevCore" \
      org.opencontainers.image.source="https://github.com/techmigosglobal/DEVCoreOS"

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

# The build context is deliberately explicit: only the reviewed installer,
# system integration, live boot configuration, and immutable payload enter the
# image.
COPY usr/bin/devcore-installer /usr/bin/devcore-installer
COPY usr/libexec/devcore/devcore-installerd /usr/libexec/devcore/devcore-installerd
COPY usr/lib/systemd/system/devcore-installer.service /usr/lib/systemd/system/devcore-installer.service
COPY usr/lib/sysusers.d/devcore-live.conf /usr/lib/sysusers.d/devcore-live.conf
COPY usr/lib/tmpfiles.d/devcore-live.conf /usr/lib/tmpfiles.d/devcore-live.conf
COPY usr/share/dbus-1/system.d/org.devcore.Installer1.conf /usr/share/dbus-1/system.d/org.devcore.Installer1.conf
COPY usr/share/dbus-1/system-services/org.devcore.Installer1.service /usr/share/dbus-1/system-services/org.devcore.Installer1.service
COPY usr/share/polkit-1/actions/org.devcore.installer.policy /usr/share/polkit-1/actions/org.devcore.installer.policy
COPY etc/polkit-1/rules.d/org.devcore.installer.rules /etc/polkit-1/rules.d/org.devcore.installer.rules
COPY usr/share/devcore-installer/config.json /usr/share/devcore-installer/config.json
COPY usr/lib/image-builder/bootc/iso.yaml /usr/lib/image-builder/bootc/iso.yaml
COPY etc/greetd/config.toml /etc/greetd/config.toml
COPY etc/pam.d/greetd /etc/pam.d/greetd
COPY etc/systemd/system/greetd.service.d/live.conf /etc/systemd/system/greetd.service.d/live.conf
COPY usr/share/devcore-installer/payload/devcore-baseos.oci.tar /usr/share/devcore-installer/payload/devcore-baseos.oci.tar
COPY usr/share/devcore-installer/payload/SHA256SUMS /usr/share/devcore-installer/payload/SHA256SUMS
COPY usr/share/devcore-installer/payload/payload-image.ref /usr/share/devcore-installer/payload/payload-image.ref
COPY usr/share/devcore-installer/payload/payload-config.digest /usr/share/devcore-installer/payload/payload-config.digest

RUN if [[ "$DEVCORE_SKIP_INSTALLER_PACKAGES" != "true" ]]; then \
    dnf --assumeyes --setopt=install_weak_deps=False --setopt=ip_resolve=4 \
        --setopt=timeout=30 --setopt=retries=4 --disablerepo=fedora-cisco-openh264 install \
        bootc \
        dbus-daemon \
        dosfstools \
        dracut-live \
        dracut-config-generic \
        e2fsprogs \
        gptfdisk \
        grub2-efi-x64-cdboot \
        grub2-pc-modules \
        grub2-tools \
        polkit \
        parted \
        podman \
        shim-x64 \
        squashfs-tools \
        util-linux \
        isomd5sum \
        xorriso; \
    dnf clean all; \
    else \
        echo "Using a prebuilt local installer tool layer"; \
    fi \
    && for package in $(rpm -qa 'anaconda*'); do rpm -e --nodeps "$package" || true; done \
    && rm -rf /usr/share/anaconda /usr/libexec/anaconda /etc/anaconda /etc/anaconda.repos.d \
    && find /usr/lib/systemd /etc/systemd -name '*anaconda*' -delete \
    && rm -f /etc/systemd/system/default.target \
    && ln -s /usr/lib/systemd/system/graphical.target /etc/systemd/system/default.target \
    && mkdir -p /boot/efi /usr/lib/image-builder/bootc \
    && if compgen -G '/usr/lib/efi/*/*/EFI' >/dev/null; then cp -ra /usr/lib/efi/*/*/EFI /boot/efi; \
       elif [[ -d /usr/lib/efi/EFI ]]; then cp -ra /usr/lib/efi/EFI /boot/efi; \
       elif [[ -d /usr/lib/bootupd/updates/EFI ]]; then cp -ra /usr/lib/bootupd/updates/EFI /boot/efi; \
       else echo 'error: no UEFI vendor tree available for live media' >&2; exit 1; fi \
    && cd /usr/share/devcore-installer/payload \
    && sha256sum --strict --check SHA256SUMS \
    && test "$(wc -l < SHA256SUMS)" -eq 3 \
    && test -s payload-image.ref \
    && test -s payload-config.digest \
    && chmod 0755 /usr/bin/devcore-installer /usr/libexec/devcore/devcore-installerd \
    && systemctl enable devcore-installer.service \
    && mkdir -p /etc/systemd/system/multi-user.target.wants \
    && ln -sfn /usr/lib/systemd/system/greetd.service /etc/systemd/system/multi-user.target.wants/greetd.service \
    && ln -sfn /dev/null /etc/systemd/system/getty@tty1.service \
    && systemd-sysusers \
    && kernel="$(basename "$(dirname "$(find /usr/lib/modules -name vmlinuz -print -quit)")")" \
    && test -f "/usr/lib/modules/${kernel}/vmlinuz" \
    && DRACUT_NO_XATTR=1 dracut --force --no-hostonly --no-hostonly-cmdline \
         --modules "systemd systemd-journald dmsquash-live kernel-modules rootfs-block base fs-lib shell-interpreter" \
         --omit "multipath mdraid lvm btrfs fcoe iscsi nfs clevis clevis-pin-null clevis-pin-tpm2 clevis-pin-tang clevis-pin-sss anaconda livenet" \
         --add-drivers "overlay squashfs virtio_blk" \
         "/usr/lib/modules/${kernel}/initramfs.img" "${kernel}" \
    && if lsinitrd -m "/usr/lib/modules/${kernel}/initramfs.img" 2>/dev/null | grep -Eqi '(^|[[:space:]])anaconda([[:space:]]|$)'; then \
         echo 'error: Anaconda module remains in the live initramfs' >&2; exit 1; \
       fi
