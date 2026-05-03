use std::sync::Arc;

use arbitrary_int::prelude::*;
use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};
use getset::{CopyGetters, Getters};
use num_enum::TryFromPrimitive;
use thiserror::Error;

use crate::avtp::{
    headers::{GenericStreamData, SpecificStreamData, StreamHeader},
    subtype::Subtype,
};

#[derive(Error, Debug)]
/// Errors that can occur while parsing or constructing an AAF AVTP payload.
///
/// These errors cover validation of the AVTP stream header as well as
/// delegation to format-specific parsing logic.
pub enum InvalidAaf {
    #[error("Attempted to initialize AAF payload from a header with subtype: {0}")]
    /// The provided AVTPDU did not use the AAF subtype.
    InvalidSubtype(Subtype),

    #[error("The stream_id_valid bit (header-specific bit from the common header) was 0")]
    /// The stream ID validity flag was not set.
    ///
    /// AAF streams require a valid stream ID to be present.
    StreamIdInvalid,

    #[error("Encountered an unsupported version from the common header: {0}")]
    /// The AVTP version is not supported by this implementation.
    ///
    /// Currently only version 0 is handled.
    UnsupportedVersion(u3),

    #[error("Got a reserved (unsupported) AAF format: {0}")]
    /// The format field contains a value reserved by the specification.
    FormatReserved(u8),

