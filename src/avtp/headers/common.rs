use std::io;

use arbitrary_int::prelude::*;
use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};

use crate::{
    avtp::subtype::{Subtype, UnknownSubtype},
    io::enc_dec::{BitDecode, BitEncode, IOWrapError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Common prefix shared by AVTP stream and control packets.
///
/// This compact header is always 12 bits long and provides the subtype, a
/// context-dependent flag bit, and protocol version bits.
pub struct CommonHeader {
    /// Format identifier for the packet payload and header interpretation.
    ///
    /// See [`Subtype`] for the full list of supported values.
    pub subtype: Subtype, // 8 bits

    /// A single flag whose meaning depends on the selected header family.
    ///
    /// For stream and control headers this bit is exposed as `sv` via
    /// [`StreamHeader::stream_id_valid`] and [`ControlHeader::stream_id_valid`].
    pub header_specific_bit: bool, // 1 bit

    /// Protocol version bits for this header layout.
    ///
    /// Most formats currently use `0`. Receivers should verify this value and
    /// reject packets that advertise an unsupported version.
    pub version: u3, // 3 bits
}

impl BitEncode for CommonHeader {
    type Error = io::Error;

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        self.subtype.encode(writer)?;
        writer.write_bit(self.header_specific_bit)?;
        writer.write::<3, _>(self.version.value())?;

        Ok(())
    }
}

impl BitDecode for CommonHeader {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let subtype = Subtype::decode(reader)?;

        let header_specific_bit = reader.read_bit()?;

        let version = u3::new(reader.read::<3, u8>()?);

        Ok(Self {
            subtype,
            header_specific_bit,
            version,
        })
    }
}
