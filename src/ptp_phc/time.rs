use std::{fmt, time::Duration};

use crate::ptp_phc::{
    abi::PtpClockTime,
    error::{Error, Result},
};

/// Nanoseconds per second.
const NSEC_PER_SEC: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PtpTime {
    pub seconds: i64,
    pub nanoseconds: u32,
}

pub type Timestamp = PtpTime;

impl PtpTime {
    #[must_use]
    pub const fn new(seconds: i64, nanoseconds: u32) -> Self {
        Self { seconds, nanoseconds }
    }

    pub(crate) fn from_timespec(ts: nix::libc::timespec) -> Result<Self> {
        Ok(Self {
            seconds: ts.tv_sec,
            nanoseconds: u32::try_from(ts.tv_nsec)?,
        })
    }

    pub(crate) const fn from_abi(time: PtpClockTime) -> Self {
        Self {
            seconds: time.sec,
            nanoseconds: time.nsec,
        }
    }

    pub(crate) fn from_ns(ns: i64) -> Result<PtpClockTime> {
        if ns < 0 {
            return Err(Error::NegativeTimestamp);
        }

        Ok(PtpClockTime {
            sec: ns / NSEC_PER_SEC,
            nsec: u32::try_from(ns % NSEC_PER_SEC)?,
            reserved: 0,
        })
    }

    pub(crate) fn duration_to_abi(duration: Duration) -> Result<PtpClockTime> {
        let seconds = i64::try_from(duration.as_secs()).map_err(|_| Error::DurationTooLarge)?;

        Ok(PtpClockTime {
            sec: seconds,
            nsec: duration.subsec_nanos(),
            reserved: 0,
        })
    }
}

impl fmt::Display for PtpTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{:09}", self.seconds, self.nanoseconds)
    }
}
