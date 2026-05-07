use std::{
    fs::{File, OpenOptions},
    io::Read,
    mem,
    os::fd::{AsRawFd, RawFd},
    path::{Path, PathBuf},
    time::Duration,
};

use crate::ptp_phc::{
    Edge, ExternalTimestampFlags,
    abi::{
        PtpClockCaps, PtpClockTime, PtpExttsEvent, PtpExttsFlags, PtpExttsRequest, PtpPeroutFlags, PtpPeroutRequest,
        PtpPinDesc, ptp_clock_getcaps_ioctl, ptp_extts_request_ioctl, ptp_extts_request2_ioctl,
        ptp_perout_request_ioctl, ptp_perout_request2_ioctl, ptp_pin_getfunc_ioctl, ptp_pin_setfunc_ioctl,
    },
    error::{Error, Result},
    pin::{Pin, PinFunction},
    time::PtpTime,
};

/// Shared interface for any source of PTP time.
///
/// This trait is implemented by all PTP clock types (`PtpClockHardware`, `PtpClockSystemTime`, `PtpClock`)
/// and enables generic code to work with any clock source.
pub trait PtpTimeSource {
    /// Reads the current time from the PTP clock.
    ///
    /// # Errors
    /// Returns an error if the time reading fails.
    fn time(&self) -> Result<PtpTime>;
}

/// High-level capabilities reported by one PTP hardware clock.
///
/// This is the public, ergonomic view of the kernel's PHC capability block.
/// The values describe which optional operations are supported by the device
/// and how many channels or pins are available for each operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Maximum frequency adjustment supported by the PHC, in parts per billion.
    pub max_adjustment_ppb: i32,

    /// Number of programmable alarm channels supported by the device.
    pub programmable_alarms: i32,

    /// Number of external timestamp capture channels available.
    pub external_timestamp_channels: i32,

    /// Number of periodic-output channels available.
    pub periodic_output_channels: i32,

    /// Whether the device supports the generic PPS enable operation.
    pub pulse_per_second: bool,

    /// Number of programmable physical pins exposed by the PHC.
    pub programmable_pins: i32,

    /// Whether precise PHC/system cross timestamping is supported.
    pub cross_timestamping: bool,

    /// Whether the PHC supports phase adjustment operations.
    pub adjust_phase: bool,

    /// Maximum supported phase adjustment, in nanoseconds.
    pub max_phase_adjustment_ns: i32,
}

impl From<PtpClockCaps> for Capabilities {
    fn from(caps: PtpClockCaps) -> Self {
        Self {
            max_adjustment_ppb: caps.max_adj,
            programmable_alarms: caps.n_alarm,
            external_timestamp_channels: caps.n_ext_ts,
            periodic_output_channels: caps.n_per_out,
            pulse_per_second: caps.pps != 0,
            programmable_pins: caps.n_pins,
            cross_timestamping: caps.cross_timestamping != 0,
            adjust_phase: caps.adjust_phase != 0,
            max_phase_adjustment_ns: caps.max_phase_adj,
        }
    }
}

/// One externally captured timestamp event read from a PHC device.
///
/// Events of this type are produced by [`PtpClockHardware::read_external_timestamp_event`]
/// after a channel has been enabled with
/// [`PtpClockHardware::enable_external_timestamping`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalTimestampEvent {
    /// PHC timestamp captured for the external input event.
    pub timestamp: PtpTime,

    /// External timestamp channel that produced this event.
    pub channel: u32,

    /// Kernel event flags associated with the captured edge.
    ///
    /// These indicate properties reported by the Linux PTP ABI for the event,
    /// such as whether the event record is valid.
    pub flags: ExternalTimestampFlags,
}

/// Handle to one Linux PTP hardware clock device with full feature support.
///
/// A `PtpClockHardware` provides access to all hardware PTP operations:
/// - Reading time from the clock
/// - Querying capabilities and pins
/// - Configuring pins and periodic output
/// - Capturing external timestamps
///
/// To support systems without PTP hardware, use [`PtpClock::open_with_system_time_fallback`]
/// for runtime flexibility, or use [`PtpClockSystemTime`] for explicit system-time-only operation.
#[derive(Debug)]
pub struct PtpClockHardware {
    device: File,
    path: PathBuf,
}

