#![forbid(unsafe_code)]
//! DevCore resource telemetry command and system D-Bus service.

use std::{env, future::pending, process::ExitCode};

use devcore_core::{ResourceSnapshot, WorkloadClass, collect_snapshot, snapshot_as_json};
use zbus::{interface, object_server::SignalContext};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputMode {
    Human,
    Json,
    Daemon,
}

fn main() -> ExitCode {
    match run(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("devcored: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: impl IntoIterator<Item = String>) -> Result<(), String> {
    let mode = parse_arguments(arguments)?;
    match mode {
        OutputMode::Daemon => run_daemon(),
        OutputMode::Human | OutputMode::Json => {
            let snapshot = collect_snapshot().map_err(|error| error.to_string())?;
            match mode {
                OutputMode::Human => print_human(&snapshot),
                OutputMode::Json => println!("{}", snapshot_as_json(&snapshot)),
                OutputMode::Daemon => unreachable!("daemon is handled above"),
            }
            Ok(())
        }
    }
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<OutputMode, String> {
    let mut mode = OutputMode::Human;

    for argument in arguments {
        match argument.as_str() {
            "snapshot" => {}
            "--json" => mode = OutputMode::Json,
            "--daemon" => mode = OutputMode::Daemon,
            "--help" | "-h" => return Err(help_text().to_owned()),
            _ => return Err(format!("unknown argument {argument:?}\n{}", help_text())),
        }
    }

    Ok(mode)
}

fn print_human(snapshot: &ResourceSnapshot) {
    println!("DevCore resource snapshot");
    println!("profile: {}", snapshot.initial_profile.as_str());
    println!("logical CPUs: {}", snapshot.capacity.logical_cpus);
    println!("memory total: {} KiB", snapshot.capacity.memory_total_kib);
    match snapshot.memory_available_kib {
        Some(memory) => println!("memory available: {memory} KiB"),
        None => println!("memory available: unavailable"),
    }
    print_pressure("CPU", snapshot.cpu_pressure);
    print_pressure("memory", snapshot.memory_pressure);
    print_pressure("I/O", snapshot.io_pressure);
    println!("battery present: {}", snapshot.power.battery_present);
    match snapshot.power.battery_percent {
        Some(percent) => println!("battery: {percent}%"),
        None => println!("battery: unavailable"),
    }
    match snapshot.power.ac_online {
        Some(true) => println!("AC: online"),
        Some(false) => println!("AC: offline"),
        None => println!("AC: unavailable"),
    }
}

fn print_pressure(name: &str, pressure: Option<devcore_core::PressureMetrics>) {
    match pressure {
        Some(pressure) => println!(
            "{name} PSI some avg10: {}.{}%",
            pressure.some_avg10_milli_percent / 1000,
            pressure.some_avg10_milli_percent % 1000
        ),
        None => println!("{name} PSI: unavailable"),
    }
}

fn help_text() -> &'static str {
    "usage: devcored [snapshot] [--json] [--daemon]"
}

/// D-Bus object exposing the latest resource snapshot.
#[derive(Debug)]
struct ResourceService {
    snapshot: ResourceSnapshot,
}

impl ResourceService {
    fn new(snapshot: ResourceSnapshot) -> Self {
        Self { snapshot }
    }
}

#[interface(name = "org.devcore.Resource1")]
impl ResourceService {
    /// Returns the conservative profile selected from available capacity.
    #[zbus(property)]
    fn profile(&self) -> String {
        self.snapshot.initial_profile.as_str().to_owned()
    }

    /// Returns the number of logical CPUs visible to DevCore.
    #[zbus(property)]
    fn logical_cpus(&self) -> u16 {
        self.snapshot.capacity.logical_cpus
    }

    /// Returns total memory in KiB.
    #[zbus(property)]
    fn memory_total_kib(&self) -> u64 {
        self.snapshot.capacity.memory_total_kib
    }

    /// Returns currently available memory in KiB, when exported by the kernel.
    #[zbus(property)]
    fn memory_available_kib(&self) -> Option<u64> {
        self.snapshot.memory_available_kib
    }

    /// Returns CPU PSI avg10 in thousandths of a percent.
    #[zbus(property)]
    fn cpu_pressure_avg10_milli_percent(&self) -> Option<u32> {
        self.snapshot
            .cpu_pressure
            .map(|pressure| pressure.some_avg10_milli_percent)
    }

    /// Returns memory PSI avg10 in thousandths of a percent.
    #[zbus(property)]
    fn memory_pressure_avg10_milli_percent(&self) -> Option<u32> {
        self.snapshot
            .memory_pressure
            .map(|pressure| pressure.some_avg10_milli_percent)
    }

    /// Returns I/O PSI avg10 in thousandths of a percent.
    #[zbus(property)]
    fn io_pressure_avg10_milli_percent(&self) -> Option<u32> {
        self.snapshot
            .io_pressure
            .map(|pressure| pressure.some_avg10_milli_percent)
    }

    /// Returns whether a battery device is present.
    #[zbus(property)]
    fn battery_present(&self) -> bool {
        self.snapshot.power.battery_present
    }

    /// Returns the reported battery percentage, when available.
    #[zbus(property)]
    fn battery_percent(&self) -> Option<u8> {
        self.snapshot.power.battery_percent
    }

    /// Returns AC online state, when available.
    #[zbus(property)]
    fn ac_online(&self) -> Option<bool> {
        self.snapshot.power.ac_online
    }

    /// Returns the current safe parallel build-worker cap.
    #[zbus(property)]
    fn build_workers(&self) -> u16 {
        self.snapshot
            .workload_budget(WorkloadClass::Build)
            .max_workers
    }

    /// Returns whether emulator work should be deferred.
    #[zbus(property)]
    fn emulator_deferred(&self) -> bool {
        self.snapshot
            .workload_budget(WorkloadClass::Emulator)
            .deferred
    }

    /// Returns a declarative resource-scope preview for a validated job id.
    ///
    /// This is intentionally a preview API: it never starts `systemd-run` and
    /// never mutates a cgroup. Applying a scope remains a separate authorized
    /// service operation.
    fn workload_scope_preview(&self, job_id: &str, workload: &str) -> zbus::fdo::Result<String> {
        let workload = parse_workload(workload).ok_or_else(|| {
            zbus::fdo::Error::InvalidArgs(format!("unknown workload class {workload:?}"))
        })?;
        let plan = self
            .snapshot
            .workload_scope(job_id, workload)
            .map_err(|error| zbus::fdo::Error::InvalidArgs(error.to_string()))?;
        Ok(format_scope_preview(&plan))
    }

    /// Returns the complete snapshot as the stable compact JSON representation.
    fn snapshot_json(&self) -> String {
        snapshot_as_json(&self.snapshot)
    }

    /// Collects a fresh snapshot on explicit request and returns its JSON form.
    async fn refresh(
        &mut self,
        #[zbus(signal_context)] context: SignalContext<'_>,
    ) -> zbus::fdo::Result<String> {
        self.snapshot =
            collect_snapshot().map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        let snapshot_json = snapshot_as_json(&self.snapshot);
        Self::snapshot_changed(&context, &snapshot_json)
            .await
            .map_err(zbus::fdo::Error::ZBus)?;
        Ok(snapshot_json)
    }

    /// Announces a new snapshot after an explicit refresh or a kernel event.
    #[zbus(signal)]
    async fn snapshot_changed(
        _context: &SignalContext<'_>,
        _snapshot_json: &str,
    ) -> zbus::Result<()>;
}

fn run_daemon() -> Result<(), String> {
    zbus::block_on(async {
        let snapshot = collect_snapshot().map_err(|error| error.to_string())?;
        let _connection = zbus::connection::Builder::system()
            .map_err(|error| error.to_string())?
            .name("org.devcore.Resource1")
            .map_err(|error| error.to_string())?
            .serve_at("/org/devcore/Resource", ResourceService::new(snapshot))
            .map_err(|error| error.to_string())?
            .build()
            .await
            .map_err(|error| error.to_string())?;

        pending::<()>().await;
        Ok::<(), String>(())
    })
}

fn parse_workload(value: &str) -> Option<WorkloadClass> {
    match value {
        "interactive" => Some(WorkloadClass::Interactive),
        "build" => Some(WorkloadClass::Build),
        "indexer" => Some(WorkloadClass::Indexer),
        "emulator" => Some(WorkloadClass::Emulator),
        "cache" => Some(WorkloadClass::Cache),
        _ => None,
    }
}

fn format_scope_preview(plan: &devcore_core::WorkloadScopePlan) -> String {
    format!(
        concat!(
            "{{\"unit_name\":\"{}\",\"workload\":\"{}\",",
            "\"max_workers\":{},\"cpu_weight\":{},",
            "\"memory_low_bytes\":{},\"memory_high_bytes\":{},",
            "\"memory_max_bytes\":{},\"deferred\":{}}}"
        ),
        plan.unit_name,
        plan.workload.as_str(),
        plan.max_workers,
        plan.cpu_weight,
        optional_u64_json(plan.memory_low_bytes),
        optional_u64_json(plan.memory_high_bytes),
        optional_u64_json(plan.memory_max_bytes),
        plan.deferred,
    )
}

fn optional_u64_json(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{OutputMode, format_scope_preview, parse_arguments};
    use devcore_core::{
        HardwareCapacity, MachineProfile, PowerState, ResourceSnapshot, WorkloadClass,
    };

    #[test]
    fn accepts_snapshot_json_mode() {
        assert_eq!(
            parse_arguments(["snapshot".to_owned(), "--json".to_owned()]),
            Ok(OutputMode::Json)
        );
    }

    #[test]
    fn accepts_daemon_mode() {
        assert_eq!(
            parse_arguments(["--daemon".to_owned()]),
            Ok(OutputMode::Daemon)
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        let error = parse_arguments(["--unsupported".to_owned()]).unwrap_err();

        assert!(error.contains("unknown argument"));
    }

    #[test]
    fn formats_a_scope_preview_without_starting_a_process() {
        let snapshot = ResourceSnapshot {
            capacity: HardwareCapacity::new(2, 4 * 1024 * 1024),
            initial_profile: MachineProfile::Low,
            memory_available_kib: Some(2 * 1024 * 1024),
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        };
        let plan = snapshot
            .workload_scope("compile-job", WorkloadClass::Build)
            .unwrap();

        let preview = format_scope_preview(&plan);

        assert!(preview.contains("\"unit_name\":\"devcore-job-compile-job\""));
        assert!(preview.contains("\"cpu_weight\":300"));
        assert!(preview.contains("\"memory_low_bytes\":null"));
    }
}
