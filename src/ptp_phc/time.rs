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
/// This is an internal implementation detail; use the public API of `PtpTime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Nanoseconds(u32);

impl Nanoseconds {
    /// Creates a `Nanoseconds` value without validation.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `nanos < 1_000_000_000`.
    #[must_use]
    const unsafe fn new_unchecked(nanos: u32) -> Self {
        Self(nanos)
    }

    /// Returns the nanoseconds value as a `u32`.
    #[must_use]
    const fn get(self) -> u32 {
        self.0
    }
}

/// Precision Hardware Clock (PHC) timestamp.
///
/// A `PtpTime` represents a snapshot of time from a PHC device, stored as seconds
/// plus fractional nanoseconds. The exact meaning of "time zero" depends on the
/// PHC's epoch.
///
/// ## Epoch Independence
///
/// Different PHC devices can have different epochs:
/// - Many (but not all) are synchronized to the Unix epoch (1970-01-01 00:00:00 UTC).
/// - Some may use TAI (International Atomic Time) or other reference points.
/// - The epoch is generally controlled by software than synchronizes the PHC time
///
/// ## Signed Time Representation
///
/// The timestamp uses signed seconds because the Linux kernel performs
/// normalization of it into that format (matching the other Linux clocks).
///
/// Theoretically, if the timestamp does actually become negative, the
/// kernel's specification tells us directly that we should interpret it to mean
/// time before the epoch.
///
/// However, the PHC's internal time is obviously never going to get "before the
/// epoch" and in the PHC hardware, the clock is just a simple unsigned counter.
/// If a negative value would actually be observed, it more likely means that the
/// corresponding Linux driver for that PHC chose to expose it in that way, most
/// likely because the underlying hardware PHC actually has a 64-bit seconds value,
/// and there is no good way to expose it over the Linux standard interface. So it
/// is fairly reasonable to assume that a negative value would actually just mean
/// that the 64-bit PHC timer got above `i64::MAX` (`u64::MAX / 2`), but it just
/// keeps incrementing from there. Hardware-wise, it simply can't really mean that
/// we're "before the epoch".
///
/// Realistically, reaching `i64::MAX` in seconds would mean >584 billion years
/// has elapsed since the epoch, which, for any reasonably chosen epoch, should
/// obviously never be reached. But this structure does not enforce a this value
/// to be non-negative explicitly.
///
/// ## Normalization
///
/// The nanoseconds component is always in the range [0, `1_000_000_000`),
/// matching the normalized form returned by the underlying hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PtpTime {
    seconds: i64,
    nanoseconds: Nanoseconds,
}

impl PtpTime {
    /// Creates a timestamp from explicit seconds and nanoseconds components.
    ///
    /// If the number of nanoseconds is greater than or equal to 1 billion,
    /// it will carry over into the seconds provided.
    ///
    /// # Panics
    ///
    /// This constructor will panic if the carry from nanoseconds overflows
    /// the seconds counter.
    #[inline]
    #[must_use]
    pub const fn new(seconds: i64, nanos: u32) -> Self {
        if nanos < NSEC_PER_SEC_U32 {
            // SAFETY: nanos < NSEC_PER_SEC_U32, so nanos is valid
            Self {
                seconds,
                nanoseconds: unsafe { Nanoseconds::new_unchecked(nanos) },
            }
        } else {
            let carry = (nanos / NSEC_PER_SEC_U32) as i64;
            let secs = seconds.checked_add(carry).expect("overflow in PtpTime::new");
            let nanos = nanos % NSEC_PER_SEC_U32;
            // SAFETY: nanos % NSEC_PER_SEC_U32 < NSEC_PER_SEC_U32
            Self {
                seconds: secs,
                nanoseconds: unsafe { Nanoseconds::new_unchecked(nanos) },
            }
        }
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

    #[must_use]
    pub const fn from_ns(ns: i64) -> Self {
        let seconds = ns.div_euclid(NSEC_PER_SEC);
        #[expect(clippy::cast_possible_truncation)]
        let nanoseconds = ns.rem_euclid(NSEC_PER_SEC) as u32;

        Self {
            seconds,
            nanoseconds: unsafe { Nanoseconds::new_unchecked(nanoseconds) },
        }
    }

    /// Converts a normalized POSIX `timespec` into a [`PtpTime`].
    ///
    /// # Safety
    /// `ts` must be normalized such that `ts.tv_nsec` is in the range
    /// `0..1_000_000_000`. This is the invariant guaranteed by successful
    /// `clock_gettime` calls.
    #[must_use]
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

    #[must_use]
    pub(crate) fn from_abi(time: PtpClockTime) -> Self {
        // Returned ABI time is guaranteed to be normalized in nanoseconds
        debug_assert!(time.nsec < NSEC_PER_SEC_U32);

        Self {
            seconds: time.sec,
            // SAFETY: ABI time is guaranteed to be normalized.
            nanoseconds: unsafe { Nanoseconds::new_unchecked(time.nsec) },
        }
    }

    #[must_use]
    pub(crate) const fn into_abi(self) -> PtpClockTime {
        PtpClockTime {
            sec: self.seconds,
            nsec: self.nanoseconds.get(),
            reserved: 0,
        }
    }

    /// Returns the whole seconds component.
    #[must_use]
    pub const fn seconds(self) -> i64 {
        self.seconds
    }

    /// Returns the subsecond nanoseconds component (always [0, `1_000_000_000`)).
    #[must_use]
    pub const fn subsec_nanos(self) -> u32 {
        self.nanoseconds.get()
    }

    /// Converts this PTP time to nanoseconds as a signed i128.
    ///
    /// This representation allows computing signed time differences that may be
    /// negative (e.g., when measuring time intervals across clock corrections).
    /// Uses i128 to avoid overflow, since PTP seconds can be large.
    ///
    /// Note: If `self.seconds` is negative, the result will be negative as well.
    #[must_use]
    pub const fn as_nanos(self) -> i128 {
        #[allow(clippy::cast_lossless)]
        let sec_nanos = (self.seconds as i128).saturating_mul(NSEC_PER_SEC as i128);
        sec_nanos.saturating_add(self.subsec_nanos() as i128)
    }
}

impl TryFrom<Duration> for PtpTime {
    type Error = Error;

    fn try_from(value: Duration) -> Result<Self> {
        let seconds = i64::try_from(value.as_secs()).map_err(|_| Error::DurationTooLarge)?;
        let nanoseconds = value.subsec_nanos();

        Ok(Self {
            seconds,
            nanoseconds: unsafe { Nanoseconds::new_unchecked(nanoseconds) },
        })
    }
}

impl fmt::Display for PtpTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{:09}", self.seconds, self.subsec_nanos())
    }
}
