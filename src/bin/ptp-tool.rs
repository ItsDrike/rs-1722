use std::{
    fs::{File, OpenOptions},
    io::Read,
    mem,
    os::fd::{AsRawFd, RawFd},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use nix::{ioctl_read, ioctl_readwrite, ioctl_write_ptr};

/// Linux PTP ioctl magic value.
///
/// This matches `PTP_CLK_MAGIC` from `<linux/ptp_clock.h>`.
const PTP_CLK_MAGIC: u8 = b'=';

/// Enables a PTP feature, used by `PTP_EXTTS_REQUEST`.
const PTP_ENABLE_FEATURE: u32 = 1 << 0;

/// Timestamp rising edges for external timestamp input.
const PTP_RISING_EDGE: u32 = 1 << 1;

/// Timestamp falling edges for external timestamp input.
const PTP_FALLING_EDGE: u32 = 1 << 2;

/// Pin function: no function assigned.
const PTP_PF_NONE: u32 = 0;

/// Pin function: external timestamp input.
const PTP_PF_EXTTS: u32 = 1;

/// Pin function: periodic output.
const PTP_PF_PEROUT: u32 = 2;

/// Periodic output flag: interpret the first time field as a phase.
///
/// Not all drivers support this. The Intel I210 through `igb` often supports plain periodic output,
/// but may reject newer optional flags.
const PTP_PEROUT_PHASE: u32 = 1 << 2;

/// Nanoseconds per second.
const NSEC_PER_SEC: i64 = 1_000_000_000;

/// CLI entry point.
///
/// Examples:
///
/// ```text
/// ptp-tool --device /dev/ptp0 caps
/// ptp-tool --device /dev/ptp0 pins
/// ptp-tool --device /dev/ptp0 set-pin 1 perout --channel 0
/// ptp-tool --device /dev/ptp0 perout 1000000000 --channel 0
/// ptp-tool --device /dev/ptp0 extts --channel 0 --edge rising --count 5
/// ```
#[derive(Debug, Parser)]
#[command(name = "ptp-tool")]
#[command(about = "Small Linux PTP /dev/ptpX helper")]
struct Args {
    /// PTP device node, for example `/dev/ptp0`.
    #[arg(short, long, default_value = "/dev/ptp0")]
    device: PathBuf,

    /// Command to run against the selected PTP device.
    #[command(subcommand)]
    command: Command,
}

/// Supported high-level commands.
///
/// This intentionally implements only the useful subset of `testptp` needed for PHC, SDP,
/// PEROUT, and EXTTS testing.
#[derive(Debug, Subcommand)]
enum Command {
    /// Print PHC capabilities.
    Caps,

    /// List current PTP pin configuration.
    Pins,

    /// Configure a PTP pin function.
    SetPin {
        /// Pin index, for example `1` for `SDP1`.
        pin: u32,

        /// Function to assign to the pin.
        function: PinFunction,

        /// Channel index for the selected function.
        ///
        /// For example, `--channel 0` maps this pin to periodic-output channel 0
        /// when `function` is `perout`.
        #[arg(short, long, default_value_t = 0)]
        channel: u32,
    },

    /// Enable periodic output on a PEROUT channel.
    Perout {
        /// Period in nanoseconds.
        ///
        /// Examples:
        ///
        /// 1 Hz: `1000000000`
        ///
        /// 1 kHz: `1000000`
        period_ns: i64,

        /// PEROUT channel index.
        #[arg(short, long, default_value_t = 0)]
        channel: u32,

        /// Optional phase in nanoseconds.
        ///
        /// This uses `PTP_PEROUT_PHASE`, which some drivers may reject.
        /// For the Intel I210, start with no phase argument.
        #[arg(long)]
        phase_ns: Option<i64>,
    },

    /// Disable periodic output on a PEROUT channel.
    StopPerout {
        /// PEROUT channel index.
        #[arg(short, long, default_value_t = 0)]
        channel: u32,
    },

    /// Enable external timestamping and read events.
    Extts {
        /// EXTTS channel index.
        #[arg(short, long, default_value_t = 0)]
        channel: u32,

        /// Edge to timestamp.
        #[arg(short, long, default_value_t = Edge::Rising)]
        edge: Edge,

        /// Number of timestamp events to read.
        #[arg(short, long, default_value_t = 5)]
        count: usize,
    },

    /// Disable external timestamping on a channel.
    StopExtts {
        /// EXTTS channel index.
        #[arg(short, long, default_value_t = 0)]
        channel: u32,
    },

    /// Read the PHC time using the Linux CLOCKFD encoding.
    Time,
}

/// PTP pin functions exposed by the Linux PTP subsystem.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum PinFunction {
    /// No function assigned.
    None,

    /// External timestamp input.
    Extts,

    /// Periodic output.
    Perout,
}

