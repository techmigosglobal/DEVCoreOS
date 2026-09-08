# DevCore Compositor

`apps/devcore-compositor` is the compositor boundary. Its default build uses
Smithay 0.7 with only the Winit, GLES, and Wayland frontend features needed for
a small nested development backend. The opt-in `native-drm` feature adds the
libseat, udev, DRM, GBM, libinput, and multi-GPU feature set required for a
physical seat. Both paths own the Wayland server, XDG toplevel configuration,
SHM buffer commits, seat keyboard, client connections, surface-tree rendering,
frame callbacks, and the shell launch boundary.

The binary accepts:

```text
devcore-compositor [--backend nested|drm] [--shell ABSOLUTE_PATH] [--socket WAYLAND_NAME]
```

When `--shell` is supplied, it is started directly with `WAYLAND_DISPLAY` set
to the compositor's private socket. No shell command string is interpreted,
and the child is terminated with the compositor lifecycle. Socket names and
shell paths are validated before startup.

The compositor keeps a bounded four-workspace registry for XDG toplevels. New
windows are assigned to the active workspace, the latest live window is the
focused surface, and `Super+1` through `Super+4` switch workspaces in both
nested and native input paths. Visible windows use a deterministic 32-pixel
stack offset so this behavior is testable without starting a graphical
session. Pointer hit-testing, drag/resize interactions, decorations, layer
surfaces, Xwayland, notifications, and persistent shell-to-compositor state
are still open work.

The default `nested` backend is safe for local UI development because Winit
nests inside an existing graphical session. The `drm` backend is a physical
seat path and must be built with `--features native-drm`; it uses libseat for
device ownership, scans the active seat's first connected connector, initializes
one GBM-backed output, handles libinput keyboard events, and renders the
Wayland surface tree through the DRM compositor. Connector hotplug, multiple
outputs, richer window management, greetd packaging, and disposable guest
boot evidence remain separate gates. No backend is boot evidence by itself.
