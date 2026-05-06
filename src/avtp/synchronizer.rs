use std::time::{Duration, Instant};

use crate::ptp_phc::{PtpClock, PtpTime};

/// Error type for clock operations.
#[derive(Debug, thiserror::Error)]
pub enum ClockError {
    /// Error accessing the PTP clock.
    #[error("PTP clock error: {0}")]
    PtpClock(#[from] crate::ptp_phc::Error),

    /// Computed elapsed time is negative (PTP clock moved backward past initialization).
    #[error("PTP clock moved backward before initialization point (clock reset or adjustment): {0} ns")]
    NegativeElapsed(i128),

    /// Elapsed duration computation overflowed u64 seconds representation.
    #[error("Elapsed time exceeds Duration::MAX (clock timer in unrecoverable state)")]
    DurationOverflow,

    /// Cannot enforce strict monotonicity (already at `Duration::MAX`).
    #[error("Monotonic timestamp requirement violated: Duration::MAX reached")]
    MonotonicityLimit,
}

/// Fast, monotonic clock synchronized to PTP time
///
/// Generates reliable monotonic timestamps efficiently by combining the speed of
/// local timers with the accuracy of PTP time. Instead of syscalling the PTP clock on
/// every timestamp request (slow), this clock reads PTP periodically and interpolates
/// between readings using the fast, monotonic [`Instant`] timer. A phase-locked loop
/// (PLL) algorithm continuously measures clock drift and adjusts the interpolation
/// frequency, keeping timestamps synchronized with real PTP time.
///
/// ## Architecture
///
/// **Local interpolation** (fast path): Each [`Self::elapsed`] call multiplies the
/// (local) time since the last PTP sync by a frequency adjustment factor and adds
/// it to the base PTP time. This avoids syscall overhead for every timestamp.
///
/// **Periodic synchronization** (PLL feedback): Every `resync_interval` (default 100ms),
/// the clock reads PTP time directly during the [`Self::elapsed`] call, measures how
/// much local time has elapsed, calculates the frequency ratio (actual PTP rate / local
/// timer rate), and smooths this into the frequency adjustment via exponential moving
/// average. This corrects any drift in the local timer.
///
/// **Monotonicity guarantee**: Each call to `elapsed()` returns a value strictly
/// greater than the previous, even if PTP time steps backward or when frequency
/// adjustments cause temporary speed changes.
///
/// ## Benefits for AVTP
///
/// - **No syscall per timestamp**: Typical AVTP talkers emit 1000+ packets per second;
///   each timestamp via `elapsed()` is a single arithmetic operation, not a kernel call.
/// - **Drift-free long streams**: PTP synchronization ensures timestamps stay aligned
///   with the network master clock over hours or days of continuous operation.
/// - **Smooth timestamps**: Exponential moving average prevents frequency jumps that
///   would introduce audible discontinuities in audio playback.
/// - **Strict monotonicity**: Required by AVTP spec; each timestamp must be strictly
///   greater than the previous, which this clock enforces even across clock corrections.
pub struct PllClock<'a> {
    clock: &'a PtpClock,

    /// Initial PTP time (reference point for elapsed calculations)
    initial_ptp_time: PtpTime,

    /// Most recent PTP time reading from resync
    last_ptp_time: PtpTime,

    /// Instant when `last_ptp_time` was read
    last_ptp_instant: Instant,

    /// PLL frequency adjustment factor (1.0 = nominal clock speed)
    ///
    /// - `> 1.0`: local clock running fast, needs to slow down
    /// - `< 1.0`: local clock running slow, needs to speed up
    /// - `= 1.0`: perfectly synchronized
    frequency_adjustment: f64,

    /// PLL phase offset correction in nanoseconds.
    ///
    /// Accumulated via exponential moving average during resync to correct
    /// constant timing offsets between predicted and actual PTP time.
    phase_offset_ns: i64,

    /// Last value returned by `elapsed()` (for monotonicity guarantee)
    last_returned: Duration,

    /// Time interval between PTP resyncs (ensures consistent behavior regardless of call frequency)
    resync_interval: Duration,
}

