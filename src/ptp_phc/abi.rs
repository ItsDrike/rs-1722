//! This file contains the matching definitions of the PTP structures and ioctl interactions exposed
//! by the Linux kernel. For the exact reference, see: `<linux/ptp_clock.h>`. You can find this in:
//! <https://github.com/torvalds/linux/blob/master/include/uapi/linux/ptp_clock.h>

use nix::{ioctl_read, ioctl_readwrite, ioctl_write_ptr};

/// Linux PTP ioctl magic value.
///
/// This matches `PTP_CLK_MAGIC` from `<linux/ptp_clock.h>`.
const PTP_CLK_MAGIC: u8 = b'=';

/// Kernel ABI type matching `struct ptp_clock_time`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PtpClockTime {
    pub sec: i64,
    pub nsec: u32,
    pub reserved: u32,
}

/// Kernel ABI type matching `struct ptp_clock_caps`.
///
/// Describes the capabilities exposed by one PHC device.
///
/// The values reported here determine which other PTP clock operations are
/// valid for this device. For example, [`Self::n_ext_ts`] tells how many
/// external timestamp channels may be configured with
/// [`ptp_extts_request_ioctl`], [`Self::n_per_out`] tells how many periodic
/// output channels may be configured with [`ptp_perout_request_ioctl`], and
/// [`Self::n_pins`] tells how many programmable pins may be queried or
/// configured with [`ptp_pin_getfunc_ioctl`] / [`ptp_pin_setfunc_ioctl`].
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PtpClockCaps {
    /// Maximum frequency adjustment supported by the PHC, in parts per billion.
    ///
    /// This is the largest positive or negative frequency correction that can
    /// be applied to the clock.
    pub max_adj: i32,

    /// Number of programmable alarm channels.
    ///
    /// Alarm support is device-dependent and many PHCs report zero here.
    pub n_alarm: i32,

    /// Number of external timestamp capture channels.
    ///
    /// Valid external timestamp channel indices are in the range
    /// `0..Self::n_ext_ts`.
    ///
    /// These channels can be configured with [`ptp_extts_request_ioctl`]. If
    /// programmable pins are supported, a physical pin can be routed to one of
    /// these channels by setting [`PtpPinDesc::func`] to
    /// [`PtpPinFunction::PTP_PF_EXTTS`] and [`PtpPinDesc::chan`] to the desired
    /// channel.
    pub n_ext_ts: i32,

    /// Number of periodic output channels.
    ///
    /// Valid periodic output channel indices are in the range
    /// `0..Self::n_per_out`.
    ///
    /// These channels can be configured with [`ptp_perout_request_ioctl`]. If
    /// programmable pins are supported, a physical pin can be routed to one of
    /// these channels by setting [`PtpPinDesc::func`] to
    /// [`PtpPinFunction::PTP_PF_PEROUT`] and [`PtpPinDesc::chan`] to the desired
    /// channel.
    pub n_per_out: i32,

    /// Whether PPS support is available.
    ///
    /// A nonzero value indicates that the PHC supports the generic PPS enable
    /// operation. This is separate from routing a periodic output signal to a
    /// programmable physical pin.
    pub pps: i32,

    /// Number of programmable physical pins.
    ///
    /// Valid pin indices are in the range `0..Self::n_pins`.
    ///
    /// These pins can be queried and configured with [`ptp_pin_getfunc_ioctl`]
    /// and [`ptp_pin_setfunc_ioctl`].
    pub n_pins: i32,

    /// Whether precise system/PHC cross timestamping is supported.
    ///
    /// A nonzero value indicates that the device can provide a precise
    /// timestamp pair relating the PHC to a system clock.
    pub cross_timestamping: i32,

    /// Whether phase adjustment is supported.
    ///
    /// A nonzero value indicates that the PHC supports phase adjustment through
    /// the clock adjustment API.
    pub adjust_phase: i32,

    /// Maximum supported phase adjustment, in nanoseconds.
    ///
    /// Meaningful only when [`Self::adjust_phase`] is nonzero.
    pub max_phase_adj: i32,

    /// Reserved for future use.
    ///
    /// Keep this zero-initialized when passing the structure to the kernel.
    pub rsv: [i32; 11],
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PtpExttsFlags: u32 {
        /// Enables a PTP feature, used by `PTP_EXTTS_REQUEST`.
        const ENABLE_FEATURE = 1 << 0;

        /// Timestamp rising edges for external timestamp input.
        const RISING_EDGE = 1 << 1;

        /// Timestamp falling edges for external timestamp input.
        const FALLING_EDGE = 1 << 2;
    }
}

