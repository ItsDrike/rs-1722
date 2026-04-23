use std::{io, sync::Arc};

use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};

use crate::{
    avtp::{Subtype, UnknownSubtype, stream_id::StreamID},
    io::{
        enc_dec::{BitDecode, BitEncode, IOWrapError},
        utils::read_arc,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderType {
    Control,
    Stream,
    Alternative,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Common prefix shared by AVTP stream and control packets.
///
/// This compact header is always 12 bits long and provides the subtype, a
/// context-dependent flag bit, and protocol version bits.
pub struct AvtpCommonHeader {
    /// Format identifier for the packet payload and header interpretation.
    ///
    /// See [`Subtype`] for the full list of supported values.
    pub subtype: Subtype, // 8 bits

    /// A single flag whose meaning depends on the selected header family.
    ///
    /// For stream and control headers this bit is exposed as `sv` via
    /// [`AvtpStreamHeader::stream_id_valid`] and [`AvtpControlHeader::stream_id_valid`].
    pub header_specific_bit: bool, // 1 bit

    /// Protocol version bits for this header layout.
    ///
    /// Most formats currently use `0`. Receivers should verify this value and
    /// reject packets that advertise an unsupported version.
    pub version: u8, // 3 bits
}

impl BitEncode for AvtpCommonHeader {
    type Error = io::Error;

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        self.subtype.encode(writer)?;
        writer.write_bit(self.header_specific_bit)?;
        writer.write::<3, u8>(self.version)?;

        Ok(())
    }
}

impl BitDecode for AvtpCommonHeader {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let subtype = Subtype::decode(reader)?;

        let header_specific_bit = reader.read_bit()?;

        let version = reader.read::<3, u8>()?;

        Ok(Self {
            subtype,
            header_specific_bit,
            version,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Header used by AVTP stream-oriented packets.
///
/// It contains generic stream metadata plus several format-defined extension
/// fields (`format_specific_data*`) that are interpreted by subtype.
pub struct AvtpStreamHeader {
    /// The preceding common header. See [`AvtpCommonHeader`].
    ///
    /// `common.header_specific_bit` maps to `sv` (stream ID valid). See
    /// [`Self::stream_id_valid`].
    pub common: AvtpCommonHeader, // 12 bits

    /// Media-clock restart indicator (`mr`).
    ///
    /// Talkers toggle this bit when the underlying media clock source is reset
    /// or switches (for example, when changing live inputs). The toggle pattern
    /// makes restart events visible even if individual packets are lost.
    ///
    /// A practical receiver strategy is to treat a toggle as a resync hint and
    /// re-lock timing as quickly as possible.
    pub media_clock_restart: bool, // 1 bit

    /// Subtype-defined extension bits (`f_s_d`).
    ///
    /// Interpretation depends on [`AvtpCommonHeader::subtype`].
    pub format_specific_data: u8, // 2 bits

    /// Timestamp validity flag (`tv`) for [`Self::avtp_timestamp`].
    ///
    /// When `false`, receivers should treat [`Self::avtp_timestamp`] as undefined
    /// and avoid scheduling playback from it.
    pub avtp_timestamp_valid: bool, // 1 bit

    /// Per-stream packet sequence counter.
    ///
    /// Talkers increment this byte on each outgoing AVTPDU, wrapping from
    /// `0xFF` to `0x00`. Receivers can use gaps to detect packet loss and
    /// must tolerate arbitrary starting values when joining mid-stream.
    pub sequence_num: u8, // 8 bits

    /// Additional subtype-defined extension bits.
    pub format_specific_data_1: u8, // 7 bits

    /// Timestamp-uncertain flag (`tu`).
    ///
    /// Set by a talker when correlation to the current gPTP timeline may be
    /// temporarily unreliable (for example after a clock-step event). Receivers
    /// can use this signal to hold or smooth timeline recovery until timing
    /// stabilizes again.
    pub timestamp_uncertain: bool, // 1 bit

    /// Stable 64-bit identifier of the source stream.
    pub stream_id: StreamID, // 8 bytes

    /// Presentation timestamp in nanoseconds on a wrapping 32-bit timeline.
    ///
    /// This value is meaningful only when [`Self::avtp_timestamp_valid`] is
    /// `true`. It is derived from absolute gPTP time as:
    ///
    /// ```text
    /// (gptp_seconds * 1_000_000_000 + gptp_nanoseconds) mod 2^32
    /// ```
    ///
    /// Because the field is 32-bit, it wraps roughly every 4.29 seconds.
    pub avtp_timestamp: u32,

    /// Additional subtype-defined extension bytes.
    pub format_specific_data_2: [u8; 4], // 4 bytes

    /// The length (in bytes) of the [`Self::stream_data_payload`] field.
    pub stream_data_length: u16, // 2 bytes

    /// Additional subtype-defined extension bytes.
    pub format_specific_data_3: [u8; 2], // 2 bytes

    /// Format payload bytes.
    ///
    /// Layout and interpretation are defined by [`AvtpCommonHeader::subtype`].
    pub stream_data_payload: Arc<[u8]>,
}

impl AvtpStreamHeader {
    #[must_use]
    /// Returns the `sv` bit (stream ID valid).
    ///
    /// Stream-header based formats generally require a valid [`Self::stream_id`],
    /// so this is usually `true`.
    pub const fn stream_id_valid(&self) -> bool {
        self.common.header_specific_bit
    }

    /// Decode the Stream Header info from a reader that had already decoded the
    /// [`AvtpCommonHeader`] header.
    ///
    /// This is useful because the common header is generally read first, as it contains data based
    /// on which the type of header is decided.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be positioned immediately after decoding an [`AvtpCommonHeader`].
    ///   When decoding begins from a byte-aligned input, this places the reader at a 4-bit offset.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Stream`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// The reader will be in a byte-aligned position after decoding completes.
    pub(crate) fn decode_after_common<R: io::Read>(
        common: AvtpCommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        debug_assert_eq!(common.subtype.header_type(), HeaderType::Stream);

        // bit aligned
        let media_clock_restart = reader.read_bit()?;
        let format_specific_data = reader.read::<2, _>()?;
        let avtp_timestamp_valid = reader.read_bit()?;
        let sequence_num = reader.read::<8, _>()?;
        let format_specific_data_1 = reader.read::<7, _>()?;
        let timestamp_uncertain = reader.read_bit()?;

        // The reader is guaranteed to be byte aligned after this
        // (assuming it came in unaligned, from after reading the common header)
        debug_assert!(reader.byte_aligned());

        let stream_id = StreamID::decode(reader)?;
        let avtp_timestamp = reader.read_to()?;
        let format_specific_data_2 = reader.read_to()?;
        let stream_data_length = reader.read_to()?;
        let format_specific_data_3 = reader.read_to()?;
        let stream_data_payload = read_arc(reader, usize::from(stream_data_length))?;

        Ok(Self {
            common,
            media_clock_restart,
            format_specific_data,
            avtp_timestamp_valid,
            sequence_num,
            format_specific_data_1,
            timestamp_uncertain,
            stream_id,
            avtp_timestamp,
            format_specific_data_2,
            stream_data_length,
            format_specific_data_3,
            stream_data_payload,
        })
    }
}

impl BitEncode for AvtpStreamHeader {
    type Error = io::Error;

    /// Encode the Stream Header info.
    ///
    /// # Preconditions
    ///
    /// - The provided writer must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Stream`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// The writer will be in a byte-aligned position after encoding completes.
    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        debug_assert_eq!(self.common.subtype.header_type(), HeaderType::Stream);

        // We must start with a byte-aligned writer
        debug_assert!(writer.byte_aligned());

        // Bit aligned
        self.common.encode(writer)?;
        writer.write_bit(self.media_clock_restart)?;
        writer.write::<2, _>(self.format_specific_data)?;
        writer.write_bit(self.avtp_timestamp_valid)?;
        writer.write::<8, _>(self.sequence_num)?;
        writer.write::<7, _>(self.format_specific_data_1)?;
        writer.write_bit(self.timestamp_uncertain)?;

        // The rest of the writes are guaranteed to be byte aligned
        debug_assert!(writer.byte_aligned());

        self.stream_id.encode(writer)?;
        writer.write_from(self.avtp_timestamp)?;
        writer.write_from(self.format_specific_data_2)?;
        writer.write_from(self.stream_data_length)?;
        writer.write_from(self.format_specific_data_3)?;

        // TODO: Consider if this debug assert should be a runtime error.
        // This is hot path code though.
        debug_assert_eq!(usize::from(self.stream_data_length), self.stream_data_payload.len());
        writer.write_bytes(&self.stream_data_payload)?;

        Ok(())
    }
}

impl BitDecode for AvtpStreamHeader {
    type Error = IOWrapError<UnknownSubtype>;

    /// Decode the Stream Header.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Stream`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// The reader will be in a byte-aligned position after decoding completes.
    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        // We must start with a byte-aligned reader
        debug_assert!(reader.byte_aligned());

        let common = AvtpCommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;

        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        Ok(decoded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Header used by AVTP control-oriented packets.
pub struct AvtpControlHeader {
    /// The preceding common header. See [`AvtpCommonHeader`].
    ///
    /// `common.header_specific_bit` maps to `sv` (stream ID valid). See
    /// [`Self::stream_id_valid`].
    pub common: AvtpCommonHeader, // 12 bits

    /// Control-format extension bits.
    ///
    /// Meaning depends on [`AvtpCommonHeader::subtype`].
    pub format_specific_data: u16, // 9 bits

    /// Size of [`Self::control_data_payload`] in bytes.
    pub control_data_length: u16, // 11 bits

    /// Optional stream identifier associated with this control message.
    ///
    /// Only meaningful when [`Self::stream_id_valid`] returns `true`.
    pub stream_id: StreamID,

    /// Control payload bytes interpreted by the selected control subtype.
    pub control_data_payload: Arc<[u8]>,
}

impl AvtpControlHeader {
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
        common: AvtpCommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        debug_assert_eq!(common.subtype.header_type(), HeaderType::Control);

        // Bit aligned
        let format_specific_data = reader.read::<9, _>()?;
        let control_data_length = reader.read::<11, _>()?;

        // The reader is guaranteed to be byte aligned after this
        // (assuming it came in unaligned, from after reading the common header)
        debug_assert!(reader.byte_aligned());

        // Byte aligned
        let stream_id = StreamID::decode(reader)?;
        let control_data_payload = read_arc(reader, usize::from(control_data_length))?;

        Ok(Self {
            common,
            format_specific_data,
            control_data_length,
            stream_id,
            control_data_payload,
        })
    }
}

impl BitEncode for AvtpControlHeader {
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
        writer.write::<9, _>(self.format_specific_data)?;
        writer.write::<11, _>(self.control_data_length)?;

        // The rest of the writes are guaranteed to be byte aligned
        debug_assert!(writer.byte_aligned());

        self.stream_id.encode(writer)?;

        // TODO: Consider if this debug assert should be a runtime error.
        // This is hot path code though.
        debug_assert_eq!(usize::from(self.control_data_length), self.control_data_payload.len());
        writer.write_bytes(&self.control_data_payload)?;

        Ok(())
    }
}

impl BitDecode for AvtpControlHeader {
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

        let common = AvtpCommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;

        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        Ok(decoded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// This header is used by formats that do not conform to the usual control or stream common
/// headers.
///
/// It is designed to be very flexible and allow representing essentially any custom structures by
/// containing a dynamic payload, which are just prefixed by the common header.
pub struct AvtpAlternativeHeader {
    /// The preceding common header. See [`AvtpCommonHeader`].
    ///
    /// `common.header_specific_bit` meaning differs depending on the format that
    /// use the alternative header.
    pub common: AvtpCommonHeader, // 12 bits

    // The meaning of this field is format-specific.
    //
    // The actual implementation is defined by each format that uses the alternative header.
    pub alternative_data_payload: Arc<[u8]>,
}

impl AvtpAlternativeHeader {
    /// Decode the Alternative Header info from a reader that had already decoded the
    /// [`AvtpCommonHeader`] header.
    ///
    /// This is useful because the common header is generally read first, as it contains data based
    /// on which the type of header is decided.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be positioned immediately after decoding an [`AvtpCommonHeader`].
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
        common: AvtpCommonHeader,
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

impl BitEncode for AvtpAlternativeHeader {
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

impl BitDecode for AvtpAlternativeHeader {
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

        let common = AvtpCommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;

        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        Ok(decoded)
    }
}