    #[error(transparent)]
    /// Error originating from format-specific parsing logic.
    FormatSpecificError(#[from] AafFormatSpecificError),
}

#[derive(Error, Debug)]
/// Errors originating from parsing of a specific AAF format variant.
///
/// Each variant corresponds to one of the supported AAF payload formats.
pub enum AafFormatSpecificError {
    #[error(transparent)]
    /// PCM audio format parsing error.
    Pcm(#[from] pcm::InvalidPcmAaf),

    #[error(transparent)]
    /// AES3 audio format parsing error.
    Aes3(#[from] aes3::InvalidAes3Aaf),
}

/// PCM-specific handling for AAF payloads.
///
/// This module contains parsing, validation and serialization logic for
/// uncompressed PCM audio transported via AAF.
pub mod pcm {
    use std::{num::NonZero, sync::Arc};

    use arbitrary_int::prelude::*;
    use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};
    use getset::{CopyGetters, Getters};
    use num_enum::TryFromPrimitive;
    use thiserror::Error;

    use crate::avtp::formats::aaf::{AafFormat, AafSpecificData};

    #[derive(Error, Debug)]
    /// Errors that can occur when parsing PCM AAF payload data.
    pub enum InvalidPcmAaf {
        #[error("Attempted to initialize a PCM AAF from a non-PCM format: {0:?}")]
        /// The AAF format field does not correspond to a PCM-compatible format.
        NonPcmFormat(AafFormat),

        #[error("Got a reserved (unsupported) nominal sample rate (nsr) for AAF PCM AVTPDU: {0}")]
        /// The nominal sample rate field contains a reserved value.
        SampleRateReserved(u4),

        #[error("Got a bit depth of {0}, which is not supported for the used PCM format: {1:?}")]
        /// The bit depth is inconsistent with the selected PCM format.
        ///
        /// This can happen if the bit depth exceeds the container size or
        /// violates format-specific requirements.
        BitDepthInvalid(u8, PcmFormat),

        #[error("Got a bit depth of 0")]
        /// A bit depth of zero is not valid (regardless of pcm format).
        BitDepthZero,

        #[error("Got a channels per frame value of 0")]
        /// A channels per frame of zero is not valid (at least 1 channel is required).
        ChannelsPerFrameZero,

        #[error("The size of the payload ({payload_size}) does not conform to the expected frame size {frame_size}")]
        /// The payload size does not align to a whole number of sample frames.
        PayloadSizeInvalid { payload_size: usize, frame_size: usize },

        #[error("The size of the payload ({0}) overflows u16::MAX")]
        /// The payload size can never overflow a u16, as the payload size is encoded with a u16
        /// during transport.
        PayloadTooLarge(usize),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
    #[repr(u8)]
    /// PCM-compatible subset of [`AafFormat`].
    ///
    /// Only these formats are interpreted as raw PCM audio samples.
    pub enum PcmFormat {
        /// User-defined format (interpretation is application-specific).
        User = 0x00,

        /// 32-bit IEEE floating point samples.
        Float32Bit = 0x01,

        /// 32-bit signed integer samples.
        Int32Bit = 0x02,

        /// 24-bit signed integer samples.
        Int24Bit = 0x03,

        /// 16-bit signed integer samples.
        Int16Bit = 0x04,
    }

    impl PcmFormat {
        #[must_use]
        /// Returns the size of a single sample word in bits.
        ///
        /// This represents the container size for each sample, not necessarily
        /// the number of valid bits (see [`AafPcm::bit_depth`]).
        ///
        /// Returns `None` for [`Self::User`] as the size is not defined.
        pub const fn word_size(&self) -> Option<u8> {
            match self {
                Self::User => None,
                Self::Float32Bit | Self::Int32Bit => Some(32),
                Self::Int24Bit => Some(24),
                Self::Int16Bit => Some(16),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
    #[repr(u8)] // u4 actually
    /// Nominal sample rate of the PCM stream.
    ///
    /// Encoded as a compact value in the AAF header.
    pub enum SampleRate {
        UserSpecified = 0x0,
        KHz8 = 0x1,
        KHz16 = 0x2,
        KHz32 = 0x3,
        KHz44_1 = 0x4,
        KHz48 = 0x5,
        KHz88_2 = 0x6,
        KHz96 = 0x7,
        KHz176_4 = 0x8,
        KHz192 = 0x9,
        KHz24 = 0xA,
    }

    impl SampleRate {
        #[must_use]
        /// Returns the numeric sample rate in Hz (hertz).
        ///
        /// Returns `None` if the rate is user-defined.
        pub const fn rate(&self) -> Option<usize> {
            match self {
                Self::UserSpecified => None,
                Self::KHz8 => Some(8_000),
                Self::KHz16 => Some(16_000),
                Self::KHz32 => Some(32_000),
                Self::KHz44_1 => Some(44_100),
                Self::KHz48 => Some(48_000),
                Self::KHz88_2 => Some(88_200),
                Self::KHz96 => Some(96_000),
                Self::KHz176_4 => Some(176_400),
                Self::KHz192 => Some(192_000),
                Self::KHz24 => Some(24_000),
            }
        }
    }

    /// Parsed representation of a PCM AAF payload.
    ///
    /// This structure provides a structured view of the PCM-related
    /// fields extracted from the AAF format-specific data.
    #[derive(Debug, Clone, PartialEq, Eq, Getters, CopyGetters)]
    pub struct AafPcm {
        /// The sample rate of the PCM audio stream (`nsr`)
        ///
        /// Stored as an enumeration of the supported sample rates.
        /// Represents the rate in hertz; e.g. 8kHz.
        ///
        /// This determines the replay speed of the received audio samples.
        #[getset(get_copy = "pub")]
        nominal_sample_rate: SampleRate,

        /// Number of audio channels in the sample frame
        ///
        /// For example, an audio stream conveying 7.1 surround sound
        /// would have 8 channels.
        #[getset(get_copy = "pub")]
        channels_per_frame: u10, // non-zero

        /// Indicates the size and the format of the audio sample word.
        #[getset(get_copy = "pub")]
        format: PcmFormat,

        /// Indicates which bits within the sample word contain valid data.
        ///
        /// This value can never be larger than the size of the format set in the [`Self::format`]
        /// field. If the format is [`PcmFormat::Float32Bit`], this value must always be set to 32.
        ///
        /// If the bit depth is smaller that the size of the sample word, the upper `bit_depth` bits
        /// hold the actual sample, with the rest of the audio sample word container being set to zero.
        /// E.g. `sample << (format_size - bit_depth)`
        #[getset(get_copy = "pub")]
        bit_depth: NonZero<u8>,

        /// Container for the individual audio samples.
        ///
        /// All AAF AVTPDUs within the same stream should have the same number of samples. The size
        /// of a single sample word is determined by [`Self::format`], within which, the actual
        /// sample occupies [`Self::bit_depth`] bits.
        ///
        /// Each multi-byte sample word is encoded in network byte order (big-endian), meaning the
        /// most-significant byte is stored at the lowest byte offset.
        ///
        /// The payload is a contiguous sequence of these sample words, stored in presentation order,
        /// i.e. the first sample in the payload is the first sample to be presented.
        ///
        /// If [`Self::channels_per_frame`] is more than 1, the order follows a round-robin pattern.
        /// With two channels, that can look like:
        /// `sample 1, channel 1 -> sample 1, channel 2 -> sample 2, channel 1 -> sample 2, channel 2`.
        ///
        /// Note that because all AAF AVTPDUs within the same stream should have the same number of
        /// samples, the last AVTPDU cannot have be shorter than the previous ones. It can must
        /// either be omitted or padded with synthetic data.
        #[getset(get_clone = "pub")]
        payload: Arc<[u8]>,
    }

    impl AafPcm {
        /// Validates and normalizes a bit depth value for the given PCM format.
        ///
        /// This function enforces all format-specific constraints on the bit depth
        /// and converts it into a [`NonZero<u8>`] if valid.
        ///
        /// # Errors
        ///
        /// Returns [`InvalidPcmAaf`] if:
        ///
        /// - the bit depth is zero
        /// - the bit depth exceeds the container size defined by [`PcmFormat::word_size`]
        /// - the format is [`PcmFormat::Float32Bit`] and the bit depth is not exactly 32
        fn validated_bit_depth(bit_depth: u8, format: PcmFormat) -> Result<NonZero<u8>, InvalidPcmAaf> {
            let bit_depth = NonZero::new(bit_depth).ok_or(InvalidPcmAaf::BitDepthZero)?;

            if matches!(format, PcmFormat::Float32Bit) && bit_depth.get() != 32 {
                return Err(InvalidPcmAaf::BitDepthInvalid(bit_depth.get(), format));
            }

            if let Some(word_size_bits) = format.word_size()
                && bit_depth.get() > word_size_bits
            {
                return Err(InvalidPcmAaf::BitDepthInvalid(bit_depth.get(), format));
            }

            Ok(bit_depth)
        }

        /// Validates that the payload size is consistent with the PCM layout.
        ///
        /// The payload must consist of an integer number of sample frames.
        /// A frame is defined as one sample per channel.
        ///
        /// If the format defines a fixed sample word size, the payload length must be
        /// divisible by:
        ///
        /// `word_size_bytes * channels_per_frame`
        ///
        /// For formats with unspecified word size (e.g. [`PcmFormat::User`]),
        /// no validation is performed.
        ///
        /// # Errors
        ///
        /// Returns:
        /// - [`InvalidPcmAaf::PayloadSizeInvalid`] if the payload length does not align to a whole
        ///   number of frames.
        /// - [`InvalidPcmAaf::PayloadTooLarge`] if the payload length overflows `u16::MAX`.
        fn validate_payload_size(
            format: PcmFormat,
            payload_len: usize,
            channels_per_frame: u10,
        ) -> Result<(), InvalidPcmAaf> {
            if payload_len > usize::from(u16::MAX) {
                return Err(InvalidPcmAaf::PayloadTooLarge(payload_len));
            }

            if let Some(word_size_bits) = format.word_size() {
                debug_assert_eq!(word_size_bits % 8, 0);
                let word_size_bytes = usize::from(word_size_bits / 8);
                let channels = usize::from(channels_per_frame.value());

                // WARN: In theory, u8::MAX * u10::MAX would overflow a usize::MAX if we had a usize
                // of u16. Even though we don't expect to be working with u16 usizes, this is worth
                // noting.
                let frame_size = word_size_bytes * channels;
                debug_assert_ne!(frame_size, 0);

                if payload_len % frame_size != 0 {
                    return Err(InvalidPcmAaf::PayloadSizeInvalid {
                        payload_size: payload_len,
                        frame_size,
                    });
                }
            }

            Ok(())
        }

        /// Constructs a PCM representation from raw AAF format-specific data.
        ///
        /// Reserved fields will be dropped.
        pub(super) fn from_specific(data: AafSpecificData) -> Result<Self, InvalidPcmAaf> {
            let format =
                PcmFormat::try_from(data.format as u8).map_err(|_| InvalidPcmAaf::NonPcmFormat(data.format))?;

            let mut reader = BitReader::endian(&data.aaf_format_specific_data_1[..], BigEndian);

            // The unwraps here are safe, as all of these read operations are using a static buffer
            // which cannot produce an IO error here.
            let nominal_sample_rate = unsafe { u4::new_unchecked(reader.read::<4, u8>().unwrap()) };
            reader.read::<2, u8>().unwrap(); // reserved
            let channels_per_frame = unsafe { u10::new_unchecked(reader.read::<10, u16>().unwrap()) };
            let bit_depth = reader.read::<8, u8>().unwrap();
            debug_assert!(reader.byte_aligned());

            // [`AafSpecificData::asfd`] is reserved
            // [`AafSpecificData::aaf_format_specific_data_2`] is reserved

            let nominal_sample_rate = SampleRate::try_from(nominal_sample_rate.value())
                .map_err(|e| InvalidPcmAaf::SampleRateReserved(u4::new(e.number)))?;

            let bit_depth = Self::validated_bit_depth(bit_depth, format)?;

            if channels_per_frame.value() == 0 {
                return Err(InvalidPcmAaf::ChannelsPerFrameZero);
            }

            Self::validate_payload_size(format, data.aaf_format_specific_payload.len(), channels_per_frame)?;

            Ok(Self {
                nominal_sample_rate,
                channels_per_frame,
                format,
                bit_depth,
                payload: data.aaf_format_specific_payload,
            })
        }

        /// Collapse the PCM representation into a raw AAF format-specific data.
        pub(super) fn into_specific(self) -> AafSpecificData {
            let mut aaf_format_specific_data_1 = [0u8; 3];
            let mut writer = BitWriter::endian(&mut aaf_format_specific_data_1[..], BigEndian);
            writer.write::<4, _>(self.nominal_sample_rate as u8).unwrap();
            writer.write::<2, _>(0).unwrap(); // reserved
            writer.write::<10, _>(self.channels_per_frame.value()).unwrap();
            writer.write::<8, _>(self.bit_depth.get()).unwrap();
            debug_assert!(writer.byte_aligned());

            AafSpecificData {
                format: AafFormat::try_from(self.format as u8).unwrap(),
                aaf_format_specific_data_1,
                asfd: u3::new(0),              // reserved
                aaf_format_specific_data_2: 0, // reserved
                aaf_format_specific_payload: self.payload,
            }
        }

        /// Constructs a new PCM AAF payload representation.
        ///
        /// This constructor validates that:
        ///
        /// - the bit depth is compatible with the selected format
        /// - the payload length corresponds to a whole number of sample frames
        ///
        /// # Errors
        ///
        /// Returns [`InvalidPcmAaf`] if any of the above invariants are violated.
        pub fn new(
            nominal_sample_rate: SampleRate,
            channels_per_frame: u10,
            format: PcmFormat,
            bit_depth: u8,
            payload: Arc<[u8]>,
        ) -> Result<Self, InvalidPcmAaf> {
            let bit_depth = Self::validated_bit_depth(bit_depth, format)?;

            if channels_per_frame.value() == 0 {
                return Err(InvalidPcmAaf::ChannelsPerFrameZero);
            }

            Self::validate_payload_size(format, payload.len(), channels_per_frame)?;

            Ok(Self {
                nominal_sample_rate,
                channels_per_frame,
                format,
                bit_depth,
                payload,
            })
        }

        /// Updates the nominal sample rate.
        ///
        /// This does not affect payload layout and is always valid.
        pub const fn set_nominal_sample_rate(&mut self, rate: SampleRate) {
            self.nominal_sample_rate = rate;
        }

        /// Returns a zero-copy view of the PCM payload.
        ///
        /// This provides read-only access to the underlying audio data without
        /// cloning the internal buffer.
        ///
        /// For shared ownership of the payload, use `payload()` instead.
        #[must_use]
        pub fn payload_slice(&self) -> &[u8] {
            &self.payload
        }

        #[must_use]
        /// Returns the number of sample frames contained in the payload.
        ///
        /// A frame consists of one sample per channel.
        ///
        /// Returns `None` if the format does not define a fixed sample word size
        /// (e.g. [`PcmFormat::User`]).
        pub fn sample_count(&self) -> Option<usize> {
            let word_size_bits = self.format.word_size()?;
            debug_assert_eq!(word_size_bits % 8, 0);
            let word_size_bytes = usize::from(word_size_bits / 8);
            let channels = usize::from(self.channels_per_frame.value());
            let frame_size = word_size_bytes * channels;
            debug_assert_eq!(self.payload.len() % frame_size, 0); // validated invariant
            Some(self.payload.len() / frame_size)
        }
    }
}

/// AES3-specific handling for AAF payloads.
///
/// This module contains parsing, validation and serialization logic for
/// AES3 formatted audio transported via AAF.
pub mod aes3 {
    use getset::{CopyGetters, Getters};
    use thiserror::Error;

    use crate::avtp::formats::aaf::{AafFormat, AafSpecificData};

    #[derive(Error, Debug)]
    pub enum InvalidAes3Aaf {
        #[error("Attempted to initialize an AES3 AAF from a non-AES3 format: {0:?}")]
        NonAes3Format(AafFormat),
    }

    // TODO: This is currently WIP
    // (Acts as a simple container around the AAF specific data)
    #[derive(Debug, Clone, PartialEq, Eq, Getters, CopyGetters)]
    pub struct AafAes3(AafSpecificData);

    impl AafAes3 {
        pub(super) fn from_specific(data: AafSpecificData) -> Result<Self, InvalidAes3Aaf> {
            match data.format {
                AafFormat::User
                | AafFormat::Float32Bit
                | AafFormat::Int32Bit
                | AafFormat::Int24Bit
                | AafFormat::Int16Bit => return Err(InvalidAes3Aaf::NonAes3Format(data.format)),
                AafFormat::AES3_32Bit => {}
            }

            Ok(Self(data))
        }

        pub(super) fn into_specific(self) -> AafSpecificData {
            self.0
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
/// Enumeration of supported AAF payload format types.
///
/// This value determines how the format-specific data and payload
/// should be interpreted.
pub enum AafFormat {
    /// User specified format
    User = 0x00,

    /// 32-bit floating point
    Float32Bit = 0x01,

    /// 32-bit integer
    Int32Bit = 0x02,

    /// 24-bit integer
    Int24Bit = 0x03,

    /// 16-bit integer
    Int16Bit = 0x04,

    /// 32-bit AES3 format
    AES3_32Bit = 0x05,
}

/// Internal container holding raw AAF format-specific fields.
///
/// This structure mirrors the layout of the format-specific section
/// of an AAF AVTPDU and is used as an intermediate representation
/// between wire-level parsing and format-specific interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AafSpecificData {
    /// Format identifier determining how the payload is interpreted.
    format: AafFormat,

    /// First format-specific data block.
    ///
    /// The field name follows the spec.
    aaf_format_specific_data_1: [u8; 3],

    /// Second format-specific data block.
    ///
    /// The field name follows the spec.
    asfd: u3,

    /// Third format-specific data block.
    ///
    /// The field name follows the spec.
    aaf_format_specific_data_2: u8,

    /// Raw payload containing audio samples or encoded data.
    aaf_format_specific_payload: Arc<[u8]>,
}

/// High-level representation of AAF payload variants.
///
/// Each variant corresponds to a different audio encoding format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AafVariant {
    /// Uncompressed PCM audio.
    Pcm(pcm::AafPcm),

    /// AES3 formatted audio data.
    Aes3(aes3::AafAes3),
}

impl From<AafVariant> for AafSpecificData {
    fn from(val: AafVariant) -> Self {
        match val {
            AafVariant::Pcm(data) => data.into_specific(),
            AafVariant::Aes3(data) => data.into_specific(),
        }
    }
}

impl TryFrom<AafSpecificData> for AafVariant {
    type Error = AafFormatSpecificError;

    fn try_from(value: AafSpecificData) -> Result<Self, Self::Error> {
        match value.format {
            AafFormat::User
            | AafFormat::Float32Bit
            | AafFormat::Int32Bit
            | AafFormat::Int24Bit
            | AafFormat::Int16Bit => Ok(Self::Pcm(pcm::AafPcm::from_specific(value)?)),
            AafFormat::AES3_32Bit => Ok(Self::Aes3(aes3::AafAes3::from_specific(value)?)),
        }
    }
}

/// AVTP Audio Format (AAF) payload.
///
/// This structure represents the complete AAF payload, including
/// both generic stream metadata and format-specific audio data.
#[derive(Debug, Clone, PartialEq, Eq, Getters, CopyGetters)]
pub struct Aaf {
    /// Generic stream-related fields from the AVTP header.
    #[getset(get = "pub")]
    stream_data: GenericStreamData,

    /// AAF data specific to the AAF sub-format variant.
    #[getset(get = "pub")]
    format_data: AafVariant,

    /// Sparse timestamping mode flag (`sp`).
    ///
    /// In sparse timestamping mode (if true), the AVTP presentation time is
    /// only contained in every eighth AAF AVTPDU. (In normal mode, it is
    /// contained in every AAF AVTPDU.)
    ///
    /// The timestamp mode for a stream must remain constant.
    ///
    /// If enabled, every eighth AAF AVTPDU should set [`GenericStreamData::avtp_timestamp_valid`]
    /// to 1 (true), rest should use 0 (false). If disabled, every AAF AVTPDU should use 1 (true).
    #[getset(get_copy = "pub")]
    sparse_timestamp: bool,

    /// Event-related flags carried in the AAF header (`evt`).
    ///
    /// These bits are reserved for signaling events within the stream.
    #[getset(get_copy = "pub")]
    event_flags: u4,
}

impl Aaf {
    /// Constructs a new AAF payload.
    ///
    /// This validates that the provided stream metadata is compatible with
    /// the AAF subtype and meets basic protocol requirements.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidAaf`] if:
    ///
    /// - the subtype is not AAF
    /// - the stream ID is not marked as valid
    /// - the AVTP version is not supported
    pub fn new(
        stream_data: GenericStreamData,
        format_data: AafVariant,
        sparse_timestamp: bool,
        event_flags: u4,
    ) -> Result<Self, InvalidAaf> {
        Self::validate_stream_data(&stream_data)?;

        Ok(Self {
            stream_data,
            format_data,
            sparse_timestamp,
            event_flags,
        })
    }

    /// Internal helper to ensure the given stream data are valid
    /// for an AAF format's expectations.
    fn validate_stream_data(stream_data: &GenericStreamData) -> Result<(), InvalidAaf> {
        if stream_data.common().subtype != Subtype::AAF {
            return Err(InvalidAaf::InvalidSubtype(stream_data.common().subtype));
        }

        if !stream_data.stream_id_valid() {
            return Err(InvalidAaf::StreamIdInvalid);
        }

        if stream_data.common().version.value() != 0 {
            return Err(InvalidAaf::UnsupportedVersion(stream_data.common().version));
        }

        Ok(())
    }

    /// Update the [`Self::stream_data`] value.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidAaf`] if:
    ///
    /// - the subtype is not AAF
    /// - the stream ID is not marked as valid
    /// - the AVTP version is not supported
    pub fn set_stream_data(&mut self, stream_data: GenericStreamData) -> Result<(), InvalidAaf> {
        Self::validate_stream_data(&stream_data)?;
        self.stream_data = stream_data;
        Ok(())
    }
}

impl TryFrom<StreamHeader> for Aaf {
    type Error = InvalidAaf;

    fn try_from(stream_header: StreamHeader) -> Result<Self, Self::Error> {
        Self::validate_stream_data(&stream_header.generic)?;

        // Reserved fields:
        // [`stream_header.specific.format_specific_data`] is reserved (2 bits)
        // [`stream_header.specific.format_specific_data_1`] is reserved (7 bits)
        //
        // We currently completely ignore these and do not even store the values.
        // When writing, the spec expects these to be zero, when receiving, they
        // can safely be dropped (even if non-zero).

        let [format, f1, f2, f3] = stream_header.specific.format_specific_data_2();
        let aaf_format_specific_data_1 = [f1, f2, f3];

        // The unwraps here are safe, as all of these read operations are using a static buffer
        // which cannot produce an IO error here.
        let format_specific_data_3 = &stream_header.specific.format_specific_data_3()[..];
        let mut reader = BitReader::endian(format_specific_data_3, BigEndian);
        let asfd = unsafe { u3::new_unchecked(reader.read::<3, u8>().unwrap()) };
        let sparse_timestamp = reader.read::<1, bool>().unwrap();
        let event_flags = unsafe { u4::new_unchecked(reader.read::<4, u8>().unwrap()) };
        let aaf_format_specific_data_2 = reader.read::<8, u8>().unwrap();
        debug_assert!(reader.byte_aligned());

        let aaf_format_specific_payload = stream_header.specific.stream_data_payload();

        let format = AafFormat::try_from(format).map_err(|e| InvalidAaf::FormatReserved(e.number))?;

        let generic_fmt_data = AafSpecificData {
            format,
            aaf_format_specific_data_1,
            asfd,
            aaf_format_specific_data_2,
            aaf_format_specific_payload,
        };

        Ok(Self {
            stream_data: stream_header.generic,
            format_data: generic_fmt_data.try_into()?,
            sparse_timestamp,
            event_flags,
        })
    }
}

#[expect(clippy::fallible_impl_from)]
impl From<Aaf> for StreamHeader {
    fn from(val: Aaf) -> Self {
        let mut buffer = [0u8; 2];
        let mut writer = BitWriter::endian(&mut buffer[..], BigEndian);

        let generic_fmt_data: AafSpecificData = val.format_data.into();

        // These unwraps for the writes are safe, the operations are guaranteed infallible
        writer.write::<3, _>(generic_fmt_data.asfd.value()).unwrap();
        writer.write::<1, _>(val.sparse_timestamp).unwrap();
        writer.write::<4, _>(val.event_flags.value()).unwrap();
        writer
            .write::<8, _>(generic_fmt_data.aaf_format_specific_data_2)
            .unwrap();

        // The individual variants must ensure that their invariants do not allow the payload length
        // to overflow u16::MAX.
        let specific = SpecificStreamData::new_unchecked(
            u2::new(0),
            u7::new(0),
            [
                generic_fmt_data.format as u8,
                generic_fmt_data.aaf_format_specific_data_1[0],
                generic_fmt_data.aaf_format_specific_data_1[1],
                generic_fmt_data.aaf_format_specific_data_1[2],
            ],
            buffer,
            generic_fmt_data.aaf_format_specific_payload,
        )
        .expect("Payload length overflows u16::MAX");

        Self {
            generic: val.stream_data,
            specific,
        }
    }
}