/// Kernel ABI type matching `struct ptp_extts_request`.
///
/// Configures one external timestamp capture channel of a PHC.
///
/// External timestamp capture is used to timestamp electrical edges arriving
/// on a physical input pin. The physical pin itself is selected separately with
/// [`PtpPinDesc`], by assigning that pin to [`PtpPinFunction::PTP_PF_EXTTS`]
/// and choosing the same channel in [`PtpPinDesc::chan`].
///
/// Once enabled, timestamp events can be read from the corresponding `/dev/ptpX`
/// device as [`PtpExttsEvent`] values.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PtpExttsRequest {
    /// External timestamp channel to configure.
    ///
    /// Valid values are in the range `0..PtpClockCaps::n_ext_ts`.
    ///
    /// This corresponds to [`PtpPinDesc::chan`] for a pin assigned
    /// [`PtpPinFunction::PTP_PF_EXTTS`].
    pub index: u32,

    /// External timestamp request flags.
    ///
    /// This controls whether timestamp capture is enabled and which signal
    /// edges should be captured, such as rising edges, falling edges, or both.
    ///
    /// Set this to `0` to disable timestamp capture for [`Self::index`].
    pub flags: PtpExttsFlags,

    /// Reserved for future use.
    ///
    /// Keep this zero-initialized when passing the structure to the kernel.
    pub rsv: [u32; 2],
}

/// Kernel ABI type matching `struct ptp_extts_event`.
///
/// Event returned by reading from a `/dev/ptpX` device after external timestamp
/// capture has been enabled with [`ptp_extts_request_ioctl`].
///
/// Each event represents one captured edge on an external timestamp channel.
/// The timestamp is taken by the PHC hardware at the time the edge is observed.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PtpExttsEvent {
    /// PHC timestamp of the captured external event.
    ///
    /// This is the hardware clock time at which the configured edge was
    /// observed.
    pub t: PtpClockTime,

    /// External timestamp channel that produced this event.
    ///
    /// This corresponds to [`PtpExttsRequest::index`] and to
    /// [`PtpPinDesc::chan`] for the pin routed to
    /// [`PtpPinFunction::PTP_PF_EXTTS`].
    pub index: u32,

    /// Event flags reported by the kernel.
    ///
    /// These describe properties of the captured event, such as which edge was
    /// observed when the driver reports that information.
    pub flags: PtpExttsFlags,

    /// Reserved for future use.
    pub rsv: [u32; 2],
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct PtpPeroutFlags: u32 {
        /// Generate a single output event instead of a repeating periodic signal.
        ///
        /// When this flag is set, the request describes a one-shot output rather than
        /// a continuously repeating periodic output. Support is driver- and
        /// hardware-dependent.
        const ONE_SHOT = 1 << 0;

        /// Interpret [`PtpPeroutRequest::on_or_rsv`] as the output's active time.
        ///
        /// When this flag is set, [`PtpPeroutRequest::on_or_rsv`] specifies how
        /// long the signal should remain active during each period. The active time
        /// must be smaller than [`PtpPeroutRequest::period`].
        ///
        /// If this flag is not set, the field is reserved and should be zeroed. The
        /// waveform shape or duty cycle is then determined by the driver/hardware.
        const DUTY_CYCLE = 1 << 1;

        /// Periodic output flag: interpret the first time field as a phase.
        ///
        /// Not all drivers support this. The Intel I210 through `igb` often supports plain periodic output,
        /// but may reject newer optional flags.
        const PHASE = 1 << 2;
    }
}

/// Kernel ABI type matching `struct ptp_perout_request`.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PtpPeroutRequest {
    /// Absolute start time or phase offset.
    ///
    /// If [`PtpPeroutFlags::PHASE`] is not set in [`Self::flags`], this field is interpreted as the
    /// absolute PHC time at which the periodic output should start.
    ///
    /// If [`PtpPeroutFlags::DUTY_CYCLE`] is set in [`Self::flags`], this field is interpreted as a
    /// phase offset within the period. In that mode, the output starts as soon as possible at an
    /// implementation-chosen integer multiple of [`Self::period`] plus this offset.
    pub start_or_phase: PtpClockTime,

    /// Desired periodic-output period.
    ///
    /// A zero value disables the selected periodic-output channel.
    pub period: PtpClockTime,

    /// Periodic-output channel to configure.
    ///
    /// This corresponds to the channel selected by [`PtpPinDesc::chan`] when a pin is assigned
    /// [`PtpPinFunction::PTP_PF_PEROUT`].
    pub index: u32,

    /// Periodic-output request flags.
    pub flags: PtpPeroutFlags,

    /// Signal on-time or reserved storage.
    ///
    /// If [`PTP_PEROUT_DUTY_CYCLE`] is set in [`Self::flags`], this field is interpreted as the
    /// signal's on-time and must be smaller than [`Self::period`].
    ///
    /// If [`PTP_PEROUT_DUTY_CYCLE`] is not set, this field is reserved and should be zero.
    pub on_or_rsv: PtpClockTime,
}

