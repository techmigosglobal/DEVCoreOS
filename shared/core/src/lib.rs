#![forbid(unsafe_code)]
//! Shared, dependency-free domain types for DevCore services.

mod policy;
mod profile;
mod telemetry;

pub use policy::{
    MemoryProtection, ScopePlanError, WorkloadBudget, WorkloadClass, WorkloadScopePlan,
};
pub use profile::{HardwareCapacity, MachineProfile};
pub use telemetry::{
    PowerState, PressureMetrics, ResourceSnapshot, SnapshotError, collect_snapshot,
    snapshot_as_json,
};
