use std::{io, num::TryFromIntError, path::PathBuf};

use thiserror::Error;

/// Result type used throughout the public PHC API.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors returned by the public PHC API.
#[derive(Debug, Error)]
pub enum Error {
    /// Opening the requested `/dev/ptpX` device failed.
    #[error("failed to open PTP device {path}")]
    OpenDevice {
        /// Path that was being opened.
        path: PathBuf,

        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },

    /// A Linux PTP ioctl call failed.
    #[error("{operation} ioctl failed")]
    Ioctl {
        /// Short kernel ioctl operation name.
        operation: &'static str,

        /// Underlying ioctl error from `nix`.
        #[source]
        source: nix::Error,
    },

    /// Reading PHC time through `clock_gettime` failed.
    #[error("clock_gettime failed")]
    ClockGettime(#[source] io::Error),

    /// Reading one external timestamp event from the device failed.
    #[error("failed to read external timestamp event")]
    ReadExternalTimestamp(#[source] io::Error),

    /// A negative timestamp value was supplied where the kernel ABI requires a non-negative one.
    #[error("timestamp value must be non-negative")]
    NegativeTimestamp,

    /// A `Duration` did not fit into the Linux PTP timestamp representation.
    #[error("duration is too large for PTP clock time")]
    DurationTooLarge,

    /// Integer conversion failed while translating between Rust and kernel ABI types.
    #[error("integer conversion failed")]
    IntegerConversion(#[from] TryFromIntError),
}
