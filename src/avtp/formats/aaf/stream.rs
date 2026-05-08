use std::num::NonZero;
use std::sync::Arc;

use arbitrary_int::prelude::*;
use thiserror::Error;

use crate::avtp::{
    AvtpTimestamp, Avtpdu, ClockError, PtpSynchronizedClock, StreamID,
    headers::{CommonHeader, GenericStreamData},
    stream::{StreamFilter, StreamListener, StreamTalker},
    subtype::Subtype,
};
use crate::ptp_phc::PtpTimeSource;

use super::pcm::InvalidPcmAaf;
use super::{Aaf, AafPcm, AafVariant, InvalidAaf, PcmFormat, SampleRate};

/// Errors that can occur when configuring an [`AafPcmTalker`].
#[derive(Error, Debug)]
pub enum AafPcmTalkerConfigError {
    #[error("bit_depth cannot be zero")]
    BitDepthZero,

    #[error("channels must be at least 1")]
    ChannelsPerFrameZero,
}

/// Errors that can occur when building an outgoing AAF PCM AVTP packet.
#[derive(Error, Debug)]
pub enum AafPcmTalkerError {
    #[error(transparent)]
    Pcm(#[from] InvalidPcmAaf),

    #[error("PTP clock error: {0}")]
    Clock(#[from] ClockError),
}

/// Received PCM audio frame from an AVTP stream.
///
/// Contains the raw audio payload and metadata about the frame.
#[derive(Debug, Clone)]
pub struct ReceivedPcm {
    /// AVTP presentation timestamp (absolute gPTP time mod 2^32 nanoseconds).
    pub avtp_timestamp: AvtpTimestamp,

    /// AVTP stream sequence number.
    pub seq_num: u8,

    /// Big-endian encoded PCM audio samples, ready for byte-order conversion.
    pub payload_be: Arc<[u8]>,

    /// Sample rate of the PCM stream.
    pub sample_rate: SampleRate,

    /// Number of audio channels in each sample frame.
    pub channels: u10,

    /// Number of valid bits per sample.
    pub bit_depth: NonZero<u8>,

    /// Sample word container format used in the AVTP payload.
    pub format: PcmFormat,

    /// Number of packets lost before this one (sequence number gap).
    pub packets_lost: u32,
}

/// Encodes PCM audio into AAF AVTP packets.
///
/// Manages the state of a single outgoing audio stream, including sequence numbering,
/// PTP-synchronized timestamp generation, and packet construction.
pub struct AafPcmTalker<T: PtpTimeSource> {
    stream_id: StreamID,
    format: PcmFormat,
    sample_rate: SampleRate,
    channels: u10,
    bit_depth: NonZero<u8>,
    sequence_num: u8,
    ptp_clock: PtpSynchronizedClock<T>,
    /// Presentation delay in nanoseconds, added to the PTP time for AVTP timestamps.
    playback_delay_ns: u32,
}

impl<T: PtpTimeSource> AafPcmTalker<T> {
    /// Creates a new AAF PCM talker for encoding audio packets.
    ///
    /// Timestamps are synchronized to PTP time via the provided clock.
    ///
    /// # Arguments
    ///
    /// - `stream_id`: The stream identifier (MAC address + unique ID).
    /// - `format`: The PCM sample format (`Int16Bit`, `Int24Bit`, etc.).
    /// - `sample_rate`: The audio sample rate.
    /// - `channels`: The number of audio channels per frame.
    /// - `bit_depth`: The number of valid bits per sample.
    /// - `ptp_clock`: PTP synchronized clock for drift-free timestamp generation.
    /// - `playback_delay_ms`: Presentation delay in milliseconds to add to AVTP timestamps.
    ///
    /// # Errors
    ///
    /// Returns [`AafPcmTalkerConfigError`] if the supplied talker configuration is invalid.
    pub fn new(
        stream_id: StreamID,
        format: PcmFormat,
        sample_rate: SampleRate,
        channels: u10,
        bit_depth: u8,
        ptp_clock: PtpSynchronizedClock<T>,
        playback_delay_ms: u32,
    ) -> Result<Self, AafPcmTalkerConfigError> {
        let bit_depth = NonZero::new(bit_depth).ok_or(AafPcmTalkerConfigError::BitDepthZero)?;

        if channels.value() == 0 {
            return Err(AafPcmTalkerConfigError::ChannelsPerFrameZero);
        }

        Ok(Self {
            stream_id,
            format,
            sample_rate,
            channels,
            bit_depth,
            sequence_num: 0,
            ptp_clock,
            playback_delay_ns: playback_delay_ms * 1_000_000,
        })
    }

    /// Builds and returns the next AVTP packet for transmission.
    ///
    /// Updates the sequence number and PTP-synchronized timestamp on each call.
    ///
    /// # Arguments
    ///
    /// - `payload`: Big-endian PCM audio samples (Arc-wrapped for zero-copy).
    ///
    /// # Errors
    ///
    /// Returns [`AafPcmTalkerError`] if packet construction or clock synchronization fails.
    ///
    /// # Panics
    /// Panics if this function constructs stream metadata that violates the AAF invariants
    /// enforced by [`Aaf::new`]. That would indicate an internal bug in this encoder.
    pub fn build_packet(&mut self, payload: Arc<[u8]>) -> Result<Avtpdu, AafPcmTalkerError> {
        // Get synchronized absolute PTP time (monotonically increasing)
        let ptp_time = self.ptp_clock.ptp_time()?;
        // Add presentation delay and convert to AVTP timestamp (32-bit with wraparound)
        let avtp_timestamp = AvtpTimestamp::from(ptp_time) + self.playback_delay_ns;

        // Create the PCM variant
        let pcm = AafPcm::new(
            self.sample_rate,
            self.channels,
            self.format,
            self.bit_depth.get(),
            payload,
        )?;

        let aaf_variant = AafVariant::Pcm(pcm);

        // Build the common header
        let common = CommonHeader {
            subtype: Subtype::AAF,
            header_specific_bit: true, // stream_id_valid
            version: u3::new(0),
        };

        // Build the generic stream data
        let generic = GenericStreamData::new_unchecked(
            common,
            false, // media_clock_restart
            true,  // avtp_timestamp_valid
            self.sequence_num,
            false, // timestamp_uncertain
            self.stream_id,
            avtp_timestamp,
        );

        // `Aaf::new` validates only the stream-level invariants below, all of which we set
        // explicitly to valid AAF values in this function.
        let aaf = Aaf::new(generic, aaf_variant, false, u4::new(0))
            .expect("internal bug: AAF talker constructed invalid stream metadata");

        // Convert to wire format and increment sequence number
        let stream_header = aaf.into();
        self.sequence_num = self.sequence_num.wrapping_add(1);

        Ok(Avtpdu::Stream(stream_header))
    }
}

/// Decodes PCM audio from AAF AVTP packets.
///
/// Filters incoming packets by stream ID and detects sequence number gaps
/// to report lost packets.
pub struct AafPcmListener {
    filter: StreamFilter,
    expected_seq: Option<u8>,
}

impl AafPcmListener {
    /// Creates a new listener that accepts packets matching the given stream ID.
    ///
    /// # Arguments
    ///
    /// - `stream_id`: The stream ID to match, or `None` to accept all streams.
    #[must_use]
    pub const fn new(stream_id: Option<StreamID>) -> Self {
        let filter = match stream_id {
            Some(id) => StreamFilter::Exact(id),
            None => StreamFilter::Any,
        };
        Self {
            filter,
            expected_seq: None,
        }
    }

    /// Creates a new listener that accepts packets from a specific MAC address.
    ///
    /// Ignores the stream UID (`unique_id`) field.
    #[must_use]
    pub const fn new_with_mac_filter(mac: pnet::util::MacAddr) -> Self {
        Self {
            filter: StreamFilter::MacOnly(mac),
            expected_seq: None,
        }
    }

    /// Processes an incoming AVTP packet.
    ///
    /// Returns:
    /// - `Ok(Some(frame))` if the packet is accepted and contains valid PCM data
    /// - `Ok(None)` if the packet is filtered out or not a PCM AAF stream
    ///
    /// # Errors
    ///
    /// Returns [`InvalidAaf`] if the packet fails to parse as AAF.
    pub fn process(&mut self, pdu: &Avtpdu) -> Result<Option<ReceivedPcm>, InvalidAaf> {
        // Only process Stream PDUs
        let Avtpdu::Stream(stream_header) = pdu else {
            return Ok(None);
        };

        // Parse as AAF
        let aaf: Aaf = stream_header.clone().try_into()?;

        // Check stream ID filter
        let stream_id = aaf.stream_data().stream_id();
        if !self.filter.matches(stream_id) {
            return Ok(None);
        }

        // Only accept PCM format (not AES3 or others)
        let AafVariant::Pcm(aaf_pcm) = aaf.format_data() else {
            return Ok(None);
        };

        // Detect sequence number gaps
        let seq_num = aaf.stream_data().sequence_num();
        let packets_lost = match self.expected_seq {
            None => {
                // First packet from this stream
                self.expected_seq = Some(seq_num.wrapping_add(1));
                0
            }
            Some(expected) => {
                let gap = seq_num.wrapping_sub(expected);
                self.expected_seq = Some(seq_num.wrapping_add(1));
                u32::from(gap)
            }
        };

        Ok(Some(ReceivedPcm {
            avtp_timestamp: aaf.stream_data().avtp_timestamp(),
            seq_num,
            payload_be: Arc::from(aaf_pcm.payload_slice()),
            sample_rate: aaf_pcm.nominal_sample_rate(),
            channels: aaf_pcm.channels_per_frame(),
            bit_depth: aaf_pcm.bit_depth(),
            format: aaf_pcm.format(),
            packets_lost,
        }))
    }
}

impl<T: PtpTimeSource + Send> StreamTalker for AafPcmTalker<T> {
    type Error = AafPcmTalkerError;

    fn build_packet(&mut self, payload: Arc<[u8]>) -> Result<Avtpdu, Self::Error> {
        Self::build_packet(self, payload)
    }
}

impl StreamListener for AafPcmListener {
    type Output = ReceivedPcm;
    type Error = InvalidAaf;

    fn process(&mut self, pdu: &Avtpdu) -> Result<Option<Self::Output>, Self::Error> {
        Self::process(self, pdu)
    }
}

#[cfg(test)]
mod tests {
    use pnet::util::MacAddr;

    use super::*;
    use crate::ptp_phc::PtpClockSystemTime;

    fn test_clock() -> PtpSynchronizedClock<PtpClockSystemTime> {
        PtpSynchronizedClock::new(PtpClockSystemTime::new()).expect("system time clock should initialize")
    }

    fn test_stream_id() -> StreamID {
        StreamID {
            mac_address: MacAddr::new(0, 1, 2, 3, 4, 5),
            unique_id: 1,
        }
    }

    #[test]
    fn talker_new_reports_zero_bit_depth_structurally() {
        let result = AafPcmTalker::new(
            test_stream_id(),
            PcmFormat::Int16Bit,
            SampleRate::KHz48,
            u10::new(2),
            0,
            test_clock(),
            50,
        );

        assert!(matches!(result, Err(AafPcmTalkerConfigError::BitDepthZero)));
    }

    #[test]
    fn talker_new_reports_zero_channels_structurally() {
        let result = AafPcmTalker::new(
            test_stream_id(),
            PcmFormat::Int16Bit,
            SampleRate::KHz48,
            u10::new(0),
            16,
            test_clock(),
            50,
        );

        assert!(matches!(result, Err(AafPcmTalkerConfigError::ChannelsPerFrameZero)));
    }
}