/// External timestamp edge selection.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Edge {
    /// Timestamp rising edges.
    Rising,

    /// Timestamp falling edges.
    Falling,

    /// Timestamp both rising and falling edges.
    Both,
}

impl std::fmt::Display for Edge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rising => formatter.write_str("rising"),
            Self::Falling => formatter.write_str("falling"),
            Self::Both => formatter.write_str("both"),
        }
    }
}

/// Kernel ABI type matching `struct ptp_clock_time`.
///
/// This must match `<linux/ptp_clock.h>` exactly:
///
/// ```c
/// struct ptp_clock_time {
///     __s64 sec;
///     __u32 nsec;
///     __u32 reserved;
/// };
/// ```
///
/// Use fixed-width Rust integers rather than `time_t`, because this is a kernel UAPI struct,
/// not a libc struct.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PtpClockTime {
    sec: i64,
    nsec: u32,
    reserved: u32,
}

/// Kernel ABI type matching `struct ptp_clock_caps`.
///
/// This describes the capabilities of a PHC device, including the number of external timestamp
/// channels, periodic output channels, and programmable pins.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PtpClockCaps {
    max_adj: i32,
    n_alarm: i32,
    n_ext_ts: i32,
    n_per_out: i32,
    pps: i32,
    n_pins: i32,
    cross_timestamping: i32,
    adjust_phase: i32,
    max_phase_adj: i32,
    rsv: [i32; 11],
}

/// Kernel ABI type matching `struct ptp_pin_desc`.
///
/// `name` is a NUL-terminated C string from the driver, for example `SDP0`.
///
/// `func` is one of:
///
/// * `0`: none
/// * `1`: external timestamp
/// * `2`: periodic output
///
/// `chan` is the associated channel index for that function.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct PtpPinDesc {
    name: [nix::libc::c_char; 64],
    index: u32,
    func: u32,
    chan: u32,
    rsv: [u32; 5],
}

impl Default for PtpPinDesc {
    fn default() -> Self {
        Self {
            name: [0; 64],
            index: 0,
            func: 0,
            chan: 0,
            rsv: [0; 5],
        }
    }
}

/// Kernel ABI type matching `struct ptp_extts_request`.
///
/// Used with `PTP_EXTTS_REQUEST` to enable or disable external timestamp capture.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PtpExttsRequest {
    index: u32,
    flags: u32,
    rsv: [u32; 2],
}

/// Kernel ABI type matching `struct ptp_extts_event`.
///
/// Returned by reading from `/dev/ptpX` after external timestamping is enabled.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PtpExttsEvent {
    t: PtpClockTime,
    index: u32,
    flags: u32,
    rsv: [u32; 2],
}

/// Kernel ABI type matching `struct ptp_perout_request`.
///
/// The first field is a union in C:
///
/// ```c
/// union {
///     struct ptp_clock_time start;
///     struct ptp_clock_time phase;
/// };
/// ```
///
/// This Rust version represents that union storage as `start_or_phase`.
///
/// The final field is also a union in C:
///
/// ```c
/// union {
///     struct ptp_clock_time on;
///     unsigned int rsv[4];
/// };
/// ```
///
/// This tool only uses plain periodic output, so it keeps that storage as raw reserved words.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct PtpPeroutRequest {
    start_or_phase: PtpClockTime,
    period: PtpClockTime,
    index: u32,
    flags: u32,
    on_or_rsv: [u32; 4],
}

ioctl_read!(
    /// Get PHC capabilities.
    ptp_clock_getcaps_ioctl,
    PTP_CLK_MAGIC,
    1,
    PtpClockCaps
);

ioctl_write_ptr!(
    /// Configure external timestamping.
    ptp_extts_request_ioctl,
    PTP_CLK_MAGIC,
    2,
    PtpExttsRequest
);

ioctl_write_ptr!(
    /// Configure periodic output.
    ptp_perout_request_ioctl,
    PTP_CLK_MAGIC,
    3,
    PtpPeroutRequest
);

