#![deny(unsafe_code)]
#![allow(missing_docs)]
//! Native live-desktop client for `org.devcore.Installer1`.
//!
//! Slint's generated rendering glue scopes its own `allow(unsafe_code)`; this
//! crate contains no handwritten unsafe code.

use std::{
    collections::HashMap,
    error::Error,
    io::Write,
    os::{fd::OwnedFd as StdOwnedFd, unix::net::UnixStream},
    thread,
};

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use zbus::{
    blocking::{Connection, Proxy},
    zvariant::{OwnedFd, OwnedObjectPath, OwnedValue},
};

const BUS_NAME: &str = "org.devcore.Installer1";
const OBJECT_PATH: &str = "/org/devcore/Installer";
const INTERFACE: &str = "org.devcore.Installer1";

slint::slint! {
    import { Button, LineEdit, TextEdit } from "std-widgets.slint";

    component RailItem inherits Rectangle {
        in property <string> label;
        in property <int> index;
        in property <int> current;
        height: 46px;
        border-radius: 11px;
        background: root.index == root.current ? #ebf4ff : transparent;
        HorizontalLayout {
            padding-left: 15px; padding-right: 12px; spacing: 11px;
            Rectangle { width: 6px; height: 6px; border-radius: 3px; background: root.index == root.current ? #1877f2 : #c4cfde; }
            Text { text: root.label; color: root.index == root.current ? #1772d8 : #3c4a62; font-size: 13px; font-weight: root.index == root.current ? 700 : 500; vertical-alignment: center; }
        }
    }

    export component InstallerWindow inherits Window {
        title: "Install DevCore OS";
        width: 1280px;
        height: 800px;
        in-out property <int> step: 0;
        in-out property <string> status: "Welcome to the DevCore live desktop";
        in-out property <string> disk-id;
        in-out property <string> disk-summary: "Select an internal disk to continue.";
        in-out property <string> confirmation;
        in-out property <string> required-confirmation;
        in-out property <string> locale: "en_US.UTF-8";
        in-out property <string> keyboard: "us";
        in-out property <string> timezone: "UTC";
        in-out property <string> username: "developer";
        in-out property <string> hostname: "devcore";
        in-out property <string> profile: "balanced";
        in-out property <string> password;
        in-out property <string> password-confirm;
        in-out property <string> job-path;
        in-out property <string> progress: "No installation is running.";
        in-out property <bool> busy: false;
        in-out property <[string]> disk-options;
        callback discover-disks();
        callback run-preflight();
        callback next();
        callback back();
        callback start-install();
        callback refresh-status();
        callback cancel-install();

        background: #f7faff;
        VerticalLayout {
            spacing: 0px;
            Rectangle {
                height: 61px;
                background: #ffffff;
                border-radius: 0px;
                HorizontalLayout {
                    padding-left: 27px; padding-right: 28px; spacing: 11px;
                    Rectangle { width: 23px; height: 23px; border-radius: 12px; background: #dff5ff; Text { text: "D"; color: #1672df; font-size: 11px; font-weight: 800; horizontal-alignment: center; vertical-alignment: center; } }
                    Text { text: "DevCore"; color: #152033; font-size: 19px; font-weight: 800; vertical-alignment: center; }
                    Text { text: "OS"; color: #8794a8; font-size: 18px; vertical-alignment: center; }
                    Rectangle { horizontal-stretch: 1; }
                    Text { text: "LIVE DESKTOP"; color: #1877f2; font-size: 11px; font-weight: 750; vertical-alignment: center; }
                    Rectangle { width: 1px; height: 27px; background: #e7edf5; }
                    Text { text: root.busy ? "INSTALLING" : "OFFLINE READY"; color: root.busy ? #e58b17 : #28a86f; font-size: 11px; font-weight: 700; vertical-alignment: center; }
                    Text { text: "Use the desktop while installation runs"; color: #6f7e92; font-size: 12px; vertical-alignment: center; }
                }
            }
            Rectangle { height: 1px; background: #e8eef6; }
            Rectangle {
                height: 62px;
                background: #ffffff;
                HorizontalLayout {
                    padding-left: 56px; padding-right: 56px; spacing: 18px;
                    Text { text: "Welcome"; color: root.step == 0 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 0 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Disk"; color: root.step == 1 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 1 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Locale"; color: root.step == 2 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 2 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Account"; color: root.step == 2 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 2 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Password"; color: root.step == 2 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 2 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Profile"; color: root.step == 2 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 2 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Review"; color: root.step == 3 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 3 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Install"; color: root.step == 4 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 4 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Finish"; color: root.step == 4 ? #1683ff : #8b9aad; font-size: 11px; font-weight: root.step == 4 ? 700 : 400; vertical-alignment: center; }
                    Rectangle { horizontal-stretch: 1; }
                }
            }
            Rectangle { height: 1px; background: #e8eef6; }
            HorizontalLayout {
                spacing: 0px;
                Rectangle {
                    width: 218px;
                    visible: false;
                    background: #ffffff;
                    VerticalLayout {
                        padding-top: 22px; padding-left: 14px; padding-right: 14px; spacing: 7px;
                        Text { text: "INSTALL DEVCORE"; color: #9aa7b8; font-size: 10px; font-weight: 750; }
                        RailItem { label: "Welcome"; index: 0; current: root.step; }
                        RailItem { label: "Select disk"; index: 1; current: root.step; }
                        RailItem { label: "Your account"; index: 2; current: root.step; }
                        RailItem { label: "Confirm erase"; index: 3; current: root.step; }
                        RailItem { label: "Install progress"; index: 4; current: root.step; }
                        Rectangle { vertical-stretch: 1; }
                        Rectangle {
                            height: 149px; border-radius: 14px; background: #f8fbff; border-width: 1px; border-color: #e4ecf6;
                            VerticalLayout {
                                padding: 15px; spacing: 8px;
                                Text { text: "Live session"; color: #1f2b3e; font-size: 13px; font-weight: 750; }
                                Text { text: "Your USB session is separate from the selected disk."; color: #718096; font-size: 11px; wrap: word-wrap; }
                                Text { text: "• Terminal and network stay available\n• Nothing changes until you confirm\n• No Internet is required"; color: #397df4; font-size: 11px; wrap: word-wrap; }
                            }
                        }
                        Text { text: "UEFI x86-64 · ext4"; color: #9aa7b8; font-size: 10px; horizontal-alignment: center; }
                    }
                }
                Rectangle { width: 1px; background: #e8eef6; }
                Rectangle {
                    horizontal-stretch: 1; background: #f7faff;
                    VerticalLayout {
                        padding-left: 48px; padding-right: 48px; padding-top: 32px; padding-bottom: 26px; spacing: 20px;
                        Text { text: root.step == 0 ? "A live desktop, with a calmer install path" : root.step == 1 ? "Choose the disk for DevCore" : root.step == 2 ? "Make DevCore yours" : root.step == 3 ? "One last review" : "DevCore is installing in the background"; color: #121b2a; font-size: 28px; font-weight: 800; }
                        Text { text: root.step == 0 ? "Explore the live session first. When you are ready, DevCore will install the BaseOS from this USB without downloading developer tools." : root.step == 1 ? "Only eligible internal disks are shown. DevCore will erase the selected disk and create a 1 GiB EFI partition plus an ext4 system partition." : root.step == 2 ? "These settings are written to the installed system before its first reboot. Your password is transferred once through a protected file descriptor." : root.step == 3 ? "Read the target carefully. This is the only destructive step." : "You can continue using the live desktop while the installer works. Do not remove the USB drive."; color: #66758a; font-size: 14px; wrap: word-wrap; }
                        Rectangle { height: 1px; background: #e6edf6; }
                        Rectangle {
                            vertical-stretch: 1; border-radius: 17px; background: #ffffff; border-width: 1px; border-color: #e1e9f3;
                            Rectangle {
                                visible: root.step == 0; width: 100%; height: 100%;
                                VerticalLayout {
                                    padding: 36px; spacing: 22px;
                                    Rectangle { height: 108px; border-radius: 15px; background: #edf6ff; HorizontalLayout { padding: 24px; spacing: 20px; Rectangle { width: 62px; height: 62px; border-radius: 18px; background: #dceeff; Text { text: "OS"; color: #1975df; font-size: 18px; font-weight: 800; horizontal-alignment: center; vertical-alignment: center; } } VerticalLayout { spacing: 7px; Text { text: "DevCore BaseOS is ready"; color: #1c2b41; font-size: 19px; font-weight: 800; } Text { text: "The installer uses the verified image embedded on this USB. Optional developer packages stay out of the base install."; color: #617189; font-size: 12px; wrap: word-wrap; } } } }
                                    Text { text: "Before you begin"; color: #1d2b3d; font-size: 16px; font-weight: 750; }
                                    Text { text: "1. Keep your USB drive connected until the final restart.\n2. Back up the disk you plan to erase.\n3. Choose Install only when you are ready to replace its contents."; color: #52647d; font-size: 14px; }
                                    Rectangle { vertical-stretch: 1; }
                                    Text { text: root.status; color: #1877f2; font-size: 12px; wrap: word-wrap; }
                                }
                            }
                            Rectangle {
                                visible: root.step == 1; width: 100%; height: 100%;
                                VerticalLayout {
                                    padding: 36px; spacing: 15px;
                                    HorizontalLayout { Text { text: "Available internal disks"; color: #1d2b3d; font-size: 18px; font-weight: 800; } Rectangle { horizontal-stretch: 1; } Button { text: "Refresh disks"; enabled: !root.busy; clicked => { root.discover-disks(); } } }
                                    Text { text: root.disk-options.length == 0 ? "No eligible disk is loaded yet." : root.disk-options.length + " eligible disk(s) discovered. Paste or select the stable /dev/disk/by-id identifier below."; color: #748196; font-size: 12px; }
                                    LineEdit { text <=> root.disk-id; placeholder-text: "/dev/disk/by-id/..."; enabled: !root.busy; height: 50px; }
                                    Rectangle { height: 106px; border-radius: 13px; background: #f8fbff; border-width: 1px; border-color: #e4edf7; VerticalLayout { padding: 18px; spacing: 8px; Text { text: "Target review"; color: #233248; font-size: 13px; font-weight: 750; } Text { text: root.disk-summary; color: #617189; font-size: 12px; wrap: word-wrap; } } }
                                    HorizontalLayout { Rectangle { horizontal-stretch: 1; } Button { text: "Review selected disk"; enabled: !root.busy && root.disk-id != ""; clicked => { root.run-preflight(); } } }
                                    Rectangle { vertical-stretch: 1; }
                                    Text { text: root.status; color: #1877f2; font-size: 12px; wrap: word-wrap; }
                                }
                            }
                            Rectangle {
                                visible: root.step == 2; width: 100%; height: 100%;
                                HorizontalLayout {
                                    padding: 36px; spacing: 28px;
                                    Rectangle { width: 50%; VerticalLayout { spacing: 10px; Text { text: "System preferences"; color: #1d2b3d; font-size: 17px; font-weight: 800; } Text { text: "Language"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.locale; height: 42px; enabled: !root.busy; } Text { text: "Keyboard"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.keyboard; height: 42px; enabled: !root.busy; } Text { text: "Time zone"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.timezone; height: 42px; enabled: !root.busy; } Text { text: "Machine name"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.hostname; height: 42px; enabled: !root.busy; } } }
                                    Rectangle { width: 1px; background: #edf1f7; }
                                    Rectangle { horizontal-stretch: 1; VerticalLayout { spacing: 10px; Text { text: "Developer account"; color: #1d2b3d; font-size: 17px; font-weight: 800; } Text { text: "Username"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.username; height: 42px; enabled: !root.busy; } Text { text: "Password"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.password; input-type: InputType.password; height: 42px; enabled: !root.busy; } Text { text: "Confirm password"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.password-confirm; input-type: InputType.password; height: 42px; enabled: !root.busy; } Text { text: "Development profile"; color: #53637a; font-size: 11px; font-weight: 650; } LineEdit { text <=> root.profile; height: 42px; enabled: !root.busy; } Text { text: "Passwords are never written to installer logs or job state."; color: #738198; font-size: 11px; wrap: word-wrap; } } }
                                }
                            }
                            Rectangle {
                                visible: root.step == 3; width: 100%; height: 100%;
                                VerticalLayout {
                                    padding: 36px; spacing: 18px;
                                    Rectangle { height: 118px; border-radius: 14px; background: #fff7ed; border-width: 1px; border-color: #fed7aa; VerticalLayout { padding: 21px; spacing: 8px; Text { text: "This will permanently erase the selected disk"; color: #9a4d06; font-size: 17px; font-weight: 800; } Text { text: root.disk-summary; color: #9a5d1b; font-size: 12px; wrap: word-wrap; } } }
                                    Text { text: "Type exactly: " + root.required-confirmation; color: #25344a; font-size: 14px; font-weight: 650; }
                                    LineEdit { text <=> root.confirmation; placeholder-text: "ERASE …"; height: 50px; enabled: !root.busy; }
                                    Text { text: "Account: " + root.username + " · " + root.hostname + " · " + root.timezone + " · " + root.profile; color: #68788f; font-size: 12px; }
                                    Rectangle { vertical-stretch: 1; }
                                    Text { text: root.status; color: #1877f2; font-size: 12px; wrap: word-wrap; }
                                }
                            }
                            Rectangle {
                                visible: root.step == 4; width: 100%; height: 100%;
                                VerticalLayout {
                                    padding: 36px; spacing: 18px;
                                    Rectangle { height: 9px; border-radius: 5px; background: #e6edf6; Rectangle { width: root.busy ? 72% : (root.job-path != "" ? 100% : 0%); height: 100%; border-radius: 5px; background: root.busy ? #2d8cf0 : #25b77a; } }
                                    Text { text: root.progress; color: #243248; font-size: 16px; font-weight: 750; wrap: word-wrap; }
                                    Text { text: "The live session remains available. You can open a terminal, inspect networking, or return here for status."; color: #6d7d92; font-size: 13px; wrap: word-wrap; }
                                    Rectangle { height: 1px; background: #e7edf5; }
                                    Text { text: "Target job"; color: #7d8ba0; font-size: 11px; font-weight: 700; }
                                    Text { text: root.job-path == "" ? "Waiting to start" : root.job-path; color: #2c70ce; font-size: 12px; }
                                    HorizontalLayout { spacing: 10px; Button { text: "Refresh status"; enabled: root.job-path != ""; clicked => { root.refresh-status(); } } Button { text: "Cancel before disk changes"; enabled: root.busy; clicked => { root.cancel-install(); } } }
                                    Rectangle { vertical-stretch: 1; }
                                    Text { text: root.status; color: #1877f2; font-size: 12px; wrap: word-wrap; }
                                }
                            }
                        }
                        HorizontalLayout {
                            spacing: 10px;
                            Text { text: root.step < 4 ? "Installation is offline-capable; developer bundles install later." : "Do not remove USB power while finalization is running."; color: #7c8a9e; font-size: 11px; vertical-alignment: center; }
                            Rectangle { horizontal-stretch: 1; }
                            Button { visible: root.step > 0 && root.step < 4; text: "Back"; enabled: !root.busy; clicked => { root.back(); } }
                            Button { visible: root.step < 3; text: root.step == 0 ? "Begin installation" : root.step == 1 ? "Continue" : "Review erase"; enabled: !root.busy; clicked => { root.next(); } }
                            Button { visible: root.step == 3; text: root.busy ? "Starting…" : "Erase disk & install"; enabled: !root.busy && root.confirmation == root.required-confirmation && root.password != "" && root.password == root.password-confirm; clicked => { root.start-install(); } }
                        }
                    }
                }
            }
        }
    }
}

type Dictionary = HashMap<String, OwnedValue>;

fn with_installer_proxy<T>(
    operation: impl FnOnce(&Proxy<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let connection =
        Connection::system().map_err(|error| format!("system D-Bus unavailable: {error}"))?;
    let proxy = Proxy::new(&connection, BUS_NAME, OBJECT_PATH, INTERFACE)
        .map_err(|error| format!("installer service unavailable: {error}"))?;
    operation(&proxy)
}

fn value_string(dictionary: &Dictionary, key: &str) -> String {
    dictionary
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .unwrap_or("")
        .to_owned()
}

fn value_u64(dictionary: &Dictionary, key: &str) -> u64 {
    dictionary
        .get(key)
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(0)
}

fn value_bool(dictionary: &Dictionary, key: &str) -> bool {
    dictionary
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn string_value(value: impl Into<String>) -> OwnedValue {
    zbus::zvariant::Str::from(value.into()).into()
}

fn make_configuration(window: &InstallerWindow) -> Dictionary {
    let mut configuration = Dictionary::new();
    for (key, value) in [
        ("locale", window.get_locale().to_string()),
        ("keyboard", window.get_keyboard().to_string()),
        ("timezone", window.get_timezone().to_string()),
        ("username", window.get_username().to_string()),
        ("hostname", window.get_hostname().to_string()),
        ("profile", window.get_profile().to_string()),
    ] {
        configuration.insert(key.to_owned(), string_value(value));
    }
    configuration
}

fn load_disks(window: slint::Weak<InstallerWindow>) {
    thread::spawn(move || {
        let result = (|| -> Result<(Vec<SharedString>, String, String), String> {
            let disks: Vec<Dictionary> = with_installer_proxy(|proxy| {
                proxy
                    .call("ListDisks", &())
                    .map_err(|error| format!("disk inventory failed: {error}"))
            })?;
            let options: Vec<SharedString> = disks
                .iter()
                .map(|disk| SharedString::from(value_string(disk, "id")))
                .collect();
            let first = disks.first().ok_or_else(|| "No eligible internal disk was found. Connect a non-removable disk of at least 32 GiB.".to_owned())?;
            Ok((
                options,
                value_string(first, "id"),
                format!(
                    "{} · {} GiB · {}",
                    value_string(first, "model"),
                    value_u64(first, "size_bytes") / 1024 / 1024 / 1024,
                    value_string(first, "device")
                ),
            ))
        })();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = window.upgrade() {
                match result {
                    Ok((options, disk, summary)) => {
                        window.set_disk_options(ModelRc::new(VecModel::from(options)));
                        window.set_disk_id(disk.into());
                        window.set_disk_summary(summary.into());
                        window.set_status(
                            "Eligible disks loaded. Review the selected target before continuing."
                                .into(),
                        );
                    }
                    Err(error) => window.set_status(error.into()),
                }
            }
        });
    });
}

fn preflight(window: slint::Weak<InstallerWindow>) {
    let disk_id = window
        .upgrade()
        .map(|window| window.get_disk_id().to_string())
        .unwrap_or_default();
    thread::spawn(move || {
        let result = (|| -> Result<(String, String), String> {
            let (phrase, review): (String, Dictionary) = with_installer_proxy(|proxy| {
                proxy
                    .call("Preflight", &(disk_id))
                    .map_err(|error| format!("disk preflight failed: {error}"))
            })?;
            Ok((
                phrase,
                format!(
                    "{} · {} GiB · {}",
                    value_string(&review, "model"),
                    value_u64(&review, "size_bytes") / 1024 / 1024 / 1024,
                    value_string(&review, "device")
                ),
            ))
        })();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = window.upgrade() {
                match result {
                    Ok((phrase, summary)) => {
                        window.set_required_confirmation(phrase.into());
                        window.set_disk_summary(summary.into());
                        window.set_status(
                            "Disk identity revalidated. Continue to account setup.".into(),
                        );
                        window.set_step(2);
                    }
                    Err(error) => window.set_status(error.into()),
                }
            }
        });
    });
}

fn start_install(window: slint::Weak<InstallerWindow>) {
    let Some(source) = window.upgrade() else {
        return;
    };
    let confirmation = source.get_confirmation().to_string();
    let configuration = make_configuration(&source);
    let mut password = source.get_password().as_bytes().to_vec();
    source.set_password("".into());
    source.set_password_confirm("".into());
    source.set_busy(true);
    source.set_status("Starting protected installer job…".into());
    thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let (mut writer, reader) = UnixStream::pair()
                .map_err(|error| format!("cannot create protected password channel: {error}"))?;
            writer
                .write_all(&password)
                .map_err(|error| format!("cannot transfer password: {error}"))?;
            drop(writer);
            let fd = OwnedFd::from(StdOwnedFd::from(reader));
            let job: OwnedObjectPath = with_installer_proxy(|proxy| {
                proxy
                    .call("Start", &(confirmation, configuration, fd))
                    .map_err(|error| format!("installer did not start: {error}"))
            })?;
            Ok(job.to_string())
        })();
        password.fill(0);
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = window.upgrade() {
                match result {
                    Ok(job) => {
                        window.set_job_path(job.into());
                        window.set_progress("Payload verification has started. You can continue using the live desktop.".into());
                        window.set_status("Installer job accepted.".into());
                        window.set_step(4);
                    }
                    Err(error) => {
                        window.set_busy(false);
                        window.set_status(error.into());
                    }
                }
            }
        });
    });
}

fn refresh_status(window: slint::Weak<InstallerWindow>) {
    let job = window
        .upgrade()
        .map(|window| window.get_job_path().to_string())
        .unwrap_or_default();
    thread::spawn(move || {
        let result = (|| -> Result<(String, bool, String), String> {
            let path = OwnedObjectPath::try_from(job)
                .map_err(|error| format!("invalid job path: {error}"))?;
            let state: Dictionary = with_installer_proxy(|proxy| {
                proxy
                    .call("Status", &(path))
                    .map_err(|error| format!("cannot read installer status: {error}"))
            })?;
            Ok((
                value_string(&state, "state"),
                value_bool(&state, "cancellable"),
                value_string(&state, "diagnostic"),
            ))
        })();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = window.upgrade() {
                match result {
                    Ok((state, cancellable, diagnostic)) => {
                        window.set_busy(cancellable || state == "preparing");
                        window.set_progress(
                            (if diagnostic.is_empty() {
                                format!("Installer state: {state}")
                            } else {
                                format!("Installer state: {state} · {diagnostic}")
                            })
                            .into(),
                        );
                        window.set_status(
                            "Status refreshed from the protected installer service.".into(),
                        );
                    }
                    Err(error) => window.set_status(error.into()),
                }
            }
        });
    });
}

fn cancel_install(window: slint::Weak<InstallerWindow>) {
    let job = window
        .upgrade()
        .map(|window| window.get_job_path().to_string())
        .unwrap_or_default();
    thread::spawn(move || {
        let result = (|| -> Result<bool, String> {
            let path = OwnedObjectPath::try_from(job)
                .map_err(|error| format!("invalid job path: {error}"))?;
            with_installer_proxy(|proxy| {
                proxy
                    .call("Cancel", &(path))
                    .map_err(|error| format!("cannot cancel installer: {error}"))
            })
        })();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(window) = window.upgrade() {
                match result {
                    Ok(true) => {
                        window.set_busy(false);
                        window.set_progress("Installation cancelled before disk changes.".into());
                    }
                    Ok(false) => window.set_progress(
                        "Cancellation is unavailable after destructive storage begins.".into(),
                    ),
                    Err(error) => window.set_status(error.into()),
                }
            }
        });
    });
}

fn main() -> Result<(), Box<dyn Error>> {
    let window = InstallerWindow::new()?;
    window.on_discover_disks({
        let weak = window.as_weak();
        move || load_disks(weak.clone())
    });
    window.on_run_preflight({
        let weak = window.as_weak();
        move || preflight(weak.clone())
    });
    window.on_next({
        let weak = window.as_weak();
        move || {
            if let Some(window) = weak.upgrade() {
                match window.get_step() {
                    0 => window.set_step(1),
                    1 => preflight(weak.clone()),
                    2 => window.set_step(3),
                    _ => {}
                }
            }
        }
    });
    window.on_back({
        let weak = window.as_weak();
        move || {
            if let Some(window) = weak.upgrade()
                && window.get_step() > 0
            {
                window.set_step(window.get_step() - 1);
            }
        }
    });
    window.on_start_install({
        let weak = window.as_weak();
        move || start_install(weak.clone())
    });
    window.on_refresh_status({
        let weak = window.as_weak();
        move || refresh_status(weak.clone())
    });
    window.on_cancel_install({
        let weak = window.as_weak();
        move || cancel_install(weak.clone())
    });
    window.run()?;
    Ok(())
}
