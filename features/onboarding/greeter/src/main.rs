#![deny(unsafe_code)]
#![allow(missing_docs)]
//! Native DevCore greeter front end.
//!
//! Authentication is delegated to greetd and its PAM configuration through
//! `devcore-session`. This process never stores credentials beyond the current
//! UI response and never implements password verification itself.

use std::{error::Error, thread};

use devcore_core::collect_snapshot;
use devcore_session::{
    AuthMessageKind, GreetdResponse, GreetdSession, SessionError, SessionLaunchSpec,
};
use slint::ComponentHandle;
use zbus::blocking::Connection;

slint::slint! {
    import { Button, LineEdit } from "std-widgets.slint";

    export component GreeterWindow inherits Window {
        title: "DevCore OS";
        width: 960px;
        height: 640px;
        in-out property <string> username;
        in-out property <string> response;
        in-out property <string> prompt: "Sign in to DevCore";
        in-out property <string> status-message: "Authentication is handled by PAM";
        in-out property <bool> auth-visible: false;
        in-out property <bool> response-required: false;
        in-out property <bool> response-secret: false;
        in-out property <bool> busy: false;
        callback submit();
        callback cancel();

        background: #09121f;

        HorizontalLayout {
            padding: 36px;
            spacing: 34px;
            Rectangle {
                width: 330px;
                border-radius: 24px;
                background: #102238;
                VerticalLayout {
                    padding: 28px;
                    spacing: 14px;
                    Text { text: "◇"; color: #22c7f2; font-size: 64px; }
                    Text { text: "DevCore"; color: #f5f8fc; font-size: 30px; font-weight: 800; }
                    Text { text: "A focused developer OS"; color: #aab8ca; font-size: 15px; }
                    Rectangle { vertical-stretch: 1; }
                    Text { text: "Wayland native  ·  PAM backed"; color: #24c7d5; font-size: 12px; }
                    Text { text: "Low-resource by design"; color: #8190a4; font-size: 12px; }
                }
            }
            Rectangle {
                horizontal-stretch: 1;
                VerticalLayout {
                    spacing: 16px;
                    Rectangle { vertical-stretch: 1; }
                    Text {
                        text: root.prompt;
                        color: #f5f8fc;
                        font-size: 28px;
                        font-weight: 750;
                        wrap: word-wrap;
                    }
                    Text {
                        text: root.auth-visible ? (root.response-required ? "Authentication response" : "Continue") : "Username";
                        color: #aab8ca;
                        font-size: 13px;
                    }
                    LineEdit {
                        visible: !root.auth-visible;
                        enabled: !root.busy;
                        text <=> root.username;
                        placeholder-text: "Developer username";
                        height: 54px;
                    }
                    LineEdit {
                        visible: root.auth-visible && root.response-required;
                        enabled: !root.busy;
                        text <=> root.response;
                        input-type: root.response-secret ? password : text;
                        placeholder-text: "Response";
                        height: 54px;
                    }
                    Text {
                        text: root.status-message;
                        color: #8190a4;
                        font-size: 12px;
                        wrap: word-wrap;
                    }
                    HorizontalLayout {
                        spacing: 12px;
                        Button {
                            enabled: !root.busy;
                            text: root.auth-visible ? "Continue" : "Sign in";
                            clicked => { root.submit(); }
                        }
                        Button {
                            enabled: !root.busy;
                            text: "Cancel";
                            clicked => { root.cancel(); }
                        }
                    }
                    Text {
                        text: "Passwords are handled by the system PAM stack and are never stored by DevCore.";
                        color: #637187;
                        font-size: 11px;
                        wrap: word-wrap;
                    }
                    Rectangle { vertical-stretch: 1; }
                }
            }
        }
    }

    export component FirstBootWindow inherits Window {
        title: "DevCore first boot";
        width: 1280px;
        height: 800px;
        in-out property <int> step: 0;
        in-out property <string> language: "en_US.UTF-8";
        in-out property <string> keyboard: "us";
        in-out property <string> timezone: "UTC";
        in-out property <string> username: "developer";
        in-out property <string> hostname: "devcore";
        in-out property <string> profile: "balanced";
        in-out property <string> account-password;
        in-out property <string> account-password-confirm;
        in-out property <string> hardware-readout;
        in-out property <string> status-message: "Configure DevCore for this machine";
        in-out property <bool> busy: false;
        callback next();
        callback back();
        callback cycle-profile();
        callback finish();

        // This follows the supplied DevCore onboarding reference: a quiet,
        // light canvas, compact progress rail, and one focused card per step.
        // It intentionally uses the existing native Slint software renderer.
        background: #f6f9fd;

        VerticalLayout {
            spacing: 0px;
            Rectangle {
                height: 72px;
                background: #ffffff;
                HorizontalLayout {
                    padding-left: 32px;
                    padding-right: 32px;
                    spacing: 10px;
                    Text { text: "◇"; color: #239cf3; font-size: 27px; vertical-alignment: center; }
                    Text { text: "DevCore"; color: #101827; font-size: 20px; font-weight: 800; vertical-alignment: center; }
                    Text { text: "OS"; color: #718096; font-size: 19px; vertical-alignment: center; }
                    Rectangle { horizontal-stretch: 1; }
                    Text {
                        text: "Setup · " + (root.step + 1) + " of 8";
                        color: #397df4;
                        font-size: 13px;
                        font-weight: 650;
                        vertical-alignment: center;
                    }
                }
            }
            Rectangle {
                height: 62px;
                background: #fbfdff;
                HorizontalLayout {
                    padding-left: 52px;
                    padding-right: 52px;
                    spacing: 18px;
                    Text { text: "Welcome"; color: root.step == 0 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 0 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Language"; color: root.step == 1 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 1 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Keyboard"; color: root.step == 2 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 2 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Time zone"; color: root.step == 3 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 3 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Account"; color: root.step == 4 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 4 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Password"; color: root.step == 5 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 5 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Hardware"; color: root.step == 6 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 6 ? 700 : 400; vertical-alignment: center; }
                    Text { text: "Profile"; color: root.step == 7 ? #287cf1 : #8b9aad; font-size: 11px; font-weight: root.step == 7 ? 700 : 400; vertical-alignment: center; }
                    Rectangle { horizontal-stretch: 1; }
                }
            }
            Rectangle { height: 1px; background: #e5edf6; }
            Rectangle {
                vertical-stretch: 1;
                background: #f6f9fd;
                HorizontalLayout {
                    padding: 48px;
                    spacing: 52px;
                    Rectangle {
                        width: 390px;
                        border-radius: 28px;
                        background: #eaf5ff;
                        VerticalLayout {
                            padding: 42px;
                            spacing: 18px;
                            Image {
                                source: @image-url("assets/setup-illustration.png");
                                height: 214px;
                                width: 100%;
                                image-fit: contain;
                            }
                            Text { text: "DevCore setup"; color: #15253a; font-size: 29px; font-weight: 800; }
                            Text { text: "A focused development environment that adapts to your hardware without carrying unnecessary software."; color: #5d7088; font-size: 15px; wrap: word-wrap; }
                            Rectangle { height: 1px; background: #cde4f7; }
                            Text { text: "• Native Wayland desktop"; color: #287cf1; font-size: 14px; }
                            Text { text: "• Resource-aware workloads"; color: #287cf1; font-size: 14px; }
                            Text { text: "• Optional tools download later"; color: #287cf1; font-size: 14px; }
                            Rectangle { vertical-stretch: 1; }
                            Text { text: "You can change most choices later."; color: #718096; font-size: 12px; }
                        }
                    }
                    Rectangle {
                        horizontal-stretch: 1;
                        border-radius: 20px;
                        border-width: 1px;
                        border-color: #dce7f2;
                        background: #ffffff;
                        Rectangle {
                            visible: root.step == 0;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 16px;
                                Text { text: "Welcome to DevCore OS"; color: #101827; font-size: 29px; font-weight: 800; }
                                Text { text: "Set up a calm, fast development workspace. The BaseOS is ready; developer tools stay optional and download only on this computer after you connect."; color: #5d6b80; font-size: 15px; wrap: word-wrap; }
                                Rectangle { vertical-stretch: 1; }
                                Text { text: root.status-message; color: #287cf1; font-size: 13px; wrap: word-wrap; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 1;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 14px;
                                Text { text: "Choose your language"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "This controls the language used by DevCore and your terminals."; color: #5d6b80; font-size: 14px; }
                                Text { text: "Language"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.language; enabled: !root.busy; placeholder-text: "en_US.UTF-8"; height: 48px; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 2;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 14px;
                                Text { text: "Choose your keyboard layout"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "Pick the layout that matches your physical keyboard."; color: #5d6b80; font-size: 14px; }
                                Text { text: "Keyboard layout"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.keyboard; enabled: !root.busy; placeholder-text: "us"; height: 48px; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 3;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 14px;
                                Text { text: "Select your time zone"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "Keep the system clock, commits, and scheduled tasks accurate."; color: #5d6b80; font-size: 14px; }
                                Text { text: "Time zone"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.timezone; enabled: !root.busy; placeholder-text: "UTC"; height: 48px; }
                                Text { text: "Device name"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.hostname; enabled: !root.busy; placeholder-text: "devcore"; height: 48px; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 4;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 14px;
                                Text { text: "Create your user account"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "This account owns your projects and rootless development environments."; color: #5d6b80; font-size: 14px; wrap: word-wrap; }
                                Text { text: "Username"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.username; enabled: !root.busy; placeholder-text: "developer"; height: 48px; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 5;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 14px;
                                Text { text: "Set your password"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "DevCore forwards this once to the system PAM-backed account setup. It is not written to a DevCore file."; color: #5d6b80; font-size: 14px; wrap: word-wrap; }
                                Text { text: "Password"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.account-password; enabled: !root.busy; input-type: password; placeholder-text: "At least 8 characters"; height: 46px; }
                                Text { text: "Confirm password"; color: #334155; font-size: 12px; font-weight: 650; }
                                LineEdit { text <=> root.account-password-confirm; enabled: !root.busy; input-type: password; placeholder-text: "Repeat password"; height: 46px; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 6;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 16px;
                                Text { text: "Hardware analysis"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "DevCore starts with conservative resource limits and adapts when the system experiences pressure."; color: #5d6b80; font-size: 14px; wrap: word-wrap; }
                                Rectangle { height: 1px; background: #e5edf6; }
                                Text { text: root.hardware-readout; color: #287cf1; font-size: 15px; wrap: word-wrap; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                        Rectangle {
                            visible: root.step == 7;
                            width: 100%; height: 100%;
                            VerticalLayout {
                                padding: 48px; spacing: 16px;
                                Text { text: "Choose your development profile"; color: #101827; font-size: 28px; font-weight: 800; }
                                Text { text: "Profile defaults are conservative. PSI and battery state can reduce background work when responsiveness matters."; color: #5d6b80; font-size: 14px; wrap: word-wrap; }
                                Rectangle { height: 92px; border-radius: 14px; border-width: 1px; border-color: #bfd9fb; background: #f3f8ff; HorizontalLayout { padding: 20px; Text { text: "Profile: " + root.profile; color: #1d4ed8; font-size: 20px; font-weight: 750; vertical-alignment: center; } Rectangle { horizontal-stretch: 1; } Button { text: "Change profile"; enabled: !root.busy; clicked => { root.cycle-profile(); } } } }
                                Text { text: "Review: " + root.username + " on " + root.hostname + " · " + root.timezone; color: #5d6b80; font-size: 13px; wrap: word-wrap; }
                                Text { text: root.status-message; color: #287cf1; font-size: 13px; wrap: word-wrap; }
                                Rectangle { vertical-stretch: 1; }
                            }
                        }
                    }
                }
            }
            Rectangle { height: 1px; background: #e5edf6; }
            Rectangle {
                height: 82px;
                background: #ffffff;
                HorizontalLayout {
                    padding-left: 48px; padding-right: 48px; spacing: 12px;
                    Text { text: "Need help? You can return to these preferences later."; color: #718096; font-size: 12px; vertical-alignment: center; }
                    Rectangle { horizontal-stretch: 1; }
                    Button { visible: root.step > 0; enabled: !root.busy; text: "Back"; clicked => { root.back(); } }
                    Button { visible: root.step < 7; enabled: !root.busy; text: "Next"; clicked => { root.next(); } }
                    Button { visible: root.step == 7; enabled: !root.busy; text: root.busy ? "Applying…" : "Finish"; clicked => { root.finish(); } }
                }
            }
        }
    }
}

#[derive(Debug)]
struct LoginController {
    session: Option<GreetdSession>,
    launch: SessionLaunchSpec,
}

impl LoginController {
    fn new() -> Self {
        Self {
            session: None,
            launch: SessionLaunchSpec::devcore(),
        }
    }

    fn submit(
        &mut self,
        username: &str,
        response: Option<String>,
    ) -> Result<LoginOutcome, SessionError> {
        let reply = if let Some(session) = self.session.as_mut() {
            session.post_auth_message_response(response)?
        } else {
            if username.trim().is_empty() {
                return Ok(LoginOutcome::Failure("Username is required".to_owned()));
            }
            let mut session = GreetdSession::connect_from_environment()?;
            let reply = session.create_session(username.to_owned())?;
            self.session = Some(session);
            reply
        };

        self.process_reply(reply)
    }

    fn process_reply(&mut self, reply: GreetdResponse) -> Result<LoginOutcome, SessionError> {
        match reply {
            GreetdResponse::Success => {
                let session = self
                    .session
                    .as_mut()
                    .ok_or_else(|| SessionError::InvalidSocket("missing greetd session".into()))?;
                match session.start_session(&self.launch)? {
                    GreetdResponse::Success => Ok(LoginOutcome::Started),
                    GreetdResponse::AuthMessage { kind, message } => {
                        Ok(LoginOutcome::prompt(kind, message))
                    }
                    GreetdResponse::Error { description, .. } => {
                        Ok(LoginOutcome::Failure(description))
                    }
                }
            }
            GreetdResponse::AuthMessage { kind, message } => {
                Ok(LoginOutcome::prompt(kind, message))
            }
            GreetdResponse::Error { description, .. } => {
                self.session = None;
                Ok(LoginOutcome::Failure(description))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LoginOutcome {
    Prompt {
        kind: AuthMessageKind,
        message: String,
        response_required: bool,
    },
    Started,
    Failure(String),
}

impl LoginOutcome {
    fn prompt(kind: AuthMessageKind, message: String) -> Self {
        Self::Prompt {
            response_required: matches!(kind, AuthMessageKind::Visible | AuthMessageKind::Secret),
            kind,
            message,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FirstBootInfo {
    suggested_profile: String,
}

#[derive(Debug)]
struct FirstBootClient {
    connection: Connection,
}

impl FirstBootClient {
    fn connect() -> Result<Self, String> {
        Connection::system()
            .map(|connection| Self { connection })
            .map_err(|error| format!("first-boot system-bus connection failed: {error}"))
    }

    fn proxy(&self) -> Result<zbus::blocking::Proxy<'_>, String> {
        zbus::blocking::Proxy::new(
            &self.connection,
            "org.devcore.FirstBoot1",
            "/org/devcore/FirstBoot",
            "org.devcore.FirstBoot1",
        )
        .map_err(|error| format!("first-boot service proxy failed: {error}"))
    }

    fn status(&self) -> Result<(bool, String), String> {
        self.proxy()?
            .call("Status", &())
            .map_err(|error| format!("first-boot status failed: {error}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        language: &str,
        keyboard: &str,
        timezone: &str,
        username: &str,
        hostname: &str,
        profile: &str,
        password: &str,
    ) -> Result<(), String> {
        let _: () = self
            .proxy()?
            .call(
                "Apply",
                &(
                    language, keyboard, timezone, username, hostname, profile, password,
                ),
            )
            .map_err(|error| format!("first-boot apply failed: {error}"))?;
        Ok(())
    }
}

fn first_boot_status() -> Result<Option<FirstBootInfo>, String> {
    let client = FirstBootClient::connect()?;
    let (required, suggested_profile) = client.status()?;
    if required {
        Ok(Some(FirstBootInfo { suggested_profile }))
    } else {
        Ok(None)
    }
}

fn next_profile(profile: &str) -> &'static str {
    match profile {
        "low" => "balanced",
        "balanced" => "standard",
        "standard" => "workstation",
        _ => "low",
    }
}

fn run_first_boot(info: FirstBootInfo) -> Result<(), Box<dyn Error>> {
    let window = FirstBootWindow::new()?;
    window.set_profile(info.suggested_profile.into());
    let hardware_readout = match collect_snapshot() {
        Ok(snapshot) => format!(
            "Detected {} logical CPUs · {} MiB memory · suggested {} profile",
            snapshot.capacity.logical_cpus,
            snapshot.capacity.memory_total_kib / 1024,
            snapshot.initial_profile.as_str(),
        ),
        Err(error) => format!("Hardware analysis unavailable: {error}"),
    };
    window.set_hardware_readout(hardware_readout.into());

    let next_window = window.as_weak();
    window.on_next(move || {
        if let Some(window) = next_window.upgrade() {
            window.set_step((window.get_step() + 1).min(7));
        }
    });

    let back_window = window.as_weak();
    window.on_back(move || {
        if let Some(window) = back_window.upgrade() {
            window.set_step((window.get_step() - 1).max(0));
        }
    });

    let profile_window = window.as_weak();
    window.on_cycle_profile(move || {
        if let Some(window) = profile_window.upgrade() {
            let profile = next_profile(window.get_profile().as_str());
            window.set_profile(profile.into());
        }
    });

    let finish_window = window.as_weak();
    window.on_finish(move || {
        let Some(window) = finish_window.upgrade() else {
            return;
        };
        let password = window.get_account_password().to_string();
        if password != window.get_account_password_confirm().as_str() {
            window.set_status_message("Passwords do not match".into());
            return;
        }
        if password.len() < 8 {
            window.set_status_message("Password must contain at least 8 characters".into());
            return;
        }
        window.set_busy(true);
        window.set_status_message("Applying system settings and creating the account…".into());

        let language = window.get_language().to_string();
        let keyboard = window.get_keyboard().to_string();
        let timezone = window.get_timezone().to_string();
        let username = window.get_username().to_string();
        let hostname = window.get_hostname().to_string();
        let profile = window.get_profile().to_string();
        let result_window = window.as_weak();
        thread::Builder::new()
            .name("devcore-firstboot-apply".to_owned())
            .spawn(move || {
                let mut password = password;
                let result = FirstBootClient::connect().and_then(|client| {
                    client.apply(
                        &language, &keyboard, &timezone, &username, &hostname, &profile, &password,
                    )
                });
                password.clear();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(window) = result_window.upgrade() {
                        window.set_busy(false);
                        match result {
                            Ok(()) => {
                                window.set_account_password("".into());
                                window.set_account_password_confirm("".into());
                                window.set_status_message(
                                    "Setup complete. Starting the login screen…".into(),
                                );
                                let _ = slint::quit_event_loop();
                            }
                            Err(error) => {
                                window.set_status_message(error.into());
                            }
                        }
                    }
                });
            })
            .map_err(|error| {
                window.set_busy(false);
                window.set_status_message(format!("Could not start setup worker: {error}").into());
            })
            .ok();
    });

    window.run()?;
    Ok(())
}

fn run_login() -> Result<(), Box<dyn Error>> {
    let window = GreeterWindow::new()?;
    let controller = std::rc::Rc::new(std::cell::RefCell::new(LoginController::new()));

    let submit_controller = controller.clone();
    let submit_window = window.as_weak();
    window.on_submit(move || {
        let Some(window) = submit_window.upgrade() else {
            return;
        };
        let username = window.get_username().to_string();
        let mut response = window.get_response().to_string();
        let input = if window.get_auth_visible() {
            Some(std::mem::take(&mut response))
        } else {
            None
        };
        window.set_response("".into());
        window.set_busy(true);

        let outcome = submit_controller
            .borrow_mut()
            .submit(&username, input)
            .unwrap_or_else(|error| LoginOutcome::Failure(error.to_string()));
        window.set_busy(false);
        apply_outcome(&window, outcome);
    });

    let cancel_window = window.as_weak();
    window.on_cancel(move || {
        if cancel_window.upgrade().is_some() {
            let _ = slint::quit_event_loop();
        }
    });

    window.run()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    match first_boot_status() {
        Ok(Some(info)) => run_first_boot(info)?,
        Ok(None) => {}
        Err(error) => {
            eprintln!("first-boot service unavailable; continuing to login: {error}");
        }
    }
    run_login()
}

fn apply_outcome(window: &GreeterWindow, outcome: LoginOutcome) {
    match outcome {
        LoginOutcome::Prompt {
            kind,
            message,
            response_required,
        } => {
            window.set_prompt(message.into());
            window.set_auth_visible(true);
            window.set_response_required(response_required);
            window.set_response_secret(kind == AuthMessageKind::Secret);
            window.set_status_message("PAM requested the next authentication step".into());
        }
        LoginOutcome::Started => {
            let _ = slint::quit_event_loop();
        }
        LoginOutcome::Failure(message) => {
            window.set_auth_visible(false);
            window.set_response_required(false);
            window.set_response_secret(false);
            window.set_status_message(message.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthMessageKind, FirstBootInfo, LoginOutcome, next_profile};

    #[test]
    fn profile_cycle_is_stable_and_conservative_for_unknown_values() {
        assert_eq!(next_profile("low"), "balanced");
        assert_eq!(next_profile("balanced"), "standard");
        assert_eq!(next_profile("standard"), "workstation");
        assert_eq!(next_profile("workstation"), "low");
        assert_eq!(next_profile("unexpected"), "low");
    }

    #[test]
    fn first_boot_info_keeps_only_the_profile_hint() {
        assert_eq!(
            FirstBootInfo {
                suggested_profile: "balanced".to_owned(),
            },
            FirstBootInfo {
                suggested_profile: "balanced".to_owned(),
            }
        );
    }

    #[test]
    fn only_visible_and_secret_messages_request_input() {
        assert!(matches!(
            LoginOutcome::prompt(AuthMessageKind::Secret, "response".to_owned()),
            LoginOutcome::Prompt {
                response_required: true,
                kind: AuthMessageKind::Secret,
                ..
            }
        ));
        assert!(matches!(
            LoginOutcome::prompt(AuthMessageKind::Info, "notice".to_owned()),
            LoginOutcome::Prompt {
                response_required: false,
                kind: AuthMessageKind::Info,
                ..
            }
        ));
    }
}
