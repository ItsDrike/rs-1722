use crate::net::mac_address::MacAddress;
use std::{fmt, fs, io, path::PathBuf};

/// Represents a network interface on the system.
///
/// This is a lightweight, validated wrapper around a Linux network interface
/// name (e.g., `"eth0"`, `"enp3s0"`). Validation is performed by checking for
/// the existence of the corresponding entry in `/sys/class/net`.
///
/// This type is primarily intended to provide type safety and avoid passing
/// arbitrary strings where a valid interface is required.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetworkInterface(String);

impl NetworkInterface {
    /// Creates a new [`NetworkInterface`] if the given name exists on the system.
    ///
    /// This checks for the presence of `/sys/class/net/{name}` to verify that
    /// the interface is available.
    ///
    /// # Arguments
    /// * `name` - The name of the network interface (e.g., `"eth0"`).
    ///
    /// # Returns
    /// * `Some(NetworkInterface)` if the interface exists
    /// * `None` if the interface does not exist
    #[must_use]
    pub fn new(name: String) -> Option<Self> {
        let path = PathBuf::from("/sys/class/net").join(&name);

        if !path.exists() {
            return None;
        }

        Some(Self(name))
    }

    /// Returns the name of the network interface as a string slice.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }

    /// Consumes the interface and returns the underlying name.
    #[must_use]
    pub fn into_name(self) -> String {
        self.0
    }

    /// Returns the sysfs path corresponding to this network interface.
    ///
    /// This resolves to `/sys/class/net/{name}`.
    #[must_use]
    pub fn sysfs_path(&self) -> PathBuf {
        PathBuf::from("/sys/class/net").join(&self.0)
    }

    /// Returns the MAC address of this interface.
    ///
    /// This reads `/sys/class/net/{name}/address` and parses it into a [`MacAddress`].
    ///
    /// # Errors
    /// Returns an error if the sysfs file cannot be read (e.g., the interface
    /// disappears or permissions are insufficient).
    ///
    /// # Panics
    /// Panics if the kernel-provided MAC address is not in a valid
    /// `xx:xx:xx:xx:xx:xx` format. This should never happen under normal
    /// conditions, as the value is provided by the kernel.
    pub fn mac_address(&self) -> io::Result<MacAddress> {
        let path = self.sysfs_path().join("address");
        let content = fs::read_to_string(path)?;

        let mac = content.trim().parse().expect("kernel provided invalid MAC address");

        Ok(mac)
    }
}

impl fmt::Display for NetworkInterface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