ioctl_readwrite!(
    /// Get one pin's current function.
    ptp_pin_getfunc_ioctl,
    PTP_CLK_MAGIC,
    6,
    PtpPinDesc
);

ioctl_write_ptr!(
    /// Set one pin's current function.
    ptp_pin_setfunc_ioctl,
    PTP_CLK_MAGIC,
    7,
    PtpPinDesc
);

ioctl_write_ptr!(
    /// Configure external timestamping using the newer ioctl number.
    ptp_extts_request2_ioctl,
    PTP_CLK_MAGIC,
    11,
    PtpExttsRequest
);

ioctl_write_ptr!(
    /// Configure periodic output using the newer ioctl number.
    ptp_perout_request2_ioctl,
    PTP_CLK_MAGIC,
    12,
    PtpPeroutRequest
);

fn main() -> Result<()> {
    let args = Args::parse();

    let mut device = open_ptp_device(&args.device)?;

    match args.command {
        Command::Caps => print_caps(&device),
        Command::Pins => print_pins(&device),
        Command::SetPin { pin, function, channel } => set_pin(&device, pin, function, channel),
        Command::Perout {
            period_ns,
            channel,
            phase_ns,
        } => enable_perout(&device, channel, period_ns, phase_ns),
        Command::StopPerout { channel } => disable_perout(&device, channel),
        Command::Extts { channel, edge, count } => read_extts(&mut device, channel, edge, count),
        Command::StopExtts { channel } => disable_extts(&device, channel),
        Command::Time => print_time(&device),
    }
}

/// Open a PTP device node for reading and writing.
fn open_ptp_device(path: &PathBuf) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))
}

/// Print PHC capabilities.
fn print_caps(device: &File) -> Result<()> {
    let caps = get_caps(device)?;

    println!("capabilities:");
    println!("  {} maximum frequency adjustment (ppb)", caps.max_adj);
    println!("  {} programmable alarms", caps.n_alarm);
    println!("  {} external timestamp channels", caps.n_ext_ts);
    println!("  {} programmable periodic signals", caps.n_per_out);
    println!("  {} pulse per second", caps.pps);
    println!("  {} programmable pins", caps.n_pins);
    println!("  {} cross timestamping", caps.cross_timestamping);
    println!("  {} adjust_phase", caps.adjust_phase);
    println!("  {} maximum phase adjustment (ns)", caps.max_phase_adj);

    Ok(())
}

/// Print the current function of every programmable PTP pin.
fn print_pins(device: &File) -> Result<()> {
    let caps = get_caps(device)?;

    for index in 0..caps.n_pins {
        let pin_index = u32::try_from(index).context("invalid pin index")?;
        let desc = get_pin(device, pin_index)?;

        println!(
            "name {} index {} func {} ({}) chan {}",
            pin_name(&desc),
            desc.index,
            desc.func,
            pin_function_name(desc.func),
            desc.chan,
        );
    }

    Ok(())
}

/// Configure one programmable pin.
fn set_pin(device: &File, pin: u32, function: PinFunction, channel: u32) -> Result<()> {
    let func = match function {
        PinFunction::None => PTP_PF_NONE,
        PinFunction::Extts => PTP_PF_EXTTS,
        PinFunction::Perout => PTP_PF_PEROUT,
    };

    let desc = PtpPinDesc {
        index: pin,
        func,
        chan: channel,
        ..PtpPinDesc::default()
    };

    // SAFETY:
    // The file descriptor refers to an open `/dev/ptpX` device.
    // `desc` is a valid pointer to a `#[repr(C)]` struct matching the kernel UAPI.
    unsafe {
        ptp_pin_setfunc_ioctl(device.as_raw_fd(), &desc).context("PTP_PIN_SETFUNC failed")?;
    }

    println!("set pin function okay");
    Ok(())
}

