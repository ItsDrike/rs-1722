use std::{io, num::TryFromIntError, path::PathBuf};

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to open PTP device {path}")]
    OpenDevice {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{operation} ioctl failed")]
    Ioctl {
        operation: &'static str,
        #[source]
        source: nix::Error,
    },

    #[error("clock_gettime failed")]
    ClockGettime(#[source] io::Error),

    #[error("failed to read external timestamp event")]
    ReadExternalTimestamp(#[source] io::Error),

    #[error("timestamp value must be non-negative")]
    NegativeTimestamp,

    #[error("duration is too large for PTP clock time")]
    DurationTooLarge,

    #[error("integer conversion failed")]
    IntegerConversion(#[from] TryFromIntError),

    #[error("unsupported pin function value {0}")]
    UnsupportedPinFunction(u32),
}
