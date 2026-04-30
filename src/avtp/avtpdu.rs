use std::io;

use bitstream_io::{BigEndian, BitReader, BitWrite, BitWriter};
use thiserror::Error;

use crate::{
    avtp::{
        headers::{AlternativeHeader, CommonHeader, ControlHeader, HeaderType, StreamDataError, StreamHeader},
        subtype::{IncompatibleSubtype, Subtype, UnknownSubtype},
    },
    io::enc_dec::{BitDecode, BitEncode, IOWrapError},
};

#[derive(Debug, Error)]
pub enum AvtpduError {
    #[error(transparent)]
    UnknownSubtype(#[from] UnknownSubtype),

    #[error(transparent)]
    IncompatibleSubtype(#[from] IncompatibleSubtype),

    #[error(transparent)]
    StreamHeaderError(#[from] StreamDataError),
}

impl From<IOWrapError<UnknownSubtype>> for IOWrapError<AvtpduError> {
    fn from(err: IOWrapError<UnknownSubtype>) -> Self {
        err.map_specific(AvtpduError::UnknownSubtype)
    }
}
impl From<IOWrapError<StreamDataError>> for IOWrapError<AvtpduError> {
    fn from(err: IOWrapError<StreamDataError>) -> Self {
        err.map_specific(AvtpduError::StreamHeaderError)
    }
}

#[derive(Debug, Clone)]
pub enum Avtpdu {
    Stream(StreamHeader),
    Control(ControlHeader),
    Alternative(AlternativeHeader),
}

impl BitDecode for Avtpdu {
    type Error = IOWrapError<AvtpduError>;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let common = CommonHeader::decode(reader)?;

        match common.subtype.header_type() {
            HeaderType::Stream => Ok(Self::Stream(StreamHeader::decode_after_common(common, reader)?)),
            HeaderType::Control => Ok(Self::Control(ControlHeader::decode_after_common(common, reader)?)),
            HeaderType::Alternative => Ok(Self::Alternative(AlternativeHeader::decode_after_common(
                common, reader,
            )?)),
        }
    }
}

impl BitEncode for Avtpdu {
    type Error = IOWrapError<AvtpduError>;

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        match self {
            Self::Stream(h) => Ok(h.encode(writer)?),
            Self::Control(h) => Ok(h.encode(writer)?),
            Self::Alternative(h) => Ok(h.encode(writer)?),
        }
    }
}

impl Avtpdu {
    #[must_use]
    /// Obtain the header type variant for this AVTPDU
    pub const fn header_type(&self) -> HeaderType {
        match self {
            Self::Stream(_) => HeaderType::Stream,
            Self::Control(_) => HeaderType::Control,
            Self::Alternative(_) => HeaderType::Alternative,
        }
    }

    #[must_use]
    /// Obtain the common header shared by all AVTPDU header variants
    pub fn common_header(&self) -> &CommonHeader {
        match self {
            Self::Stream(h) => h.generic.common(),
            Self::Control(h) => &h.common,
            Self::Alternative(h) => &h.common,
        }
    }

    #[must_use]
    /// Obtain the subtype from the common header shared by all AVTPDU header variants
    pub fn subtype(&self) -> Subtype {
        self.common_header().subtype
    }

    /// Validate that the given Subtype matches the expected header type variant
    fn validate_subtype(&self) -> Result<(), IncompatibleSubtype> {
        let subtype = self.subtype();
        let header_type = self.header_type();

        if subtype.header_type() != header_type {
            return Err(IncompatibleSubtype { subtype, header_type });
        }

        Ok(())
    }

    /// Encodes this AVTPDU into the provided writer.
    ///
    /// # Notes
    ///
    /// - If using a buffer writer, the caller is responsible for ensuring the buffer is in the
    ///   desired state (e.g. calling `clear()` when reusing a `Vec<u8>`).
    /// - If you're using a dynamically growing buffer, the recommended initial capacity
    ///   is 1500 bytes (Ethernet MTU) to avoid re-allocs.
    ///
    /// # Errors
    ///
    /// The encoding process can result in an [`io::Error`], if the underlying header encoding
    /// failed. This is propagated as [`IOWrapError::Io`]. This can happen if:
    ///
    /// - The passed writer fails on a write operation due to some external issue.
    /// - The values inside of the header do not meet the internal consistency expectations for
    ///   bit-size and cannot be encoded. (E.g. if trying to encode a 3-bit number, represented as a
    ///   u8 underlying type in Rust, with a value that overflows the 3-bit max - such as 10).
    ///
    /// Additionally, if [`Self::subtype`] does not correspond to the [`Self::header_type`], the
    /// [`IOWrapError::Specific`] variant is returned, with the [`IncompatibleSubtype`] error
    /// contained.
    ///
    /// If the passed writer is a simple buffer, and the caller made sure the internal state is
    /// consistent with the expected invariants, this can be considered as infallible.
    pub fn write<W: io::Write>(&self, writer: &mut W) -> Result<(), IOWrapError<AvtpduError>> {
        if let Err(exc) = self.validate_subtype() {
            return Err(IOWrapError::Specific(AvtpduError::from(exc)));
        }

        let mut writer = BitWriter::endian(writer, BigEndian);
        self.encode(&mut writer)?;

        // Just a sanity-check, encoding the struct should always leave our writer in a byte aligned
        // state. If it didn't, it's an issue in our lib.
        debug_assert!(writer.byte_aligned());

        Ok(())
    }

    /// Attempts to read (decode) an AVTPDU.
    ///
    /// # Errors
    ///
    /// - The reading process can result in an [`io::Error`], which is returned as
    ///   [`IOWrapError::Io`] err variant. These errors can occur from underlying issues with
    ///   reading the data using the provided reader.
    /// - Additionally, [`IOWrapError::Specific`] err variant is returned if the packet data were in
    ///   an internally inconsistent state, which couldn't be parsed.
    pub fn read<R: io::Read>(reader: &mut R) -> Result<Self, IOWrapError<AvtpduError>> {
        let mut reader = BitReader::endian(reader, BigEndian);
        Self::decode(&mut reader)
    }
}
