#!/usr/bin/env bash
set -Eeuo pipefail

container_engine="${CONTAINER_ENGINE:-docker}"
image_ref="${DEVCORE_IMAGE_REF:-localhost/devcore-os:alpha}"

if ! command -v "$container_engine" >/dev/null 2>&1; then
    printf 'error: required container engine not found: %s\n' "$container_engine" >&2
    exit 1
fi

image_id="$($container_engine image inspect "$image_ref" --format '{{.Id}}')"
architecture="$($container_engine image inspect "$image_ref" --format '{{.Architecture}}')"
operating_system="$($container_engine image inspect "$image_ref" --format '{{.Os}}')"
[[ "$architecture" == "amd64" ]] || {
    printf 'error: image architecture is %s, expected amd64\n' "$architecture" >&2
    exit 1
}
[[ "$operating_system" == "linux" ]] || {
    printf 'error: image OS is %s, expected linux\n' "$operating_system" >&2
    exit 1
}

for path in \
    /usr/libexec/devcore/devcored \
    /usr/libexec/devcore/devcore-workd \
    /usr/libexec/devcore/devcore-provisiond \
    /usr/bin/devcore-shell \
    /usr/bin/devcore-greeter \
    /usr/bin/devcore-compositor \
    /usr/bin/flatpak \
    /etc/greetd/config.toml \
    /usr/lib/systemd/system/devcored.service \
    /usr/share/dbus-1/system.d/org.devcore.Resource1.conf \
    /usr/share/dbus-1/system-services/org.devcore.Resource1.service \
    /usr/share/dbus-1/services/org.devcore.Work1.service \
    /usr/share/dbus-1/services/org.devcore.Provision1.service; do
    if ! "$container_engine" run --rm --entrypoint /usr/bin/test "$image_ref" -e "$path"; then
        printf 'error: OCI artifact is missing required path: %s\n' "$path" >&2
        exit 1
    fi
done

snapshot="$($container_engine run --rm --entrypoint /usr/libexec/devcore/devcored "$image_ref" --json)"
grep -Fq '"profile":"' <<<"$snapshot"
grep -Fq '"pressure":' <<<"$snapshot"
[[ "$($container_engine run --rm --entrypoint /usr/bin/systemctl "$image_ref" is-enabled devcored.service)" == "enabled" ]]

printf 'OCI artifact verified: %s (%s)\n' "$image_ref" "$image_id"