impl PtpClockHardware {
    /// Opens a PTP hardware clock device.
    ///
    /// # Errors
    /// Returns an error if the device file cannot be opened (does not exist, permission denied, etc.)
    ///
    /// # Note
    /// This does NOT check whether the given file path points to a valid PTP device.
    /// If this is not the case, errors will occur later on, when communication with
    /// this device is actually attempted.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| Error::OpenDevice {
                path: path.clone(),
                source,
            })?;

        Ok(Self { device, path })
    }

    /// Returns the filesystem path of the opened PHC device.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the capabilities reported by the PTP device.
    ///
    /// # Errors
    /// Returns an error if the underlying capability ioctl fails.
    pub fn capabilities(&self) -> Result<Capabilities> {
        Ok(Capabilities::from(self.raw_capabilities()?))
    }

    /// Returns all programmable pins exposed by the PTP device.
    ///
    /// # Errors
    /// Returns an error if the capability query fails, the pin count cannot be converted,
    /// or any pin query ioctl fails.
    pub fn pins(&self) -> Result<Vec<Pin>> {
        let caps = self.raw_capabilities()?;
        let pin_count = u32::try_from(caps.n_pins)?;

        (0..pin_count).map(|index| self.pin(index)).collect()
    }

    /// Returns one programmable pin description by index.
    ///
    /// # Errors
    /// Returns an error if the underlying pin query ioctl fails.
    pub fn pin(&self, index: u32) -> Result<Pin> {
        Ok(Pin::from_desc(self.raw_pin(index)?))
    }

    /// Routes a PTP function to a programmable pin.
    ///
    /// # Errors
    /// Returns an error if the underlying set-function ioctl fails.
    pub fn set_pin_function(&self, index: u32, function: PinFunction, channel: u32) -> Result<()> {
        let desc = PtpPinDesc {
            index,
            func: function.to_abi(),
            chan: channel,
            ..PtpPinDesc::default()
        };

        unsafe {
            ptp_pin_setfunc_ioctl(self.hardware_fd(), &raw const desc).map_err(|source| Error::Ioctl {
                operation: "PTP_PIN_SETFUNC",
                source,
            })?;
        }

        Ok(())
    }

    /// Enables periodic output on a PTP periodic-output channel.
    ///
    /// # Errors
    /// Returns an error if the period or phase cannot be represented in the kernel ABI, if the
    /// current clock time cannot be read for an absolute start time, or if the periodic-output
    /// ioctl fails.
    pub fn enable_periodic_output(&self, channel: u32, period: Duration, phase: Option<Duration>) -> Result<()> {
        let period = PtpTime::try_from(period)?;

        let mut request = PtpPeroutRequest {
            period: period.into_abi(),
            index: channel,
            ..PtpPeroutRequest::default()
        };

        if let Some(phase) = phase {
            request.start_or_phase = PtpTime::try_from(phase)?.into_abi();
            request.flags = PtpPeroutFlags::PHASE;
        } else {
            let now = self.clock_gettime()?;

            // Match the kernel's `testptp` default: start on a whole-second
            // boundary with about 1-2 seconds of slack so the absolute start
            // time is safely in the future by the time the ioctl is handled.
            request.start_or_phase = PtpClockTime {
                sec: now.tv_sec + 2,
                nsec: 0,
                reserved: 0,
            };
        }

        self.perout_request(&request)
    }

    /// Enables periodic output using nanosecond values.
    ///
    /// # Errors
    /// Returns an error if the period or phase is negative or cannot be represented in the kernel
    /// ABI, if the current clock time cannot be read for an absolute start time, or if the
    /// periodic-output ioctl fails.
    pub fn enable_periodic_output_ns(&self, channel: u32, period_ns: i64, phase_ns: Option<i64>) -> Result<()> {
        let period = PtpTime::from_ns(period_ns);

        let mut request = PtpPeroutRequest {
            period: period.into_abi(),
            index: channel,
            ..PtpPeroutRequest::default()
        };

        if let Some(phase_ns) = phase_ns {
            request.start_or_phase = PtpTime::from_ns(phase_ns).into_abi();
            request.flags = PtpPeroutFlags::PHASE;
        } else {
            let now = self.clock_gettime()?;

            // Match the kernel's `testptp` default: start on a whole-second
            // boundary with about 1-2 seconds of slack so the absolute start
            // time is safely in the future by the time the ioctl is handled.
            request.start_or_phase = PtpClockTime {
                sec: now.tv_sec + 2,
                nsec: 0,
                reserved: 0,
            };
        }

        self.perout_request(&request)
    }

    /// Disables periodic output on a PTP periodic-output channel.
    ///
    /// # Errors
    /// Returns an error if the periodic-output ioctl fails.
    pub fn disable_periodic_output(&self, channel: u32) -> Result<()> {
        let request = PtpPeroutRequest {
            index: channel,
            ..PtpPeroutRequest::default()
        };

        self.perout_request(&request)
    }

    /// Enables external timestamp capture on a channel for the selected edge mode.
    ///
    /// # Errors
    /// Returns an error if the external-timestamp ioctl fails.
    pub fn enable_external_timestamping(&self, channel: u32, edge: Edge) -> Result<()> {
        let edge_flags = match edge {
            Edge::Rising => PtpExttsFlags::RISING_EDGE,
            Edge::Falling => PtpExttsFlags::FALLING_EDGE,
            Edge::Both => PtpExttsFlags::RISING_EDGE | PtpExttsFlags::FALLING_EDGE,
        };

        let request = PtpExttsRequest {
            index: channel,
            flags: PtpExttsFlags::ENABLE_FEATURE | edge_flags,
            rsv: [0; 2],
        };

        self.extts_request(&request)
    }

    /// Disables external timestamp capture on a channel.
    ///
    /// # Errors
    /// Returns an error if the external-timestamp ioctl fails.
    pub fn disable_external_timestamping(&self, channel: u32) -> Result<()> {
        let request = PtpExttsRequest {
            index: channel,
            flags: PtpExttsFlags::empty(),
            rsv: [0; 2],
        };

        self.extts_request(&request)
    }

    /// Reads one external timestamp event from the device.
    ///
    /// # Errors
    /// Returns an error if reading the event bytes from the device fails.
    pub fn read_external_timestamp_event(&mut self) -> Result<ExternalTimestampEvent> {
        let mut event = PtpExttsEvent::default();

        let event_bytes =
            unsafe { std::slice::from_raw_parts_mut((&raw mut event).cast::<u8>(), mem::size_of::<PtpExttsEvent>()) };

        self.device.read_exact(event_bytes).map_err(Error::ReadExternalTimestamp)?;

        Ok(ExternalTimestampEvent {
            timestamp: PtpTime::from_abi(event.t),
            channel: event.index,
            flags: event.flags,
        })
    }

    fn hardware_fd(&self) -> RawFd {
        self.device.as_raw_fd()
    }

    fn raw_capabilities(&self) -> Result<PtpClockCaps> {
        let mut caps = PtpClockCaps::default();

        unsafe {
            ptp_clock_getcaps_ioctl(self.hardware_fd(), &raw mut caps).map_err(|source| Error::Ioctl {
                operation: "PTP_CLOCK_GETCAPS",
                source,
            })?;
        }

        Ok(caps)
    }

    fn raw_pin(&self, index: u32) -> Result<PtpPinDesc> {
        let mut desc = PtpPinDesc {
            index,
            ..PtpPinDesc::default()
        };

        unsafe {
            ptp_pin_getfunc_ioctl(self.hardware_fd(), &raw mut desc).map_err(|source| Error::Ioctl {
                operation: "PTP_PIN_GETFUNC",
                source,
            })?;
        }

        Ok(desc)
    }

    fn perout_request(&self, request: &PtpPeroutRequest) -> Result<()> {
        let new_result = unsafe { ptp_perout_request2_ioctl(self.hardware_fd(), request) };

        if new_result.is_ok() {
            return Ok(());
        }

        unsafe {
            ptp_perout_request_ioctl(self.hardware_fd(), request).map_err(|source| Error::Ioctl {
                operation: "PTP_PEROUT_REQUEST",
                source,
            })?;
        }

        Ok(())
    }

    fn extts_request(&self, request: &PtpExttsRequest) -> Result<()> {
        let new_result = unsafe { ptp_extts_request2_ioctl(self.hardware_fd(), request) };

        if new_result.is_ok() {
            return Ok(());
        }

        unsafe {
            ptp_extts_request_ioctl(self.hardware_fd(), request).map_err(|source| Error::Ioctl {
                operation: "PTP_EXTTS_REQUEST",
                source,
            })?;
        }

        Ok(())
    }

    fn clock_gettime(&self) -> Result<nix::libc::timespec> {
        let clock_id = clock_id_from_fd(self.hardware_fd());

        let mut ts = nix::libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let ret = unsafe { nix::libc::clock_gettime(clock_id, &raw mut ts) };

        if ret < 0 {
            return Err(Error::ClockGettime(std::io::Error::last_os_error()));
        }

        Ok(ts)
    }
}

