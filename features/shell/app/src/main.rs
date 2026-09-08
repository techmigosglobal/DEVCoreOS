#![deny(unsafe_code)]
#![allow(missing_docs)]
//! A native, low-overhead DevCore shell dashboard.
//!
//! This is the first implemented screen from the supplied DevCore references.
//! It uses Slint's software renderer and Wayland backend rather than a web
//! runtime. The resource view reads the system `devcored` service when it is
//! available and falls back to one-shot kernel collection for nested/local
//! development.
//!
//! Slint's generated rendering/FFI glue scopes its own `allow(unsafe_code)`.
//! DevCore source in this crate contains no handwritten unsafe code.

use std::{
    error::Error,
    fs,
    path::Path,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use devcore_core::{PressureMetrics, ResourceSnapshot, collect_snapshot};
use devcore_extensions::ExtensionHost;
use serde::Deserialize;
use slint::ComponentHandle;

slint::slint! {
    import { Button, LineEdit, TextEdit } from "std-widgets.slint";

    component ProjectCard inherits Rectangle {
        in property <string> project-name;
        in property <string> project-detail;
        in property <string> project-stack;
        in property <color> accent: #3b82f6;
        in property <bool> dark-mode;

        border-radius: 16px;
        border-width: 1px;
        border-color: dark-mode ? #253244 : #dce5f0;
        background: dark-mode ? #111b29 : #ffffff;

        VerticalLayout {
            padding: 16px;
            spacing: 9px;
            HorizontalLayout {
                Rectangle {
                    width: 12px;
                    height: 12px;
                    border-radius: 6px;
                    background: root.accent;
                }
                Text {
                    text: root.project-name;
                    color: root.dark-mode ? #f5f8fc : #101827;
                    font-size: 18px;
                    font-weight: 700;
                    horizontal-stretch: 1;
                }
                Text {
                    text: "•••";
                    color: root.dark-mode ? #91a2b9 : #637187;
                    font-size: 18px;
                }
            }
            Text {
                text: root.project-detail;
                color: root.dark-mode ? #aab8ca : #5d6b80;
                font-size: 13px;
                wrap: word-wrap;
                height: 42px;
            }
            Rectangle { vertical-stretch: 1; }
            HorizontalLayout {
                Text {
                    text: root.project-stack;
                    color: root.accent;
                    font-size: 12px;
                    font-weight: 600;
                }
                Rectangle { horizontal-stretch: 1; }
                Text {
                    text: "main  ·  now";
                    color: root.dark-mode ? #8190a4 : #718096;
                    font-size: 11px;
                }
            }
        }
    }

    component MetricCard inherits Rectangle {
        in property <string> metric-title;
        in property <string> metric-value;
        in property <string> metric-detail;
        in property <color> accent: #2dd4bf;
        in property <bool> dark-mode;

        border-radius: 14px;
        border-width: 1px;
        border-color: dark-mode ? #263447 : #dce5f0;
        background: dark-mode ? #111b29 : #ffffff;

        VerticalLayout {
            padding: 15px;
            spacing: 6px;
            Text {
                text: root.metric-title;
                color: root.dark-mode ? #aab8ca : #5d6b80;
                font-size: 13px;
            }
            Text {
                text: root.metric-value;
                color: root.dark-mode ? #f5f8fc : #101827;
                font-size: 24px;
                font-weight: 700;
            }
            Rectangle {
                height: 3px;
                border-radius: 2px;
                background: root.accent;
            }
            Text {
                text: root.metric-detail;
                color: root.dark-mode ? #8190a4 : #718096;
                font-size: 11px;
            }
        }
    }

    component StudioPanel inherits Rectangle {
        in property <bool> dark-mode;
        in-out property <string> environment-name;
        in-out property <string> environment-image;
        in-out property <string> environment-toolchains;
        in-out property <string> build-adapter;
        in-out property <string> workspace-directory;
        in-out property <string> explorer-directory;
        in-out property <string> explorer-entries;
        in-out property <string> explorer-readout;
        in-out property <string> file-path;
        in-out property <string> file-content;
        in-out property <string> file-readout;
        in-out property <bool> file-dirty;
        in-out property <string> git-job-id;
        in-out property <string> git-readout;
        in-out property <string> job-id;
        in-out property <string> job-readout;
        in-out property <string> extension-host-readout;
        in-out property <string> extension-source;
        in-out property <string> provisioning-bundles;
        in-out property <string> provisioning-image;
        in-out property <string> provisioning-request-id;
        in-out property <string> provisioning-readout;
        callback close-studio();
        callback action(string);

        border-width: 1px;
        border-color: root.dark-mode ? #263447 : #dce5f0;
        background: root.dark-mode ? #0f1928 : #ffffff;

        VerticalLayout {
            padding: 22px;
            spacing: 16px;
            HorizontalLayout {
                Text {
                    text: "DevCore Studio";
                    color: root.dark-mode ? #f5f8fc : #101827;
                    font-size: 26px;
                    font-weight: 800;
                }
                Rectangle { horizontal-stretch: 1; }
                Button {
                    text: "Close Studio";
                    clicked => { root.close-studio(); }
                }
            }
            HorizontalLayout {
                spacing: 14px;
                Rectangle {
                    width: 220px;
                    border-radius: 12px;
                    border-width: 1px;
                    border-color: root.dark-mode ? #263447 : #dce5f0;
                    background: root.dark-mode ? #111b29 : #f8fbff;
                    VerticalLayout {
                        padding: 14px;
                        spacing: 8px;
                        HorizontalLayout {
                            Text {
                                text: "PROJECT EXPLORER";
                                color: root.dark-mode ? #dbe7f5 : #263447;
                                font-size: 12px;
                                font-weight: 700;
                                vertical-alignment: center;
                            }
                            Rectangle { horizontal-stretch: 1; }
                            Button { text: "Refresh"; clicked => { root.action("ListFiles"); } }
                        }
                        LineEdit {
                            text <=> root.explorer-directory;
                            placeholder-text: "Directory relative to workspace (blank = root)";
                            height: 34px;
                        }
                        TextEdit {
                            vertical-stretch: 1;
                            text <=> root.explorer-entries;
                            read-only: true;
                            wrap: no-wrap;
                            font-size: 12px;
                            placeholder-text: "Refresh to list workspace entries";
                        }
                        Text {
                            text: root.explorer-readout;
                            color: root.dark-mode ? #8190a4 : #718096;
                            font-size: 11px;
                            wrap: word-wrap;
                        }
                        Rectangle { vertical-stretch: 1; }
                        HorizontalLayout {
                            Text {
                                text: "GIT STATUS";
                                color: root.dark-mode ? #dbe7f5 : #263447;
                                font-size: 12px;
                                font-weight: 700;
                                vertical-alignment: center;
                            }
                            Rectangle { horizontal-stretch: 1; }
                            Button { text: "Check"; clicked => { root.action("GitStatus"); } }
                        }
                        HorizontalLayout {
                            spacing: 6px;
                            Button { text: "Diff"; clicked => { root.action("GitDiff"); } }
                            Button { text: "History"; clicked => { root.action("GitHistory"); } }
                        }
                        Text {
                            text: root.git-readout;
                            color: root.dark-mode ? #8190a4 : #718096;
                            font-size: 11px;
                            wrap: word-wrap;
                        }
                        Button {
                            visible: root.git-job-id != "";
                            text: "Refresh Git result";
                            clicked => { root.action("RefreshGit"); }
                        }
                    }
                }
                Rectangle {
                    horizontal-stretch: 1;
                    border-radius: 12px;
                    border-width: 1px;
                    border-color: root.dark-mode ? #263447 : #dce5f0;
                    background: root.dark-mode ? #0a1421 : #fbfdff;
                    VerticalLayout {
                        padding: 14px;
                        spacing: 9px;
                        HorizontalLayout {
                            spacing: 8px;
                            Text {
                                text: "EDITOR";
                                color: root.dark-mode ? #f5f8fc : #101827;
                                font-size: 13px;
                                font-weight: 700;
                                vertical-alignment: center;
                            }
                            Rectangle { horizontal-stretch: 1; }
                            Button { text: "Load"; clicked => { root.action("ReadFile"); } }
                            Button {
                                text: root.file-dirty ? "Save *" : "Save";
                                clicked => { root.action("WriteFile"); }
                            }
                        }
                        LineEdit {
                            text <=> root.file-path;
                            placeholder-text: "Workspace-relative file path";
                            height: 34px;
                        }
                        Rectangle { height: 1px; background: root.dark-mode ? #263447 : #e3ebf4; }
                        TextEdit {
                            vertical-stretch: 1;
                            text <=> root.file-content;
                            placeholder-text: "Load a UTF-8 text file from the workspace";
                            wrap: no-wrap;
                            font-size: 13px;
                            edited(text) => { root.file-dirty = true; }
                        }
                        Text {
                            text: root.file-readout;
                            color: root.dark-mode ? #8190a4 : #718096;
                            font-size: 11px;
                            wrap: word-wrap;
                        }
                    }
                }
                Rectangle {
                    width: 250px;
                    border-radius: 12px;
                    border-width: 1px;
                    border-color: root.dark-mode ? #263447 : #dce5f0;
                    background: root.dark-mode ? #111b29 : #f8fbff;
                    VerticalLayout {
                        padding: 14px;
                        spacing: 10px;
                        Text {
                            text: "WORKSPACE";
                            color: root.dark-mode ? #dbe7f5 : #263447;
                            font-size: 12px;
                            font-weight: 700;
                        }
                        LineEdit {
                            text <=> root.environment-name;
                            placeholder-text: "Environment name";
                            height: 34px;
                        }
                        LineEdit {
                            text <=> root.environment-image;
                            placeholder-text: "OCI image@sha256:digest";
                            height: 34px;
                        }
                        LineEdit {
                            text <=> root.environment-toolchains;
                            placeholder-text: "Toolchains: rust, go, python";
                            height: 34px;
                        }
                        LineEdit {
                            text <=> root.build-adapter;
                            placeholder-text: "Build adapter: cargo, go, cmake";
                            height: 34px;
                        }
                        LineEdit {
                            text <=> root.workspace-directory;
                            placeholder-text: "Workspace directory";
                            height: 34px;
                        }
                        Button { text: "Register environment"; clicked => { root.action("Register"); } }
                        Button { text: "Build"; clicked => { root.action("Build"); } }
                        Button { text: "Unit test"; clicked => { root.action("Test"); } }
                        Button {
                            visible: root.job-id != "";
                            text: "Refresh job status";
                            clicked => { root.action("RefreshJob"); }
                        }
                        Button {
                            visible: root.job-id != "";
                            text: "Cancel job";
                            clicked => { root.action("CancelJob"); }
                        }
                        Button { text: "New terminal"; clicked => { root.action("Terminal"); } }
                        Rectangle { height: 1px; background: root.dark-mode ? #263447 : #e3ebf4; }
                        Text {
                            text: "VS CODE COMPATIBILITY";
                            color: root.dark-mode ? #dbe7f5 : #263447;
                            font-size: 12px;
                            font-weight: 700;
                        }
                        Text {
                            text: root.extension-host-readout;
                            color: root.dark-mode ? #aab8ca : #5d6b80;
                            font-size: 11px;
                            wrap: word-wrap;
                        }
                        Button { text: "Open workspace in host"; clicked => { root.action("OpenExtensionHost"); } }
                        LineEdit {
                            text <=> root.extension-source;
                            placeholder-text: "publisher.extension or /path/file.vsix";
                            height: 34px;
                        }
                        HorizontalLayout {
                            spacing: 6px;
                            Button { text: "Install"; clicked => { root.action("InstallExtension"); } }
                            Button { text: "List"; clicked => { root.action("ListExtensions"); } }
                            Button { text: "Remove"; clicked => { root.action("UninstallExtension"); } }
                        }
                        Text {
                            visible: root.job-id != "";
                            text: "Job: " + root.job-id;
                            color: root.dark-mode ? #8190a4 : #718096;
                            font-size: 11px;
                            wrap: word-wrap;
                        }
                        Text {
                            text: root.job-readout;
                            color: root.dark-mode ? #aab8ca : #5d6b80;
                            font-size: 11px;
                            wrap: word-wrap;
                        }
                        Rectangle { height: 1px; background: root.dark-mode ? #263447 : #e3ebf4; }
                        Text {
                            text: "DIAGNOSTICS";
                            color: root.dark-mode ? #dbe7f5 : #263447;
                            font-size: 12px;
                            font-weight: 700;
                        }
                        Text { text: "0 errors · 0 warnings"; color: #22c58b; font-size: 12px; }
                        Text { text: "RESOURCE POLICY"; color: root.dark-mode ? #dbe7f5 : #263447; font-size: 12px; font-weight: 700; }
                        Text { text: "Build budget follows devcored"; color: root.dark-mode ? #aab8ca : #5d6b80; font-size: 12px; wrap: word-wrap; }
                    }
                }
            }
            Rectangle {
                height: 220px;
                border-radius: 12px;
                border-width: 1px;
                border-color: root.dark-mode ? #263447 : #dce5f0;
                background: root.dark-mode ? #101a29 : #ffffff;
                VerticalLayout {
                    padding: 14px;
                    spacing: 8px;
                    Text {
                        text: "POST-INSTALL DEVELOPER PACKAGES";
                        color: root.dark-mode ? #dbe7f5 : #263447;
                        font-size: 12px;
                        font-weight: 700;
                    }
                    Text {
                        text: "Connect Wi-Fi first · downloads stay outside the small base image";
                        color: root.dark-mode ? #aab8ca : #5d6b80;
                        font-size: 11px;
                        wrap: word-wrap;
                    }
                    HorizontalLayout {
                        spacing: 8px;
                        LineEdit {
                            text <=> root.provisioning-bundles;
                            placeholder-text: "Bundles: rust, flutter, android-studio, chrome";
                            horizontal-stretch: 1;
                            height: 34px;
                        }
                        Button { text: "Plan"; clicked => { root.action("ProvisionPlan"); } }
                    }
                    LineEdit {
                        text <=> root.provisioning-image;
                        placeholder-text: "OCI image@sha256:digest for toolchain bundles";
                        height: 34px;
                    }
                    HorizontalLayout {
                        spacing: 8px;
                        Button { text: "Refresh status"; clicked => { root.action("ProvisionStatus"); } }
                        Button { text: "Start downloads"; clicked => { root.action("ProvisionStart"); } }
                        Button { text: "Cancel"; clicked => { root.action("ProvisionCancel"); } }
                    }
                    Text {
                        text: root.provisioning-readout;
                        color: root.dark-mode ? #aab8ca : #5d6b80;
                        font-size: 11px;
                        wrap: word-wrap;
                    }
                }
            }
            Rectangle {
                height: 120px;
                border-radius: 12px;
                border-width: 1px;
                border-color: root.dark-mode ? #263447 : #dce5f0;
                background: root.dark-mode ? #0a1421 : #fbfdff;
                VerticalLayout {
                    padding: 14px;
                    spacing: 6px;
                    Text { text: "TERMINAL"; color: root.dark-mode ? #dbe7f5 : #263447; font-size: 12px; font-weight: 700; }
                    Text { text: "$ devcore plan --workspace"; color: #24c7d5; font-size: 13px; font-family: "monospace"; }
                    Text { text: "Planner ready · commands run only through an OCI environment"; color: root.dark-mode ? #aab8ca : #5d6b80; font-size: 12px; }
                }
            }
        }
    }

    export component DevCoreShell inherits Window {
        title: "DevCore OS";
        width: 1600px;
        height: 900px;
        in-out property <bool> dark-mode: true;
        in-out property <string> profile: "balanced";
        in-out property <string> cpu-readout: "4 logical CPUs";
        in-out property <string> memory-readout: "Collecting memory data";
        in-out property <string> pressure-readout: "Collecting PSI";
        in-out property <string> power-readout: "Power state unavailable";
        in-out property <string> hardware-readout: "Hardware inventory unavailable";
        in-out property <string> update-readout: "Update status unavailable";
        in-out property <string> status-message: "Telemetry is ready";
        in-out property <bool> studio-open: false;
        in-out property <bool> launcher-open: false;
        in-out property <string> launcher-query;
        in-out property <int> workspace-index: 1;
        in-out property <string> environment-name: "devcore-rust";
        in-out property <string> environment-image: "";
        in-out property <string> environment-toolchains: "rust";
        in-out property <string> build-adapter: "cargo";
        in-out property <string> workspace-directory: "";
        in-out property <string> explorer-directory: "";
        in-out property <string> explorer-entries: "";
        in-out property <string> explorer-readout: "Register an environment before listing files";
        in-out property <string> file-path: "crates/devcore-core/src/policy.rs";
        in-out property <string> file-content: "";
        in-out property <string> file-readout: "Load a workspace-relative UTF-8 file to begin editing";
        in-out property <bool> file-dirty: false;
        in-out property <string> git-job-id: "";
        in-out property <string> git-readout: "Git status not loaded";
        in-out property <string> job-id: "";
        in-out property <string> job-readout: "No work job submitted";
        in-out property <string> extension-host-readout: "VS Code-compatible host not detected";
        in-out property <string> extension-source: "";
        in-out property <string> provisioning-bundles: "rust";
        in-out property <string> provisioning-image: "";
        in-out property <string> provisioning-request-id: "";
        in-out property <string> provisioning-readout: "No post-install bundle plan submitted";
        callback refresh-telemetry();
        callback toggle-theme();
        callback toggle-launcher();
        callback select-workspace(int);
        callback launch-app(string);
        callback open-studio();
        callback launch-terminal();
        callback studio-action(string);

        background: root.dark-mode ? #09121f : #f4f8fc;

        VerticalLayout {
            spacing: 0px;
            Rectangle {
                height: 66px;
                border-width: 0px;
                background: root.dark-mode ? #0c1726 : #ffffff;
                HorizontalLayout {
                    padding-left: 28px;
                    padding-right: 28px;
                    spacing: 16px;
                    Text {
                        text: "◇";
                        color: #22c7f2;
                        font-size: 30px;
                        vertical-alignment: center;
                    }
                    Text {
                        text: "DevCore";
                        color: root.dark-mode ? #f5f8fc : #101827;
                        font-size: 21px;
                        font-weight: 800;
                        vertical-alignment: center;
                    }
                    Text {
                        text: "OS";
                        color: root.dark-mode ? #93a4ba : #6b7280;
                        font-size: 20px;
                        vertical-alignment: center;
                    }
                    Button {
                        text: root.launcher-open ? "Close launcher" : "Launcher";
                        clicked => { root.toggle-launcher(); }
                    }
                    Text {
                        text: "Workspace " + root.workspace-index;
                        color: root.dark-mode ? #b7c4d5 : #536176;
                        font-size: 12px;
                        vertical-alignment: center;
                    }
                    Button { text: "1"; clicked => { root.select-workspace(1); } }
                    Button { text: "2"; clicked => { root.select-workspace(2); } }
                    Button { text: "3"; clicked => { root.select-workspace(3); } }
                    Button { text: "4"; clicked => { root.select-workspace(4); } }
                    Rectangle { horizontal-stretch: 1; }
                    Text {
                        text: root.cpu-readout;
                        color: root.dark-mode ? #b7c4d5 : #536176;
                        font-size: 12px;
                        vertical-alignment: center;
                    }
                    Text {
                        text: root.memory-readout;
                        color: root.dark-mode ? #b7c4d5 : #536176;
                        font-size: 12px;
                        vertical-alignment: center;
                    }
                    Text {
                        text: root.power-readout;
                        color: root.dark-mode ? #b7c4d5 : #536176;
                        font-size: 12px;
                        vertical-alignment: center;
                    }
                    Button {
                        text: root.dark-mode ? "Light" : "Dark";
                        clicked => { root.toggle-theme(); }
                    }
                    Button {
                        text: "Refresh";
                        clicked => { root.refresh-telemetry(); }
                    }
                    Button {
                        text: "Studio";
                        clicked => { root.open-studio(); }
                    }
                }
            }

            Rectangle {
                visible: root.launcher-open;
                height: 132px;
                border-width: 1px;
                border-color: root.dark-mode ? #263447 : #dce5f0;
                background: root.dark-mode ? #101a29 : #ffffff;
                VerticalLayout {
                    padding-left: 24px;
                    padding-right: 24px;
                    padding-top: 14px;
                    padding-bottom: 14px;
                    spacing: 10px;
                    HorizontalLayout {
                        Text {
                            text: "LAUNCHER";
                            color: root.dark-mode ? #dbe7f5 : #263447;
                            font-size: 12px;
                            font-weight: 700;
                            vertical-alignment: center;
                        }
                        Rectangle { horizontal-stretch: 1; }
                        LineEdit {
                            text <=> root.launcher-query;
                            enabled: !root.studio-open;
                            placeholder-text: "Search apps and actions";
                            width: 300px;
                            height: 34px;
                        }
                    }
                    HorizontalLayout {
                        spacing: 10px;
                        Button {
                            visible: root.launcher-query == "";
                            text: "Terminal";
                            clicked => { root.launch-app("Terminal"); }
                        }
                        Button {
                            visible: root.launcher-query == "";
                            text: "Studio";
                            clicked => { root.launch-app("Studio"); }
                        }
                        Button {
                            visible: root.launcher-query == "";
                            text: "Browser";
                            clicked => { root.launch-app("Browser"); }
                        }
                        Button {
                            visible: root.launcher-query == "";
                            text: "Codex";
                            clicked => { root.launch-app("Codex"); }
                        }
                        Button {
                            visible: root.launcher-query == "";
                            text: "Resources";
                            clicked => { root.launch-app("Resources"); }
                        }
                        Text {
                            visible: root.launcher-query != "";
                            text: "Type a built-in action name, then clear the query to show actions";
                            color: root.dark-mode ? #8190a4 : #718096;
                            font-size: 12px;
                            vertical-alignment: center;
                        }
                    }
                }
            }

            HorizontalLayout {
                visible: !root.studio-open;
                padding: 24px;
                spacing: 22px;
                Rectangle {
                    visible: root.width >= 1180px;
                    width: 300px;
                    border-radius: 18px;
                    border-width: 1px;
                    border-color: root.dark-mode ? #263447 : #dce5f0;
                    background: root.dark-mode ? #101a29 : #ffffff;

                    VerticalLayout {
                        padding: 20px;
                        spacing: 15px;
                        Text {
                            text: "SYSTEM OVERVIEW";
                            color: root.dark-mode ? #f5f8fc : #101827;
                            font-size: 13px;
                            font-weight: 700;
                        }
                        Text {
                            text: "Profile mode";
                            color: root.dark-mode ? #aab8ca : #5d6b80;
                            font-size: 12px;
                        }
                        Rectangle {
                            height: 42px;
                            border-radius: 10px;
                            border-width: 1px;
                            border-color: root.dark-mode ? #324156 : #dce5f0;
                            background: root.dark-mode ? #0c1421 : #f8fbff;
                            Text {
                                x: 13px;
                                y: 12px;
                                text: "Developer · " + root.profile;
                                color: root.dark-mode ? #dbe7f5 : #263447;
                                font-size: 13px;
                            }
                        }
                        Rectangle { height: 1px; background: root.dark-mode ? #263447 : #e3ebf4; }
                        Text {
                            text: "Build workers";
                            color: root.dark-mode ? #f5f8fc : #101827;
                            font-size: 14px;
                            font-weight: 650;
                        }
                        Text {
                            text: "0 active · resources protected";
                            color: #22c58b;
                            font-size: 12px;
                        }
                        Text {
                            text: "Memory pressure";
                            color: root.dark-mode ? #f5f8fc : #101827;
                            font-size: 14px;
                            font-weight: 650;
                        }
                        Text {
                            text: root.pressure-readout;
                            color: root.dark-mode ? #aab8ca : #5d6b80;
                            font-size: 12px;
                            wrap: word-wrap;
                        }
                        Rectangle { height: 1px; background: root.dark-mode ? #263447 : #e3ebf4; }
                        Text {
                            text: "Device status";
                            color: root.dark-mode ? #f5f8fc : #101827;
                            font-size: 14px;
                            font-weight: 650;
                        }
                        Text {
                            text: root.hardware-readout;
                            color: root.dark-mode ? #aab8ca : #5d6b80;
                            font-size: 12px;
                            wrap: word-wrap;
                        }
                        Text {
                            text: "Update status";
                            color: root.dark-mode ? #f5f8fc : #101827;
                            font-size: 14px;
                            font-weight: 650;
                        }
                        Text {
                            text: root.update-readout;
                            color: root.dark-mode ? #aab8ca : #5d6b80;
                            font-size: 12px;
                            wrap: word-wrap;
                        }
                        Rectangle { vertical-stretch: 1; }
                        Text {
                            text: root.status-message;
                            color: #22c7f2;
                            font-size: 12px;
                            wrap: word-wrap;
                        }
                    }
                }

                VerticalLayout {
                    spacing: 18px;
                    Text {
                        text: "Welcome back, Developer";
                        color: root.dark-mode ? #f5f8fc : #101827;
                        font-size: 34px;
                        font-weight: 800;
                    }
                    Text {
                        text: "Build. Test. Ship. A focused native workspace designed to stay out of your way.";
                        color: root.dark-mode ? #aab8ca : #5d6b80;
                        font-size: 15px;
                    }
                    LineEdit {
                        placeholder-text: "Search projects, files, commands…";
                        height: 46px;
                    }
                    Text {
                        text: "RECENT PROJECTS";
                        color: root.dark-mode ? #dbe7f5 : #263447;
                        font-size: 13px;
                        font-weight: 700;
                    }
                    HorizontalLayout {
                        spacing: 14px;
                        ProjectCard {
                            horizontal-stretch: 1;
                            height: 174px;
                            project-name: "SchoolDesk";
                            project-detail: "School management with clear, focused workflows.";
                            project-stack: "TypeScript";
                            accent: #27c689;
                            dark-mode: root.dark-mode;
                        }
                        ProjectCard {
                            horizontal-stretch: 1;
                            height: 174px;
                            project-name: "TutorSaaS";
                            project-detail: "A responsive tutoring platform for focused collaboration.";
                            project-stack: "Next.js";
                            accent: #3b82f6;
                            dark-mode: root.dark-mode;
                        }
                        ProjectCard {
                            horizontal-stretch: 1;
                            height: 174px;
                            project-name: "DevCore";
                            project-detail: "The operating system for developers, written with restraint.";
                            project-stack: "Rust";
                            accent: #24c7d5;
                            dark-mode: root.dark-mode;
                        }
                        ProjectCard {
                            visible: root.width >= 1380px;
                            horizontal-stretch: 1;
                            height: 174px;
                            project-name: "EmbeddedTools";
                            project-detail: "Device and debugging tools for constrained systems.";
                            project-stack: "C++";
                            accent: #4cc861;
                            dark-mode: root.dark-mode;
                        }
                    }
                    Text {
                        text: "SYSTEM HEALTH";
                        color: root.dark-mode ? #dbe7f5 : #263447;
                        font-size: 13px;
                        font-weight: 700;
                    }
                    HorizontalLayout {
                        spacing: 14px;
                        MetricCard {
                            horizontal-stretch: 1;
                            height: 136px;
                            metric-title: "CPU";
                            metric-value: root.cpu-readout;
                            metric-detail: "Current process-visible capacity";
                            accent: #3b82f6;
                            dark-mode: root.dark-mode;
                        }
                        MetricCard {
                            horizontal-stretch: 1;
                            height: 136px;
                            metric-title: "MEMORY";
                            metric-value: root.memory-readout;
                            metric-detail: "Available memory from the kernel";
                            accent: #9a6bff;
                            dark-mode: root.dark-mode;
                        }
                        MetricCard {
                            horizontal-stretch: 1;
                            height: 136px;
                            metric-title: "PRESSURE";
                            metric-value: root.pressure-readout;
                            metric-detail: "PSI before reactive policy";
                            accent: #22c58b;
                            dark-mode: root.dark-mode;
                        }
                        MetricCard {
                            visible: root.width >= 1380px;
                            horizontal-stretch: 1;
                            height: 136px;
                            metric-title: "POWER";
                            metric-value: root.power-readout;
                            metric-detail: "No power policy is active yet";
                            accent: #f4bd45;
                            dark-mode: root.dark-mode;
                        }
                    }
                    Rectangle { vertical-stretch: 1; }
                    Rectangle {
                        height: 78px;
                        border-radius: 16px;
                        border-width: 1px;
                        border-color: root.dark-mode ? #263447 : #dce5f0;
                        background: root.dark-mode ? #101a29 : #ffffff;
                        HorizontalLayout {
                            padding: 15px;
                            spacing: 12px;
                            Button { text: "New project"; }
                            Button { text: "Open workspace"; }
                            Button { text: "Clone repository"; }
                            Button { text: "New terminal"; clicked => { root.launch-terminal(); } }
                            Button {
                                text: "Open Studio";
                                clicked => { root.open-studio(); }
                            }
                            Button { text: "DevCore docs"; }
                            Rectangle { horizontal-stretch: 1; }
                            Text {
                                text: "Projects  ·  Studio  ·  Terminal  ·  Build  ·  Test  ·  Devices";
                                color: root.dark-mode ? #8190a4 : #718096;
                                font-size: 11px;
                                vertical-alignment: center;
                            }
                        }
                    }
                }
            }
            StudioPanel {
                visible: root.studio-open;
                vertical-stretch: 1;
                dark-mode: root.dark-mode;
                environment-name <=> root.environment-name;
                environment-image <=> root.environment-image;
                environment-toolchains <=> root.environment-toolchains;
                build-adapter <=> root.build-adapter;
                workspace-directory <=> root.workspace-directory;
                explorer-directory <=> root.explorer-directory;
                explorer-entries <=> root.explorer-entries;
                explorer-readout <=> root.explorer-readout;
                file-path <=> root.file-path;
                file-content <=> root.file-content;
                file-readout <=> root.file-readout;
                file-dirty <=> root.file-dirty;
                git-job-id <=> root.git-job-id;
                git-readout <=> root.git-readout;
                job-id <=> root.job-id;
                job-readout <=> root.job-readout;
                extension-host-readout <=> root.extension-host-readout;
                extension-source <=> root.extension-source;
                provisioning-bundles <=> root.provisioning-bundles;
                provisioning-image <=> root.provisioning-image;
                provisioning-request-id <=> root.provisioning-request-id;
                provisioning-readout <=> root.provisioning-readout;
                close-studio => {
                    root.studio-open = false;
                    root.status-message = "Studio closed · no background service was started";
                }
                action(string) => {
                    root.studio-action(string);
                }
            }
        }
    }
}

/// Display-only values derived from a kernel telemetry snapshot.
#[derive(Debug, Eq, PartialEq)]
struct ShellTelemetry {
    cpu_readout: String,
    memory_readout: String,
    power_readout: String,
    pressure_readout: String,
    profile: String,
}

impl From<&ResourceSnapshot> for ShellTelemetry {
    fn from(snapshot: &ResourceSnapshot) -> Self {
        let memory_readout = match snapshot.memory_available_kib {
            Some(available) => format!(
                "{} / {} GiB free",
                kib_to_gib_rounded(available),
                kib_to_gib_rounded(snapshot.capacity.memory_total_kib)
            ),
            None => format!(
                "{} GiB total",
                kib_to_gib_rounded(snapshot.capacity.memory_total_kib)
            ),
        };

        Self {
            cpu_readout: format!("{} logical CPUs", snapshot.capacity.logical_cpus),
            memory_readout,
            power_readout: power_readout(snapshot),
            pressure_readout: pressure_readout(snapshot.cpu_pressure, snapshot.memory_pressure),
            profile: snapshot.initial_profile.as_str().to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SnapshotWire {
    profile: String,
    logical_cpus: u16,
    memory_total_kib: u64,
    memory_available_kib: Option<u64>,
    pressure: PressureWire,
    power: PowerWire,
}

#[derive(Debug, Deserialize)]
struct PressureWire {
    cpu: Option<PressureWireValue>,
    memory: Option<PressureWireValue>,
    io: Option<PressureWireValue>,
}

#[derive(Debug, Deserialize)]
struct PressureWireValue {
    some_avg10_milli_percent: u32,
    full_avg10_milli_percent: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PowerWire {
    battery_present: bool,
    battery_percent: Option<u8>,
    ac_online: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct HardwareSnapshotWire {
    cpu_model: String,
    logical_cpus: u16,
    memory_total_kib: u64,
    battery_present: bool,
    battery_percent: Option<u8>,
    ac_online: Option<bool>,
    network_interfaces: Vec<String>,
    gpu_devices: u16,
}

#[derive(Debug, Eq, PartialEq)]
struct SystemReadouts {
    hardware: String,
    update: String,
}

const MAX_HARDWARE_JSON_BYTES: usize = 64 * 1024;
const MAX_UPDATE_STATUS_CHARS: usize = 16 * 1024;
const MAX_SYSTEM_READOUT_CHARS: usize = 160;

fn snapshot_from_json(value: &str) -> Result<ResourceSnapshot, String> {
    let wire: SnapshotWire = serde_json::from_str(value)
        .map_err(|error| format!("resource service returned invalid snapshot JSON: {error}"))?;
    Ok(ResourceSnapshot {
        capacity: devcore_core::HardwareCapacity::new(wire.logical_cpus, wire.memory_total_kib),
        initial_profile: profile_from_str(&wire.profile)?,
        memory_available_kib: wire.memory_available_kib,
        cpu_pressure: wire.pressure.cpu.map(PressureMetrics::from),
        memory_pressure: wire.pressure.memory.map(PressureMetrics::from),
        io_pressure: wire.pressure.io.map(PressureMetrics::from),
        power: devcore_core::PowerState {
            battery_present: wire.power.battery_present,
            battery_percent: wire.power.battery_percent,
            ac_online: wire.power.ac_online,
        },
    })
}

impl From<PressureWireValue> for PressureMetrics {
    fn from(value: PressureWireValue) -> Self {
        Self {
            some_avg10_milli_percent: value.some_avg10_milli_percent,
            full_avg10_milli_percent: value.full_avg10_milli_percent,
        }
    }
}

fn hardware_snapshot_from_json(value: &str) -> Result<HardwareSnapshotWire, String> {
    if value.len() > MAX_HARDWARE_JSON_BYTES {
        return Err("hardware snapshot exceeded the shell input limit".to_owned());
    }
    serde_json::from_str(value)
        .map_err(|error| format!("hardware service returned invalid snapshot JSON: {error}"))
}

fn format_hardware_readout(snapshot: &HardwareSnapshotWire) -> String {
    let power = match (
        snapshot.battery_present,
        snapshot.battery_percent,
        snapshot.ac_online,
    ) {
        (true, Some(percent), Some(true)) => format!("battery {percent}% · AC online"),
        (true, Some(percent), Some(false)) => format!("battery {percent}% · on battery"),
        (true, Some(percent), None) => format!("battery {percent}%"),
        (true, None, Some(true)) => "battery present · AC online".to_owned(),
        (true, None, Some(false)) => "battery present · on battery".to_owned(),
        (true, None, None) => "battery present".to_owned(),
        (false, _, Some(true)) => "AC online".to_owned(),
        (false, _, Some(false)) => "AC offline".to_owned(),
        (false, _, None) => "power unavailable".to_owned(),
    };
    format!(
        "{} · {} CPU · {} GiB · {} GPU · {} net · {power}",
        compact_readout(&snapshot.cpu_model, 56),
        snapshot.logical_cpus,
        kib_to_gib_rounded(snapshot.memory_total_kib),
        snapshot.gpu_devices,
        snapshot.network_interfaces.len(),
    )
}

fn format_update_readout(channel: &str, status: &str) -> String {
    let bounded_status = compact_readout(status, MAX_UPDATE_STATUS_CHARS);
    let status = compact_readout(&bounded_status, MAX_SYSTEM_READOUT_CHARS);
    if status.is_empty() {
        format!("channel {channel} · status query returned no output")
    } else {
        format!("channel {channel} · {status}")
    }
}

fn compact_readout(value: &str, max_chars: usize) -> String {
    let mut result = String::new();
    let mut truncated = false;
    for character in value.chars() {
        if result.chars().count() >= max_chars {
            truncated = true;
            break;
        }
        result.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if truncated {
        result.push('…');
    }
    result.trim().to_owned()
}

const WORK_BUS_NAME: &str = "org.devcore.Work1";
const WORK_OBJECT_PATH: &str = "/org/devcore/Work";
const HARDWARE_BUS_NAME: &str = "org.devcore.Hardware1";
const HARDWARE_OBJECT_PATH: &str = "/org/devcore/Hardware";
const UPDATE_BUS_NAME: &str = "org.devcore.Update1";
const UPDATE_OBJECT_PATH: &str = "/org/devcore/Update";
const TERMINAL_PROGRAM: &str = "/usr/bin/foot";
const TERMINAL_TITLE: &str = "--title=DevCore Terminal";
const TERMINAL_WORKING_DIRECTORY_OPTION: &str = "--working-directory";
const BROWSER_PROGRAMS: &[&str] = &[
    "/usr/bin/google-chrome-stable",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
];
const CODEX_PROGRAMS: &[&str] = &["/usr/bin/codex", "/usr/local/bin/codex"];

#[derive(Debug)]
struct JobStatusView {
    id: String,
    lifecycle: String,
    exit_code: Option<i32>,
    output_truncated: bool,
    duration_millis: u64,
    error: Option<String>,
}

#[derive(Debug)]
struct JobLogView {
    bytes: Vec<u8>,
    end_of_file: bool,
    total_bytes: u64,
}

struct WorkClient {
    connection: zbus::blocking::Connection,
}

impl WorkClient {
    fn connect() -> Result<Self, String> {
        zbus::blocking::Connection::session()
            .map(|connection| Self { connection })
            .map_err(|error| format!("work service connection failed: {error}"))
    }

    fn proxy(&self, interface: &'static str) -> Result<zbus::blocking::Proxy<'static>, String> {
        zbus::blocking::Proxy::new(&self.connection, WORK_BUS_NAME, WORK_OBJECT_PATH, interface)
            .map_err(|error| format!("work service proxy failed: {error}"))
    }

    fn define_environment(
        &self,
        name: &str,
        image: &str,
        workspace: &str,
        toolchains: &[String],
    ) -> Result<(), String> {
        let proxy = self.proxy("org.devcore.Environment1")?;
        let _: () = proxy
            .call(
                "Define",
                &(
                    name.to_owned(),
                    image.to_owned(),
                    workspace.to_owned(),
                    toolchains.to_vec(),
                    false,
                ),
            )
            .map_err(|error| format!("environment registration failed: {error}"))?;
        Ok(())
    }

    fn ensure_environment(
        &self,
        name: &str,
        image: &str,
        workspace: &str,
        toolchains: &[String],
    ) -> Result<(), String> {
        let proxy = self.proxy("org.devcore.Environment1")?;
        let _: () = proxy
            .call(
                "Ensure",
                &(
                    name.to_owned(),
                    image.to_owned(),
                    workspace.to_owned(),
                    toolchains.to_vec(),
                    false,
                ),
            )
            .map_err(|error| format!("environment preparation failed: {error}"))?;
        Ok(())
    }

    fn read_workspace_file(
        &self,
        environment: &str,
        path: &str,
    ) -> Result<(String, String), String> {
        let proxy = self.proxy("org.devcore.Workspace1")?;
        proxy
            .call("ReadText", &(environment.to_owned(), path.to_owned()))
            .map_err(|error| format!("file load failed: {error}"))
    }

    fn write_workspace_file(
        &self,
        environment: &str,
        path: &str,
        contents: &str,
    ) -> Result<(String, u64), String> {
        let proxy = self.proxy("org.devcore.Workspace1")?;
        proxy
            .call(
                "WriteText",
                &(environment.to_owned(), path.to_owned(), contents.to_owned()),
            )
            .map_err(|error| format!("file save failed: {error}"))
    }

    fn list_workspace(
        &self,
        environment: &str,
        directory: &str,
    ) -> Result<Vec<(String, bool, bool, u64)>, String> {
        let proxy = self.proxy("org.devcore.Workspace1")?;
        proxy
            .call("List", &(environment.to_owned(), directory.to_owned()))
            .map_err(|error| format!("workspace listing failed: {error}"))
    }

    fn start_git_status(
        &self,
        environment: &str,
        job_id: &str,
        workspace: &str,
    ) -> Result<String, String> {
        let proxy = self.proxy("org.devcore.Git1")?;
        proxy
            .call(
                "StartStatus",
                &(
                    environment.to_owned(),
                    job_id.to_owned(),
                    workspace.to_owned(),
                ),
            )
            .map_err(|error| format!("Git status submission failed: {error}"))
    }

    fn start_git_diff(
        &self,
        environment: &str,
        job_id: &str,
        workspace: &str,
    ) -> Result<String, String> {
        self.start_git_read_operation("StartDiff", environment, job_id, workspace, "diff")
    }

    fn start_git_history(
        &self,
        environment: &str,
        job_id: &str,
        workspace: &str,
    ) -> Result<String, String> {
        self.start_git_read_operation("StartHistory", environment, job_id, workspace, "history")
    }

    fn start_git_read_operation(
        &self,
        method: &str,
        environment: &str,
        job_id: &str,
        workspace: &str,
        label: &str,
    ) -> Result<String, String> {
        let proxy = self.proxy("org.devcore.Git1")?;
        proxy
            .call(
                method,
                &(
                    environment.to_owned(),
                    job_id.to_owned(),
                    workspace.to_owned(),
                ),
            )
            .map_err(|error| format!("Git {label} submission failed: {error}"))
    }

    fn start_build(
        &self,
        environment: &str,
        job_id: &str,
        adapter: &str,
        workspace: &str,
    ) -> Result<String, String> {
        let proxy = self.proxy("org.devcore.Build1")?;
        proxy
            .call(
                "StartBuild",
                &(
                    environment.to_owned(),
                    job_id.to_owned(),
                    format!("studio-{adapter}-build"),
                    adapter.to_owned(),
                    workspace.to_owned(),
                ),
            )
            .map_err(|error| format!("build submission failed: {error}"))
    }

    fn start_unit_test(
        &self,
        environment: &str,
        job_id: &str,
        workspace: &str,
    ) -> Result<String, String> {
        let proxy = self.proxy("org.devcore.Test1")?;
        proxy
            .call(
                "StartTest",
                &(
                    environment.to_owned(),
                    job_id.to_owned(),
                    "studio-unit-test".to_owned(),
                    "unit".to_owned(),
                    "cargo".to_owned(),
                    vec!["test".to_owned()],
                    workspace.to_owned(),
                ),
            )
            .map_err(|error| format!("test submission failed: {error}"))
    }

    fn describe_job(&self, job_id: &str) -> Result<String, String> {
        let job_proxy = self.proxy("org.devcore.Job1")?;
        let (id, lifecycle, exit_code, output_truncated, duration_millis, error): (
            String,
            String,
            Option<i32>,
            bool,
            u64,
            Option<String>,
        ) = job_proxy
            .call("Status", &(job_id.to_owned(),))
            .map_err(|error| format!("job status failed: {error}"))?;
        let log_result: Result<(Vec<u8>, bool, u64), _> = job_proxy.call(
            "ReadLog",
            &(job_id.to_owned(), "stdout".to_owned(), 0_u64, 4096_u64),
        );
        let (bytes, end_of_file, total_bytes) = match log_result {
            Ok(log) => log,
            Err(_error) if lifecycle == "queued" || lifecycle == "running" => {
                (Vec::new(), false, 0)
            }
            Err(error) => return Err(format!("job log read failed: {error}")),
        };
        Ok(format_job_readout(
            &JobStatusView {
                id,
                lifecycle,
                exit_code,
                output_truncated,
                duration_millis,
                error,
            },
            &JobLogView {
                bytes,
                end_of_file,
                total_bytes,
            },
        ))
    }

    fn cancel_job(&self, job_id: &str) -> Result<(), String> {
        let proxy = self.proxy("org.devcore.Job1")?;
        let _: () = proxy
            .call("Cancel", &(job_id.to_owned(),))
            .map_err(|error| format!("job cancellation failed: {error}"))?;
        Ok(())
    }
}

const PROVISION_BUS_NAME: &str = "org.devcore.Provision1";
const PROVISION_OBJECT_PATH: &str = "/org/devcore/Provision";
const DISK_PROGRAM: &str = "/usr/bin/df";

struct ProvisionClient {
    connection: zbus::blocking::Connection,
}

impl ProvisionClient {
    fn connect() -> Result<Self, String> {
        zbus::blocking::Connection::session()
            .map(|connection| Self { connection })
            .map_err(|error| format!("provision service connection failed: {error}"))
    }

    fn proxy(&self) -> Result<zbus::blocking::Proxy<'static>, String> {
        zbus::blocking::Proxy::new(
            &self.connection,
            PROVISION_BUS_NAME,
            PROVISION_OBJECT_PATH,
            "org.devcore.Provision1",
        )
        .map_err(|error| format!("provision service proxy failed: {error}"))
    }

    fn plan(
        &self,
        bundle_ids: &[String],
        profile: &str,
        available_disk_bytes: u64,
        environment_image: &str,
    ) -> Result<String, String> {
        self.proxy()?
            .call(
                "Plan",
                &(
                    bundle_ids.to_vec(),
                    profile.to_owned(),
                    available_disk_bytes,
                    false,
                    environment_image.to_owned(),
                ),
            )
            .map_err(|error| format!("provision plan failed: {error}"))
    }

    fn start(
        &self,
        bundle_ids: &[String],
        profile: &str,
        available_disk_bytes: u64,
        environment_image: &str,
    ) -> Result<String, String> {
        self.proxy()?
            .call(
                "Start",
                &(
                    bundle_ids.to_vec(),
                    profile.to_owned(),
                    available_disk_bytes,
                    true,
                    environment_image.to_owned(),
                ),
            )
            .map_err(|error| format!("provision start failed: {error}"))
    }

    fn status(&self) -> Result<String, String> {
        self.proxy()?
            .call("StatusJson", &())
            .map_err(|error| format!("provision status failed: {error}"))
    }

    fn catalog(&self) -> Result<String, String> {
        self.proxy()?
            .call("CatalogJson", &())
            .map_err(|error| format!("provision catalog failed: {error}"))
    }

    fn cancel(&self, request_id: &str) -> Result<(), String> {
        let _: () = self
            .proxy()?
            .call("Cancel", &(request_id.to_owned(),))
            .map_err(|error| format!("provision cancellation failed: {error}"))?;
        Ok(())
    }
}

fn parse_provision_bundles(value: &str) -> Result<Vec<String>, String> {
    let mut bundles = Vec::new();
    for raw_value in value.split(',') {
        let value = raw_value.trim().to_ascii_lowercase();
        if value.is_empty() {
            continue;
        }
        let value = match value.as_str() {
            "android" => "android-sdk",
            other => other,
        };
        if !value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        }) {
            return Err(format!("invalid post-install bundle identifier {value:?}"));
        }
        if !bundles.iter().any(|bundle| bundle == value) {
            bundles.push(value.to_owned());
        }
    }
    if bundles.is_empty() {
        return Err("select at least one post-install bundle".to_owned());
    }
    if bundles.len() > 16 {
        return Err("select no more than 16 post-install bundles".to_owned());
    }
    Ok(bundles)
}

fn available_disk_bytes(workspace: &str) -> Result<u64, String> {
    let path = if !workspace.trim().is_empty()
        && Path::new(workspace).is_absolute()
        && Path::new(workspace).is_dir()
    {
        workspace.trim().to_owned()
    } else {
        "/".to_owned()
    };
    let output = Command::new(DISK_PROGRAM)
        .args(["-B1", "--output=avail", "--", &path])
        .output()
        .map_err(|error| format!("disk-space probe failed for {DISK_PROGRAM}: {error}"))?;
    if !output.status.success() {
        return Err(format!("disk-space probe failed with {}", output.status));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| line.trim().parse::<u64>().ok())
        .ok_or_else(|| "disk-space probe returned no numeric free-space value".to_owned())
}

fn update_provision_status(shell: &DevCoreShell, status: &str) {
    if let Ok(document) = serde_json::from_str::<serde_json::Value>(status)
        && let Some(request_id) = document
            .get("state")
            .and_then(|state| state.get("request_id"))
            .and_then(serde_json::Value::as_str)
    {
        shell.set_provisioning_request_id(request_id.into());
    }
    shell.set_provisioning_readout(compact_readout(status, MAX_SYSTEM_READOUT_CHARS).into());
}

static NEXT_STUDIO_JOB: AtomicU64 = AtomicU64::new(0);

fn next_studio_job(kind: &str) -> String {
    format!(
        "studio-{kind}-{}-{}",
        std::process::id(),
        NEXT_STUDIO_JOB.fetch_add(1, Ordering::Relaxed)
    )
}

fn validate_studio_environment(image: &str, workspace: &str) -> Result<(), String> {
    if image.trim().is_empty() {
        return Err("enter a digest-pinned OCI image before submitting work".to_owned());
    }
    if workspace.trim().is_empty() {
        return Err("enter an absolute workspace directory before submitting work".to_owned());
    }
    Ok(())
}

fn parse_studio_toolchains(value: &str) -> Result<Vec<String>, String> {
    let mut toolchains = Vec::new();
    for raw_value in value.split(',') {
        let value = raw_value.trim().to_ascii_lowercase();
        if value.is_empty() {
            continue;
        }
        let canonical = match value.as_str() {
            "rust" => "rust",
            "go" => "go",
            "python" => "python",
            "node" | "nodejs" => "node",
            "flutter" | "dart" => "flutter",
            "android" | "android-sdk" => "android-sdk",
            "java" => "java",
            "qt" => "qt",
            "cmake" | "c-cpp" => "cmake",
            _ => {
                return Err(format!(
                    "unsupported Studio toolchain {value:?}; choose rust, go, python, node, flutter, android-sdk, java, qt, or cmake"
                ));
            }
        };
        if !toolchains.iter().any(|toolchain| toolchain == canonical) {
            toolchains.push(canonical.to_owned());
        }
    }
    if toolchains.is_empty() {
        return Err("enter at least one Studio toolchain".to_owned());
    }
    Ok(toolchains)
}

fn parse_studio_build_adapter(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "cargo" | "go" | "cmake" | "gradle" | "flutter" | "qt" | "npm" | "pnpm" => Ok(value),
        _ => Err(format!(
            "unsupported Studio build adapter {value:?}; choose cargo, go, cmake, gradle, flutter, qt, npm, or pnpm"
        )),
    }
}

fn validate_studio_file_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("enter a workspace-relative file path before loading or saving".to_owned());
    }
    if Path::new(path).is_absolute() {
        return Err("Studio file paths must be relative to the workspace".to_owned());
    }
    Ok(())
}

fn validate_studio_directory(path: &str) -> Result<(), String> {
    if Path::new(path).is_absolute() {
        return Err("Studio explorer directories must be relative to the workspace".to_owned());
    }
    Ok(())
}

fn format_workspace_entries(entries: &[(String, bool, bool, u64)]) -> String {
    if entries.is_empty() {
        return "Workspace directory is empty".to_owned();
    }
    entries
        .iter()
        .map(|(path, directory, symlink, bytes)| {
            let marker = if *directory { "▾" } else { "·" };
            let link = if *symlink { " ↗" } else { "" };
            let size = if *directory || *symlink {
                String::new()
            } else {
                format!(" · {bytes} B")
            };
            format!("{marker} {path}{link}{size}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn launch_terminal(workspace: &str) -> Result<String, String> {
    let workspace = workspace.trim();
    if !workspace.is_empty() {
        let path = Path::new(workspace);
        if !path.is_absolute() || path == Path::new("/") || !path.is_dir() {
            return Err(format!(
                "terminal workspace must be an existing absolute directory: {workspace}"
            ));
        }
    }
    let mut command = Command::new(TERMINAL_PROGRAM);
    command.arg(TERMINAL_TITLE);
    if !workspace.is_empty() {
        command
            .arg(TERMINAL_WORKING_DIRECTORY_OPTION)
            .arg(workspace);
    }
    command
        .spawn()
        .map(|_| {
            if workspace.is_empty() {
                "Terminal launched · foot owns the interactive PTY".to_owned()
            } else {
                format!("Terminal launched in {workspace} · foot owns the interactive PTY")
            }
        })
        .map_err(|error| format!("terminal launch failed for {TERMINAL_PROGRAM}: {error}"))
}

fn first_available_program(programs: &[&'static str]) -> Option<&'static str> {
    programs
        .iter()
        .copied()
        .find(|program| Path::new(program).is_file())
}

fn launch_optional_app(app: &str) -> Result<String, String> {
    match app {
        "Browser" => {
            let Some(program) = first_available_program(BROWSER_PROGRAMS) else {
                return Err(
                    "Browser is not installed in the minimal image; add it as an optional user app"
                        .to_owned(),
                );
            };
            Command::new(program)
                .spawn()
                .map(|_| format!("Browser launched from {program}"))
                .map_err(|error| format!("browser launch failed for {program}: {error}"))
        }
        "Codex" => {
            let Some(program) = first_available_program(CODEX_PROGRAMS) else {
                return Err(
                    "Codex is not installed in the minimal image; install it from the terminal"
                        .to_owned(),
                );
            };
            Command::new(TERMINAL_PROGRAM)
                .args(["--title=DevCore Codex", "--", program])
                .spawn()
                .map(|_| format!("Codex launched in a dedicated terminal from {program}"))
                .map_err(|error| format!("Codex launch failed for {program}: {error}"))
        }
        other => Err(format!("unsupported optional app {other:?}")),
    }
}

fn extension_host_readout() -> String {
    ExtensionHost::discover().map_or_else(
        || {
            "VS Code-compatible host not detected · install code, code-oss, or codium as an optional user app"
                .to_owned()
        },
        |host| {
            format!(
                "VS Code-compatible host ready · {}",
                host.program().display()
            )
        },
    )
}

fn launch_extension_workspace(workspace: &str) -> Result<String, String> {
    let host = ExtensionHost::discover().ok_or_else(|| {
        "VS Code-compatible host not detected · install code, code-oss, or codium as an optional user app"
            .to_owned()
    })?;
    let command = host
        .open_workspace_command(Path::new(workspace))
        .map_err(|error| error.to_string())?;
    Command::new(&command[0])
        .args(&command[1..])
        .spawn()
        .map(|_| format!("Opened workspace in {}", host.program().display()))
        .map_err(|error| format!("VS Code-compatible host launch failed: {error}"))
}

fn launch_extension_terminal_command(command: &[String]) -> Result<String, String> {
    Command::new(TERMINAL_PROGRAM)
        .args(["--title=DevCore Extensions", "--"])
        .args(command)
        .spawn()
        .map(|_| "Extension command launched in a bounded terminal session".to_owned())
        .map_err(|error| format!("extension command launch failed for {TERMINAL_PROGRAM}: {error}"))
}

fn install_extension(source: &str) -> Result<String, String> {
    let host = ExtensionHost::discover().ok_or_else(|| {
        "VS Code-compatible host not detected · install code, code-oss, or codium as an optional user app"
            .to_owned()
    })?;
    let command = host
        .install_command(source)
        .map_err(|error| error.to_string())?;
    launch_extension_terminal_command(&command)
}

fn list_extensions() -> Result<String, String> {
    let host = ExtensionHost::discover().ok_or_else(|| {
        "VS Code-compatible host not detected · install code, code-oss, or codium as an optional user app"
            .to_owned()
    })?;
    launch_extension_terminal_command(&host.list_command())
}

fn uninstall_extension(identifier: &str) -> Result<String, String> {
    let host = ExtensionHost::discover().ok_or_else(|| {
        "VS Code-compatible host not detected · install code, code-oss, or codium as an optional user app"
            .to_owned()
    })?;
    let command = host
        .uninstall_command(identifier)
        .map_err(|error| error.to_string())?;
    launch_extension_terminal_command(&command)
}

fn clamp_workspace_index(value: i32) -> i32 {
    value.clamp(1, 4)
}

fn handle_launcher_action(shell: &DevCoreShell, action: &str) {
    match action {
        "Terminal" => match launch_terminal(&shell.get_workspace_directory()) {
            Ok(message) => shell.set_status_message(message.into()),
            Err(error) => shell.set_status_message(error.into()),
        },
        "Studio" => {
            shell.set_launcher_open(false);
            shell.set_launcher_query("".into());
            shell.set_studio_open(true);
            shell.set_status_message("Studio opened from the launcher".into());
        }
        "Resources" => {
            shell.set_launcher_open(false);
            shell.set_launcher_query("".into());
            refresh_shell(shell);
        }
        "Browser" | "Codex" => match launch_optional_app(action) {
            Ok(message) => shell.set_status_message(message.into()),
            Err(error) => shell.set_status_message(error.into()),
        },
        other => shell.set_status_message(format!("Unknown launcher action: {other}").into()),
    }
}

fn format_job_readout(status: &JobStatusView, log: &JobLogView) -> String {
    let exit = status
        .exit_code
        .map_or_else(|| "pending".to_owned(), |code| code.to_string());
    let log_text = String::from_utf8_lossy(&log.bytes).trim().to_owned();
    let log_state = if log.end_of_file {
        "complete"
    } else {
        "partial"
    };
    let detail = match (status.error.as_deref(), log_text.is_empty()) {
        (Some(message), true) => format!(" · error: {message}"),
        (Some(message), false) => format!(" · error: {message}\n{log_text}"),
        (None, true) => String::new(),
        (None, false) => format!("\n{log_text}"),
    };
    format!(
        "{} · {} · exit {exit} · {} ms · log {} bytes ({log_state}){}{}",
        status.id,
        status.lifecycle,
        status.duration_millis,
        log.total_bytes,
        if status.output_truncated {
            " · output truncated"
        } else {
            ""
        },
        detail,
    )
}

fn handle_studio_action(shell: &DevCoreShell, action: &str) {
    let environment = shell.get_environment_name().to_string();
    let image = shell.get_environment_image().to_string();
    let toolchains_input = shell.get_environment_toolchains().to_string();
    let build_adapter = shell.get_build_adapter().to_string();
    let workspace = shell.get_workspace_directory().to_string();
    let mut submitted_job = None;
    let mut submitted_git_job = None;
    let result = match action {
        "Register" => validate_studio_environment(&image, &workspace)
            .and_then(|()| parse_studio_toolchains(&toolchains_input))
            .and_then(|toolchains| {
                WorkClient::connect()?
                    .define_environment(&environment, &image, &workspace, &toolchains)
                    .map(|()| format!("Environment registered: {environment}"))
            }),
        "ListFiles" => validate_studio_environment(&image, &workspace)
            .and_then(|()| {
                let directory = shell.get_explorer_directory().to_string();
                validate_studio_directory(&directory)?;
                WorkClient::connect()?.list_workspace(&environment, &directory)
            })
            .map(|entries| {
                let directory = shell.get_explorer_directory().to_string();
                let listing = format_workspace_entries(&entries);
                shell.set_explorer_entries(listing.into());
                shell.set_explorer_readout(
                    format!(
                        "{} entries · {}",
                        entries.len(),
                        if directory.is_empty() {
                            "workspace root"
                        } else {
                            directory.as_str()
                        }
                    )
                    .into(),
                );
                format!("Workspace listing refreshed · {} entries", entries.len())
            }),
        "ReadFile" => validate_studio_environment(&image, &workspace)
            .and_then(|()| {
                let path = shell.get_file_path().to_string();
                validate_studio_file_path(&path)?;
                WorkClient::connect()?.read_workspace_file(&environment, &path)
            })
            .map(|(path, contents)| {
                shell.set_file_path(path.clone().into());
                shell.set_file_content(contents.clone().into());
                shell.set_file_dirty(false);
                shell.set_file_readout(
                    format!(
                        "Loaded {path} · {} bytes · editable UTF-8 text",
                        contents.len()
                    )
                    .into(),
                );
                format!("Loaded workspace file: {path}")
            }),
        "WriteFile" => validate_studio_environment(&image, &workspace)
            .and_then(|()| {
                let path = shell.get_file_path().to_string();
                let contents = shell.get_file_content().to_string();
                validate_studio_file_path(&path)?;
                WorkClient::connect()?.write_workspace_file(&environment, &path, &contents)
            })
            .map(|(path, bytes)| {
                shell.set_file_path(path.clone().into());
                shell.set_file_dirty(false);
                shell.set_file_readout(
                    format!("Saved {path} · {bytes} bytes · atomic workspace update").into(),
                );
                format!("Saved workspace file: {path}")
            }),
        "GitStatus" => validate_studio_environment(&image, &workspace).and_then(|()| {
            let job_id = next_studio_job("git-status");
            let toolchains = parse_studio_toolchains(&toolchains_input)?;
            let client = WorkClient::connect()?;
            client.ensure_environment(&environment, &image, &workspace, &toolchains)?;
            let result = client
                .start_git_status(&environment, &job_id, &workspace)
                .map(|accepted| format!("Git status job accepted: {accepted}"));
            if result.is_ok() {
                submitted_git_job = Some(job_id);
            }
            result
        }),
        "GitDiff" => validate_studio_environment(&image, &workspace).and_then(|()| {
            let job_id = next_studio_job("git-diff");
            let toolchains = parse_studio_toolchains(&toolchains_input)?;
            let client = WorkClient::connect()?;
            client.ensure_environment(&environment, &image, &workspace, &toolchains)?;
            let result = client
                .start_git_diff(&environment, &job_id, &workspace)
                .map(|accepted| format!("Git diff job accepted: {accepted}"));
            if result.is_ok() {
                submitted_git_job = Some(job_id);
            }
            result
        }),
        "GitHistory" => validate_studio_environment(&image, &workspace).and_then(|()| {
            let job_id = next_studio_job("git-history");
            let toolchains = parse_studio_toolchains(&toolchains_input)?;
            let client = WorkClient::connect()?;
            client.ensure_environment(&environment, &image, &workspace, &toolchains)?;
            let result = client
                .start_git_history(&environment, &job_id, &workspace)
                .map(|accepted| format!("Git history job accepted: {accepted}"));
            if result.is_ok() {
                submitted_git_job = Some(job_id);
            }
            result
        }),
        "RefreshGit" => {
            let job_id = shell.get_git_job_id().to_string();
            if job_id.is_empty() {
                Err("no Git status job has been submitted".to_owned())
            } else {
                WorkClient::connect().and_then(|client| {
                    client.describe_job(&job_id).inspect(|readout| {
                        shell.set_git_readout(readout.clone().into());
                    })
                })
            }
        }
        "Build" => validate_studio_environment(&image, &workspace).and_then(|()| {
            let job_id = next_studio_job("build");
            let toolchains = parse_studio_toolchains(&toolchains_input)?;
            let adapter = parse_studio_build_adapter(&build_adapter)?;
            let client = WorkClient::connect()?;
            client.ensure_environment(&environment, &image, &workspace, &toolchains)?;
            let result = client
                .start_build(&environment, &job_id, &adapter, &workspace)
                .map(|accepted| format!("{} build job accepted: {accepted}", adapter));
            if result.is_ok() {
                submitted_job = Some(job_id);
            }
            result
        }),
        "Test" => validate_studio_environment(&image, &workspace).and_then(|()| {
            let job_id = next_studio_job("test");
            let toolchains = parse_studio_toolchains(&toolchains_input)?;
            let client = WorkClient::connect()?;
            client.ensure_environment(&environment, &image, &workspace, &toolchains)?;
            let result = client
                .start_unit_test(&environment, &job_id, &workspace)
                .map(|accepted| format!("Unit-test job accepted: {accepted}"));
            if result.is_ok() {
                submitted_job = Some(job_id);
            }
            result
        }),
        "RefreshJob" => {
            let job_id = shell.get_job_id().to_string();
            if job_id.is_empty() {
                Err("no Studio job has been submitted".to_owned())
            } else {
                WorkClient::connect().and_then(|client| client.describe_job(&job_id))
            }
        }
        "CancelJob" => {
            let job_id = shell.get_job_id().to_string();
            if job_id.is_empty() {
                Err("no Studio job has been submitted".to_owned())
            } else {
                WorkClient::connect()
                    .and_then(|client| client.cancel_job(&job_id))
                    .map(|()| format!("Cancellation requested: {job_id}"))
            }
        }
        "Terminal" => launch_terminal(&workspace),
        "OpenExtensionHost" => launch_extension_workspace(&workspace),
        "InstallExtension" => {
            let source = shell.get_extension_source().to_string();
            install_extension(&source)
        }
        "ListExtensions" => list_extensions(),
        "UninstallExtension" => {
            let identifier = shell.get_extension_source().to_string();
            uninstall_extension(&identifier)
        }
        "ProvisionCatalog" => ProvisionClient::connect().and_then(|client| client.catalog()),
        "ProvisionStatus" => ProvisionClient::connect().and_then(|client| {
            client
                .status()
                .inspect(|status| update_provision_status(shell, status))
        }),
        "ProvisionPlan" => ProvisionClient::connect().and_then(|client| {
            let bundles = parse_provision_bundles(&shell.get_provisioning_bundles())?;
            let disk = available_disk_bytes(&workspace)?;
            client
                .plan(
                    &bundles,
                    shell.get_profile().as_str(),
                    disk,
                    shell.get_provisioning_image().as_str(),
                )
                .map(|plan| {
                    update_provision_status(shell, &plan);
                    format!("Post-install plan ready · {} bundles · {} free bytes observed", bundles.len(), disk)
                })
        }),
        "ProvisionStart" => ProvisionClient::connect().and_then(|client| {
            let bundles = parse_provision_bundles(&shell.get_provisioning_bundles())?;
            let disk = available_disk_bytes(&workspace)?;
            client
                .start(
                    &bundles,
                    shell.get_profile().as_str(),
                    disk,
                    shell.get_provisioning_image().as_str(),
                )
                .map(|request_id| {
                    shell.set_provisioning_request_id(request_id.clone().into());
                    shell.set_provisioning_readout(
                        format!("Downloads accepted after explicit confirmation · {request_id}").into(),
                    );
                    "Post-install downloads accepted · connect Wi-Fi if status remains waiting-network"
                        .to_owned()
                })
        }),
        "ProvisionCancel" => {
            let request_id = shell.get_provisioning_request_id().to_string();
            if request_id.is_empty() {
                Err("no post-install provisioning request is active".to_owned())
            } else {
                ProvisionClient::connect().and_then(|client| {
                    client
                        .cancel(&request_id)
                        .map(|()| format!("Post-install cancellation requested: {request_id}"))
                })
            }
        }
        other => Err(format!("unsupported Studio action {other:?}")),
    };

    match result {
        Ok(message) => {
            if let Some(job_id) = submitted_job {
                shell.set_job_id(job_id.into());
            }
            if let Some(job_id) = submitted_git_job {
                shell.set_git_job_id(job_id.into());
            }
            shell.set_job_readout(message.clone().into());
            shell.set_status_message(message.into());
        }
        Err(error) => {
            if matches!(
                action,
                "GitStatus" | "GitDiff" | "GitHistory" | "RefreshGit"
            ) {
                shell.set_git_readout(error.clone().into());
            }
            shell.set_job_readout(error.clone().into());
            shell.set_status_message(format!("Studio action unavailable: {error}").into());
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let shell = DevCoreShell::new()?;
    shell.set_extension_host_readout(extension_host_readout().into());
    spawn_resource_signal_listener(shell.as_weak());
    refresh_shell(&shell);
    if live_installer_should_open() {
        match Command::new("/usr/bin/devcore-installer")
            .args(["--mode", "live", "--open"])
            .spawn()
        {
            Ok(_) => shell.set_status_message(
                "Live installer opened · the desktop remains available during installation".into(),
            ),
            Err(error) => {
                shell.set_status_message(format!("Live installer could not open: {error}").into())
            }
        }
    }

    let weak_shell = shell.as_weak();
    shell.on_refresh_telemetry(move || {
        if let Some(shell) = weak_shell.upgrade() {
            refresh_shell(&shell);
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_toggle_theme(move || {
        if let Some(shell) = weak_shell.upgrade() {
            shell.set_dark_mode(!shell.get_dark_mode());
            shell.set_status_message("Theme updated without starting extra services".into());
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_toggle_launcher(move || {
        if let Some(shell) = weak_shell.upgrade() {
            let open = !shell.get_launcher_open();
            shell.set_launcher_open(open);
            if open {
                shell.set_launcher_query("".into());
                shell.set_status_message(
                    "Launcher open · Terminal, Studio, Browser, Codex, and Resources".into(),
                );
            }
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_select_workspace(move |index| {
        if let Some(shell) = weak_shell.upgrade() {
            let index = clamp_workspace_index(index);
            shell.set_workspace_index(index);
            shell.set_status_message(
                format!(
                    "Workspace {index} selected · compositor-level window routing remains staged"
                )
                .into(),
            );
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_launch_app(move |action| {
        if let Some(shell) = weak_shell.upgrade() {
            handle_launcher_action(&shell, action.as_str());
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_open_studio(move || {
        if let Some(shell) = weak_shell.upgrade() {
            shell.set_studio_open(true);
            if shell.get_environment_image().trim().is_empty()
                || shell.get_workspace_directory().trim().is_empty()
            {
                shell.set_status_message(
                    "Studio opened · register a digest-pinned environment to browse files".into(),
                );
            } else {
                handle_studio_action(&shell, "ListFiles");
            }
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_launch_terminal(move || {
        if let Some(shell) = weak_shell.upgrade() {
            let workspace = shell.get_workspace_directory().to_string();
            match launch_terminal(&workspace) {
                Ok(message) => shell.set_status_message(message.into()),
                Err(error) => shell.set_status_message(error.into()),
            }
        }
    });

    let weak_shell = shell.as_weak();
    shell.on_studio_action(move |action| {
        if let Some(shell) = weak_shell.upgrade() {
            handle_studio_action(&shell, action.as_str());
        }
    });

    shell.run()?;
    Ok(())
}

fn live_installer_should_open() -> bool {
    let command_line = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    live_installer_should_open_from(&command_line)
}

fn live_installer_should_open_from(command_line: &str) -> bool {
    command_line
        .split_ascii_whitespace()
        .any(|argument| argument == "devcore.live=1")
        && command_line
            .split_ascii_whitespace()
            .any(|argument| argument == "devcore.installer=auto")
}

fn refresh_shell(shell: &DevCoreShell) {
    match collect_shell_snapshot() {
        Ok((snapshot, status)) => apply_shell_snapshot(shell, &snapshot, status),
        Err(error) => shell.set_status_message(format!("Telemetry unavailable: {error}").into()),
    }
    spawn_system_surface_refresh(shell.as_weak());
}

fn apply_shell_snapshot(shell: &DevCoreShell, snapshot: &ResourceSnapshot, status: &str) {
    let telemetry = ShellTelemetry::from(snapshot);
    shell.set_profile(telemetry.profile.into());
    shell.set_cpu_readout(telemetry.cpu_readout.into());
    shell.set_memory_readout(telemetry.memory_readout.into());
    shell.set_pressure_readout(telemetry.pressure_readout.into());
    shell.set_power_readout(telemetry.power_readout.into());
    shell.set_status_message(status.into());
}

fn spawn_resource_signal_listener(shell: slint::Weak<DevCoreShell>) {
    let _ = thread::Builder::new()
        .name("devcore-resource-events".to_owned())
        .spawn(move || {
            let Ok(connection) = zbus::blocking::Connection::system() else {
                return;
            };
            let Ok(proxy) = zbus::blocking::Proxy::new(
                &connection,
                "org.devcore.Resource1",
                "/org/devcore/Resource",
                "org.devcore.Resource1",
            ) else {
                return;
            };
            // Activate the system service through a read-only method before
            // installing the match rule. This avoids a startup race where a
            // not-yet-activated service would reject signal subscription.
            let Ok::<String, _>(_) = proxy.call("SnapshotJson", &()) else {
                return;
            };
            let Ok(signals) = proxy.receive_signal("SnapshotChanged") else {
                return;
            };

            for message in signals {
                let Ok(snapshot_json) = message.body().deserialize::<String>() else {
                    continue;
                };
                let Ok(snapshot) = snapshot_from_json(&snapshot_json) else {
                    continue;
                };
                let weak_shell = shell.clone();
                if slint::invoke_from_event_loop(move || {
                    if let Some(shell) = weak_shell.upgrade() {
                        apply_shell_snapshot(
                            &shell,
                            &snapshot,
                            "Resource event received · dashboard updated",
                        );
                    }
                })
                .is_err()
                {
                    break;
                }
            }
        });
}

fn collect_shell_snapshot() -> Result<(ResourceSnapshot, &'static str), String> {
    match collect_resource_service_snapshot() {
        Ok(snapshot) => Ok((
            snapshot,
            "Resource service snapshot · explicit refresh plus signal updates",
        )),
        Err(service_error) => collect_snapshot()
            .map(|snapshot| (snapshot, "Direct kernel snapshot · resource service unavailable"))
            .map_err(|snapshot_error| {
                format!(
                    "resource service unavailable ({service_error}); direct snapshot failed: {snapshot_error}"
                )
            }),
    }
}

fn collect_resource_service_snapshot() -> Result<ResourceSnapshot, String> {
    let connection = zbus::blocking::Connection::system().map_err(|error| error.to_string())?;
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        "org.devcore.Resource1",
        "/org/devcore/Resource",
        "org.devcore.Resource1",
    )
    .map_err(|error| error.to_string())?;

    let snapshot_json: String = proxy
        .call("Refresh", &())
        .map_err(|error| error.to_string())?;
    snapshot_from_json(&snapshot_json)
}

fn spawn_system_surface_refresh(shell: slint::Weak<DevCoreShell>) {
    let _ = thread::Builder::new()
        .name("devcore-system-surfaces".to_owned())
        .spawn(move || {
            let readouts = collect_system_readouts();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(shell) = shell.upgrade() {
                    shell.set_hardware_readout(readouts.hardware.into());
                    shell.set_update_readout(readouts.update.into());
                }
            });
        });
}

fn collect_system_readouts() -> SystemReadouts {
    let connection = match zbus::blocking::Connection::system() {
        Ok(connection) => connection,
        Err(error) => {
            let detail = compact_readout(&error.to_string(), 96);
            return SystemReadouts {
                hardware: format!("Hardware service unavailable · {detail}"),
                update: format!("Update service unavailable · {detail}"),
            };
        }
    };

    let hardware = match zbus::blocking::Proxy::new(
        &connection,
        HARDWARE_BUS_NAME,
        HARDWARE_OBJECT_PATH,
        HARDWARE_BUS_NAME,
    ) {
        Ok(proxy) => {
            let snapshot_result: Result<String, _> = proxy.call("Refresh", &());
            match snapshot_result {
                Ok(snapshot_json) => hardware_snapshot_from_json(&snapshot_json)
                    .map(|snapshot| format_hardware_readout(&snapshot))
                    .unwrap_or_else(|error| format!("Hardware snapshot unavailable · {error}")),
                Err(error) => format!(
                    "Hardware service unavailable · {}",
                    compact_readout(&error.to_string(), 96)
                ),
            }
        }
        Err(error) => format!(
            "Hardware service unavailable · {}",
            compact_readout(&error.to_string(), 96)
        ),
    };

    let update = match zbus::blocking::Proxy::new(
        &connection,
        UPDATE_BUS_NAME,
        UPDATE_OBJECT_PATH,
        UPDATE_BUS_NAME,
    ) {
        Ok(proxy) => {
            let channel = proxy.get_property::<String>("Channel");
            let status: Result<String, _> = proxy.call("StatusJson", &());
            match (channel, status) {
                (Ok(channel), Ok(status)) => format_update_readout(&channel, &status),
                (Err(error), _) => format!(
                    "Update service unavailable · {}",
                    compact_readout(&error.to_string(), 96)
                ),
                (_, Err(error)) => format!(
                    "Update status unavailable · {}",
                    compact_readout(&error.to_string(), 96)
                ),
            }
        }
        Err(error) => format!(
            "Update service unavailable · {}",
            compact_readout(&error.to_string(), 96)
        ),
    };

    SystemReadouts { hardware, update }
}

fn profile_from_str(value: &str) -> Result<devcore_core::MachineProfile, String> {
    match value {
        "low" => Ok(devcore_core::MachineProfile::Low),
        "balanced" => Ok(devcore_core::MachineProfile::Balanced),
        "standard" => Ok(devcore_core::MachineProfile::Standard),
        "workstation" => Ok(devcore_core::MachineProfile::Workstation),
        _ => Err(format!(
            "resource service returned unknown profile {value:?}"
        )),
    }
}

fn kib_to_gib_rounded(kib: u64) -> u64 {
    const KIB_PER_GIB: u64 = 1024 * 1024;
    (kib + (KIB_PER_GIB / 2)) / KIB_PER_GIB
}

fn power_readout(snapshot: &ResourceSnapshot) -> String {
    match (snapshot.power.battery_percent, snapshot.power.ac_online) {
        (Some(percent), Some(true)) => format!("{percent}% · charging"),
        (Some(percent), Some(false)) => format!("{percent}% · battery"),
        (Some(percent), None) => format!("{percent}% battery"),
        (None, Some(true)) => "AC online".to_owned(),
        (None, Some(false)) => "AC offline".to_owned(),
        (None, None) => "Power unavailable".to_owned(),
    }
}

fn pressure_readout(cpu: Option<PressureMetrics>, memory: Option<PressureMetrics>) -> String {
    match (cpu, memory) {
        (Some(cpu), Some(memory)) => format!(
            "CPU {} · MEM {}",
            milli_percent(cpu.some_avg10_milli_percent),
            milli_percent(memory.some_avg10_milli_percent)
        ),
        (Some(cpu), None) => format!("CPU {}", milli_percent(cpu.some_avg10_milli_percent)),
        (None, Some(memory)) => format!("MEM {}", milli_percent(memory.some_avg10_milli_percent)),
        (None, None) => "PSI unavailable".to_owned(),
    }
}

fn milli_percent(value: u32) -> String {
    format!("{}.{:01}%", value / 1000, (value % 1000) / 100)
}

#[cfg(test)]
mod tests {
    use super::{
        BROWSER_PROGRAMS, CODEX_PROGRAMS, JobLogView, JobStatusView, ShellTelemetry,
        TERMINAL_PROGRAM, TERMINAL_TITLE, TERMINAL_WORKING_DIRECTORY_OPTION, clamp_workspace_index,
        compact_readout, first_available_program, format_hardware_readout, format_job_readout,
        format_update_readout, format_workspace_entries, hardware_snapshot_from_json,
        kib_to_gib_rounded, live_installer_should_open_from, milli_percent,
        parse_provision_bundles, parse_studio_build_adapter, parse_studio_toolchains,
        power_readout, pressure_readout, profile_from_str, snapshot_from_json,
        validate_studio_directory,
    };
    use devcore_core::{
        HardwareCapacity, MachineProfile, PowerState, PressureMetrics, ResourceSnapshot,
    };

    #[test]
    fn shell_view_model_preserves_kernel_snapshot_meaning() {
        let snapshot = ResourceSnapshot {
            capacity: HardwareCapacity::new(4, 8 * 1024 * 1024),
            initial_profile: MachineProfile::Balanced,
            memory_available_kib: Some(3 * 1024 * 1024),
            cpu_pressure: Some(PressureMetrics {
                some_avg10_milli_percent: 1_200,
                full_avg10_milli_percent: Some(100),
            }),
            memory_pressure: Some(PressureMetrics {
                some_avg10_milli_percent: 400,
                full_avg10_milli_percent: Some(0),
            }),
            io_pressure: None,
            power: PowerState {
                battery_present: true,
                battery_percent: Some(73),
                ac_online: Some(false),
            },
        };

        assert_eq!(
            ShellTelemetry::from(&snapshot),
            ShellTelemetry {
                cpu_readout: "4 logical CPUs".to_owned(),
                memory_readout: "3 / 8 GiB free".to_owned(),
                power_readout: "73% · battery".to_owned(),
                pressure_readout: "CPU 1.2% · MEM 0.4%".to_owned(),
                profile: "balanced".to_owned(),
            }
        );
    }

    #[test]
    fn presentation_helpers_round_and_degrade_without_false_values() {
        assert_eq!(kib_to_gib_rounded(1_572_864), 2);
        assert_eq!(milli_percent(9_720), "9.7%");
        assert_eq!(
            pressure_readout(
                None,
                Some(PressureMetrics {
                    some_avg10_milli_percent: 0,
                    full_avg10_milli_percent: None,
                })
            ),
            "MEM 0.0%"
        );

        let snapshot = ResourceSnapshot {
            capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
            initial_profile: MachineProfile::Low,
            memory_available_kib: None,
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        };
        assert_eq!(power_readout(&snapshot), "Power unavailable");
    }

    #[test]
    fn resource_service_profiles_are_strictly_mapped() {
        assert_eq!(
            profile_from_str("balanced").unwrap(),
            MachineProfile::Balanced
        );
        assert!(profile_from_str("unknown").is_err());
    }

    #[test]
    fn workspace_selector_is_bounded_to_four_desktops() {
        assert_eq!(clamp_workspace_index(-1), 1);
        assert_eq!(clamp_workspace_index(1), 1);
        assert_eq!(clamp_workspace_index(4), 4);
        assert_eq!(clamp_workspace_index(99), 4);
    }

    #[test]
    fn install_boot_entry_opens_the_native_installer_only_in_live_mode() {
        assert!(live_installer_should_open_from(
            "rd.live.image devcore.live=1 devcore.installer=auto quiet"
        ));
        assert!(!live_installer_should_open_from(
            "devcore.live=1 devcore.installer=manual"
        ));
        assert!(!live_installer_should_open_from("devcore.installer=auto"));
    }

    #[test]
    fn optional_launchers_use_only_fixed_paths() {
        assert!(
            first_available_program(BROWSER_PROGRAMS)
                .is_none_or(|path| { BROWSER_PROGRAMS.contains(&path) })
        );
        assert!(
            first_available_program(CODEX_PROGRAMS)
                .is_none_or(|path| { CODEX_PROGRAMS.contains(&path) })
        );
    }

    #[test]
    fn resource_signal_payload_decodes_all_dashboard_fields() {
        let snapshot = snapshot_from_json(
            r#"{
                "profile":"balanced",
                "logical_cpus":4,
                "memory_total_kib":8388608,
                "memory_available_kib":3145728,
                "pressure":{
                    "cpu":{"some_avg10_milli_percent":1200,"full_avg10_milli_percent":100},
                    "memory":{"some_avg10_milli_percent":400,"full_avg10_milli_percent":0},
                    "io":null
                },
                "power":{"battery_present":true,"battery_percent":73,"ac_online":false}
            }"#,
        )
        .unwrap();

        assert_eq!(snapshot.initial_profile, MachineProfile::Balanced);
        assert_eq!(snapshot.capacity.logical_cpus, 4);
        assert_eq!(snapshot.memory_available_kib, Some(3_145_728));
        assert_eq!(snapshot.power.battery_percent, Some(73));
        assert_eq!(
            snapshot
                .cpu_pressure
                .map(|pressure| pressure.some_avg10_milli_percent),
            Some(1_200)
        );
        assert!(snapshot.io_pressure.is_none());
    }

    #[test]
    fn hardware_payload_formats_bounded_inventory_for_the_overview() {
        let snapshot = hardware_snapshot_from_json(
            r#"{
                "cpu_model":"AMD Ryzen 7 7840U",
                "logical_cpus":8,
                "memory_total_kib":16777216,
                "battery_present":true,
                "battery_percent":72,
                "ac_online":true,
                "network_interfaces":["enp1s0","lo"],
                "gpu_devices":1
            }"#,
        )
        .unwrap();

        assert_eq!(
            format_hardware_readout(&snapshot),
            "AMD Ryzen 7 7840U · 8 CPU · 16 GiB · 1 GPU · 2 net · battery 72% · AC online"
        );
        assert!(hardware_snapshot_from_json(&"x".repeat(64 * 1024 + 1)).is_err());
    }

    #[test]
    fn update_readout_is_single_line_and_status_is_bounded() {
        assert_eq!(
            format_update_readout("alpha", "deployment ok\nno reboot requested"),
            "channel alpha · deployment ok no reboot requested"
        );
        assert_eq!(
            format_update_readout("stable", ""),
            "channel stable · status query returned no output"
        );
        assert_eq!(compact_readout("a\nb", 10), "a b");
        assert!(compact_readout(&"x".repeat(200), 10).ends_with('…'));
    }

    #[test]
    fn studio_toolchain_input_is_canonicalized_and_deduplicated() {
        assert_eq!(
            parse_studio_toolchains(" Rust, nodejs, android, node, c-cpp ").unwrap(),
            vec![
                "rust".to_owned(),
                "node".to_owned(),
                "android-sdk".to_owned(),
                "cmake".to_owned()
            ]
        );
        assert!(parse_studio_toolchains(" ").is_err());
        assert!(parse_studio_toolchains("rust, wasm").is_err());
    }

    #[test]
    fn studio_build_adapter_input_is_a_fixed_allowlist() {
        assert_eq!(
            parse_studio_build_adapter("  PNPM ").unwrap(),
            "pnpm".to_owned()
        );
        assert!(parse_studio_build_adapter("sh -c cargo").is_err());
        assert!(parse_studio_build_adapter("").is_err());
    }

    #[test]
    fn post_install_bundle_input_is_bounded_and_canonicalized() {
        assert_eq!(
            parse_provision_bundles(" Rust, android, chrome, rust ").unwrap(),
            vec![
                "rust".to_owned(),
                "android-sdk".to_owned(),
                "chrome".to_owned()
            ]
        );
        assert!(parse_provision_bundles(" ").is_err());
        assert!(parse_provision_bundles("chrome; rm").is_err());
    }

    #[test]
    fn workspace_listing_formats_directories_files_and_links_without_fake_paths() {
        let listing = format_workspace_entries(&[
            ("src".to_owned(), true, false, 0),
            ("README.md".to_owned(), false, false, 42),
            ("link".to_owned(), false, true, 0),
        ]);
        assert_eq!(listing, "▾ src\n· README.md · 42 B\n· link ↗");
        assert!(validate_studio_directory("").is_ok());
        assert!(validate_studio_directory("src").is_ok());
        assert!(validate_studio_directory("/etc").is_err());
    }

    #[test]
    fn job_readout_keeps_status_metadata_and_bounded_log_text() {
        assert_eq!(
            format_job_readout(
                &JobStatusView {
                    id: "job-1".to_owned(),
                    lifecycle: "succeeded".to_owned(),
                    exit_code: Some(0),
                    output_truncated: false,
                    duration_millis: 12,
                    error: None,
                },
                &JobLogView {
                    bytes: b"cargo ok\n".to_vec(),
                    end_of_file: true,
                    total_bytes: 9,
                },
            ),
            "job-1 · succeeded · exit 0 · 12 ms · log 9 bytes (complete)\ncargo ok"
        );
        assert!(
            format_job_readout(
                &JobStatusView {
                    id: "job-2".to_owned(),
                    lifecycle: "failed".to_owned(),
                    exit_code: Some(1),
                    output_truncated: true,
                    duration_millis: 4,
                    error: Some("command failed".to_owned()),
                },
                &JobLogView {
                    bytes: Vec::new(),
                    end_of_file: true,
                    total_bytes: 0,
                },
            )
            .contains("output truncated · error: command failed")
        );
    }

    #[test]
    fn terminal_launch_contract_uses_a_fixed_absolute_program() {
        assert_eq!(TERMINAL_PROGRAM, "/usr/bin/foot");
        assert!(TERMINAL_PROGRAM.starts_with('/'));
        assert!(!TERMINAL_PROGRAM.contains(' '));
        assert_eq!(TERMINAL_TITLE, "--title=DevCore Terminal");
        assert_eq!(TERMINAL_WORKING_DIRECTORY_OPTION, "--working-directory");
    }
}
