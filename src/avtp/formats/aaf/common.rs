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
    Pcm(#[from] super::pcm::InvalidPcmAaf),

    #[error(transparent)]
    /// AES3 audio format parsing error.
    Aes3(#[from] super::aes3::InvalidAes3Aaf),
}

/// PCM-specific handling for AAF payloads.
///
/// This module contains parsing, validation and serialization logic for
/// uncompressed PCM audio transported via AAF.

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
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
pub(super) struct AafSpecificData {
    /// Format identifier determining how the payload is interpreted.
    pub(super) format: AafFormat,

    /// First format-specific data block.
    ///
    /// The field name follows the spec.
    pub(super) aaf_format_specific_data_1: [u8; 3],

    /// Second format-specific data block.
    ///
    /// The field name follows the spec.
    pub(super) asfd: u3,

    /// Third format-specific data block.
    ///
    /// The field name follows the spec.
    pub(super) aaf_format_specific_data_2: u8,

    /// Raw payload containing audio samples or encoded data.
    pub(super) aaf_format_specific_payload: Arc<[u8]>,
}

/// High-level representation of AAF payload variants.
///
/// Each variant corresponds to a different audio encoding format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AafVariant {
    /// Uncompressed PCM audio.
    Pcm(super::pcm::AafPcm),

    /// AES3 formatted audio data.
    Aes3(super::aes3::AafAes3),
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
            | AafFormat::Int16Bit => Ok(Self::Pcm(super::pcm::AafPcm::from_specific(value)?)),
            AafFormat::AES3_32Bit => Ok(Self::Aes3(super::aes3::AafAes3::from_specific(value)?)),
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