impl PtpTimeSource for PtpClockHardware {
    fn time(&self) -> Result<PtpTime> {
        let ts = self.clock_gettime()?;

        // SAFETY: `clock_gettime` returns a normalized `timespec` with
        // `tv_nsec` in the range `0..1_000_000_000` on success.
        Ok(unsafe { PtpTime::from_normalized_timespec(ts) })
    }
}

/// System time fallback using `CLOCK_REALTIME`.
///
/// **Warning**: For testing and development only. Timestamps are not synchronized with PTP hardware
/// and may drift. Never use in production without actual hardware PTP.
///
/// This type is useful for running PTP-dependent code on systems without hardware PTP devices,
/// but should never be relied upon for production synchronization.
///
/// Create with [`PtpClockSystemTime::new`].
#[derive(Debug, Clone, Copy)]
pub struct PtpClockSystemTime;

impl PtpClockSystemTime {
    /// Creates a new system-time clock.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for PtpClockSystemTime {
    fn default() -> Self {
        Self::new()
    }
}

impl PtpTimeSource for PtpClockSystemTime {
    fn time(&self) -> Result<PtpTime> {
        let mut ts = nix::libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let ret = unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_REALTIME, &raw mut ts) };

        if ret < 0 {
            return Err(Error::ClockGettime(std::io::Error::last_os_error()));
        }

        // SAFETY: `clock_gettime` returns a normalized `timespec` with
        // `tv_nsec` in the range `0..1_000_000_000` on success.
        Ok(unsafe { PtpTime::from_normalized_timespec(ts) })
    }
}

