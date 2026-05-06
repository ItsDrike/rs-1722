//! Safe-ish wrapper around Linux PTP hardware clock devices.
//!
//! This module exposes a small Rust API over Linux `/dev/ptpX` character
//! devices using the kernel PTP UAPI.
//!
//! The public API focuses on the common hardware-clock operations that are
//! useful from applications:
//! - querying clock capabilities
//! - reading the current PHC time
//! - routing programmable pins
//! - enabling periodic outputs
//! - enabling external timestamp capture
//!
//! The lower-level ABI definitions used to talk to the kernel live in the
//! internal `abi` module; this module exposes safer, higher-level Rust types for
//! the same concepts.
//!
//! # Example
//!
//! ```no_run
//! use rs_1722::ptp_phc::PtpClock;
//!
//! # fn main() -> rs_1722::ptp_phc::Result<()> {
//! let clock = PtpClock::open("/dev/ptp0")?;
//! let caps = clock.capabilities()?;
//! println!("PHC supports {} programmable pins", caps.programmable_pins);
//! println!("Current PHC time: {}", clock.time()?);
//! # Ok(())
//! # }
//! ```

mod abi;
mod clock;
mod error;
mod pin;
mod time;

/// Bitflags reported for external timestamp capture requests and events.
///
/// This is the public flag type used by [`ExternalTimestampEvent::flags`] and
/// corresponds to the Linux PTP external-timestamp flag field.
pub type ExternalTimestampFlags = crate::ptp_phc::abi::PtpExttsFlags;

pub use crate::ptp_phc::{
    clock::{Capabilities, ExternalTimestampEvent, PtpClock},
    error::{Error, Result},
    pin::{Pin, PinFunction},
    time::{Nanoseconds, PtpTime},
};

/// External timestamp edge selection.
///
/// This controls which edges a PHC external-timestamp channel should capture
/// when passed to [`PtpClock::enable_external_timestamping`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Timestamp rising edges.
    Rising,

    /// Timestamp falling edges.
    Falling,

    /// Timestamp both rising and falling edges.
    Both,
}
