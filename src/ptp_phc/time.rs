use std::{fmt, time::Duration};

use crate::ptp_phc::{
    abi::PtpClockTime,
    error::{Error, Result},
};

/// Nanoseconds per second.
const NSEC_PER_SEC: i64 = 1_000_000_000;
const NSEC_PER_SEC_U32: u32 = 1_000_000_000;

/// Nanoseconds within a second (0-999_999_999).
///
/// This type enforces the invariant that nanoseconds are always less than
/// one billion, matching the constraint of a second's fractional component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Nanoseconds(u32);

impl Nanoseconds {
    /// Creates a `Nanoseconds` value, returning an error if the value exceeds the bound.
    ///
    /// # Errors
    ///
    /// Returns `Error::NegativeTimestamp` if `nanos >= 1_000_000_000`.
    pub const fn new(nanos: u32) -> Result<Self> {
        if nanos >= NSEC_PER_SEC_U32 {
            Err(Error::NegativeTimestamp)
        } else {
            Ok(Self(nanos))
        }
    }

    /// Creates a `Nanoseconds` value without validation.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `nanos < 1_000_000_000`.
    #[must_use]
    pub const unsafe fn new_unchecked(nanos: u32) -> Self {
        Self(nanos)
    }

    /// Returns the nanoseconds value as a `u32`.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Nanoseconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Timestamp value used by the public PHC API.
///
/// This is a second-plus-nanosecond representation of PHC time. Values
/// returned by this module are normalized so that `nanoseconds < 1_000_000_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PtpTime {
    /// Whole seconds component of the timestamp.
    pub seconds: i64,

    /// Fractional nanoseconds component of the timestamp.
    pub nanoseconds: Nanoseconds,
}

impl PtpTime {
    /// Creates a timestamp from explicit seconds and nanoseconds components.
    ///
    /// # Errors
    ///
    /// Returns `Error::NegativeTimestamp` if `nanoseconds >= 1_000_000_000`.
    pub fn new(seconds: i64, nanoseconds: u32) -> Result<Self> {
        let ns = Nanoseconds::new(nanoseconds)?;
        Ok(Self { seconds, nanoseconds: ns })
    }

    /// Creates a timestamp from explicit seconds and nanoseconds components without validation.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `nanoseconds < 1_000_000_000`.
    #[must_use]
    pub const unsafe fn new_unchecked(seconds: i64, nanoseconds: u32) -> Self {
        Self {
            seconds,
            // SAFETY: The caller guarantees nanoseconds < 1_000_000_000.
            nanoseconds: unsafe { Nanoseconds::new_unchecked(nanoseconds) },
        }
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
        let nanos = ts.tv_nsec as u32;

        Self {
            seconds: ts.tv_sec,
            // SAFETY: `tv_nsec` is guaranteed to be in range by the precondition.
            nanoseconds: unsafe { Nanoseconds::new_unchecked(nanos) },
        }
    }

    pub(crate) fn from_abi(time: PtpClockTime) -> Self {
        // Returned ABI time is guaranteed to be normalized in nanoseconds
        debug_assert!(time.nsec < 1_000_000_000);

        Self {
            seconds: time.sec,
            // SAFETY: ABI time is guaranteed to be normalized.
            nanoseconds: unsafe { Nanoseconds::new_unchecked(time.nsec) },
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

    /// Converts this PTP time to nanoseconds as a signed i128.
    ///
    /// This representation allows computing signed time differences that may be
    /// negative (e.g., when measuring time intervals across clock corrections).
    /// Uses i128 to avoid overflow, since PTP seconds can be large.
    #[must_use]
    pub const fn as_nanos(self) -> i128 {
        #[allow(clippy::cast_lossless)]
        let sec_nanos = (self.seconds as i128).saturating_mul(NSEC_PER_SEC as i128);
        sec_nanos.saturating_add(self.nanoseconds.get() as i128)
    }
}

impl fmt::Display for PtpTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{:09}", self.seconds, self.nanoseconds.get())
    }
}
