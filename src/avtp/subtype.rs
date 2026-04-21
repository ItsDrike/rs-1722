use std::io;

use bitstream_io::{BigEndian, BitRead, BitWrite, BitWriter};
use thiserror::Error;

use crate::{
    avtp::HeaderType,
    io::enc_dec::{BitDecode, BitEncode, IOWrapError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// High-level packetization behavior for AVTPDUs over UDP.
///
/// The style is derived from [`Subtype`] and is useful when deciding whether
/// packets are expected as a time-ordered stream (`Continuous`) or as discrete
/// control/data units (`Discrete`).
pub enum EncapsulationStyle {
    Continuous,
    Discrete,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("unknown subtype: {0}")]
pub struct UnknownSubtype(pub u8);

#[allow(non_camel_case_types)] // We want to stick to the spec names
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// AVTP subtype byte identifying the payload/control format.
///
/// This value drives header interpretation (stream/control/alternative) and
/// transport behavior via [`Self::header_type`] and [`Self::encapsulation_style`].
pub enum Subtype {
    /// IEC 61883/IIDC format
    IEC_61883_IIDC = 0x00,

    /// MMA streams
    MMA_STREAM = 0x01,

    /// AVTP Audio Format
    AAF = 0x02,

    /// Compressed Video Format
    CVF = 0x03,

    /// Clock Reference Format
    CRF = 0x04,

    /// Time-Synchronous Control Format
    TSCF = 0x05,

    /// SDI Video Format
    SVF = 0x06,

    /// Raw Video Format
    RVF = 0x07,

    // Range 0x08 - 0x6D is reserved
    //
    /// AES Encrypted Format Continuous
    AEF_CONTINUOUS = 0x6E,

    /// Vendor Specific Format Stream
    VSF_STREAM = 0x6F,

    // Range 0x70 - 0x7E is reserved
    //
    /// Experimental Format Stream
    EF_STREAM = 0x7F,

    // Range 0x80 - 0x81 is reserved
    //
    /// Non-Time-Synchronous Control Format
    NTSCF = 0x82,

    // Range 0x83 - 0xED is reserved
    //
    /// ECC Signed Control Format
    ESCF = 0xEC,

    /// ECC Encrypted Control Format
    EECF = 0xED,

    /// AES Encrypted Format Discrete
    AEF_DISCRETE = 0xEE,

    // Range 0xEF - 0xF9 is reserved
    //
    /// AVDECC Discovery Protocol
    ADP = 0xFA,

    /// AVDECC Enumeration and Control Protocol
    AECP = 0xFB,

    /// AVDECC Connection Management Protocol
    ACMP = 0xFC,

    // Reserved 0xFD
    //
    /// MAAP Protocol
    MAAP = 0xFD,

    /// Experimental Format Control
    EF_CONTROL = 0xFF,
}

impl Subtype {
    #[must_use]
    pub const fn header_type(&self) -> HeaderType {
        #[expect(clippy::match_same_arms)]
        match self {
            Self::IEC_61883_IIDC => HeaderType::Stream,
            Self::MMA_STREAM => HeaderType::Stream,
            Self::AAF => HeaderType::Stream,
            Self::CVF => HeaderType::Stream,
            Self::CRF => HeaderType::Alternative,
            Self::TSCF => HeaderType::Stream,
            Self::SVF => HeaderType::Stream,
            Self::RVF => HeaderType::Stream,
            Self::AEF_CONTINUOUS => HeaderType::Alternative,
            Self::VSF_STREAM => HeaderType::Stream,
            Self::EF_STREAM => HeaderType::Stream,
            Self::NTSCF => HeaderType::Alternative,
            Self::ESCF => HeaderType::Alternative,
            Self::EECF => HeaderType::Alternative,
            Self::AEF_DISCRETE => HeaderType::Alternative,
            Self::ADP => HeaderType::Control,
            Self::AECP => HeaderType::Control,
            Self::ACMP => HeaderType::Control,
            Self::MAAP => HeaderType::Control,
            Self::EF_CONTROL => HeaderType::Control,
        }
    }

    #[must_use]
    pub const fn encapsulation_style(&self) -> EncapsulationStyle {
        #[expect(clippy::match_same_arms)]
        match self {
            Self::IEC_61883_IIDC => EncapsulationStyle::Continuous,
            Self::MMA_STREAM => EncapsulationStyle::Continuous,
            Self::AAF => EncapsulationStyle::Continuous,
            Self::CVF => EncapsulationStyle::Continuous,
            Self::CRF => EncapsulationStyle::Continuous,
            Self::TSCF => EncapsulationStyle::Continuous,
            Self::SVF => EncapsulationStyle::Continuous,
            Self::RVF => EncapsulationStyle::Continuous,
            Self::AEF_CONTINUOUS => EncapsulationStyle::Continuous,
            Self::VSF_STREAM => EncapsulationStyle::Continuous,
            Self::EF_STREAM => EncapsulationStyle::Continuous,
            Self::NTSCF => EncapsulationStyle::Discrete,
            Self::ESCF => EncapsulationStyle::Discrete,
            Self::EECF => EncapsulationStyle::Discrete,
            Self::AEF_DISCRETE => EncapsulationStyle::Discrete,
            Self::ADP => EncapsulationStyle::Discrete,
            Self::AECP => EncapsulationStyle::Discrete,
            Self::ACMP => EncapsulationStyle::Discrete,
            Self::MAAP => EncapsulationStyle::Discrete,
            Self::EF_CONTROL => EncapsulationStyle::Discrete,
        }
    }
}

impl std::fmt::Display for Subtype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} (0x{:02X})", self, *self as u8)
    }
}

impl TryFrom<u8> for Subtype {
    type Error = UnknownSubtype;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        let subtype = match value {
            0x00 => Self::IEC_61883_IIDC,
            0x01 => Self::MMA_STREAM,
            0x02 => Self::AAF,
            0x03 => Self::CVF,
            0x04 => Self::CRF,
            0x05 => Self::TSCF,
            0x06 => Self::SVF,
            0x07 => Self::RVF,

            0x6E => Self::AEF_CONTINUOUS,
            0x6F => Self::VSF_STREAM,
            0x7F => Self::EF_STREAM,

            0x82 => Self::NTSCF,

            0xEC => Self::ESCF,
            0xED => Self::EECF,
            0xEE => Self::AEF_DISCRETE,

            0xFA => Self::ADP,
            0xFB => Self::AECP,
            0xFC => Self::ACMP,
            0xFD => Self::MAAP,
            0xFF => Self::EF_CONTROL,

            _ => return Err(UnknownSubtype(value)),
        };

        Ok(subtype)
    }
}

impl BitEncode for Subtype {
    type Error = io::Error;

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        writer.write_from(*self as u8)?;
        Ok(())
    }
}

impl BitDecode for Subtype {
    type Error = IOWrapError<UnknownSubtype>;

    fn decode<R: io::Read>(reader: &mut bitstream_io::BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let raw = reader.read_to::<u8>()?;
        Self::try_from(raw).map_err(Self::Error::Specific)
    }
}