/// Runtime-flexible PTP clock that can be either hardware or system-time based.
///
/// A `PtpClock` provides access to time reading from either:
/// - **Hardware variant**: An open `/dev/ptpX` file descriptor with full PTP hardware support
/// - **System time variant**: `CLOCK_REALTIME` via syscall (testing only)
///
/// For code that statically knows it needs hardware operations (pins, external timestamps, etc.),
/// use [`PtpClockHardware`] directly to get compile-time guarantees that the operations exist.
///
/// For code that needs runtime flexibility (e.g., binaries with `--system-time-fallback`),
/// use `PtpClock` and implement time access through the [`PtpTimeSource`] trait.
///
/// # Creating a `PtpClock`
///
/// ```no_run
/// use rs_1722::ptp_phc::{PtpClock, PtpTimeSource};
///
/// # fn main() -> rs_1722::ptp_phc::Result<()> {
/// // Hardware-only (strict)
/// let clock = PtpClock::open("/dev/ptp0")?;
/// println!("Current time: {}", clock.time()?);
/// # Ok(())
/// # }
/// ```
///
/// ```no_run
/// use rs_1722::ptp_phc::{PtpClock, PtpTimeSource};
///
/// # fn main() -> rs_1722::ptp_phc::Result<()> {
/// // With system-time fallback
/// let clock = PtpClock::open_with_system_time_fallback("/dev/ptp0")?;
/// println!("Current time: {}", clock.time()?);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub enum PtpClock {
    /// Hardware PTP device with full feature support.
    Hardware(PtpClockHardware),

    /// System time fallback (testing/development only).
    SystemTime(PtpClockSystemTime),
}

impl PtpClock {
    /// Opens a PTP hardware clock device in strict mode.
    ///
    /// This method requires the device to exist and be accessible. If you need to support
    /// systems without PTP hardware, use [`Self::open_with_system_time_fallback`] instead.
    ///
    /// # Errors
    /// Returns an error if the device file cannot be opened (does not exist, permission denied, etc.)
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        PtpClockHardware::open(path).map(Self::Hardware)
    }

    /// Opens a PTP hardware clock device with system time fallback.
    ///
    /// **Warning**: This mode is intended for **testing and development only**. Use it only when:
    /// - You cannot access hardware PTP devices
    /// - You are running automated tests or development builds
    /// - You understand that timestamps will not be synchronized with real PTP hardware
    ///
    /// If the device exists, it will be used (hardware mode). If the device does not exist,
    /// this method falls back to `CLOCK_REALTIME` (system time) and logs a warning.
    ///
    /// In fallback mode:
    /// - `time()` will work normally
    /// - Timestamps may drift relative to real PTP synchronization
    ///
    /// # Errors
    /// Returns an error if the device cannot be opened for any reason other than "not found",
    /// or if system time is unavailable (extremely rare).
    pub fn open_with_system_time_fallback(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(device) => Ok(Self::Hardware(PtpClockHardware {
                device,
                path,
            })),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(Self::SystemTime(PtpClockSystemTime))
                } else {
                    Err(Error::OpenDevice {
                        path,
                        source: e,
                    })
                }
            }
        }
    }
}

impl PtpTimeSource for PtpClock {
    fn time(&self) -> Result<PtpTime> {
        match self {
            Self::Hardware(hw) => hw.time(),
            Self::SystemTime(st) => st.time(),
        }
    }
}

impl From<PtpClockHardware> for PtpClock {
    fn from(hw: PtpClockHardware) -> Self {
        Self::Hardware(hw)
    }
}

impl From<PtpClockSystemTime> for PtpClock {
    fn from(st: PtpClockSystemTime) -> Self {
        Self::SystemTime(st)
    }
}

const fn clock_id_from_fd(fd: RawFd) -> nix::libc::clockid_t {
    const CLOCKFD: nix::libc::clockid_t = 3;
    ((!fd) << 3) | CLOCKFD
}
