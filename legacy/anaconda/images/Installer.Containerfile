# Graphical, offline-capable DevCore BaseOS installer environment.
#
# The BaseOS payload is supplied at build time through DEVCORE_BASE_IMAGE and
# embedded by the unified image-builder `bootc-installer` image type. This
# environment contains only installer infrastructure; it must not contain
# optional developer SDKs or third-party desktop applications.
ARG DEVCORE_BASE_IMAGE
FROM ${DEVCORE_BASE_IMAGE}

COPY etc/anaconda/interactive-defaults.ks /usr/share/anaconda/interactive-defaults.ks
COPY usr/lib/image-builder/bootc/iso.yaml /usr/lib/image-builder/bootc/iso.yaml

RUN dnf --assumeyes --setopt=install_weak_deps=False install \
        anaconda \
        anaconda-install-img-deps \
        anaconda-dracut \
        dracut-config-generic \
        dracut-network \
        net-tools \
        grub2-efi-x64-cdboot \
        grub2-pc-modules \
        shim-x64 \
        plymouth \
        default-fonts-core-sans \
        default-fonts-other-sans \
        google-noto-sans-cjk-fonts \
        jq \
        squashfs-tools \
        xorriso \
    && dnf clean all \
    && mkdir -p /boot/efi /var/mnt /usr/lib/image-builder/bootc \
    && if compgen -G '/usr/lib/efi/*/*/EFI' >/dev/null; then cp -ra /usr/lib/efi/*/*/EFI /boot/efi; \
       elif [[ -d /usr/lib/efi/EFI ]]; then cp -ra /usr/lib/efi/EFI /boot/efi; \
       elif [[ -d /usr/lib/bootupd/updates/EFI ]]; then cp -ra /usr/lib/bootupd/updates/EFI /boot/efi; \
       else echo 'warning: EFI vendor tree not present in installer payload'; fi \
    && printf 'install:x:0:0:root:/root:/usr/libexec/anaconda/run-anaconda\n' >> /etc/passwd \
    && printf 'install::14438:0:99999:7:::\n' >> /etc/shadow \
    && passwd -d root \
    && mv /usr/share/anaconda/list-harddrives-stub /usr/bin/list-harddrives \
    && mv /etc/yum.repos.d /etc/anaconda.repos.d \
    && ln -sf /usr/lib/systemd/system/anaconda.target /etc/systemd/system/default.target \
    && rm -f /usr/lib/systemd/system-generators/systemd-gpt-auto-generator \
    && ln -sf /usr/lib/systemd/system/anaconda-shell@.service /usr/lib/systemd/system/autovt@.service \
    && mkdir -p /usr/lib/systemd/logind.conf.d \
    && printf '[Login]\nReserveVT=2\n' > /usr/lib/systemd/logind.conf.d/anaconda-shell.conf \
    && kernel="$(kernel-install list --json pretty | jq -r '.[] | select(.has_kernel == true) | .version')" \
    && DRACUT_NO_XATTR=1 dracut --force --zstd --no-hostonly \
        --add anaconda \
        "/usr/lib/modules/${kernel}/initramfs.img" "${kernel}" \
    && mkdir -p /etc/systemd/user/pipewire.service.d /etc/systemd/user/pipewire.socket.d \
    && printf '[Unit]\nConditionUser=\n' > /etc/systemd/user/pipewire.service.d/allowroot.conf \
    && printf '[Unit]\nConditionUser=\n' > /etc/systemd/user/pipewire.socket.d/allowroot.conf

ARG DEVCORE_PAYLOAD_REF=localhost/devcore-os:alpha-base
RUN printf 'bootc --source-imgref registry:%s --target-imgref %s\n' "$DEVCORE_PAYLOAD_REF" "$DEVCORE_PAYLOAD_REF" >> /usr/share/anaconda/interactive-defaults.ks
