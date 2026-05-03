use std::{path::PathBuf, time::Duration};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rs_1722::ptp_phc::{Edge, PinFunction, PtpClock};

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
    #[arg(short, long, default_value = "/dev/ptp0")]
    device: PathBuf,

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
        function: CliPinFunction,

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
        period_ns: u64,

        /// PEROUT channel index.
        #[arg(short, long, default_value_t = 0)]
        channel: u32,

        /// Optional phase in nanoseconds.
        ///
        /// This uses `PTP_PEROUT_PHASE`, which some drivers may reject.
        /// For the Intel I210, start with no phase argument.
        #[arg(long)]
        phase_ns: Option<u64>,
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
        #[arg(short, long, default_value_t = CliEdge::Rising)]
        edge: CliEdge,

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
enum CliPinFunction {
    /// No function assigned.
    None,

    /// External timestamp input.
    Extts,

    /// Periodic output.
    Perout,
}

impl From<CliPinFunction> for PinFunction {
    fn from(function: CliPinFunction) -> Self {
        match function {
            CliPinFunction::None => Self::None,
            CliPinFunction::Extts => Self::ExternalTimestamp,
            CliPinFunction::Perout => Self::PeriodicOutput,
        }
    }
}

/// External timestamp edge selection.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliEdge {
    /// Timestamp rising edges.
    Rising,

    /// Timestamp falling edges.
    Falling,

    /// Timestamp both rising and falling edges.
    Both,
}

impl From<CliEdge> for Edge {
    fn from(edge: CliEdge) -> Self {
        match edge {
            CliEdge::Rising => Self::Rising,
            CliEdge::Falling => Self::Falling,
            CliEdge::Both => Self::Both,
        }
    }
}

impl std::fmt::Display for CliEdge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rising => formatter.write_str("rising"),
            Self::Falling => formatter.write_str("falling"),
            Self::Both => formatter.write_str("both"),
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut clock = PtpClock::open(&args.device)?;

    match args.command {
        Command::Caps => {
            let caps = clock.capabilities()?;
            println!("capabilities:");
            println!("  {} maximum frequency adjustment (ppb)", caps.max_adjustment_ppb);
            println!("  {} programmable alarms", caps.programmable_alarms);
            println!("  {} external timestamp channels", caps.external_timestamp_channels);
            println!("  {} programmable periodic signals", caps.periodic_output_channels);
            println!("  {} pulse per second", u8::from(caps.pulse_per_second));
            println!("  {} programmable pins", caps.programmable_pins);
            println!("  {} cross timestamping", u8::from(caps.cross_timestamping));
            println!("  {} adjust_phase", u8::from(caps.adjust_phase));
            println!("  {} maximum phase adjustment (ns)", caps.max_phase_adjustment_ns);
        }

        Command::Pins => {
            for pin in clock.pins()? {
                println!(
                    "name {} index {} func {:?} chan {}",
                    pin.name, pin.index, pin.function, pin.channel
                );
            }
        }

        Command::SetPin { pin, function, channel } => {
            clock.set_pin_function(pin, function.into(), channel)?;
            println!("set pin function okay");
        }

        Command::Perout {
            period_ns,
            channel,
            phase_ns,
        } => {
            if period_ns == 0 {
                bail!("period_ns must be greater than zero");
            }

            let period = Duration::from_nanos(period_ns);
            let phase = phase_ns.map(Duration::from_nanos);

            clock.enable_periodic_output(channel, period, phase)?;
            println!("periodic output request okay");
        }

        Command::StopPerout { channel } => {
            clock.disable_periodic_output(channel)?;
            println!("periodic output disabled");
        }

        Command::Extts { channel, edge, count } => {
            clock.enable_external_timestamping(channel, edge.into())?;
            println!("external timestamp request okay");

            for _ in 0..count {
                let event = clock.read_external_timestamp_event()?;
                println!("event index {} at {}", event.channel, event.timestamp);
            }

            clock.disable_external_timestamping(channel)?;
            println!("external timestamp disabled");
        }

        Command::StopExtts { channel } => {
            clock.disable_external_timestamping(channel)?;
            println!("external timestamp disabled");
        }

        Command::Time => {
            println!("clock time: {}", clock.time()?);
        }
    }

    Ok(())
}