/// Enable periodic output on a PHC PEROUT channel.
///
/// Without `phase_ns`, this follows `testptp` behavior:
///
/// * read current PHC time
/// * start at `current_second + 2`, nanosecond 0
///
/// That makes the output aligned to a PHC whole-second boundary.
fn enable_perout(device: &File, channel: u32, period_ns: i64, phase_ns: Option<i64>) -> Result<()> {
    if period_ns <= 0 {
        bail!("period_ns must be greater than zero");
    }

    let mut request = PtpPeroutRequest {
        period: ns_to_ptp_time(period_ns)?,
        index: channel,
        ..PtpPeroutRequest::default()
    };

    if let Some(phase_ns) = phase_ns {
        if phase_ns < 0 {
            bail!("phase_ns must be non-negative");
        }

        request.start_or_phase = ns_to_ptp_time(phase_ns)?;
        request.flags = PTP_PEROUT_PHASE;
    } else {
        let now = clock_gettime_for_fd(device.as_raw_fd()).context("failed to read PHC time")?;
        request.start_or_phase = PtpClockTime {
            sec: now.tv_sec + 2,
            nsec: 0,
            reserved: 0,
        };
    }

    perout_request(device.as_raw_fd(), &request).context("PTP_PEROUT_REQUEST failed")?;

    println!("periodic output request okay");
    Ok(())
}

/// Disable periodic output on a PHC PEROUT channel.
///
/// This sends a zeroed periodic output request with only the channel index set.
fn disable_perout(device: &File, channel: u32) -> Result<()> {
    let request = PtpPeroutRequest {
        index: channel,
        ..PtpPeroutRequest::default()
    };

    perout_request(device.as_raw_fd(), &request).context("PTP_PEROUT_REQUEST disable failed")?;

    println!("periodic output disabled");
    Ok(())
}

/// Enable external timestamping, read a fixed number of events, then disable it.
fn read_extts(device: &mut File, channel: u32, edge: Edge, count: usize) -> Result<()> {
    enable_extts(device, channel, edge)?;

    for _ in 0..count {
        let mut event = PtpExttsEvent::default();

        // SAFETY:
        // `event` is plain old data with `#[repr(C)]`.
        // We create a byte slice covering exactly its storage so `read_exact` can fill it
        // with the kernel-provided event data.
        let event_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut event as *mut PtpExttsEvent).cast::<u8>(),
                mem::size_of::<PtpExttsEvent>(),
            )
        };

        device
            .read_exact(event_bytes)
            .context("failed to read external timestamp event")?;

        println!("event index {} at {}.{:09}", event.index, event.t.sec, event.t.nsec);
    }

    disable_extts(device, channel)
}

/// Enable external timestamp capture on a PHC EXTTS channel.
fn enable_extts(device: &File, channel: u32, edge: Edge) -> Result<()> {
    let edge_flags = match edge {
        Edge::Rising => PTP_RISING_EDGE,
        Edge::Falling => PTP_FALLING_EDGE,
        Edge::Both => PTP_RISING_EDGE | PTP_FALLING_EDGE,
    };

    let request = PtpExttsRequest {
        index: channel,
        flags: PTP_ENABLE_FEATURE | edge_flags,
        rsv: [0; 2],
    };

    extts_request(device.as_raw_fd(), &request).context("PTP_EXTTS_REQUEST enable failed")?;

    println!("external timestamp request okay");
    Ok(())
}

/// Disable external timestamp capture on a PHC EXTTS channel.
fn disable_extts(device: &File, channel: u32) -> Result<()> {
    let request = PtpExttsRequest {
        index: channel,
        flags: 0,
        rsv: [0; 2],
    };

    extts_request(device.as_raw_fd(), &request).context("PTP_EXTTS_REQUEST disable failed")?;

    println!("external timestamp disabled");
    Ok(())
}

/// Print the PHC time by converting the file descriptor into a Linux dynamic clock id.
fn print_time(device: &File) -> Result<()> {
    let ts = clock_gettime_for_fd(device.as_raw_fd()).context("failed to read PHC time")?;
    println!("clock time: {}.{:09}", ts.tv_sec, ts.tv_nsec);
    Ok(())
}

/// Read PHC capabilities using `PTP_CLOCK_GETCAPS`.
fn get_caps(device: &File) -> Result<PtpClockCaps> {
    let mut caps = PtpClockCaps::default();

    // SAFETY:
    // The file descriptor refers to an open `/dev/ptpX` device.
    // `caps` is a valid pointer to a `#[repr(C)]` struct matching the kernel UAPI.
    unsafe {
        ptp_clock_getcaps_ioctl(device.as_raw_fd(), &mut caps).context("PTP_CLOCK_GETCAPS failed")?;
    }

    Ok(caps)
}

