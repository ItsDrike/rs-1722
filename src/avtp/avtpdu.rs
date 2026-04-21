use std::io;

use bitstream_io::{BigEndian, BitReader, BitWriter};

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
