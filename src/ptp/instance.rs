use std::{
    fs::File,
    io::{self, Write},
    path::PathBuf,
    process,
};
use thiserror::Error;
use tracing::{debug, info, info_span, trace, warn};

use crate::{
    net::interface::NetworkInterface,
    ptp::state::{PtpSnapshot, SnapshotParseError},
};

const SNAPSHOT_POLL_LOG_TARGET: &str = "rs_1722::ptp::snapshot_poll";

#[derive(Debug, Error)]
pub enum PtpQueryError {
    /// Failed to execute the `pmc` process.
    #[error("failed to execute pmc: {0}")]
    Io(std::io::Error),

    /// `pmc` exited with a non-zero status.
    #[error("pmc exited with status {0}")]
    CommandFailed(process::ExitStatus),

    /// Output was not valid UTF-8.
    #[error("pmc output was not valid UTF-8")]
    InvalidUtf8(std::string::FromUtf8Error),

    /// `pmc` responded but `ptp4l` has not published all datasets yet.
    #[error("ptp4l is still initializing (missing dataset `{0}`)")]
    NotReady(&'static str),

    /// Managed `ptp4l` process has already exited.
    #[error("ptp4l process exited with status {0}")]
    ProcessExited(process::ExitStatus),

    /// Managed `ptp4l` process is not currently running.
    #[error("ptp4l process is not running")]
    ProcessNotRunning,

    /// Failed to parse the output into a snapshot.
    #[error(transparent)]
    Parse(#[from] SnapshotParseError),
}

/// Defines the role of a PTP instance.
///
/// A [`PtpInstance`] operates either as:
/// - [`PtpRole::Master`]: acts as the grandmaster clock
/// - [`PtpRole::Slave`]: synchronizes to a remote master
///
/// This directly maps to the presence or absence of the `-s` flag in `ptp4l`.
#[derive(Debug, Clone, Copy)]
pub enum PtpRole {
    /// Grandmaster clock (no upstream synchronization source).
    Master,
    /// Client clock that synchronizes to a master.
    Slave,
}

/// A managed instance of the `ptp4l` daemon bound to a specific network interface.
///
/// This struct provides a type-safe abstraction for:
/// - spawning a `ptp4l` process
/// - isolating its control sockets
/// - querying its synchronization state via `pmc`
///
/// # Design goals
///
/// - No reliance on pre-existing configuration files
/// - No shared global sockets between instances
/// - Safe to run multiple instances concurrently
/// - Explicit ownership and cleanup of runtime resources
///
/// # Configuration
///
/// A temporary configuration file is generated in `/tmp`, containing
/// unique socket paths derived from the instance name and process ID.
///
/// # Lifecycle
///
/// - [`start`](Self::start) spawns the `ptp4l` process
/// - [`get_status`](Self::get_status) queries synchronization state
/// - [`stop`](Self::stop) terminates the process
///
/// Resources are automatically cleaned up when the instance is dropped.
///
/// # Permissions
///
/// Running `ptp4l` typically requires:
/// - `CAP_NET_ADMIN`
/// - `CAP_SYS_TIME`
#[derive(Debug)]
pub struct PtpInstance {
    interface: NetworkInterface,
    role: PtpRole,
    config_path: PathBuf,
    process: Option<process::Child>,
}

impl PtpInstance {
    /// Creates a new [`PtpInstance`] bound to a network interface and role.
    ///
    /// This generates a temporary configuration file in `/tmp` with
    /// unique socket paths to avoid conflicts with other instances.
    ///
    /// # Arguments
    /// * `interface` - A network interface to run on
    /// * `role` - Whether this instance acts as master or slave
    /// * `name` - A unique identifier used to namespace runtime resources
    ///
    /// # Errors
    /// Returns an error if the configuration file cannot be created.
    pub fn new(interface: NetworkInterface, role: PtpRole, name: &str) -> io::Result<Self> {
        let config_path = Self::create_config(name)?;

        info!(
            instance = name,
            interface = %interface,
            role = ?role,
            config_path = %config_path.display(),
            "created PTP instance"
        );

        Ok(Self {
            interface,
            role,
            config_path,
            process: None,
        })
    }

    /// Generates a temporary `ptp4l` configuration file with isolated sockets.
    ///
    /// The configuration includes:
    /// - `uds_address`
    /// - `uds_ro_address`
    ///
    /// These are uniquely namespaced using the provided `name` and process ID
    /// to avoid collisions between multiple instances.
    ///
    /// # Errors
    /// Returns an error if the file cannot be created or written.
    fn create_config(name: &str) -> io::Result<PathBuf> {
        let pid = std::process::id();
        let path = PathBuf::from(format!("/tmp/ptp4l-{name}-{pid}.cfg"));

        let mut file = File::create(&path)?;

        writeln!(
            file,
            "[global]\nuds_address /var/run/ptp4l-{name}-{pid}\nuds_ro_address /var/run/ptp4l-{name}-{pid}-ro"
        )?;

        trace!(
            instance = name,
            config_path = %path.display(),
            "generated temporary ptp4l configuration"
        );

        Ok(path)
    }

    /// Starts the `ptp4l` process for this instance.
    ///
    /// The process is configured with:
    /// - hardware timestamping (`-H`)
    /// - IEEE 802.3 transport (`-2`)
    /// - the generated configuration file
    ///
    /// If the role is [`PtpRole::Slave`], the `-s` flag is included.
    ///
    /// # Errors
    /// Returns an error if the process cannot be spawned.
    pub fn start(&mut self) -> io::Result<()> {
        let span = info_span!("start", role = ?self.role, interface = %self.interface);
        let _enter = span.enter();

        if self.process.is_some() {
            warn!("start requested while process handle already exists");
        }

        let mut args = vec![
            "-i".to_string(),
            self.interface.name().to_string(),
            "-H".to_string(),
            "-m".to_string(),
            "-2".to_string(),
            "-f".to_string(),
            self.config_path.to_string_lossy().into_owned(),
        ];

        if matches!(self.role, PtpRole::Slave) {
            args.push("-s".to_string());
        }

        debug!(args = ?args, "starting ptp4l process");

        let child = process::Command::new("ptp4l")
            .args(&args)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .spawn()?;

        let pid = child.id();

        self.process = Some(child);

        info!(pid, "started ptp4l process");

        Ok(())
    }

    /// Stops the running `ptp4l` process, if any.
    ///
    /// # Errors
    /// Returns an error if the process exists but cannot be terminated.
    pub fn stop(&mut self) -> io::Result<()> {
        let span = info_span!("stop", role = ?self.role, interface = %self.interface);
        let _enter = span.enter();

        if let Some(mut child) = self.process.take() {
            let pid = child.id();
            child.kill()?;
            let _ = child.wait();

            info!(pid, "stopped ptp4l process");
        } else {
            debug!("stop requested but no running process was tracked");
        }

        Ok(())
    }

    /// Queries the current PTP state via `pmc` and returns a parsed [`PtpSnapshot`].
    ///
    /// This executes a single `pmc` invocation requesting:
    /// - `TIME_STATUS_NP`
    /// - `PORT_DATA_SET`
    /// - `CURRENT_DATA_SET`
    ///
    /// The output is then parsed into a strongly-typed [`PtpSnapshot`].
    ///
    /// # Errors
    /// Returns an error if:
    /// - the managed `ptp4l` process is not running
    /// - the managed `ptp4l` process exited
    /// - the `pmc` process fails to execute
    /// - the process exits with a non-zero status
    /// - the output cannot be parsed into a valid snapshot
    pub fn snapshot(&mut self) -> Result<PtpSnapshot, PtpQueryError> {
        let span = info_span!("snapshot", role = ?self.role, interface = %self.interface);
        let _enter = span.enter();

        self.ensure_process_running()?;

        trace!(
            target: SNAPSHOT_POLL_LOG_TARGET,
            config_path = %self.config_path.display(),
            "querying PTP snapshot via pmc"
        );

        let output = process::Command::new("pmc")
            .args([
                "-u",
                "-b",
                "0",
                "-f",
                self.config_path.to_string_lossy().as_ref(),
                "GET TIME_STATUS_NP",
                "GET PORT_DATA_SET",
                "GET CURRENT_DATA_SET",
            ])
            .output()
            .map_err(|error| {
                warn!(error = %error, "failed to execute pmc");
                PtpQueryError::Io(error)
            })?;

        if !output.status.success() {
            self.ensure_process_running()?;

            warn!(status = %output.status, "pmc exited with non-zero status");
            return Err(PtpQueryError::CommandFailed(output.status));
        }

        let text = String::from_utf8(output.stdout).map_err(|error| {
            warn!(error = %error, "pmc output was not valid UTF-8");
            PtpQueryError::InvalidUtf8(error)
        })?;

        let snapshot = match PtpSnapshot::parse_pmc_output(&text) {
            Ok(snapshot) => snapshot,
            Err(SnapshotParseError::MissingDataset(dataset)) => {
                self.ensure_process_running()?;

                return Err(PtpQueryError::NotReady(dataset));
            }
            Err(error) => {
                warn!(error = %error, "failed to parse pmc output");
                return Err(PtpQueryError::Parse(error));
            }
        };

        trace!(
            target: SNAPSHOT_POLL_LOG_TARGET,
            synchronized = snapshot.is_synchronized(),
            state = ?snapshot.port_data.port_state,
            "PTP snapshot collected"
        );

        Ok(snapshot)
    }

    /// Verifies whether the managed `ptp4l` process is alive.
    ///
    /// # Errors
    /// Returns:
    /// - [`PtpQueryError::ProcessNotRunning`] if no child process is tracked
    /// - [`PtpQueryError::ProcessExited`] if the child has terminated
    /// - [`PtpQueryError::Io`] if the process state cannot be queried
    fn ensure_process_running(&mut self) -> Result<(), PtpQueryError> {
        let Some(child) = self.process.as_mut() else {
            return Err(PtpQueryError::ProcessNotRunning);
        };

        match child.try_wait().map_err(PtpQueryError::Io)? {
            Some(status) => {
                self.process = None;
                Err(PtpQueryError::ProcessExited(status))
            }
            None => Ok(()),
        }
    }
}

impl Drop for PtpInstance {
    fn drop(&mut self) {
        let span = info_span!("drop", role = ?self.role, interface = %self.interface);
        let _enter = span.enter();

        // Best effort cleanup, no panics

        // Stop process if still running
        if let Some(mut child) = self.process.take() {
            let pid = child.id();

            match child.kill() {
                Ok(()) => {
                    let _ = child.wait();
                    debug!(pid, "killed ptp4l process during drop");
                }
                Err(error) => {
                    warn!(pid, error = %error, "failed to kill ptp4l process during drop");
                }
            }
        }

        // Remove the temporary config file
        match std::fs::remove_file(&self.config_path) {
            Ok(()) => {
                trace!(config_path = %self.config_path.display(), "removed temporary ptp4l configuration");
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(config_path = %self.config_path.display(), error = %error, "failed to remove temporary ptp4l configuration");
            }
        }
    }
}
