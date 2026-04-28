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
/// Transport-level metadata shared by all AVTP stream-oriented packets.
///
/// This structure contains fields that are interpreted uniformly across all
/// AVTP stream subtypes, independent of the specific media format carried in
/// the payload. These include timing, sequencing, and stream identification
/// information required for synchronization and loss detection.
///
/// The remaining subtype-defined fields and payload are stored separately in
/// [`SpecificStreamData`].
pub struct GenericStreamData {
    /// The preceding common header. See [`CommonHeader`].
    ///
    /// [`CommonHeader::header_specific_bit`] maps to [`Self::stream_id_valid`].
    pub common: CommonHeader, // 12 bits

    /// Media-clock restart indicator (`mr`).
    ///
    /// Talkers toggle this bit when the underlying media clock source is reset
    /// or switches (for example, when changing live inputs). The toggle pattern
    /// makes restart events visible even if individual packets are lost.
    ///
    /// A practical receiver strategy is to treat a toggle as a resync hint and
    /// re-lock timing as quickly as possible.
    pub media_clock_restart: bool,

    /// Timestamp validity flag (`tv`) for [`Self::avtp_timestamp`].
    ///
    /// When `false`, receivers should treat [`Self::avtp_timestamp`] as undefined
    /// and avoid scheduling playback from it.
    pub avtp_timestamp_valid: bool,

    /// Per-stream packet sequence counter.
    ///
    /// Talkers increment this byte on each outgoing AVTPDU, wrapping from
    /// `0xFF` to `0x00`. Receivers can use gaps to detect packet loss and
    /// must tolerate arbitrary starting values when joining mid-stream.
    pub sequence_num: u8,

    /// Timestamp-uncertain flag (`tu`).
    ///
    /// Set by a talker when correlation to the current gPTP timeline may be
    /// temporarily unreliable (for example after a clock-step event). Receivers
    /// can use this signal to hold or smooth timeline recovery until timing
    /// stabilizes again.
    pub timestamp_uncertain: bool,

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

    /// The length (in bytes) of the [`SpecificStreamData::stream_data_payload`] field.
    pub stream_data_length: u16,
}