impl<'a> PllClock<'a> {
    /// Creates a new PLL clock synchronized to the given PTP clock.
    ///
    /// Uses a default resync interval of 100ms, which provides good balance between
    /// PTP clock read frequency and synchronization tightness.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial PTP clock read fails.
    pub fn new(clock: &'a PtpClock) -> Result<Self, ClockError> {
        Self::with_resync_interval(clock, Duration::from_millis(100))
    }

    /// Creates a new PLL clock with a custom resync interval.
    ///
    /// # Arguments
    ///
    /// - `clock`: Reference to the PTP clock device
    /// - `resync_interval`: Time between PTP resyncs. Ensures the PLL stays synchronized
    ///   even if the caller goes idle.
    ///   - Smaller (10-50ms): tighter sync but more frequent PTP reads
    ///   - Larger (500ms-1s): fewer reads but more drift before correction
    ///   - Typical: 100ms
    ///
    /// # Errors
    ///
    /// Returns an error if the initial PTP clock read fails.
    pub fn with_resync_interval(clock: &'a PtpClock, resync_interval: Duration) -> Result<Self, ClockError> {
        let initial_ptp_time = clock.time()?;
        let now = Instant::now();
        Ok(Self {
            clock,
            initial_ptp_time,
            last_ptp_time: initial_ptp_time,
            last_ptp_instant: now,
            frequency_adjustment: 1.0,
            phase_offset_ns: 0,
            last_returned: Duration::ZERO,
            resync_interval,
        })
    }

    /// Returns strictly monotonically increasing elapsed time synchronized to PTP.
    ///
    /// The returned duration:
    /// - Is strictly greater than any previous return value (by at least 1 nanosecond)
    /// - Tracks the actual PTP elapsed time smoothly via the PLL algorithm
    /// - Uses frequency adjustment to eliminate clock drift
    /// - Interpolates between PTP resyncs using fast local Instant measurements
    ///
    /// Automatically triggers a PTP resync if the resync interval has elapsed.
    ///
    /// ## Calculation
    ///
    /// ```text
    /// ptp_base = (last_ptp_time - initial_ptp_time)
    /// instant_adjusted = instant_since_last_sync * frequency_adjustment
    /// elapsed = ptp_base + instant_adjusted
    /// return max(elapsed, last_returned + 1ns)
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if a PTP resync fails or if elapsed computation overflows.
    pub fn elapsed(&mut self) -> Result<Duration, ClockError> {
        // Compute current time once to avoid time advancing between checks
        let now = Instant::now();
        let instant_since_sync = now.duration_since(self.last_ptp_instant);

        // Resync if enough time has passed since last sync
        if instant_since_sync >= self.resync_interval {
            self.resync_ptp(now)?;
        }

        self.compute_elapsed(now)
    }

    /// Returns elapsed time without forcing a PTP resync, even if the resync interval has elapsed.
    ///
    /// This maintains the same monotonicity guarantee as [`Self::elapsed`], but uses only the
    /// most recent PTP measurement without updating it. Useful for:
    /// - Tight loops that call frequently and don't need constant PTP updates
    /// - Testing or debugging PLL behavior without external sync events
    /// - Avoiding PTP syscalls in performance-critical sections
    ///
    /// # Errors
    ///
    /// Returns an error if elapsed computation overflows. Monotonicity is preserved even on error.
    pub fn elapsed_no_resync(&mut self) -> Result<Duration, ClockError> {
        let now = Instant::now();
        self.compute_elapsed(now)
    }

