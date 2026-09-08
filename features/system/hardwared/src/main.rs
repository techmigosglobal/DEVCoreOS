#![forbid(unsafe_code)]
//! Read-only Hardware Manager D-Bus service.

use std::{env, future::pending, process::ExitCode};

use devcore_hardware::{HardwareSnapshot, collect_snapshot, snapshot_as_json};
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
            eprintln!("devcore-hardwared: {message}");
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
            if mode == OutputMode::Json {
                println!("{}", snapshot_as_json(&snapshot));
            } else {
                print_human(&snapshot);
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

fn print_human(snapshot: &HardwareSnapshot) {
    println!("DevCore hardware snapshot");
    println!("CPU: {}", snapshot.cpu_model);
    println!("logical CPUs: {}", snapshot.logical_cpus);
    println!("memory total: {} KiB", snapshot.memory_total_kib);
    println!("GPU devices: {}", snapshot.gpu_devices);
    println!(
        "network interfaces: {}",
        snapshot.network_interfaces.join(", ")
    );
    println!("battery present: {}", snapshot.battery_present);
    match snapshot.battery_percent {
        Some(percent) => println!("battery: {percent}%"),
        None => println!("battery: unavailable"),
    }
    match snapshot.ac_online {
        Some(true) => println!("AC: online"),
        Some(false) => println!("AC: offline"),
        None => println!("AC: unavailable"),
    }
}

fn help_text() -> &'static str {
    "usage: devcore-hardwared [snapshot] [--json] [--daemon]"
}

#[derive(Debug)]
struct HardwareService {
    snapshot: HardwareSnapshot,
}

impl HardwareService {
    fn new(snapshot: HardwareSnapshot) -> Self {
        Self { snapshot }
    }
}

#[interface(name = "org.devcore.Hardware1")]
impl HardwareService {
    /// Returns the kernel-reported CPU model.
    #[zbus(property)]
    fn cpu_model(&self) -> String {
        self.snapshot.cpu_model.clone()
    }

    /// Returns visible logical CPUs.
    #[zbus(property)]
    fn logical_cpus(&self) -> u16 {
        self.snapshot.logical_cpus
    }

    /// Returns total memory in KiB.
    #[zbus(property)]
    fn memory_total_kib(&self) -> u64 {
        self.snapshot.memory_total_kib
    }

    /// Returns whether a battery is present.
    #[zbus(property)]
    fn battery_present(&self) -> bool {
        self.snapshot.battery_present
    }

    /// Returns the first available battery percentage.
    #[zbus(property)]
    fn battery_percent(&self) -> Option<u8> {
        self.snapshot.battery_percent
    }

    /// Returns the first available external-power state.
    #[zbus(property)]
    fn ac_online(&self) -> Option<bool> {
        self.snapshot.ac_online
    }

    /// Returns the number of visible network interfaces.
    #[zbus(property)]
    fn network_interface_count(&self) -> u16 {
        u16::try_from(self.snapshot.network_interfaces.len()).unwrap_or(u16::MAX)
    }

    /// Returns the number of primary DRM GPU devices.
    #[zbus(property)]
    fn gpu_devices(&self) -> u16 {
        self.snapshot.gpu_devices
    }

    /// Returns the complete bounded inventory as JSON.
    fn snapshot_json(&self) -> String {
        snapshot_as_json(&self.snapshot)
    }

    /// Refreshes the inventory and emits the changed snapshot.
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

    /// Announces a refreshed hardware inventory.
    #[zbus(signal)]
    async fn snapshot_changed(
        _context: &SignalContext<'_>,
        _snapshot_json: &str,
    ) -> zbus::Result<()>;
}

fn run_daemon() -> Result<(), String> {
    zbus::block_on(async {
        let snapshot = collect_snapshot().map_err(|error| error.to_string())?;
        let builder = if env::var("DEVCORE_HARDWARE_BUS").as_deref() == Ok("session") {
            zbus::connection::Builder::session()
        } else {
            zbus::connection::Builder::system()
        };
        let _connection = builder
            .map_err(|error| error.to_string())?
            .name("org.devcore.Hardware1")
            .map_err(|error| error.to_string())?
            .serve_at("/org/devcore/Hardware", HardwareService::new(snapshot))
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
    use super::{OutputMode, parse_arguments};

    #[test]
    fn accepts_json_and_daemon_modes() {
        assert_eq!(
            parse_arguments(["snapshot".to_owned(), "--json".to_owned()]),
            Ok(OutputMode::Json)
        );
        assert_eq!(
            parse_arguments(["--daemon".to_owned()]),
            Ok(OutputMode::Daemon)
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert!(
            parse_arguments(["--unsupported".to_owned()])
                .unwrap_err()
                .contains("unknown argument")
        );
    }
}
