use std::time::Duration;

use crate::ptp_phc::PtpTime;

/// 32-bit AVTP presentation timestamp in nanoseconds.
///
/// Derived from absolute gPTP time using the formula:
/// ```text
/// (gptp_seconds * 1_000_000_000 + gptp_nanoseconds) mod 2^32
/// ```
///
/// Because the field is 32-bit, it wraps roughly every 4.29 seconds. This design intentionally
/// drops the most significant bits from the seconds component. This is acceptable because
/// AVTP synchronization operates within this wrapping window; time differences larger than
/// ~4.29 seconds are not relevant for stream synchronization and playback scheduling. By
/// discarding the high-order bits, we preserve the full nanosecond precision of the lower
/// 32 bits, which is critical for accurate timestamp comparison and drift detection within
/// the wrapping window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AvtpTimestamp(u32);

impl AvtpTimestamp {
    /// Creates a timestamp from a raw 32-bit value.
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        Self(value)
    }

    /// Extracts the raw 32-bit value.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Consumes the timestamp and returns the raw 32-bit value.
    #[must_use]
    pub const fn into_u32(self) -> u32 {
        self.0
    }

    /// Adds nanoseconds to this timestamp using AVTP's 32-bit wraparound semantics.
    #[must_use]
    pub const fn wrapping_add(self, rhs: u32) -> Self {
        Self(self.0.wrapping_add(rhs))
    }

    /// Expand this wrapping 32-bit AVTP timestamp to the absolute [`PtpTime`]
    /// nearest to a reference time.
    ///
    /// This relies on the assumption that the reference time is synchronized to
    /// the same underlying PTP timeline and is within one AVTP wrap window
    /// (`2^32` ns, about 4.29 s) of the intended absolute timestamp.
    ///
    /// # Panics
    /// Panics if internal arithmetic produces a timestamp that cannot be represented
    /// as [`PtpTime`]. This should be unreachable for valid [`PtpTime`] references.
    #[must_use]
    pub fn expand_near(self, reference_time: PtpTime) -> PtpTime {
        const AVTP_WRAP_NS: i128 = 1_i128 << 32;

        let reference_ns = reference_time.as_nanos();
        let base_cycles = reference_ns.div_euclid(AVTP_WRAP_NS);
        let base_candidate = base_cycles * AVTP_WRAP_NS + i128::from(self.0);

        let candidates = [
            base_candidate - AVTP_WRAP_NS,
            base_candidate,
            base_candidate + AVTP_WRAP_NS,
        ];

        let expanded_ns = candidates
            .into_iter()
            .min_by_key(|candidate| (candidate - reference_ns).abs())
            .expect("candidate set is non-empty");

        PtpTime::from_ns_i128(expanded_ns).expect("expanded AVTP timestamp should always fit within PtpTime range")
    }
}

impl std::ops::Add<u32> for AvtpTimestamp {
    type Output = Self;

    fn add(self, rhs: u32) -> Self::Output {
        self.wrapping_add(rhs)
    }
}

impl std::ops::AddAssign<u32> for AvtpTimestamp {
    fn add_assign(&mut self, rhs: u32) {
        *self = self.wrapping_add(rhs);
    }
}

impl From<u32> for AvtpTimestamp {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<AvtpTimestamp> for u32 {
    fn from(ts: AvtpTimestamp) -> Self {
        ts.0
    }
}

impl From<Duration> for AvtpTimestamp {
    fn from(duration: Duration) -> Self {
        // Convert to nanoseconds and wrap to 32 bits
        // This cast essentially performs a mod 2^32 operation
        #[allow(clippy::cast_possible_truncation)]
        let nanos = duration.as_nanos() as u32;
        Self(nanos)
    }
}

impl From<crate::ptp_phc::PtpTime> for AvtpTimestamp {
    fn from(ptp_time: crate::ptp_phc::PtpTime) -> Self {
        // (gptp_seconds * 1_000_000_000 + gptp_nanoseconds) mod 2^32
        //
        // Intentional truncation per AVTP spec (wrapping 32-bit timeline).
        // Sign loss on the i64->u64 cast is fine: we're performing wrapping arithmetic
        // and then truncating to u32 anyway, so we just need the bits.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let total_nanos = (ptp_time.seconds() as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(u64::from(ptp_time.subsec_nanos())) as u32;
        Self(total_nanos)
    }
}

impl std::fmt::Display for AvtpTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ns", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::AvtpTimestamp;
    use crate::ptp_phc::PtpTime;

    #[test]
    fn addition_wraps_like_avtp_timestamps() {
        let timestamp = AvtpTimestamp::from(u32::MAX - 1);

        assert_eq!((timestamp + 3).as_u32(), 1);
    }

    #[test]
    fn expansion_chooses_absolute_time_near_reference() {
        let reference = PtpTime::from_ns_i128((5_i128 << 32) + 123).unwrap();
        let timestamp = AvtpTimestamp::from(456_u32);

        assert_eq!(
            timestamp.expand_near(reference),
            PtpTime::from_ns_i128((5_i128 << 32) + 456).unwrap()
        );
    }
}
