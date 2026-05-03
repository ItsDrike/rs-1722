use std::{
    process,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use pnet::datalink::{NetworkInterface, interfaces};
use rs_1722::ptp_proto::instance::{PtpInstance, PtpQueryError, PtpRole};
use rs_1722::ptp_proto::state::{PortState, PtpSnapshot};
use tracing::{debug, error, info, info_span, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const MASTER_INTERFACE: &str = "enp1s0";
const SLAVE_INTERFACE: &str = "enp3s0";
const UNSYNCHRONIZED_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SYNCHRONIZED_POLL_INTERVAL: Duration = Duration::from_secs(5);
const DEGRADED_STATUS_LOG_INTERVAL: Duration = Duration::from_secs(10);

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
    /// Last time an "unavailable" status log was emitted for the slave snapshot.
    last_slave_pending_log: Option<Instant>,
    /// Last time an "unsynchronized" status log was emitted for the slave.
    last_slave_unsync_log: Option<Instant>,
    /// Last time a "slave process down" error was emitted.
    last_slave_process_down_log: Option<Instant>,
    /// Last observed raw master PTP port state.
    last_master_port_state: Option<PortState>,
    /// Last time an "unavailable" status log was emitted for the master.
    last_master_pending_log: Option<Instant>,
    /// Last time a "master process down" error was emitted.
    last_master_process_down_log: Option<Instant>,
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
    if !wait_for_initial_sync(&mut master, &mut slave, running.as_ref(), &mut monitor_state) {
        info!("shutdown requested before initial synchronization completed");
        stop_instance(&mut master, "master");
        stop_instance(&mut slave, "slave");
        return;
    }

    info!("initial synchronization complete; entering application loop");
    monitor_loop(&mut master, &mut slave, running.as_ref(), &mut monitor_state);

    info!("shutting down PTP instances");
    stop_instance(&mut master, "master");
    stop_instance(&mut slave, "slave");
}

/// Returns a validated network interface or terminates the process.
fn validated_interface(name: &str) -> NetworkInterface {
    interfaces()
        .into_iter()
        .find(|interface| interface.name == name)
        .unwrap_or_else(|| fatal(&format!("interface {name} does not exist")))
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
    master: &mut PtpInstance,
    slave: &mut PtpInstance,
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
fn monitor_loop(master: &mut PtpInstance, slave: &mut PtpInstance, running: &AtomicBool, state: &mut MonitorState) {
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
fn handle_slave_snapshot(slave: &mut PtpInstance, state: &mut MonitorState, warn_on_unsync: bool) -> bool {
    match slave.snapshot() {
        Ok(snapshot) => {
            if state.last_slave_process_down_log.is_some() {
                info!("slave ptp4l process became reachable again");
            }

            state.last_slave_process_down_log = None;

            state.on_slave_snapshot(&snapshot, warn_on_unsync)
        }
        Err(PtpQueryError::NotReady(dataset)) => {
            if state.last_slave_process_down_log.is_some() {
                info!("slave ptp4l process became reachable again");
            }

            state.last_slave_process_down_log = None;

            if warn_on_unsync
                && state.slave_sync_state == SlaveSyncState::Synchronized
                && state.last_slave_pending_log.is_none()
            {
                warn!(
                    missing_dataset = dataset,
                    "slave snapshot temporarily unavailable after synchronization"
                );
            } else if state.last_slave_pending_log.is_none() {
                debug!(
                    missing_dataset = dataset,
                    "slave snapshot not ready yet; waiting for ptp4l startup"
                );
            }

            state.last_slave_pending_log.get_or_insert_with(Instant::now);
            false
        }
        Err(PtpQueryError::ProcessExited(status)) => {
            state.last_slave_pending_log = None;
            state.slave_sync_state = SlaveSyncState::Unsynchronized;

            if state.last_slave_process_down_log.is_none() {
                error!(status = %status, "slave ptp4l process exited unexpectedly");
                state.last_slave_process_down_log = Some(Instant::now());
            } else if MonitorState::should_emit_periodic(
                &mut state.last_slave_process_down_log,
                DEGRADED_STATUS_LOG_INTERVAL,
            ) {
                warn!(
                    status = %status,
                    "slave ptp4l process remains down; no auto-restart configured"
                );
            }

            false
        }
        Err(PtpQueryError::ProcessNotRunning) => {
            state.last_slave_pending_log = None;
            state.slave_sync_state = SlaveSyncState::Unsynchronized;

            if state.last_slave_process_down_log.is_none() {
                error!("slave ptp4l process not running");
                state.last_slave_process_down_log = Some(Instant::now());
            } else if MonitorState::should_emit_periodic(
                &mut state.last_slave_process_down_log,
                DEGRADED_STATUS_LOG_INTERVAL,
            ) {
                warn!("slave ptp4l process not running");
            }

            false
        }
        Err(error) => {
            state.last_slave_process_down_log = None;
            state.last_slave_pending_log = None;
            error!(error = %error, "failed to collect slave snapshot");
            false
        }
    }
}

impl MonitorState {
    /// Updates slave synchronization state and emits transition-aware logs.
    ///
    /// Returns `true` when the provided snapshot is synchronized.
    fn on_slave_snapshot(&mut self, snapshot: &PtpSnapshot, warn_on_unsync: bool) -> bool {
        if self.last_slave_pending_log.is_some() {
            debug!("slave snapshot became available");
            self.last_slave_pending_log = None;
        }

        let current_state = &snapshot.port_data.port_state;
        let previous_state = self.last_slave_port_state.clone();
        let state_changed = previous_state.as_ref() != Some(current_state);

        if snapshot.is_synchronized() {
            self.on_synchronized_slave_snapshot(
                snapshot,
                warn_on_unsync,
                current_state,
                previous_state.as_ref(),
                state_changed,
            );
            return true;
        }

        self.on_unsynchronized_slave_snapshot(warn_on_unsync, current_state, previous_state.as_ref(), state_changed);
        false
    }

    /// Applies the synchronized-path logging and state updates for a slave snapshot.
    fn on_synchronized_slave_snapshot(
        &mut self,
        snapshot: &PtpSnapshot,
        warn_on_unsync: bool,
        current_state: &PortState,
        previous_state: Option<&PortState>,
        state_changed: bool,
    ) {
        let offset = snapshot.offset_ns().unwrap_or(0);

        if self.slave_sync_state != SlaveSyncState::Synchronized {
            if warn_on_unsync {
                info!("slave clock is now synchronized");
            } else {
                info!("initial slave synchronization established");
            }
        }

        if state_changed {
            Self::log_synchronized_slave_state_change(snapshot, current_state, previous_state, offset);
            self.last_slave_port_state = Some(current_state.clone());
        } else if warn_on_unsync {
            debug!(
                offset_ns = offset,
                gm_identity = %snapshot.time_status.gm_identity,
                "slave synchronization offset"
            );
        }

        self.slave_sync_state = SlaveSyncState::Synchronized;
        self.last_slave_unsync_log = None;
    }

    /// Emits the transition log for a slave snapshot that is currently synchronized.
    fn log_synchronized_slave_state_change(
        snapshot: &PtpSnapshot,
        current_state: &PortState,
        previous_state: Option<&PortState>,
        offset: i64,
    ) {
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
    }

    /// Applies the unsynchronized-path logging and state updates for a slave snapshot.
    fn on_unsynchronized_slave_snapshot(
        &mut self,
        warn_on_unsync: bool,
        current_state: &PortState,
        previous_state: Option<&PortState>,
        state_changed: bool,
    ) {
        let unexpected_state = current_state.is_unexpected_for_slave();

        self.log_unsynchronized_slave_status(warn_on_unsync, current_state, unexpected_state, state_changed);

        if state_changed {
            Self::log_unsynchronized_slave_state_change(previous_state, current_state, unexpected_state);
            self.last_slave_port_state = Some(current_state.clone());
        }

        self.slave_sync_state = SlaveSyncState::Unsynchronized;
    }

    /// Emits the periodic or transitional status log for an unsynchronized slave.
    fn log_unsynchronized_slave_status(
        &mut self,
        warn_on_unsync: bool,
        current_state: &PortState,
        unexpected_state: bool,
        state_changed: bool,
    ) {
        if unexpected_state && state_changed {
            return;
        }

        if warn_on_unsync && self.slave_sync_state == SlaveSyncState::Synchronized {
            warn!(state = ?current_state, "slave clock is not synchronized");
            self.last_slave_unsync_log = Some(Instant::now());
            return;
        }

        if warn_on_unsync && Self::should_emit_periodic(&mut self.last_slave_unsync_log, DEGRADED_STATUS_LOG_INTERVAL)
        {
            if unexpected_state {
                warn!(state = ?current_state, "slave remains in unexpected unsynchronized state");
            } else {
                debug!(state = ?current_state, "slave remains unsynchronized");
            }

            return;
        }

        if !warn_on_unsync && self.slave_sync_state != SlaveSyncState::Unsynchronized {
            if unexpected_state {
                warn!(state = ?current_state, "unexpected slave state while waiting for initial synchronization");
            } else {
                debug!(state = ?current_state, "waiting for initial slave synchronization");
            }
        }
    }

    /// Emits a slave state transition log while the slave is unsynchronized.
    fn log_unsynchronized_slave_state_change(
        previous_state: Option<&PortState>,
        current_state: &PortState,
        unexpected_state: bool,
    ) {
        if let Some(previous_state) = previous_state {
            if unexpected_state {
                warn!(
                    previous_state = ?previous_state,
                    state = ?current_state,
                    "slave state changed to unexpected value"
                );
            } else {
                debug!(previous_state = ?previous_state, state = ?current_state, "slave state changed");
            }

            return;
        }

        if unexpected_state {
            warn!(
                previous_state = "unobserved",
                state = ?current_state,
                "slave state changed to unexpected value"
            );
        } else {
            debug!(previous_state = "unobserved", state = ?current_state, "slave state changed");
        }
    }

    /// Records a successful master snapshot and logs any state transition.
    fn on_master_snapshot_ok(&mut self, snapshot: &PtpSnapshot) {
        if self.last_master_process_down_log.is_some() {
            info!("master ptp4l process became reachable again");
        }

        self.last_master_process_down_log = None;

        if self.last_master_pending_log.is_some() {
            debug!("master snapshot became available");
            self.last_master_pending_log = None;
        }

        let current_state = &snapshot.port_data.port_state;
        let unexpected_state = current_state.is_unexpected_for_master();
        match self.last_master_port_state.as_ref() {
            Some(previous_state) if previous_state != current_state => {
                if unexpected_state {
                    warn!(
                        previous_state = ?previous_state,
                        state = ?current_state,
                        "master state changed to unexpected value"
                    );
                } else {
                    debug!(previous_state = ?previous_state, state = ?current_state, "master state changed");
                }
            }
            None => {
                if unexpected_state {
                    warn!(state = ?current_state, "master initial state is unexpected");
                } else {
                    debug!(state = ?current_state, "master initial state observed");
                }
            }
            _ => {}
        }

        self.last_master_port_state = Some(current_state.clone());
    }

    /// Handles the case where the master `pmc` snapshot is temporarily incomplete.
    fn on_master_snapshot_not_ready(&mut self, dataset: &'static str) {
        if self.last_master_process_down_log.is_some() {
            info!("master ptp4l process became reachable again");
        }

        self.last_master_process_down_log = None;

        if self.last_master_pending_log.is_none() {
            if self.last_master_port_state.is_some() {
                warn!(missing_dataset = dataset, "master snapshot unavailable after startup");
            } else {
                debug!(
                    missing_dataset = dataset,
                    "master snapshot not ready yet; waiting for ptp4l startup"
                );
            }

            self.last_master_pending_log = Some(Instant::now());
        } else if Self::should_emit_periodic(&mut self.last_master_pending_log, DEGRADED_STATUS_LOG_INTERVAL) {
            if self.last_master_port_state.is_some() {
                warn!(missing_dataset = dataset, "master snapshot still unavailable");
            } else {
                debug!(
                    missing_dataset = dataset,
                    "master snapshot still not ready during startup"
                );
            }
        }
    }

    /// Emits degraded-status logs when the master `ptp4l` process is down.
    fn on_master_process_down(&mut self, status: Option<process::ExitStatus>) {
        self.last_master_pending_log = None;

        if self.last_master_process_down_log.is_none() {
            if let Some(status) = status {
                error!(status = %status, "master ptp4l process exited unexpectedly");
            } else {
                error!("master ptp4l process not running");
            }

            self.last_master_process_down_log = Some(Instant::now());
        } else if Self::should_emit_periodic(&mut self.last_master_process_down_log, DEGRADED_STATUS_LOG_INTERVAL) {
            if let Some(status) = status {
                warn!(status = %status, "master ptp4l process not running");
            } else {
                warn!("master ptp4l process not running");
            }
        }
    }

    /// Returns whether enough time has elapsed to emit another periodic log.
    fn should_emit_periodic(last_emitted: &mut Option<Instant>, interval: Duration) -> bool {
        let now = Instant::now();

        match last_emitted {
            Some(previous) if now.duration_since(*previous) < interval => false,
            _ => {
                *last_emitted = Some(now);
                true
            }
        }
    }
}

/// Collects and logs one master snapshot with reduced verbosity.
fn handle_master_snapshot(master: &mut PtpInstance, state: &mut MonitorState) {
    match master.snapshot() {
        Ok(snapshot) => state.on_master_snapshot_ok(&snapshot),
        Err(PtpQueryError::NotReady(dataset)) => state.on_master_snapshot_not_ready(dataset),
        Err(PtpQueryError::ProcessExited(status)) => state.on_master_process_down(Some(status)),
        Err(PtpQueryError::ProcessNotRunning) => state.on_master_process_down(None),
        Err(error) => {
            state.last_master_process_down_log = None;
            state.last_master_pending_log = None;
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
