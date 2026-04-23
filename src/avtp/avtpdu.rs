use std::io;

use bitstream_io::{BigEndian, BitReader, BitWrite, BitWriter};

use crate::{
    avtp::{AvtpAlternativeHeader, AvtpCommonHeader, AvtpControlHeader, AvtpStreamHeader, HeaderType, UnknownSubtype},
    io::enc_dec::{BitDecode, BitEncode, IOWrapError},
};

#[derive(Debug, Clone)]
pub enum Avtpdu {
    Stream(AvtpStreamHeader),
    Control(AvtpControlHeader),
    Alternative(AvtpAlternativeHeader),
}

impl BitDecode for Avtpdu {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let common = AvtpCommonHeader::decode(reader)?;

        match common.subtype.header_type() {
            HeaderType::Stream => Ok(Self::Stream(AvtpStreamHeader::decode_after_common(common, reader)?)),
            HeaderType::Control => Ok(Self::Control(AvtpControlHeader::decode_after_common(common, reader)?)),
            HeaderType::Alternative => Ok(Self::Alternative(AvtpAlternativeHeader::decode_after_common(
                common, reader,
            )?)),
        }
    }
}

impl BitEncode for Avtpdu {
    type Error = io::Error;

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        match self {
            Self::Stream(h) => h.encode(writer),
            Self::Control(h) => h.encode(writer),
            Self::Alternative(h) => h.encode(writer),
        }
    }
}

impl Avtpdu {
    #[must_use]
    pub const fn header_type(&self) -> HeaderType {
        match self {
            Self::Stream(_) => HeaderType::Stream,
            Self::Control(_) => HeaderType::Control,
            Self::Alternative(_) => HeaderType::Alternative,
        }
    }

    /// Encodes this AVTPDU into the provided writer.
    ///
    /// # Notes
    ///
    /// - If using a buffer writer, the caller is responsible for ensuring the buffer is in the
    ///   desired state (e.g. calling `clear()` when reusing a `Vec<u8>`).
    /// - If you're using a dynamically growing buffer, the recommended initial capacity
    ///   is 1500 bytes (Ethernet MTU) to avoid re-allocs.
    /// - This method avoids buffer allocation and is preferred in hot paths with many encodes.
    ///   If you wish to construct the buffer too (with allocation), you can use a short-hand
    ///   function [`Self::to_bytes`] instead.
    ///
    /// # Errors
    ///
    /// The encoding process can result in an [`io::Error`], if the underlying header encoding
    /// failed. Depending on the passed writer, and the state of the contained headers, it is
    /// possible for this to be infallible. The caller is responsible for ensuring this guarantee.
    pub fn encode_into<W: io::Write>(&self, writer: &mut W) -> io::Result<()> {
        let mut writer = BitWriter::endian(writer, BigEndian);
        self.encode(&mut writer)?;

        // Just a sanity-check, encoding the struct should always leave our writer in a byte aligned
        // state. If it didn't, it's an issue in our lib.
        debug_assert!(writer.byte_aligned());

        Ok(())
    }

    /// Encodes this AVTPDU into a newly allocated [`Vec`] buffer.
    ///
    /// The buffer is preallocated to the Ethernet MTU (1500 bytes) before encode,
    /// which avoids reallocations for typical AVTP packets.
    ///
    /// # Notes
    ///
    /// In hot path code which performs many consequent encodes, it is often better to re-use a
    /// single buffer, as this function will always allocate. See [`Self::encode_into`].
    ///
    /// # Errors
    ///
    /// The encoding process can result in an [`io::Error`], if the underlying header encoding
    /// failed. This can only happen if the state of the contained headers doesn't meet certain
    /// internal consistency expectations for the given header type. If the caller can ensure that
    /// the contained header is internally consistent, this operation is infallible.
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(1500); // Ethernet MTU (Maximum Transmission Unit, payload size)
        self.encode_into(&mut buf)?;
        Ok(buf)
    }

    /// Attempts to read (decode) an AVTPDU.
    ///
    /// # Errors
    ///
    /// The reading process can result in an [`io::Error`], which is returned as [`IOWrapError::Io`]
    /// err variant. These errors generally come from underlying issues with writing the data.
    /// Additionally, [`IOWrapError::Specific`] err variant is returned if the packet data were in
    /// an internally inconsistent state.
    pub fn read<R: io::Read>(reader: &mut R) -> Result<Self, IOWrapError<UnknownSubtype>> {
        let mut reader = BitReader::endian(reader, BigEndian);
        Self::decode(&mut reader)
    }
}
