#![forbid(unsafe_code)]
//! Read-only Update and Release Management D-Bus service.

use std::{env, future::pending, process::ExitCode};

use devcore_execution::{
    CancellationToken, ExecutionEngine, ExecutionLimits, ExecutionRequest, JobState,
};
use devcore_update::{BOOTC_PATH, ReleaseChannel, UpdateOperation, UpdatePlan, UpdateTarget};
use zbus::interface;

const STATUS_OUTPUT_LIMIT: usize = 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("devcore-updated: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    run_daemon()
}

#[derive(Debug, Default)]
struct UpdateService;

#[interface(name = "org.devcore.Update1")]
impl UpdateService {
    /// Returns the configured default channel without applying it.
    #[zbus(property)]
    fn channel(&self) -> String {
        env::var("DEVCORE_UPDATE_CHANNEL")
            .ok()
            .and_then(|value| ReleaseChannel::parse(&value).ok())
            .unwrap_or(ReleaseChannel::Alpha)
            .as_str()
            .to_owned()
    }

    /// Runs the fixed bootc status query with bounded output.
    fn status_json(&self) -> zbus::fdo::Result<String> {
        let program = status_program();
        let request = ExecutionRequest::new("update-status", program, ["status", "--json"]);
        let result = ExecutionEngine::new()
            .run(
                &request,
                &CancellationToken::new(),
                ExecutionLimits {
                    max_output_bytes: STATUS_OUTPUT_LIMIT,
                },
            )
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        if result.state != JobState::Succeeded {
            return Err(zbus::fdo::Error::Failed(format!(
                "bootc status failed with state {:?} and exit code {:?}",
                result.state, result.exit_code
            )));
        }
        if result.output_truncated {
            return Err(zbus::fdo::Error::Failed(
                "bootc status output exceeded the Update1 limit".to_owned(),
            ));
        }
        String::from_utf8(result.stdout).map_err(|_| {
            zbus::fdo::Error::Failed("bootc status returned non-UTF-8 output".to_owned())
        })
    }

    /// Returns a validated direct-argv plan without executing it.
    fn plan(&self, operation: &str, channel: &str, image: &str) -> zbus::fdo::Result<String> {
        let operation = UpdateOperation::parse(operation)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        let channel = ReleaseChannel::parse(channel)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        let target = (operation == UpdateOperation::Apply)
            .then(|| UpdateTarget::new(channel, image))
            .transpose()
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        let plan = UpdatePlan::for_operation(operation, target.as_ref())
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        Ok(format_plan_json(&plan))
    }
}

fn status_program() -> String {
    if env::var("DEVCORE_UPDATE_BUS").as_deref() == Ok("session") {
        env::var("DEVCORE_BOOTC_BIN").unwrap_or_else(|_| BOOTC_PATH.to_owned())
    } else {
        BOOTC_PATH.to_owned()
    }
}

fn format_plan_json(plan: &UpdatePlan) -> String {
    let argv = plan
        .argv()
        .iter()
        .map(|argument| format!("\"{}\"", json_escape(argument)))
        .collect::<Vec<_>>()
        .join(",");
    let channel = plan.channel().map_or_else(
        || "null".to_owned(),
        |channel| format!("\"{}\"", channel.as_str()),
    );
    format!(
        "{{\"operation\":\"{}\",\"channel\":{},\"argv\":[{}]}}",
        plan.operation().as_str(),
        channel,
        argv
    )
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write;

                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn run_daemon() -> Result<(), String> {
    zbus::block_on(async {
        let builder = if env::var("DEVCORE_UPDATE_BUS").as_deref() == Ok("session") {
            zbus::connection::Builder::session()
        } else {
            zbus::connection::Builder::system()
        };
        let _connection = builder
            .map_err(|error| error.to_string())?
            .name("org.devcore.Update1")
            .map_err(|error| error.to_string())?
            .serve_at("/org/devcore/Update", UpdateService)
            .map_err(|error| error.to_string())?
            .build()
            .await
            .map_err(|error| error.to_string())?;
        pending::<()>().await;
        Ok::<(), String>(())
    })
}

#[cfg(test)]
mod tests {
    use super::format_plan_json;
    use devcore_update::{ReleaseChannel, UpdateOperation, UpdatePlan, UpdateTarget};

    #[test]
    fn formats_a_plan_without_shell_interpolation() {
        let target = UpdateTarget::new(
            ReleaseChannel::Beta,
            "quay.io/devcore/os@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let plan = UpdatePlan::for_operation(UpdateOperation::Apply, Some(&target)).unwrap();
        let json = format_plan_json(&plan);

        assert!(json.contains("\"operation\":\"apply\""));
        assert!(json.contains("\"channel\":\"beta\""));
        assert!(json.contains("ostree-container"));
    }
}
