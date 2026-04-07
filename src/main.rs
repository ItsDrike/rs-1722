use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use rs_1722::net::interface::NetworkInterface;
use rs_1722::ptp::instance::{PtpInstance, PtpQueryError, PtpRole};
use rs_1722::ptp::state::{PortState, PtpSnapshot};
use tracing::{debug, error, info, info_span, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const MASTER_INTERFACE: &str = "enp1s0";
const SLAVE_INTERFACE: &str = "enp3s0";
const UNSYNCHRONIZED_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SYNCHRONIZED_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SlaveSyncState {
    #[default]
    Unknown,
    Unsynchronized,
    Synchronized,
}

/// Mutable state persisted across monitor loop iterations.
#[derive(Debug, Default)]
struct MonitorState {
    /// Last observed synchronization state of the slave instance.
    slave_sync_state: SlaveSyncState,
    /// Last observed raw slave PTP port state.
    last_slave_port_state: Option<PortState>,
    /// Tracks whether the slave snapshot has been temporarily unavailable.
    slave_snapshot_pending: bool,
    /// Last observed raw master PTP port state.
    last_master_port_state: Option<PortState>,
    /// Tracks whether the master snapshot has been temporarily unavailable.
    master_snapshot_pending: bool,
}

/// Starts PTP instances, waits for initial slave synchronization,
/// then runs the monitoring loop until shutdown is requested.
fn main() {
    init_tracing();

    let master_iface = validated_interface(MASTER_INTERFACE);
    let slave_iface = validated_interface(SLAVE_INTERFACE);

    debug!("network interfaces validated");

    let ptp_span = info_span!("ptp");
    let _ptp_enter = ptp_span.enter();

    let mut master = PtpInstance::new(master_iface, PtpRole::Master, "master")
        .unwrap_or_else(|e| fatal(&format!("failed to create master: {e}")));

    let mut slave = PtpInstance::new(slave_iface, PtpRole::Slave, "slave")
        .unwrap_or_else(|e| fatal(&format!("failed to create slave: {e}")));

    master
        .start()
        .unwrap_or_else(|e| fatal(&format!("failed to start master: {e}")));

    slave
        .start()
        .unwrap_or_else(|e| fatal(&format!("failed to start slave: {e}")));

    let running = setup_shutdown_flag();

    let mut monitor_state = MonitorState::default();
    if !wait_for_initial_sync(&master, &slave, running.as_ref(), &mut monitor_state) {
        info!("shutdown requested before initial synchronization completed");
        stop_instance(&mut master, "master");
        stop_instance(&mut slave, "slave");
        return;
    }

    info!("initial synchronization complete; entering application loop");
    monitor_loop(&master, &slave, running.as_ref(), &mut monitor_state);

    info!("shutting down PTP instances");
    stop_instance(&mut master, "master");
    stop_instance(&mut slave, "slave");
}

/// Returns a validated network interface or terminates the process.
fn validated_interface(name: &str) -> NetworkInterface {
    NetworkInterface::new(name.to_string()).unwrap_or_else(|| fatal(&format!("interface {name} does not exist")))
}

/// Installs a Ctrl+C handler and returns the shared shutdown flag.
///
/// The handler sets the flag to `false`, allowing loops to exit cleanly.
fn setup_shutdown_flag() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let handler_running = Arc::clone(&running);

    ctrlc::set_handler(move || {
        info!("received Ctrl+C signal, stopping monitor loop");
        handler_running.store(false, Ordering::SeqCst);
    })
    .unwrap_or_else(|e| fatal(&format!("failed to install Ctrl+C handler: {e}")));

    running
}

/// Blocks until the slave reports synchronized state or shutdown is requested.
///
/// While waiting, both slave and master snapshots are polled to keep
/// transition logs and baseline states up to date.
///
/// Returns `true` when synchronization is reached before shutdown.
fn wait_for_initial_sync(
    master: &PtpInstance,
    slave: &PtpInstance,
    running: &AtomicBool,
    state: &mut MonitorState,
) -> bool {
    let span = info_span!("wait");
    let _enter = span.enter();

    info!("waiting for initial slave synchronization");

    while running.load(Ordering::SeqCst) {
        let slave_synchronized = handle_slave_snapshot(slave, state, false);
        handle_master_snapshot(master, state);

        if slave_synchronized {
            return true;
        }

        thread::sleep(UNSYNCHRONIZED_POLL_INTERVAL);
    }

    false
}

/// Polls PTP state until shutdown and adapts polling cadence by sync status.
fn monitor_loop(master: &PtpInstance, slave: &PtpInstance, running: &AtomicBool, state: &mut MonitorState) {
    let span = info_span!("monitor");
    let _enter = span.enter();

    while running.load(Ordering::SeqCst) {
        let slave_synchronized = handle_slave_snapshot(slave, state, true);
        handle_master_snapshot(master, state);

        let poll_interval = if slave_synchronized {
            SYNCHRONIZED_POLL_INTERVAL
        } else {
            UNSYNCHRONIZED_POLL_INTERVAL
        };

        thread::sleep(poll_interval);
    }
}

