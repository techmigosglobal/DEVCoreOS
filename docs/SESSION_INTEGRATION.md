# DevCore Session Integration

`devcore-session` is the login-session boundary for the native greeter.
It speaks the greetd length-prefixed JSON protocol over `GREETD_SOCK`; greetd
continues to own PAM authentication, account checks, privilege transitions,
seat setup, and session creation.

The crate deliberately does not store or interpret passwords. It exposes
opaque authentication-message kinds so a UI can hide secret responses and
forwards each response once to greetd. After a successful conversation it
requests the direct DevCore compositor command with non-secret Wayland session
environment entries.

The graphical greeter is packaged behind the native compositor: the trusted
greetd default session starts `devcore-compositor --backend drm` with
`devcore-greeter` as its Wayland child. After PAM success, the greeter asks
greetd to start the authenticated compositor/shell session. The expanded image
context contains this configuration and direct launch contract. The session
contract is tested with an in-memory UNIX socket, and the native compositor is
compile-tested in an isolated Linux container; live PAM/logind, seat startup,
and guest graphical-session behavior remain unverified.

## First boot

Before login, the greeter queries `org.devcore.FirstBoot1`. If setup is
required, it presents native language, keyboard, timezone, account, hostname,
hardware-summary, and development-profile steps. The final submission runs on
a short-lived worker and sends the password only as a D-Bus method argument to
the root-owned service; the service forwards it through a pipe to the existing
`chpasswd` tool and does not write it to the completion file, logs, or argv.

The service is D-Bus activated only while `/var/lib/devcore/first-boot.conf`
does not exist. Its completion record is atomically published after locale,
keyboard, timezone, hostname, account, and password setup all succeed. A
partial system-tool failure can leave an account or other setting applied, so
recovery must be handled by a later guest-tested repair flow rather than an
implicit destructive rollback. The repository currently has a disposable
session-bus status smoke check; it deliberately does not invoke root-level
setup commands on the development workstation.
