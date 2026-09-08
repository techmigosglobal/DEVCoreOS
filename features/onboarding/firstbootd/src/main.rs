#![forbid(unsafe_code)]
#![allow(missing_docs)]
//! Root-owned D-Bus boundary for the DevCore first-boot flow.
//!
//! The service performs only validated setup operations through
//! `devcore-firstboot`. The password is accepted as a transient D-Bus method
//! argument, passed to the backend over a pipe, and cleared from this process's
//! owned `String` before the method returns. It is never logged, serialized to
//! state, or placed in command arguments.

use std::{
    env,
    error::Error,
    future::pending,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use devcore_core::collect_snapshot;
use devcore_firstboot::{
    DEFAULT_STATE_DIR, FirstBootCoordinator, FirstBootError, FirstBootStore, SetupConfig,
    SystemBackend,
};
use zbus::{connection::Builder, fdo, interface};

const BUS_NAME: &str = "org.devcore.FirstBoot1";
const OBJECT_PATH: &str = "/org/devcore/FirstBoot";

type Coordinator = FirstBootCoordinator<SystemBackend>;

#[derive(Debug, Clone)]
struct FirstBootApi {
    state: Arc<Mutex<Coordinator>>,
}

#[interface(name = "org.devcore.FirstBoot1")]
impl FirstBootApi {
    fn status(&self) -> fdo::Result<(bool, String)> {
        let state = self
            .state
            .lock()
            .map_err(|_| fdo::Error::Failed("first-boot state is poisoned".to_owned()))?;
        let (required, profile) = state.status().map_err(invalid_args)?;
        Ok((required, profile.as_str().to_owned()))
    }

    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        language: String,
        keyboard: String,
        timezone: String,
        username: String,
        hostname: String,
        profile: String,
        mut password: String,
    ) -> fdo::Result<()> {
        let result = (|| {
            let config =
                SetupConfig::new(language, keyboard, timezone, username, hostname, &profile)
                    .map_err(invalid_args)?;
            let mut state = self
                .state
                .lock()
                .map_err(|_| fdo::Error::Failed("first-boot state is poisoned".to_owned()))?;
            state
                .apply(config, password.as_bytes())
                .map_err(invalid_args)
        })();
        password.clear();
        result
    }
}

fn invalid_args(error: FirstBootError) -> fdo::Error {
    fdo::Error::InvalidArgs(error.to_string())
}

fn state_root() -> PathBuf {
    env::var_os("DEVCORE_FIRSTBOOT_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

fn run() -> Result<(), Box<dyn Error>> {
    zbus::block_on(async {
        let snapshot = collect_snapshot()?;
        let store = FirstBootStore::open(state_root())?;
        let coordinator = FirstBootCoordinator::new(store, SystemBackend, snapshot.initial_profile);
        let state = Arc::new(Mutex::new(coordinator));

        // The production unit leaves this unset and therefore always uses the
        // system bus. A disposable session-bus mode keeps the D-Bus status
        // contract testable without installing a root service on the host.
        let builder = if env::var("DEVCORE_FIRSTBOOT_BUS").as_deref() == Ok("session") {
            Builder::session()?
        } else {
            Builder::system()?
        };
        let _connection = builder
            .name(BUS_NAME)?
            .serve_at(OBJECT_PATH, FirstBootApi { state })?
            .build()
            .await?;

        pending::<()>().await;
        Ok::<(), Box<dyn Error>>(())
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    run()
}