/// Read one pin descriptor using `PTP_PIN_GETFUNC`.
fn get_pin(device: &File, index: u32) -> Result<PtpPinDesc> {
    let mut desc = PtpPinDesc {
        index,
        ..PtpPinDesc::default()
    };

    // SAFETY:
    // The file descriptor refers to an open `/dev/ptpX` device.
    // `desc` is a valid pointer to a `#[repr(C)]` struct matching the kernel UAPI.
    unsafe {
        ptp_pin_getfunc_ioctl(device.as_raw_fd(), &mut desc).context("PTP_PIN_GETFUNC failed")?;
    }

    Ok(desc)
}

/// Wrapper for periodic output request.
///
/// The newer `PTP_PEROUT_REQUEST2` is tried first. If the driver rejects it, the older
/// `PTP_PEROUT_REQUEST` is tried as a fallback.
fn perout_request(fd: RawFd, request: &PtpPeroutRequest) -> Result<()> {
    // SAFETY:
    // `request` points to a valid `#[repr(C)]` UAPI-compatible struct.
    let new_result = unsafe { ptp_perout_request2_ioctl(fd, request) };

    if new_result.is_ok() {
        return Ok(());
    }

    // SAFETY:
    // Same as above, using the older ioctl number.
    unsafe {
        ptp_perout_request_ioctl(fd, request).context("legacy PTP_PEROUT_REQUEST failed")?;
    }

    Ok(())
}

/// Wrapper for external timestamp request.
///
/// The newer `PTP_EXTTS_REQUEST2` is tried first. If the driver rejects it, the older
/// `PTP_EXTTS_REQUEST` is tried as a fallback.
fn extts_request(fd: RawFd, request: &PtpExttsRequest) -> Result<()> {
    // SAFETY:
    // `request` points to a valid `#[repr(C)]` UAPI-compatible struct.
    let new_result = unsafe { ptp_extts_request2_ioctl(fd, request) };

    if new_result.is_ok() {
        return Ok(());
    }

    // SAFETY:
    // Same as above, using the older ioctl number.
    unsafe {
        ptp_extts_request_ioctl(fd, request).context("legacy PTP_EXTTS_REQUEST failed")?;
    }

    Ok(())
}

/// Convert a kernel pin name from NUL-terminated C string storage to Rust `String`.
fn pin_name(desc: &PtpPinDesc) -> String {
    let bytes: Vec<u8> = desc
        .name
        .iter()
        .copied()
        .take_while(|byte| *byte != 0)
        .map(|byte| byte as u8)
        .collect();

    String::from_utf8_lossy(&bytes).into_owned()
}

/// Convert a numeric pin function to a readable name.
fn pin_function_name(function: u32) -> &'static str {
    match function {
        PTP_PF_NONE => "none",
        PTP_PF_EXTTS => "extts",
        PTP_PF_PEROUT => "perout",
        _ => "unknown",
    }
}

/// Convert a non-negative nanosecond duration into `struct ptp_clock_time`.
fn ns_to_ptp_time(ns: i64) -> Result<PtpClockTime> {
    if ns < 0 {
        bail!("timestamp value must be non-negative");
    }

    Ok(PtpClockTime {
        sec: ns / NSEC_PER_SEC,
        nsec: u32::try_from(ns % NSEC_PER_SEC).context("nanosecond remainder did not fit u32")?,
        reserved: 0,
    })
}

/// Read PHC time using `clock_gettime`.
///
/// Linux exposes `/dev/ptpX` devices as dynamic POSIX clocks. The clock id is derived
/// from the file descriptor using the `CLOCKFD` encoding used by `testptp`.
fn clock_gettime_for_fd(fd: RawFd) -> Result<nix::libc::timespec> {
    let clock_id = clock_id_from_fd(fd);

    let mut ts = nix::libc::timespec { tv_sec: 0, tv_nsec: 0 };

    // SAFETY:
    // `ts` is a valid writable pointer, and `clock_id` is the Linux dynamic clock id
    // corresponding to the open PTP device file descriptor.
    let ret = unsafe { nix::libc::clock_gettime(clock_id, &mut ts) };

    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("clock_gettime failed");
    }

    Ok(ts)
}

/// Convert a file descriptor into a Linux dynamic clock id.
///
/// This matches the helper from kernel selftest `testptp.c`:
///
/// ```c
/// #define CLOCKFD 3
/// return (((unsigned int) ~fd) << 3) | CLOCKFD;
/// ```
fn clock_id_from_fd(fd: RawFd) -> nix::libc::clockid_t {
    const CLOCKFD: nix::libc::clockid_t = 3;
    ((!fd) << 3) | CLOCKFD
}
