use crate::ptp_phc::abi::{PtpPinDesc, PtpPinFunction};

/// PTP pin functions exposed by the Linux PTP subsystem.
///
/// These values describe how a programmable PHC pin is currently routed.
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

    /// Unknown function number assigned by the driver.
    ///
    /// This can be observed when reading a pin configuration from hardware that
    /// exposes a kernel value not currently modeled by this crate.
    Unknown(u32),
}

impl PinFunction {
    pub(crate) const fn to_abi(self) -> PtpPinFunction {
        match self {
            Self::None => PtpPinFunction::PTP_PF_NONE,
            Self::ExternalTimestamp => PtpPinFunction::PTP_PF_EXTTS,
            Self::PeriodicOutput => PtpPinFunction::PTP_PF_PEROUT,
            Self::PhysicalSync => PtpPinFunction::PTP_PF_PHYSYNC,
            Self::Unknown(raw) => PtpPinFunction(raw),
        }
    }

    pub(crate) const fn from_abi(raw: PtpPinFunction) -> Self {
        match raw {
            PtpPinFunction::PTP_PF_NONE => Self::None,
            PtpPinFunction::PTP_PF_EXTTS => Self::ExternalTimestamp,
            PtpPinFunction::PTP_PF_PEROUT => Self::PeriodicOutput,
            PtpPinFunction::PTP_PF_PHYSYNC => Self::PhysicalSync,
            PtpPinFunction(other) => Self::Unknown(other),
        }
    }
}

/// Description of one programmable PHC pin.
///
/// A pin maps one physical hardware pin to one PTP-related hardware function,
/// optionally selecting which channel of that function is routed there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// Human-readable pin name reported by the kernel or driver.
    pub name: String,

    /// Physical pin index on the PHC.
    pub index: u32,

    /// Function currently assigned to the pin.
    pub function: PinFunction,

    /// Channel number used by the assigned function.
    ///
    /// For example, when [`Self::function`] is [`PinFunction::PeriodicOutput`],
    /// this selects which periodic-output channel drives the pin.
    pub channel: u32,
}

impl Pin {
    pub(crate) fn from_desc(desc: PtpPinDesc) -> Self {
        Self {
            name: desc.pin_name(),
            index: desc.index,
            function: PinFunction::from_abi(desc.func),
            channel: desc.chan,
        }
    }
}
