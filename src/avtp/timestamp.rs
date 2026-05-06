use std::time::Duration;

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
        let total_nanos = (ptp_time.seconds as u64)
            .wrapping_mul(1_000_000_000)
            .wrapping_add(u64::from(ptp_time.nanoseconds)) as u32;
        Self(total_nanos)
    }
}

impl std::fmt::Display for AvtpTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ns", self.0)
    }
}
