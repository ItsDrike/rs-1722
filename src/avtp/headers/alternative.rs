use std::{io, sync::Arc};

use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};

use crate::{
    avtp::{
        headers::{CommonHeader, HeaderType},
        subtype::{Subtype, UnknownSubtype},
    },
    io::enc_dec::{BitDecode, BitEncode, IOWrapError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// This header is used by formats that do not conform to the usual control or stream common
/// headers.
///
/// It is designed to be very flexible and allow representing essentially any custom structures by
/// containing a dynamic payload, which are just prefixed by the common header.
pub struct AlternativeHeader {
    /// The preceding common header. See [`CommonHeader`].
    ///
    /// `common.header_specific_bit` meaning differs depending on the format that
    /// use the alternative header.
    pub common: CommonHeader, // 12 bits

    // The meaning of this field is format-specific.
    //
    // The actual implementation is defined by each format that uses the alternative header.
    pub alternative_data_payload: Arc<[u8]>,
}

impl AlternativeHeader {
    /// Decode the Alternative Header info from a reader that had already decoded the
    /// [`CommonHeader`] header.
    ///
    /// This is useful because the common header is generally read first, as it contains data based
    /// on which the type of header is decided.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be positioned immediately after decoding a [`CommonHeader`].
    ///   When decoding begins from a byte-aligned input, this places the reader at a 4-bit offset.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Alternative`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// - The reader will be in a byte-aligned position after decoding completes.
    /// - The last nibble (4-bits) of the last byte of [`Self::alternative_data_payload`] will be
    ///   padding (zeroes) introduced by this implementation. This just reflects the internal
    ///   structural representation of the payload that follows the 12-bit common header, to end off
    ///   in a byte-aligned state. This only holds if the payload wasn't empty.
    ///
    #[expect(clippy::unnecessary_wraps)]
    pub(crate) fn decode_after_common<R: io::Read>(
        common: CommonHeader,
        _reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        debug_assert_eq!(common.subtype.header_type(), HeaderType::Alternative);

        // The structure of the alternative header differs based on the subtype
        #[expect(unused_variables)]
        let alternative_data_payload = match common.subtype {
            Subtype::IEC_61883_IIDC
            | Subtype::MMA_STREAM
            | Subtype::AAF
            | Subtype::CVF
            | Subtype::TSCF
            | Subtype::SVF
            | Subtype::RVF
            | Subtype::VSF_STREAM
            | Subtype::EF_STREAM
            | Subtype::ADP
            | Subtype::AECP
            | Subtype::ACMP
            | Subtype::MAAP
            | Subtype::EF_CONTROL => {
                unreachable!("Not an alternatvie header: {0}", common.subtype)
            }

            Subtype::CRF
            | Subtype::AEF_CONTINUOUS
            | Subtype::NTSCF
            | Subtype::ESCF
            | Subtype::EECF
            | Subtype::AEF_DISCRETE => todo!("Alternative subtype {0} is not yet supported", common.subtype),
        };

        #[expect(unreachable_code)]
        Ok(Self {
            common,
            alternative_data_payload,
        })
    }
}

impl BitEncode for AlternativeHeader {
    type Error = io::Error;

    /// Encode the Alternative Header info.
    ///
    /// # Preconditions
    ///
    /// - The provided writer must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Alternative`]
    ///   subtype.
    /// - The last nibble (4-bits) of the last byte of [`Self::alternative_data_payload`] (if
    ///   non-empty) must be padding (zeroes). This just reflects the internal structural
    ///   representation of the payload that follows the 12-bit common header, to end off in a
    ///   byte-aligned state. (If using [`Self::decode`], this will be guaranteed.)
    ///
    /// # Postconditions
    ///
    /// The writer will be in a byte-aligned position after encoding completes.
    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        debug_assert_eq!(self.common.subtype.header_type(), HeaderType::Alternative);

        // We must start with a byte-aligned writer
        debug_assert!(writer.byte_aligned());

        self.common.encode(writer)?; // 12-bit

        // The logic for writing the payload differs per-subtype at the field level, but overall, it
        // always ends up byte-aligned. What this means is that we need to write it without the last
        // 4 bits of the last byte in the stored payload of this struct, as the common header is
        // 12-bit, so the last nibble in the payload is always just going to be padding with how we
        // store the data. This padding is an implementation detail.
        if let Some((last, rest)) = self.alternative_data_payload.split_last() {
            writer.write_bytes(rest)?;

            debug_assert_eq!(last & 0x0F, 0, "lower nibble must be zero (padding)");
            let upper_nibble = last >> 4;
            writer.write::<4, _>(upper_nibble)?;
        } else {
            // If the payload is empty, we still need to restore alignment after the 12-bit common
            // header (by writing 4 zero bits).
            //
            // This can only happen if the caller created the alternative_data_payload manually to
            // be empty. The spec wording does technically allow for alternative_data_payload to
            // have a length of 0, but the decode function guarantees to return at least 1 byte even
            // for empty payloads, for the 4-bit zero padding (to byte-align), so this is only
            // reached if the caller passed in an empty payload manually.
            writer.byte_align()?;
        }

        // The writer is guaranteed to be byte aligned at the end
        debug_assert!(writer.byte_aligned());

        Ok(())
    }
}

impl BitDecode for AlternativeHeader {
    type Error = IOWrapError<UnknownSubtype>;

    /// Decode the Alternative Header.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Alternative`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// - The reader will be in a byte-aligned position after decoding completes.
    /// - The last nibble (4-bits) of the last byte of [`Self::alternative_data_payload`] will be
    ///   padding (zeroes) introduced by this implementation. This just reflects the internal
    ///   structural representation of the payload that follows the 12-bit common header, to end off
    ///   in a byte-aligned state. This only holds if the payload wasn't empty.
    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        let common = CommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;

        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        Ok(decoded)
    }
}
