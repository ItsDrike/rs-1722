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

fn main() {
    // Create interfaces (fail fast if missing)
    let master_iface =
        NetworkInterface::new("enp1s0".to_string()).unwrap_or_else(|| fatal("interface enp1s0 does not exist"));

    let slave_iface =
        NetworkInterface::new("enp3s0".to_string()).unwrap_or_else(|| fatal("interface enp3s0 does not exist"));

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

    println!("PTP instances started (master=enp1s0, slave=enp3s0)");

    // Handle Ctrl+C for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        ctrlc::set_handler(move || {
            running.store(false, Ordering::SeqCst);
        })
        .expect("failed to install Ctrl+C handler");
    }

    // Monitoring loop
    while running.load(Ordering::SeqCst) {
        // Query slave (more interesting)
        match slave.snapshot() {
            Ok(snapshot) => {
                if snapshot.is_synchronized() {
                    let offset = snapshot.offset_ns().unwrap_or(0);
                    println!("[slave] synchronized | offset = {offset} ns");
                } else {
                    println!("[slave] not synchronized | state = {:?}", snapshot.port_data.port_state);
                }
            }
            Err(e) => {
                eprintln!("[slave] snapshot error: {e}");
            }
        }

        // Query master (sanity check)
        match master.snapshot() {
            Ok(snapshot) => {
                println!("[master] state = {:?}", snapshot.port_data.port_state);
            }
            Err(e) => {
                eprintln!("[master] snapshot error: {e}");
            }
        }

        thread::sleep(Duration::from_secs(1));
    }

    println!("Shutting down...");

    // Explicit stop (Drop would also handle it, but this is cleaner)
    if let Err(e) = master.stop() {
        eprintln!("failed to stop master: {e}");
    }

    if let Err(e) = slave.stop() {
        eprintln!("failed to stop slave: {e}");
    }
}

fn fatal(msg: &str) -> ! {
    eprintln!("fatal: {msg}");
    process::exit(1);
}
