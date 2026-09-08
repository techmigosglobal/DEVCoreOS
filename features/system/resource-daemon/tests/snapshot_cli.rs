//! End-to-end coverage for the one-shot `devcored` command.

use std::process::Command;

#[test]
fn emits_a_json_resource_snapshot_from_the_running_kernel() {
    let output = Command::new(env!("CARGO_BIN_EXE_devcored"))
        .arg("--json")
        .output()
        .expect("devcored should start");

    assert!(
        output.status.success(),
        "devcored failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    assert!(stdout.starts_with("{\"profile\":"));
    assert!(stdout.contains("\"memory_total_kib\":"));
    assert!(stdout.contains("\"pressure\":"));
}
