# Native, offline-capable DevCore OS live-installer image.
# The build script supplies a digest-pinned DevCore BaseOS image and an OCI
# archive of that exact local image. No network registry is used at install time.
ARG DEVCORE_BASE_IMAGE
FROM ${DEVCORE_BASE_IMAGE}

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
COPY usr/share/devcore-installer/payload/devcore-baseos.oci.tar /usr/share/devcore-installer/payload/devcore-baseos.oci.tar
COPY usr/share/devcore-installer/payload/SHA256SUMS /usr/share/devcore-installer/payload/SHA256SUMS
COPY usr/share/devcore-installer/payload/payload-image.ref /usr/share/devcore-installer/payload/payload-image.ref

RUN dnf --assumeyes --setopt=install_weak_deps=False install \
        bootc \
        dbus-daemon \
        dosfstools \
        e2fsprogs \
        gptfdisk \
        grub2-efi-x64-cdboot \
        grub2-pc-modules \
        grub2-tools \
        polkit \
        podman \
        shim-x64 \
        squashfs-tools \
        util-linux \
        xorriso \
    && dnf clean all \
    && mkdir -p /boot/efi /usr/lib/image-builder/bootc \
    && if compgen -G '/usr/lib/efi/*/*/EFI' >/dev/null; then cp -ra /usr/lib/efi/*/*/EFI /boot/efi; \
       elif [[ -d /usr/lib/efi/EFI ]]; then cp -ra /usr/lib/efi/EFI /boot/efi; \
       elif [[ -d /usr/lib/bootupd/updates/EFI ]]; then cp -ra /usr/lib/bootupd/updates/EFI /boot/efi; \
       else echo 'error: no UEFI vendor tree available for live media' >&2; exit 1; fi \
    && cd /usr/share/devcore-installer/payload \
    && sha256sum --strict --check SHA256SUMS \
    && test "$(wc -l < SHA256SUMS)" -eq 1 \
    && test -s payload-image.ref \
    && chmod 0755 /usr/bin/devcore-installer /usr/libexec/devcore/devcore-installerd \
    && systemctl enable devcore-installer.service