    /// Internal: Computes elapsed time using the most recent PTP measurement.
    ///
    /// Does not trigger a resync; called by both [`Self::elapsed`] (after potential resync)
    /// and [`Self::elapsed_no_resync`].
    fn compute_elapsed(&mut self, now: Instant) -> Result<Duration, ClockError> {
        let instant_since_sync = now.duration_since(self.last_ptp_instant);

        // Compute elapsed time entirely in i128 nanoseconds to avoid truncation/overflow issues
        let initial_ns = self.initial_ptp_time.as_nanos();
        let last_ns = self.last_ptp_time.as_nanos();

        // Base elapsed since initialization. Can be negative if clock was adjusted backward after init.
        // (This can never overflow i128::MIN, as the PHC ns times can actually be at most ~94 bits)
        let ptp_base_ns = last_ns - initial_ns;

        // Apply frequency adjustment to local Instant elapsed
        // Note: u128 to f64 cast loses precision for very large values (>2^53 ns ~= 108 days),
        // but for practical resync intervals (<1s), this is negligible.
        #[allow(clippy::cast_precision_loss)]
        let instant_ns = instant_since_sync.as_nanos() as f64;
        #[allow(clippy::cast_possible_truncation)]
        let adjusted_instant_ns = (instant_ns * self.frequency_adjustment).round() as i128;

        // Combine PTP base + frequency-adjusted local elapsed
        let mut elapsed_ns = ptp_base_ns
            .checked_add(adjusted_instant_ns)
            .ok_or(ClockError::DurationOverflow)?;

        // Apply phase offset from PLL
        elapsed_ns = elapsed_ns
            .checked_add(i128::from(self.phase_offset_ns))
            .ok_or(ClockError::DurationOverflow)?;

        // Convert i128 nanoseconds to Duration, failing on overflow/negative elapsed
        let mut duration = i128_nanos_to_duration(elapsed_ns)?;

        // Enforce strict monotonicity: always strictly greater than before
        if duration <= self.last_returned {
            // Try to increment by 1 nanosecond
            duration = self
                .last_returned
                .checked_add(Duration::from_nanos(1))
                .ok_or(ClockError::MonotonicityLimit)?;
        }

        self.last_returned = duration;
        Ok(duration)
    }

    /// Internal: Resync the PLL by measuring actual clock rate and adjusting both frequency and phase.
    ///
    /// This is the core of the PLL: it measures how much the local clock has advanced
    /// versus how much PTP time has advanced, calculates the frequency ratio, and
    /// updates both frequency and phase adjustments using exponential moving average filters.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::suboptimal_flops
    )]
    fn resync_ptp(&mut self, now: Instant) -> Result<(), ClockError> {
        // PLL tuning parameters. These control convergence behavior and represent
        // reasonable bounds based on typical oscillator characteristics.

        // Loop filter alphas: control exponential moving average convergence rate.
        // Higher alpha (e.g., 0.2) = faster convergence but more noise.
        // Lower alpha (e.g., 0.05) = slower convergence but smoother.
        // 0.1 provides good balance for ~100ms resync intervals.
        const PLL_ALPHA: f64 = 0.1;
        const PHASE_ALPHA: f64 = 0.1;

        // Frequency adjustment bounds (in terms of fractional deviation from nominal).
        // Crystal oscillators typically have <100ppm drift. We use +-0.1% (1000ppm) as
        // a conservative bound that handles temperature-compensated oscillators while
        // still being restrictive enough to catch clock step events as errors.
        const FREQ_MIN: f64 = 0.999;
        const FREQ_MAX: f64 = 1.001;

        // Phase offset bounds. Should be small enough to represent genuine drift but
        // large enough to absorb read latency and code processing time.
        const PHASE_OFFSET_MAX_NS: i64 = 100_000; // +-100us

        // Large jump detection threshold. If phase error exceeds this, the PTP clock
        // likely experienced a step (discontinuous adjustment, counter wrap, etc.).
        // Rather than try to converge through frequency adjustment (which takes time and
        // distorts the frequency estimate), we reset the PLL and let it re-lock.
        const LARGE_JUMP_THRESHOLD_NS: f64 = 1e6; // 1ms

        let current_ptp = self.clock.time()?;
        let instant_elapsed = now.duration_since(self.last_ptp_instant);
        let instant_ns = instant_elapsed.as_nanos() as f64;

        // Compute signed nanosecond difference (handles forward and backward jumps)
        #[allow(clippy::cast_possible_truncation)]
        let ptp_diff_ns = (current_ptp.as_nanos() - self.last_ptp_time.as_nanos()) as f64;

        // Unconditionally measure and correct frequency from any sufficient movement
        if instant_ns > 1e6 {
            let measured_frequency = ptp_diff_ns / instant_ns;
            self.frequency_adjustment = (1.0 - PLL_ALPHA) * self.frequency_adjustment + PLL_ALPHA * measured_frequency;
            self.frequency_adjustment = self.frequency_adjustment.clamp(FREQ_MIN, FREQ_MAX);
        }

        // Detect large jumps: if phase error is huge, clock experienced a step event
        let predicted_ptp_ns = instant_ns * self.frequency_adjustment;
        let phase_error_ns = ptp_diff_ns - predicted_ptp_ns;

        if phase_error_ns.abs() > LARGE_JUMP_THRESHOLD_NS {
            // Clock step detected (wrap, adjustment, or synchronization event).
            // Reset the PLL rather than trying to converge through frequency adjustment.
            self.frequency_adjustment = 1.0;
            self.phase_offset_ns = 0;
            self.last_ptp_time = current_ptp;
            self.last_ptp_instant = now;
            return Ok(());
        }

        // Phase correction: measure how actual movement differs from prediction.
        // Clamp to prevent unbounded growth and detect persistent offset issues.
        let new_phase_offset = (1.0 - PHASE_ALPHA) * self.phase_offset_ns as f64 + PHASE_ALPHA * phase_error_ns;
        self.phase_offset_ns =
            (new_phase_offset.clamp(-(PHASE_OFFSET_MAX_NS as f64), PHASE_OFFSET_MAX_NS as f64)) as i64;

        // Update reference point for next interpolation
        self.last_ptp_time = current_ptp;
        self.last_ptp_instant = now;

        Ok(())
    }
}

