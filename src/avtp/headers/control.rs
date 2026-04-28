use std::{io, sync::Arc};

use arbitrary_int::prelude::*;
use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};

use crate::{
    avtp::{
        headers::{CommonHeader, HeaderType},
        stream_id::StreamID,
        subtype::UnknownSubtype,
    },
    io::{
        enc_dec::{BitDecode, BitEncode, IOWrapError},
        utils::read_arc,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Header used by AVTP control-oriented packets.
pub struct ControlHeader {
    /// The preceding common header. See [`AvtpCommonHeader`].
    ///
    /// `common.header_specific_bit` maps to `sv` (stream ID valid). See
    /// [`Self::stream_id_valid`].
    pub common: CommonHeader, // 12 bits

    /// Control-format extension bits.
    ///
    /// Meaning depends on [`AvtpCommonHeader::subtype`].
    pub format_specific_data: u9,

    /// Size of [`Self::control_data_payload`] in bytes.
    pub control_data_length: u11,

    /// Optional stream identifier associated with this control message.
    ///
    /// Only meaningful when [`Self::stream_id_valid`] returns `true`.
    pub stream_id: StreamID,

    /// Control payload bytes interpreted by the selected control subtype.
    pub control_data_payload: Arc<[u8]>,
}

impl ControlHeader {
    #[must_use]
    /// Returns the `sv` bit (stream ID valid).
    ///
    /// `false` means the subtype does not define `stream_id` for this packet;
    /// `true` means [`Self::stream_id`] is present and usable.
    pub const fn stream_id_valid(&self) -> bool {
        self.common.header_specific_bit
    }

    /// Decode the Control Header info from a reader that had already decoded the
    /// [`AvtpCommonHeader`] header.
    ///
    /// This is useful because the common header is generally read first, as it contains data based
    /// on which the type of header is decided.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be positioned immediately after decoding an [`AvtpCommonHeader`].
    ///   When decoding begins from a byte-aligned input, this places the reader at a 4-bit offset.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Control`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// The reader will be in a byte-aligned position after decoding completes.
    pub(crate) fn decode_after_common<R: io::Read>(
        common: CommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        debug_assert_eq!(common.subtype.header_type(), HeaderType::Control);

        // Bit aligned
        let format_specific_data = u9::new(reader.read::<9, _>()?);
        let control_data_length = u11::new(reader.read::<11, _>()?);

        // The reader is guaranteed to be byte aligned after this
        // (assuming it came in unaligned, from after reading the common header)
        debug_assert!(reader.byte_aligned());

        // Byte aligned
        let stream_id = StreamID::decode(reader)?;
        let control_data_payload = read_arc(reader, usize::from(control_data_length.value()))?;

        Ok(Self {
            common,
            format_specific_data,
            control_data_length,
            stream_id,
            control_data_payload,
        })
    }
}

impl BitEncode for ControlHeader {
    type Error = io::Error;

    /// Encode the Control Header info.
    ///
    /// # Preconditions
    ///
    /// - The provided writer must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Control`]
    ///   subtype.
    /// - The [`Self::control_data_length`] must match the length of [`Self::control_data_payload`].
    ///
    /// # Postconditions
    ///
    /// The writer will be in a byte-aligned position after encoding completes.
    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        debug_assert_eq!(self.common.subtype.header_type(), HeaderType::Control);

        // We must start with a byte-aligned writer
        debug_assert!(writer.byte_aligned());

        // Bit aligned
        self.common.encode(writer)?;
        writer.write::<9, _>(self.format_specific_data.value())?;
        writer.write::<11, _>(self.control_data_length.value())?;

        // The rest of the writes are guaranteed to be byte aligned
        debug_assert!(writer.byte_aligned());

        self.stream_id.encode(writer)?;

        // TODO: Consider if this debug assert should be a runtime error.
        // This is hot path code though.
        debug_assert_eq!(
            usize::from(self.control_data_length.value()),
            self.control_data_payload.len()
        );
        writer.write_bytes(&self.control_data_payload)?;

        Ok(())
    }
}

impl BitDecode for ControlHeader {
    type Error = IOWrapError<UnknownSubtype>;

    /// Decode the Control Header.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Control`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// The reader will be in a byte-aligned position after decoding completes.
    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        // We must start with a byte-aligned reader
        debug_assert!(reader.byte_aligned());

        let common = CommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;

        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        Ok(decoded)
    }
}
