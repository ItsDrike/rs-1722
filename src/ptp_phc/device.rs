//! PTP device discovery and interface association.
//!
//! This module provides utilities for discovering PTP (Precision Time Protocol) hardware
//! devices and determining which devices are associated with network interfaces on Linux.

use std::fs;
use std::path::{Path, PathBuf};

/// Find the PTP device associated with a network interface.
///
/// On Linux, PTP devices are associated with network interfaces through the sysfs filesystem.
/// This function checks `/sys/class/net/<interface>/device/ptp/` to find the PTP device
/// (if any) that is directly associated with the given interface.
///
/// # Arguments
/// * `iface_name` - The name of the network interface (e.g., "eth0", "enp2s0")
///
/// # Returns
/// * `Some(PathBuf)` containing the path to the PTP device (e.g., `/dev/ptp0`) if found
/// * `None` if no PTP device is associated with the interface
///
/// # Example
/// ```no_run
/// # use rs_1722::ptp_phc::device;
/// let ptp_dev = device::find_ptp_device_for_interface("enp2s0");
/// match ptp_dev {
///     Some(path) => println!("PTP device: {}", path.display()),
///     None => println!("No PTP device found for this interface"),
/// }
/// ```
#[must_use]
pub fn find_ptp_device_for_interface(iface_name: &str) -> Option<PathBuf> {
    // Check sysfs for PTP device associated with this interface
    let sysfs_ptp_path = format!("/sys/class/net/{iface_name}/device/ptp");
    let ptp_dir = Path::new(&sysfs_ptp_path);

    if !ptp_dir.exists() {
        return None;
    }

    // List files in the ptp directory, expecting something like "ptp0", "ptp1", etc.
    if let Ok(entries) = fs::read_dir(ptp_dir) {
        for entry in entries.flatten() {
            if let Some(file_name) = entry.file_name().to_str() {
                // PTP device names match pattern "ptpX" where X is a number
                if file_name.starts_with("ptp") && file_name[3..].chars().all(|c| c.is_ascii_digit()) {
                    return Some(PathBuf::from(format!("/dev/{file_name}")));
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ptp_device_path_construction() {
        // This test verifies the logic without requiring actual sysfs
        // On systems without PTP, the function should return None gracefully
        let result = find_ptp_device_for_interface("nonexistent_interface_xyz");
        assert_eq!(result, None);
    }
}