/// Converts i128 nanoseconds to Duration, with explicit overflow checking.
///
/// Duration is represented as u64 seconds + u32 nanoseconds, so it can represent
/// up to `u64::MAX` seconds (about 584 billion years).
///
/// # Errors
///
/// * `NegativeElapsed`: If the computed nanoseconds is negative. This indicates that the
///   PTP clock moved backward after initialization, and even after the subsequent additions
///   (from the elapsed time since the captured instant) were unable to overcome this offset.
///   If this offset is small (a few seconds), this could indicate a heavily out-of-sync clock,
///   that moved backwards significantly upon synchronization. If the offset is very large
///   (`u32::MAX` nanoseconds), it indicates either an overflow of the internal PHC counter, or
///   a manual adjustment of the PHC clock to an entirely different value (e.g. an epoch change).
///
/// * `DurationOverflow` if computed seconds exceed `u64::MAX`. This could realistically only
///   happen if the clock was manually moved forward by a huge amount, resulting in the
///   computed difference being already very large, alongside the elapsed time addition pushing
///   it over the u64 limit. Alternatively, it could indicate a bug in our phase/frequency
///   adjustment code.
fn i128_nanos_to_duration(nanos: i128) -> Result<Duration, ClockError> {
    // Negative elapsed time indicates clock moved backward significantly after initialization.
    // Normal backward adjustments are small and would be absorbed by interval elapsed +
    // frequency/phase adjustments before reaching here. A negative value here means something
    // went wrong (e.g., PHC counter wrapped, or user made a large manual clock adjustment).
    if nanos < 0 {
        return Err(ClockError::NegativeElapsed(nanos));
    }

    // Convert to seconds and remaining nanoseconds
    let secs = nanos / 1_000_000_000;
    #[expect(clippy::cast_sign_loss)]
    let nsecs = (nanos % 1_000_000_000) as u32;

    // Check if seconds exceeds u64::MAX
    //
    // This should never happen in normal operation, as the PHC's seconds are
    // stored as an i64, the only way we could reach u64::MAX difference would
    // be a manual adjustment that moved the clock forward by a huge margin from
    // the previous value, combined with the interval elapsed addition.
    //
    // Alternatively, it could indicate that our phase/offset is faulty somehow,
    // and got the value to something this large.
    let Ok(secs) = u64::try_from(secs) else {
        return Err(ClockError::DurationOverflow);
    };

    Ok(Duration::new(secs, nsecs))
}
