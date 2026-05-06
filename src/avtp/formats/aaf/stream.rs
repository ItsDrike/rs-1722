use std::num::NonZero;
use std::sync::Arc;
use std::time::Instant;

use arbitrary_int::prelude::*;
use thiserror::Error;

use crate::avtp::{
    headers::{CommonHeader, GenericStreamData},
    stream::{StreamFilter, StreamListener, StreamTalker},
    subtype::Subtype,
    Avtpdu, StreamID,
};

use super::{AafPcm, AafVariant, InvalidAaf, PcmFormat, SampleRate, Aaf};

/// Errors that can occur during AAF stream encoding or decoding.
#[derive(Error, Debug)]
pub enum AafStreamError {
    #[error("Invalid stream configuration: {0}")]
    InvalidStream(String),

    #[error("Failed to construct AAF packet: {0}")]
    AafConstruction(String),

    #[error(transparent)]
    InvalidAaf(#[from] InvalidAaf),
}

/// Received PCM audio frame from an AVTP stream.
///
/// Contains the raw audio payload and metadata about the frame.
#[derive(Debug, Clone)]
pub struct ReceivedPcm {
    /// Big-endian encoded PCM audio samples, ready for byte-order conversion.
    pub payload_be: Arc<[u8]>,

    /// Sample rate of the PCM stream.
    pub sample_rate: SampleRate,

    /// Number of audio channels in each sample frame.
    pub channels: u10,

    /// Number of valid bits per sample.
    pub bit_depth: NonZero<u8>,

    /// Number of packets lost before this one (sequence number gap).
    pub packets_lost: u32,
}

/// Encodes PCM audio into AAF AVTP packets.
///
/// Manages the state of a single outgoing audio stream, including sequence numbering,
/// timestamp generation, and packet construction.
pub struct AafPcmTalker {
    stream_id: StreamID,
    format: PcmFormat,
    sample_rate: SampleRate,
    channels: u10,
    bit_depth: NonZero<u8>,
    sequence_num: u8,
    start_time: Instant,
}

impl AafPcmTalker {
    /// Creates a new AAF PCM talker for encoding audio packets.
    ///
    /// # Arguments
    ///
    /// - `stream_id`: The stream identifier (MAC address + unique ID).
    /// - `format`: The PCM sample format (`Int16Bit`, `Int24Bit`, etc.).
    /// - `sample_rate`: The audio sample rate.
    /// - `channels`: The number of audio channels per frame.
    /// - `bit_depth`: The number of valid bits per sample.
    ///
    /// # Errors
    ///
    /// Returns [`AafStreamError`] if the configuration is invalid.
    pub fn new(
        stream_id: StreamID,
        format: PcmFormat,
        sample_rate: SampleRate,
        channels: u10,
        bit_depth: u8,
    ) -> Result<Self, AafStreamError> {
        let bit_depth = NonZero::new(bit_depth)
            .ok_or_else(|| AafStreamError::InvalidStream("bit_depth cannot be zero".to_string()))?;

        if channels.value() == 0 {
            return Err(AafStreamError::InvalidStream(
                "channels must be at least 1".to_string(),
            ));
        }

        Ok(Self {
            stream_id,
            format,
            sample_rate,
            channels,
            bit_depth,
            sequence_num: 0,
            start_time: Instant::now(),
        })
    }

    /// Builds and returns the next AVTP packet for transmission.
    ///
    /// Updates the sequence number and timestamp on each call.
    ///
    /// # Arguments
    ///
    /// - `payload`: Big-endian PCM audio samples (Arc-wrapped for zero-copy).
    ///
    /// # Errors
    ///
    /// Returns [`AafStreamError`] if the packet construction fails.
    pub fn build_packet(&mut self, payload: Arc<[u8]>) -> Result<Avtpdu, AafStreamError> {
        // Compute the AVTP timestamp from the elapsed time since stream start
        // The timestamp is naturally wrapped to 32 bits per the AVTP spec
        #[allow(clippy::cast_possible_truncation)]
        let avtp_timestamp = self.start_time.elapsed().as_nanos() as u32;

        // Create the PCM variant
        let pcm = AafPcm::new(
            self.sample_rate,
            self.channels,
            self.format,
            self.bit_depth.get(),
            payload,
        )
        .map_err(|e| AafStreamError::AafConstruction(e.to_string()))?;

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

        // Build the AAF payload
        let aaf =
            Aaf::new(generic, aaf_variant, false, u4::new(0))
                .map_err(|e| AafStreamError::AafConstruction(format!("{e:?}")))?;

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
    /// Returns [`AafStreamError`] if the packet fails to parse as AAF.
    pub fn process(&mut self, pdu: &Avtpdu) -> Result<Option<ReceivedPcm>, AafStreamError> {
        // Only process Stream PDUs
        let Avtpdu::Stream(stream_header) = pdu else {
            return Ok(None);
        };

        // Parse as AAF
        let aaf: Aaf = stream_header
            .clone()
            .try_into()
            .map_err(|e: InvalidAaf| AafStreamError::InvalidAaf(e))?;

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
            payload_be: Arc::from(aaf_pcm.payload_slice()),
            sample_rate: aaf_pcm.nominal_sample_rate(),
            channels: aaf_pcm.channels_per_frame(),
            bit_depth: aaf_pcm.bit_depth(),
            packets_lost,
        }))
    }
}

impl StreamTalker for AafPcmTalker {
    type Error = AafStreamError;

    fn build_packet(&mut self, payload: Arc<[u8]>) -> Result<Avtpdu, Self::Error> {
        Self::build_packet(self, payload)
    }
}

impl StreamListener for AafPcmListener {
    type Output = ReceivedPcm;
    type Error = AafStreamError;

    fn process(&mut self, pdu: &Avtpdu) -> Result<Option<Self::Output>, Self::Error> {
        Self::process(self, pdu)
    }
}
