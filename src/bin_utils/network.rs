//! Network interface utilities for CLI applications.

use std::path::{Path, PathBuf};

use pnet::datalink::NetworkInterface;
use thiserror::Error;

use std::fmt;

use crate::ptp_phc::{self, PtpClock, PtpClockSystemTime, device};

/// Resolved time source for a validated interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockSource {
    /// Use the specified hardware PTP device.
    PtpDevice(PathBuf),

    /// Use `CLOCK_REALTIME` instead of a hardware PTP clock.
    SystemTime,
}

impl ClockSource {
    /// Open a fresh clock handle for this time source.
    ///
    /// # Errors
    /// Returns an error if the configured hardware PTP device cannot be opened.
    pub fn open_clock(&self) -> Result<PtpClock, ptp_phc::Error> {
        match self {
            Self::PtpDevice(path) => PtpClock::open(path),
            Self::SystemTime => Ok(PtpClockSystemTime::new().into()),
        }
    }
}

impl fmt::Display for ClockSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PtpDevice(path) => write!(f, "{}", path.display()),
            Self::SystemTime => f.write_str("CLOCK_REALTIME"),
        }
    }
}

/// Error type for validated interface initialization.
#[derive(Debug, Error)]
pub enum ValidatedInterfaceError {
    /// The interface does not exist.
    #[error("Interface '{0}' not found")]
    InterfaceNotFound(String),

    /// The interface has no MAC address.
    #[error("Interface '{0}' has no MAC address")]
    NoMacAddress(String),

    /// The interface is not up.
    #[error("Interface '{0}' is not up")]
    InterfaceDown(String),

    /// No PTP device found.
    #[error("No PTP device found for interface '{0}'")]
    NoPtpDevice(String),

    /// Failed to open PTP clock.
    #[error("Failed to open PTP clock for interface '{0}': {1}")]
    PtpOpenFailed(String, ptp_phc::Error),
}

/// Find a network interface by name.
///
/// Returns `None` if the interface doesn't exist.
#[must_use]
pub fn find_interface(name: &str) -> Option<NetworkInterface> {
    pnet::datalink::interfaces().into_iter().find(|i| i.name == name)
}

/// A validated network interface with an opened PTP clock.
///
/// This type ensures that:
/// - The interface exists and is up
/// - The interface has a MAC address
/// - A PTP clock has been successfully opened
///
/// This prevents scripts from having to manually perform all these checks.
#[derive(Debug)]
pub struct ValidatedInterface {
    interface: NetworkInterface,
    clock_source: ClockSource,
}

impl ValidatedInterface {
    /// Create a validated interface with auto-detected PTP device.
    ///
    /// This performs all necessary validation:
    /// - Verifies the interface exists and is up
    /// - Ensures the interface has a MAC address
    /// - Auto-detects the associated PTP device
    /// - Opens the PTP clock
    ///
    /// If no PTP device is found, returns an error.
    ///
    /// # Arguments
    /// * `name` - Network interface name (e.g., "eth0", "enp2s0")
    ///
    /// # Errors
    /// Returns a `ValidatedInterfaceError` if validation or clock opening fails.
    pub fn new(name: &str) -> Result<Self, ValidatedInterfaceError> {
        let ptp_device = device::find_ptp_device_for_interface(name)
            .ok_or_else(|| ValidatedInterfaceError::NoPtpDevice(name.to_string()))?;

        Self::with_clock_source(name, ClockSource::PtpDevice(ptp_device))
    }

    /// Create with an explicit PTP device path.
    ///
    /// Performs the same validation as `new()` but uses the provided PTP path.
    ///
    /// # Arguments
    /// * `name` - Network interface name
    /// * `ptp_device` - Path to the PTP device (e.g., `/dev/ptp0`)
    ///
    /// # Errors
    /// Returns a `ValidatedInterfaceError` if validation or clock opening fails.
    pub fn with_explicit_ptp(name: &str, ptp_device: &Path) -> Result<Self, ValidatedInterfaceError> {
        Self::with_clock_source(name, ClockSource::PtpDevice(ptp_device.to_path_buf()))
    }

    /// Create with explicit system time.
    ///
    /// Performs the same interface validation as `new()` but uses `CLOCK_REALTIME`
    /// instead of a hardware PTP device.
    ///
    /// # Arguments
    /// * `name` - Network interface name
    ///
    /// # Errors
    /// Returns a `ValidatedInterfaceError` if validation fails.
    pub fn with_system_time(name: &str) -> Result<Self, ValidatedInterfaceError> {
        Self::with_clock_source(name, ClockSource::SystemTime)
    }

    fn with_clock_source(name: &str, clock_source: ClockSource) -> Result<Self, ValidatedInterfaceError> {
        let interface =
            find_interface(name).ok_or_else(|| ValidatedInterfaceError::InterfaceNotFound(name.to_string()))?;

        if !interface.is_up() {
            return Err(ValidatedInterfaceError::InterfaceDown(name.to_string()));
        }

        if interface.mac.is_none() {
            return Err(ValidatedInterfaceError::NoMacAddress(name.to_string()));
        }

        if let ClockSource::PtpDevice(path) = &clock_source {
            clock_source
                .open_clock()
                .map_err(|e| ValidatedInterfaceError::PtpOpenFailed(name.to_string(), e))?;

            debug_assert!(path.is_absolute());
        }

        Ok(Self {
            interface,
            clock_source,
        })
    }

    /// Get the network interface.
    #[must_use]
    pub const fn interface(&self) -> &NetworkInterface {
        &self.interface
    }

    /// Get the validated interface MAC address.
    ///
    /// # Panics
    /// Panics if the internal validation invariant is broken and the interface has no MAC address.
    #[allow(clippy::missing_const_for_fn)]
    #[must_use]
    pub fn mac_address(&self) -> pnet::util::MacAddr {
        self.interface
            .mac
            .expect("validated interfaces always have a MAC address")
    }

    /// Get the resolved clock source.
    #[must_use]
    pub const fn clock_source(&self) -> &ClockSource {
        &self.clock_source
    }

    /// Open a fresh clock handle for this interface's configured time source.
    ///
    /// # Errors
    /// Returns a `ValidatedInterfaceError` if the configured PTP device can no longer be opened.
    pub fn open_clock(&self) -> Result<PtpClock, ValidatedInterfaceError> {
        self.clock_source
            .open_clock()
            .map_err(|e| ValidatedInterfaceError::PtpOpenFailed(self.interface.name.clone(), e))
    }

    /// Get the interface name.
    #[must_use]
    pub fn interface_name(&self) -> &str {
        &self.interface.name
    }
}