/// Kernel ABI type matching `enum ptp_pin_function`.
///
/// This enum describes which PTP clock function is routed to a programmable hardware pin.
///
/// It is used with [`PtpPinDesc`] and the [`ptp_pin_getfunc_ioctl`] / [`ptp_pin_setfunc_ioctl`]
/// ioctls.
#[repr(u32)]
#[derive(Debug, Clone, Copy, Default)]
#[expect(non_camel_case_types)] // Keep Linux UAPI names verbatim.
pub enum PtpPinFunction {
    /// No PTP function is assigned to the pin.
    ///
    /// The pin is not routed to an external timestamp input, periodic output,
    /// or other PTP-specific hardware function.
    #[default]
    PTP_PF_NONE = 0,

    /// External timestamp input.
    ///
    /// The pin is used as an input. Edges on the pin can be timestamped by the
    /// PHC and reported through [`ptp_extts_request_ioctl`].
    PTP_PF_EXTTS = 1,

    /// Periodic output.
    ///
    /// The pin is used as an output driven by the PHC. This is used for hardware-generated periodic
    /// signals configured through [`PtpPeroutRequest`], such as a 1 Hz or 1 kHz output.
    PTP_PF_PEROUT = 2,

    /// Physical synchronization pin.
    ///
    /// Device-specific synchronization function. Unlike [`Self::PTP_PF_EXTTS`] and
    /// [`Self::PTP_PF_PEROUT`], this does not correspond to the generic external timestamp or
    /// periodic-output APIs, and support depends on the specific PHC driver and hardware.
    PTP_PF_PHYSYNC = 3,
}

/// Kernel ABI type matching `struct ptp_pin_desc`.
///
/// Describes how one programmable PHC pin is connected to one of the
/// hardware time I/O blocks exposed by the device.
///
/// A PHC can expose multiple physical pins and multiple instances of a given
/// time I/O function. For example, a device may have several physical pins,
/// two external timestamp capture channels, and two periodic-output generator
/// channels. In that model:
///
/// - [`Self::index`] selects the physical pin.
/// - [`Self::func`] selects what kind of time I/O block is connected to it.
/// - [`Self::chan`] selects which instance of that block is used.
///
/// For example, `index = 1`, `func = PtpPinFunction::PTP_PF_PEROUT`, and
/// `chan = 0` means that physical pin 1 is routed to periodic-output channel 0.
/// A later [`ptp_perout_request_ioctl`] using channel 0 then controls the signal
/// driven on that pin.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PtpPinDesc {
    /// Hardware specific human readable pin name.
    ///
    /// This field is set by the kernel during [`ptp_pin_getfunc_ioctl`] and is
    /// ignored for [`ptp_pin_setfunc_ioctl`].
    ///
    /// Stored as a NUL-terminated C string from the driver. For example, `SDP0`.
    pub name: [nix::libc::c_char; 64],

    /// Physical programmable pin index.
    ///
    /// Valid values are in the range 0 to [`PtpClockCaps::n_pins`].
    ///
    /// This selects the physical pin being queried or configured, such as one
    /// of the SDP pins on hardware that exposes them.
    pub index: u32,

    /// PTP function assigned to this pin.
    ///
    /// This selects the kind of hardware time I/O block routed to the pin, such
    /// as external timestamp capture or periodic output generation.
    ///
    /// See [`PtpPinFunction`] for the supported function values.
    pub func: PtpPinFunction,

    /// Channel instance used by [`Self::func`].
    ///
    /// Some PHCs expose multiple independent instances of the same function,
    /// such as external timestamp channel 0 and 1, or periodic-output channel 0
    /// and 1. This field selects which instance is routed to [`Self::index`].
    pub chan: u32,

    /// Reserved for future use.
    ///
    /// Keep this zero-initialized when passing the structure to the kernel.
    pub rsv: [u32; 5],
}

