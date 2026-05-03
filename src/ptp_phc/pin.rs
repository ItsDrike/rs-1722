use crate::ptp_phc::{
    abi::{PtpPinDesc, PtpPinFunction},
    error::{Error, Result},
};

/// PTP pin functions exposed by the Linux PTP subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinFunction {
    /// No function assigned.
    None,

    /// External timestamp input.
    ExternalTimestamp,

    /// Periodic output.
    PeriodicOutput,

    /// Physical synchronization pin.
    PhysicalSync,

    /// Unknown function number assigned
    Unknown(u32),
}

impl PinFunction {
    pub(crate) const fn to_abi(self) -> Result<PtpPinFunction> {
        match self {
            Self::None => Ok(PtpPinFunction::PTP_PF_NONE),
            Self::ExternalTimestamp => Ok(PtpPinFunction::PTP_PF_EXTTS),
            Self::PeriodicOutput => Ok(PtpPinFunction::PTP_PF_PEROUT),
            Self::PhysicalSync => Ok(PtpPinFunction::PTP_PF_PHYSYNC),
            Self::Unknown(raw) => Err(Error::UnsupportedPinFunction(raw)),
        }
    }

    pub(crate) const fn from_abi(raw: PtpPinFunction) -> Self {
        match raw {
            PtpPinFunction::PTP_PF_NONE => Self::None,
            PtpPinFunction::PTP_PF_EXTTS => Self::ExternalTimestamp,
            PtpPinFunction::PTP_PF_PEROUT => Self::PeriodicOutput,
            PtpPinFunction::PTP_PF_PHYSYNC => Self::PhysicalSync,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub index: u32,
    pub function: PinFunction,
    pub channel: u32,
}

impl Pin {
    pub(crate) fn from_desc(desc: PtpPinDesc) -> Self {
        Self {
            name: pin_name(&desc),
            index: desc.index,
            function: PinFunction::from_abi(desc.func),
            channel: desc.chan,
        }
    }
}

fn pin_name(desc: &PtpPinDesc) -> String {
    let bytes: Vec<u8> = desc
        .name
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| u8::from_ne_bytes(byte.to_ne_bytes()))
        .collect();

    String::from_utf8_lossy(&bytes).into_owned()
}
