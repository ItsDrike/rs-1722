use std::{fmt, time::Duration};

use crate::ptp_phc::{
    abi::PtpClockTime,
    error::{Error, Result},
};

/// Nanoseconds per second.
const NSEC_PER_SEC: i64 = 1_000_000_000;

/// Timestamp value used by the public PHC API.
///
/// This is a simple second-plus-nanosecond representation of PHC time. Values
/// returned by this module are normalized so that `nanoseconds < 1_000_000_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PtpTime {
    /// Whole seconds component of the timestamp.
    pub seconds: i64,

    /// Fractional nanoseconds component of the timestamp.
    pub nanoseconds: u32,
}

impl PtpTime {
    /// Creates a timestamp from explicit seconds and nanoseconds components.
    ///
    /// This constructor does not normalize or validate the components.
    #[must_use]
    pub const fn new(seconds: i64, nanoseconds: u32) -> Self {
        Self { seconds, nanoseconds }
    }

    /// Converts a normalized POSIX `timespec` into a [`PtpTime`].
    ///
    /// # Safety
    /// `ts` must be normalized such that `ts.tv_nsec` is in the range
    /// `0..1_000_000_000`. This is the invariant guaranteed by successful
    /// `clock_gettime` calls.
    pub(crate) unsafe fn from_normalized_timespec(ts: nix::libc::timespec) -> Self {
        debug_assert!((0..NSEC_PER_SEC).contains(&ts.tv_nsec));

        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // SAFETY: `tv_nsec` is required to be normalized and non-negative.
        let nanoseconds = ts.tv_nsec as u32;

        Self {
            seconds: ts.tv_sec,
            nanoseconds,
        }
    }

    pub(crate) const fn from_abi(time: PtpClockTime) -> Self {
        Self {
            seconds: time.sec,
            nanoseconds: time.nsec,
        }
    }

    pub(crate) const fn from_ns(ns: i64) -> Result<PtpClockTime> {
        if ns < 0 {
            return Err(Error::NegativeTimestamp);
        }

        #[expect(clippy::cast_sign_loss)]
        // This cannot truncate nor lose a sign at this point
        let nsec = (ns % NSEC_PER_SEC) as u32;

        Ok(PtpClockTime {
            sec: ns / NSEC_PER_SEC,
            nsec,
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