impl Default for PtpPinDesc {
    fn default() -> Self {
        Self {
            name: [0; 64],
            index: 0,
            func: PtpPinFunction::default(),
            chan: 0,
            rsv: [0; 5],
        }
    }
}

ioctl_read!(
    /// Query the capabilities exposed by a PHC device.
    ///
    /// Fills a [`PtpClockCaps`] value with the number of supported external
    /// timestamp channels, periodic output channels, programmable pins, PPS
    /// support, frequency adjustment range, and other device capabilities.
    ///
    /// The file descriptor must refer to an open `/dev/ptpX` device.
    ptp_clock_getcaps_ioctl,
    PTP_CLK_MAGIC,
    1,
    PtpClockCaps
);

ioctl_write_ptr!(
    /// Enable, disable, or reconfigure an external timestamp channel.
    ///
    /// The request selects an external timestamp channel by
    /// [`PtpExttsRequest::index`] and controls capture behavior through
    /// [`PtpExttsRequest::flags`].
    ///
    /// If the PHC exposes programmable pins, a physical pin must also be routed
    /// to the same channel with [`ptp_pin_setfunc_ioctl`] and
    /// [`PtpPinFunction::PTP_PF_EXTTS`].
    ///
    /// After timestamp capture is enabled, events can be read from the
    /// `/dev/ptpX` file descriptor as [`PtpExttsEvent`] values.
    ptp_extts_request_ioctl,
    PTP_CLK_MAGIC,
    2,
    PtpExttsRequest
);

ioctl_write_ptr!(
    /// Enable, disable, or reconfigure a periodic output channel.
    ///
    /// The request selects a periodic output channel by
    /// [`PtpPeroutRequest::index`] and configures its period, start time or
    /// phase, and optional flags. A zero period disables the selected channel.
    ///
    /// If the PHC exposes programmable pins, a physical pin must also be routed
    /// to the same channel with [`ptp_pin_setfunc_ioctl`] and
    /// [`PtpPinFunction::PTP_PF_PEROUT`].
    ptp_perout_request_ioctl,
    PTP_CLK_MAGIC,
    3,
    PtpPeroutRequest
);

ioctl_readwrite!(
    /// Query the function currently assigned to a programmable PHC pin.
    ///
    /// Before calling this ioctl, set [`PtpPinDesc::index`] to the physical pin
    /// index to query. On success, the kernel fills the pin name, function, and
    /// channel fields in [`PtpPinDesc`].
    ///
    /// Valid pin indices are in the range `0..PtpClockCaps::n_pins`.
    ptp_pin_getfunc_ioctl,
    PTP_CLK_MAGIC,
    6,
    PtpPinDesc
);

ioctl_write_ptr!(
    /// Assign a PTP function to a programmable PHC pin.
    ///
    /// Uses [`PtpPinDesc::index`] to select the physical pin,
    /// [`PtpPinDesc::func`] to select the kind of time I/O block, and
    /// [`PtpPinDesc::chan`] to select the specific channel instance.
    ///
    /// For example, assigning [`PtpPinFunction::PTP_PF_PEROUT`] with channel
    /// `0` routes periodic output channel 0 to the selected pin. A later
    /// [`ptp_perout_request_ioctl`] for channel 0 then controls the signal on
    /// that pin.
    ptp_pin_setfunc_ioctl,
    PTP_CLK_MAGIC,
    7,
    PtpPinDesc
);

ioctl_write_ptr!(
    /// Enable, disable, or reconfigure an external timestamp channel.
    ///
    /// This is the newer external timestamp ioctl variant. It uses the same
    /// [`PtpExttsRequest`] layout as [`ptp_extts_request_ioctl`], but supports
    /// the newer kernel request number.
    ///
    /// Prefer this variant when targeting kernels and drivers that support the
    /// newer PTP external timestamp API. Keep the older ioctl available for
    /// compatibility.
    ptp_extts_request2_ioctl,
    PTP_CLK_MAGIC,
    11,
    PtpExttsRequest
);

ioctl_write_ptr!(
    /// Enable, disable, or reconfigure a periodic output channel.
    ///
    /// This is the newer periodic output ioctl variant. It uses the same
    /// [`PtpPeroutRequest`] layout as [`ptp_perout_request_ioctl`], but supports
    /// newer request flags such as phase and duty-cycle handling when the
    /// driver implements them.
    ///
    /// Prefer this variant when targeting kernels and drivers that support the
    /// newer PTP periodic output API. Keep the older ioctl available for
    /// compatibility.
    ptp_perout_request2_ioctl,
    PTP_CLK_MAGIC,
    12,
    PtpPeroutRequest
);