impl GenericStreamData {
    #[must_use]
    /// Returns the `sv` bit (stream ID valid).
    ///
    /// Stream-header based formats generally require a valid [`Self::stream_id`],
    /// so this is usually `true`.
    pub const fn stream_id_valid(&self) -> bool {
        self.common.header_specific_bit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Subtype-defined fields and payload for AVTP stream-oriented packets.
///
/// This structure contains all fields whose meaning depends on the selected
/// [`crate::avtp::subtype::Subtype`]. These include compact extension fields as
/// well as the actual payload bytes carried by the stream.
///
/// The AVTP transport layer does not interpret these fields; instead, they are
/// parsed and handled by subtype-specific logic (e.g. AAF, CVF).
pub struct SpecificStreamData {
    /// Subtype-defined extension bits (`f_s_d`).
    ///
    /// These bits are interpreted according to
    /// [`crate::avtp::subtype::Subtype`] and may encode small control flags or
    /// mode indicators required by the selected stream format. Their meaning is
    /// not defined at the AVTP transport layer.
    pub format_specific_data: u2,

    /// Additional subtype-defined extension bits.
    ///
    /// Like [`Self::format_specific_data`], these bits are interpreted entirely
    /// by the selected [`crate::avtp::subtype::Subtype`]. They are commonly
    /// used to extend control flags or encode compact format-specific
    /// parameters.
    pub format_specific_data_1: u7,

    /// Additional subtype-defined extension bytes.
    ///
    /// These bytes typically contain format identifiers, configuration values,
    /// or packed fields required to interpret [`Self::stream_data_payload`].
    /// The exact layout is defined by the selected
    /// [`crate::avtp::subtype::Subtype`].
    pub format_specific_data_2: [u8; 4],

    /// Additional subtype-defined extension bytes.
    ///
    /// These bytes often contain bit-packed fields extending the information
    /// provided in [`Self::format_specific_data_2`]. Interpretation is entirely
    /// subtype-specific.
    pub format_specific_data_3: [u8; 2],

    /// Format payload bytes.
    ///
    /// This field contains the actual media or control data carried by the
    /// stream. Its structure, alignment, and semantics are fully defined by the
    /// selected [`crate::avtp::subtype::Subtype`] and any associated format
    /// specification.
    ///
    /// The number of bytes in this field must match the value of
    /// [`GenericStreamData::stream_data_length`].
    pub stream_data_payload: Arc<[u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Header used by AVTP stream-oriented packets.
///
/// It contains generic stream metadata plus several format-defined extension
/// fields that are interpreted by subtype.
pub struct StreamHeader {
    /// Transport-level metadata shared across all AVTP stream subtypes.
    ///
    /// This includes timing, sequencing, and stream identification fields that
    /// are interpreted consistently regardless of the selected format.
    pub generic: GenericStreamData,

    /// Subtype-defined fields and payload.
    ///
    /// The contents of this structure are interpreted according to
    /// [`crate::avtp::subtype::Subtype`] and may vary significantly between
    /// formats (e.g. audio, video, or other media types).
    pub specific: SpecificStreamData,
}

impl StreamHeader {
    /// Decode the Stream Header info from a reader that had already decoded the
    /// [`CommonHeader`] header.
    ///
    /// This is useful because the common header is generally read first, as it contains data based
    /// on which the type of header is decided.
    ///
    /// # Preconditions
    ///
    /// - The provided reader must be positioned immediately after decoding an [`CommonHeader`].
    ///   When decoding begins from a byte-aligned input, this places the reader at a 4-bit offset.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Stream`]
    ///   subtype.
    ///
    /// # Postconditions
    ///
    /// The reader will be in a byte-aligned position after decoding completes.
    pub(crate) fn decode_after_common<R: io::Read>(
        common: CommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        debug_assert_eq!(common.subtype.header_type(), HeaderType::Stream);

        // bit aligned
        let media_clock_restart = reader.read_bit()?;
        let format_specific_data = u2::new(reader.read::<2, u8>()?);
        let avtp_timestamp_valid = reader.read_bit()?;
        let sequence_num = reader.read::<8, _>()?;
        let format_specific_data_1 = u7::new(reader.read::<7, u8>()?);
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
            generic: GenericStreamData {
                common,
                media_clock_restart,
                avtp_timestamp_valid,
                sequence_num,
                timestamp_uncertain,
                stream_id,
                avtp_timestamp,
                stream_data_length,
            },
            specific: SpecificStreamData {
                format_specific_data,
                format_specific_data_1,
                format_specific_data_2,
                format_specific_data_3,
                stream_data_payload,
            },
        })
    }
}

impl BitEncode for StreamHeader {
    type Error = io::Error;

    /// Encode the Stream Header info.
    ///
    /// # Preconditions
    ///
    /// - The provided writer must be byte-aligned.
    /// - The [common header subtype][`Subtype::header_type`] must be a [`HeaderType::Stream`]
    ///   subtype.
    /// - [`GenericStreamData::stream_data_length`] must equal to
    ///   [`SpecificStreamData::stream_data_payload`]'s length.
    ///
    /// # Postconditions
    ///
    /// The writer will be in a byte-aligned position after encoding completes.
    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        debug_assert_eq!(self.generic.common.subtype.header_type(), HeaderType::Stream);

        // We must start with a byte-aligned writer
        debug_assert!(writer.byte_aligned());

        // Bit aligned
        self.generic.common.encode(writer)?;
        writer.write_bit(self.generic.media_clock_restart)?;
        writer.write::<2, _>(self.specific.format_specific_data.value())?;
        writer.write_bit(self.generic.avtp_timestamp_valid)?;
        writer.write::<8, _>(self.generic.sequence_num)?;
        writer.write::<7, _>(self.specific.format_specific_data_1.value())?;
        writer.write_bit(self.generic.timestamp_uncertain)?;

        // The rest of the writes are guaranteed to be byte aligned
        debug_assert!(writer.byte_aligned());

        self.generic.stream_id.encode(writer)?;
        writer.write_from(self.generic.avtp_timestamp)?;
        writer.write_from(self.specific.format_specific_data_2)?;
        writer.write_from(self.generic.stream_data_length)?;
        writer.write_from(self.specific.format_specific_data_3)?;

        // TODO: Consider if this debug assert should be a runtime error.
        // This is hot path code though. Also, it would mean needing a custom
        // err type for this.
        debug_assert_eq!(
            usize::from(self.generic.stream_data_length),
            self.specific.stream_data_payload.len()
        );
        writer.write_bytes(&self.specific.stream_data_payload)?;

        Ok(())
    }
}

impl BitDecode for StreamHeader {
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

        let common = CommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;

        // The reader is guaranteed to be byte aligned at the end
        debug_assert!(reader.byte_aligned());

        Ok(decoded)
    }
}
