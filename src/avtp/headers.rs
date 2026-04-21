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

    pub(crate) fn decode_after_common<R: io::Read>(
        common: AvtpCommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        // bit aligned
        let media_clock_restart = reader.read_bit()?;
        let format_specific_data = reader.read::<2, _>()?;
        let avtp_timestamp_valid = reader.read_bit()?;
        let sequence_num = reader.read::<8, _>()?;
        let format_specific_data_1 = reader.read::<7, _>()?;
        let timestamp_uncertain = reader.read_bit()?;

        // The rest is byte aligned
        assert!(reader.byte_aligned());

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

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        // Bit aligned
        self.common.encode(writer)?;
        writer.write_bit(self.media_clock_restart)?;
        writer.write::<2, _>(self.format_specific_data)?;
        writer.write_bit(self.avtp_timestamp_valid)?;
        writer.write::<8, _>(self.sequence_num)?;
        writer.write::<7, _>(self.format_specific_data_1)?;
        writer.write_bit(self.timestamp_uncertain)?;

        // The rest is byte aligned
        assert!(writer.byte_aligned());

        self.stream_id.encode(writer)?;
        writer.write_from(self.avtp_timestamp)?;
        writer.write_from(self.format_specific_data_2)?;
        writer.write_from(self.stream_data_length)?;
        writer.write_from(self.format_specific_data_3)?;

        // TODO: Consider if this debug assert should be a runtime error
        // This is hot path code though
        assert_eq!(usize::from(self.stream_data_length), self.stream_data_payload.len());
        writer.write_bytes(&self.stream_data_payload)?;

        Ok(())
    }
}

impl BitDecode for AvtpStreamHeader {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let common = AvtpCommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;
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

    pub(crate) fn decode_after_common<R: io::Read>(
        common: AvtpCommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        // Bit aligned
        let format_specific_data = reader.read::<9, _>()?;
        let control_data_length = reader.read::<11, _>()?;

        // The rest is byte aligned
        assert!(reader.byte_aligned());

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

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        // Bit aligned
        self.common.encode(writer)?;
        writer.write::<9, _>(self.format_specific_data)?;
        writer.write::<11, _>(self.control_data_length)?;

        // The rest is byte aligned
        assert!(writer.byte_aligned());

        self.stream_id.encode(writer)?;

        // TODO: Consider if this debug assert should be a runtime error
        // This is hot path code though
        assert_eq!(usize::from(self.control_data_length), self.control_data_payload.len());
        writer.write_bytes(&self.control_data_payload)?;

        Ok(())
    }
}

impl BitDecode for AvtpControlHeader {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        // Bit aligned
        let common = AvtpCommonHeader::decode(reader)?;
        let decoded = Self::decode_after_common(common, reader)?;
        Ok(decoded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
// TODO: This structure is not yet supported
pub struct AvtpAlternativeHeader {
    /// The preceding common header. See [`AvtpCommonHeader`].
    ///
    /// `common.header_specific_bit` maps to `sv` (stream ID valid). See
    /// [`Self::stream_id_valid`].
    pub common: AvtpCommonHeader, // 12 bits
}

impl AvtpAlternativeHeader {
    pub(crate) fn decode_after_common<R: io::Read>(
        common: AvtpCommonHeader,
        reader: &mut BitReader<R, BigEndian>,
    ) -> io::Result<Self> {
        todo!()
    }
}

impl BitEncode for AvtpAlternativeHeader {
    type Error = io::Error;

    fn encode<W: io::Write>(&self, _writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        todo!();
    }
}

impl BitDecode for AvtpAlternativeHeader {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(_reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        todo!()
    }
}
