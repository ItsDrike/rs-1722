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
use rs_1722::ptp::instance::{PtpInstance, PtpRole};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

const MASTER_INTERFACE: &str = "enp1s0";
const SLAVE_INTERFACE: &str = "enp3s0";

fn main() {
    init_tracing();

    // Create interfaces (fail fast if missing)
    let master_iface = NetworkInterface::new(MASTER_INTERFACE.to_string())
        .unwrap_or_else(|| fatal(&format!("interface {MASTER_INTERFACE} does not exist")));

    let slave_iface = NetworkInterface::new(SLAVE_INTERFACE.to_string())
        .unwrap_or_else(|| fatal(&format!("interface {SLAVE_INTERFACE} does not exist")));

    info!(master_interface = %master_iface, slave_interface = %slave_iface, "network interfaces validated");

    // Create instances
    let mut master = PtpInstance::new(master_iface, PtpRole::Master, "master")
        .unwrap_or_else(|e| fatal(&format!("failed to create master: {e}")));

    let mut slave = PtpInstance::new(slave_iface, PtpRole::Slave, "slave")
        .unwrap_or_else(|e| fatal(&format!("failed to create slave: {e}")));

    // Start both
    master
        .start()
        .unwrap_or_else(|e| fatal(&format!("failed to start master: {e}")));
    slave
        .start()
        .unwrap_or_else(|e| fatal(&format!("failed to start slave: {e}")));

    info!(
        master_interface = MASTER_INTERFACE,
        slave_interface = SLAVE_INTERFACE,
        "PTP instances started"
    );

    // Handle Ctrl+C for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || {
            info!("received Ctrl+C signal, stopping monitor loop");
            running.store(false, Ordering::SeqCst);
        })
        .unwrap_or_else(|e| fatal(&format!("failed to install Ctrl+C handler: {e}")));
    }

    // Monitoring loop
    let mut was_slave_synced = None;

    while running.load(Ordering::SeqCst) {
        // Query slave (more interesting)
        match slave.snapshot() {
            Ok(snapshot) => {
                if snapshot.is_synchronized() {
                    let offset = snapshot.offset_ns().unwrap_or(0);

                    if was_slave_synced != Some(true) {
                        info!("slave clock is now synchronized");
                    }

                    debug!(
                        state = ?snapshot.port_data.port_state,
                        offset_ns = offset,
                        gm_identity = %snapshot.time_status.gm_identity,
                        "slave synchronization snapshot"
                    );

                    was_slave_synced = Some(true);
                } else {
                    if was_slave_synced != Some(false) {
                        warn!(state = ?snapshot.port_data.port_state, "slave clock is not synchronized");
                    }

                    debug!(state = ?snapshot.port_data.port_state, "slave synchronization snapshot");

                    was_slave_synced = Some(false);
                }
            }
            Err(e) => {
                error!(error = %e, "failed to collect slave snapshot");
            }
        }

        // Query master (sanity check)
        match master.snapshot() {
            Ok(snapshot) => {
                debug!(state = ?snapshot.port_data.port_state, "master synchronization snapshot");
            }
            Err(e) => {
                error!(error = %e, "failed to collect master snapshot");
            }
        }

        thread::sleep(Duration::from_secs(1));
    }

    info!("shutting down PTP instances");

    // Explicit stop (Drop would also handle it, but this is cleaner)
    if let Err(e) = master.stop() {
        error!(error = %e, "failed to stop master instance");
    }

    if let Err(e) = slave.stop() {
        error!(error = %e, "failed to stop slave instance");
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rs_1722=debug"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .compact()
                .with_target(false)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_timer(fmt::time::uptime()),
        )
        .init();
}

fn fatal(msg: &str) -> ! {
    error!("fatal: {msg}");
    process::exit(1);
}
