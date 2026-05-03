//! Safe-ish wrapper around Linux PTP Hardware Clock devices.
//!
//! This crate exposes a small Rust API over `/dev/ptpX` devices using the
//! Linux PTP UAPI.

mod abi;
mod clock;
mod error;
mod pin;
mod time;

pub use crate::ptp_phc::{
    clock::{ExternalTimestampEvent, PtpClock},
    error::{Error, Result},
    pin::{Pin, PinFunction},
    time::{PtpTime, Timestamp},
};

/// External timestamp edge selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Timestamp rising edges.
    Rising,

    /// Timestamp falling edges.
    Falling,

    /// Timestamp both rising and falling edges.
    Both,
}