/// Collects and processes one slave snapshot.
///
/// Returns `true` when the sampled snapshot is synchronized.
fn handle_slave_snapshot(slave: &PtpInstance, state: &mut MonitorState, warn_on_unsync: bool) -> bool {
    match slave.snapshot() {
        Ok(snapshot) => on_slave_snapshot(&snapshot, state, warn_on_unsync),
        Err(PtpQueryError::NotReady(dataset)) => {
            if warn_on_unsync
                && state.slave_sync_state == SlaveSyncState::Synchronized
                && !state.slave_snapshot_pending
            {
                warn!(
                    missing_dataset = dataset,
                    "slave snapshot temporarily unavailable after synchronization"
                );
            } else if !state.slave_snapshot_pending {
                debug!(
                    missing_dataset = dataset,
                    "slave snapshot not ready yet; waiting for ptp4l startup"
                );
            }

            state.slave_snapshot_pending = true;
            false
        }
        Err(error) => {
            state.slave_snapshot_pending = false;
            error!(error = %error, "failed to collect slave snapshot");
            false
        }
    }
}

/// Updates slave synchronization state and emits transition-aware logs.
///
/// Returns `true` when the provided snapshot is synchronized.
fn on_slave_snapshot(snapshot: &PtpSnapshot, state: &mut MonitorState, warn_on_unsync: bool) -> bool {
    if state.slave_snapshot_pending {
        debug!("slave snapshot became available");
        state.slave_snapshot_pending = false;
    }

    let current_state = &snapshot.port_data.port_state;
    let previous_state = state.last_slave_port_state.as_ref();
    let state_changed = previous_state != Some(current_state);

    if snapshot.is_synchronized() {
        let offset = snapshot.offset_ns().unwrap_or(0);

        if state.slave_sync_state != SlaveSyncState::Synchronized {
            if warn_on_unsync {
                info!("slave clock is now synchronized");
            } else {
                info!("initial slave synchronization established");
            }
        }

        if state_changed {
            if let Some(previous_state) = previous_state {
                debug!(
                    previous_state = ?previous_state,
                    state = ?current_state,
                    offset_ns = offset,
                    gm_identity = %snapshot.time_status.gm_identity,
                    "slave state changed"
                );
            } else {
                debug!(
                    previous_state = "unobserved",
                    state = ?current_state,
                    offset_ns = offset,
                    gm_identity = %snapshot.time_status.gm_identity,
                    "slave state changed"
                );
            }

            state.last_slave_port_state = Some(current_state.clone());
        } else if warn_on_unsync {
            debug!(
                offset_ns = offset,
                gm_identity = %snapshot.time_status.gm_identity,
                "slave synchronization offset"
            );
        }

        state.slave_sync_state = SlaveSyncState::Synchronized;
        true
    } else {
        if warn_on_unsync && state.slave_sync_state == SlaveSyncState::Synchronized {
            warn!(state = ?current_state, "slave clock is not synchronized");
        } else if !warn_on_unsync && state.slave_sync_state != SlaveSyncState::Unsynchronized {
            debug!(state = ?current_state, "waiting for initial slave synchronization");
        }

        if state_changed {
            if let Some(previous_state) = previous_state {
                debug!(previous_state = ?previous_state, state = ?current_state, "slave state changed");
            } else {
                debug!(previous_state = "unobserved", state = ?current_state, "slave state changed");
            }

            state.last_slave_port_state = Some(current_state.clone());
        }

        state.slave_sync_state = SlaveSyncState::Unsynchronized;
        false
    }
}

/// Collects and logs one master snapshot with reduced verbosity.
fn handle_master_snapshot(master: &PtpInstance, state: &mut MonitorState) {
    match master.snapshot() {
        Ok(snapshot) => {
            if state.master_snapshot_pending {
                debug!("master snapshot became available");
                state.master_snapshot_pending = false;
            }

            let current_state = &snapshot.port_data.port_state;
            match state.last_master_port_state.as_ref() {
                Some(previous_state) if previous_state != current_state => {
                    debug!(previous_state = ?previous_state, state = ?current_state, "master state changed");
                }
                None => {
                    debug!(state = ?current_state, "master initial state observed");
                }
                _ => {}
            }

            state.last_master_port_state = Some(current_state.clone());
        }
        Err(PtpQueryError::NotReady(dataset)) => {
            if !state.master_snapshot_pending {
                debug!(
                    missing_dataset = dataset,
                    "master snapshot not ready yet; waiting for ptp4l startup"
                );
            }

            state.master_snapshot_pending = true;
        }
        Err(error) => {
            state.master_snapshot_pending = false;
            error!(error = %error, "failed to collect master snapshot");
        }
    }
}

/// Stops a PTP instance and logs any shutdown failure.
fn stop_instance(instance: &mut PtpInstance, name: &str) {
    if let Err(error) = instance.stop() {
        error!(instance = name, error = %error, "failed to stop PTP instance");
    }
}

/// Initializes global tracing with environment-driven filtering.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rs_1722=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .compact()
                .with_target(false)
                .with_thread_names(true)
                .with_timer(fmt::time::uptime()),
        )
        .init();
}

/// Logs a fatal message and exits the process with status code `1`.
fn fatal(msg: &str) -> ! {
    error!("fatal: {msg}");
    process::exit(1);
}
