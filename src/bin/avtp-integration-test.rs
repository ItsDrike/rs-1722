//! AVTP/AAF/PCM integration test - transmit a WAV file over Ethernet and validate reception.
//!
//! This binary runs three concurrent tasks:
//! - Thread B (receiver): receives AVTP frames and decodes them via `AafPcmListener`
//! - Thread C (talker): sends paced AVTP packets via `AafPcmTalker`
//! - Thread A (main): drains a jitter buffer, validates the results, and writes artifacts

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arbitrary_int::prelude::*;
use clap::Parser;
use pnet::util::MacAddr;

use rs_1722::audio::wav;
use rs_1722::avtp::formats::aaf::{AafPcmListener, AafPcmTalker, PcmFormat, ReceivedPcm, SampleRate};
use rs_1722::avtp::transport::{EthernetAvtpReceiver, EthernetAvtpSender};
use rs_1722::avtp::{Avtpdu, PtpSynchronizedClock, StreamID};
use rs_1722::bin_utils;
use rs_1722::bin_utils::csv::{self, CsvRecord};
use rs_1722::bin_utils::{ClockSource, ValidatedInterface};
use rs_1722::ptp_phc::{PtpTime, PtpTimeSource};

// ============================================================================
// CLI Arguments
// ============================================================================

#[derive(Parser)]
#[command(name = "avtp-integration-test")]
#[command(about = "AVTP/AAF/PCM integration test - transmit WAV over Ethernet")]
#[command(after_help = "TOPOLOGY EXAMPLES:

  Two-NIC direct cable (auto-detect PTP on both interfaces):
    avtp-integration-test /tmp/test.wav \
      --talker-interface enp2s0 --listener-interface enp3s0

  Two-NIC with explicit PTP devices (for custom/non-standard setups):
    avtp-integration-test /tmp/test.wav \
      --talker-interface enp2s0 --listener-interface enp3s0 \
      --talker-ptp /dev/ptp1 --listener-ptp /dev/ptp2

  Use system time instead of PTP hardware clock:
    avtp-integration-test /tmp/test.wav \
      --talker-interface enp2s0 --listener-interface enp3s0 --system-time

  Virtual interface pair (Linux testing):
    ip link add veth0 type veth peer name veth1
    ip link set veth0 up && ip link set veth1 up
    avtp-integration-test /tmp/test.wav --talker-interface veth0 --listener-interface veth1 --system-time

Requires CAP_NET_RAW capability or root for raw socket access.
")]
struct Args {
    /// Input WAV file to transmit.
    #[arg(value_name = "FILE")]
    wav_file: PathBuf,

    /// Network interface for talker.
    #[arg(long)]
    talker_interface: String,

    /// Network interface for listener.
    #[arg(long)]
    listener_interface: String,

    /// PTP clock device for talker [default: auto-detect].
    #[arg(long)]
    talker_ptp: Option<PathBuf>,

    /// PTP clock device for listener [default: auto-detect].
    #[arg(long)]
    listener_ptp: Option<PathBuf>,

    /// Use `CLOCK_REALTIME` instead of PTP hardware clocks.
    #[arg(long, conflicts_with_all = ["talker_ptp", "listener_ptp"])]
    system_time: bool,

    /// Presentation delay in milliseconds [default: 50].
    #[arg(long, default_value = "50")]
    playback_delay_ms: u32,

    /// AVTP stream unique ID [default: 1].
    #[arg(long, default_value = "1")]
    stream_uid: u16,

    /// Hard timeout beyond talker completion [default: 5].
    #[arg(long, default_value = "5")]
    timeout_secs: u64,

    /// Output directory for CSVs and received WAV.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Samples per AVTP packet [default: 1ms worth].
    #[arg(long)]
    samples_per_packet: Option<u32>,
}

// ============================================================================
// Core Data Structures
// ============================================================================

struct LoadedWav {
    header: wav::WavHeader,
    pcm_le: Arc<[u8]>,
}

#[derive(Clone)]
struct TalkerRecord {
    frame_idx: u64,
    seq_num: u8,
    phc_time_at_send: PtpTime,
    avtp_timestamp_embedded: u32,
}

impl CsvRecord for TalkerRecord {
    fn csv_header() -> &'static str {
        "frame_idx,seq_num,phc_time_at_send,avtp_timestamp_embedded"
    }

    fn to_csv_line(&self) -> String {
        format!(
            "{},{},{},{}",
            self.frame_idx, self.seq_num, self.phc_time_at_send, self.avtp_timestamp_embedded
        )
    }
}

#[derive(Clone)]
struct ListenerRecord {
    frame_idx: u64,
    seq_num: u8,
    phc_time_at_receive: PtpTime,
    avtp_timestamp_embedded: u32,
    phc_time_at_present: PtpTime,
}

impl CsvRecord for ListenerRecord {
    fn csv_header() -> &'static str {
        "frame_idx,seq_num,phc_time_at_receive,avtp_timestamp_embedded,phc_time_at_present"
    }

    fn to_csv_line(&self) -> String {
        format!(
            "{},{},{},{},{}",
            self.frame_idx,
            self.seq_num,
            self.phc_time_at_receive,
            self.avtp_timestamp_embedded,
            self.phc_time_at_present
        )
    }
}

enum ReceivedEvent {
    Packet {
        pcm: ReceivedPcm,
        phc_time_at_receive: PtpTime,
    },
}

struct Buffered {
    presentation_time: PtpTime,
    phc_time_at_receive: PtpTime,
    pcm: ReceivedPcm,
}

impl Ord for Buffered {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.presentation_time.cmp(&other.presentation_time)
    }
}

impl PartialOrd for Buffered {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for Buffered {}

impl PartialEq for Buffered {
    fn eq(&self, other: &Self) -> bool {
        self.presentation_time == other.presentation_time
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let wav = load_wav(&args.wav_file)?;
    let (talker, listener) = validate_and_extract_interfaces(&args)?;
    let output_dir = setup_output_dir(&args)?;

    log_config(&args, &wav.header, &talker, &listener, &output_dir);

    let (talker_records, listener_records, received_pcm_le) = run_test(&args, &wav, &talker, &listener)?;

    eprintln!("[Saving outputs to {}]", output_dir.display());
    save_outputs(
        &output_dir,
        &talker_records,
        &listener_records,
        &received_pcm_le,
        &wav.header,
    )?;

    eprintln!("[Analyzing results]");
    analyze_and_report(
        &talker_records,
        &listener_records,
        wav.pcm_le.as_ref(),
        &received_pcm_le,
    )?;

    eprintln!("Done!");
    Ok(())
}

// ============================================================================
// Setup and Validation
// ============================================================================

fn load_wav(path: &Path) -> anyhow::Result<LoadedWav> {
    let mut file = File::open(path)?;
    let wav = wav::Wav::read(&mut file).map_err(|e| anyhow::anyhow!("Failed to read WAV: {e}"))?;

    Ok(LoadedWav {
        header: wav.header,
        pcm_le: Arc::from(wav.data),
    })
}

fn validate_and_extract_interfaces(args: &Args) -> anyhow::Result<(ValidatedInterface, ValidatedInterface)> {
    let talker = validate_interface(
        &args.talker_interface,
        args.system_time,
        args.talker_ptp.as_deref(),
        "talker",
    )?;
    let listener = validate_interface(
        &args.listener_interface,
        args.system_time,
        args.listener_ptp.as_deref(),
        "listener",
    )?;

    if talker.interface_name() == listener.interface_name() {
        anyhow::bail!(
            "Talker and listener interfaces must be different. Both specified as '{}'. \
             The Linux kernel does not loop back raw Ethernet frames on the same interface. \
             Use two separate NICs, a virtual interface pair (veth), or actual hardware.",
            talker.interface_name()
        );
    }

    Ok((talker, listener))
}

fn validate_interface(
    name: &str,
    system_time: bool,
    explicit_ptp: Option<&Path>,
    role: &str,
) -> anyhow::Result<ValidatedInterface> {
    if system_time {
        ValidatedInterface::with_system_time(name)
    } else if let Some(path) = explicit_ptp {
        ValidatedInterface::with_explicit_ptp(name, path)
    } else {
        ValidatedInterface::new(name)
    }
    .map_err(|e| anyhow::anyhow!("Failed to validate {role} interface: {e}"))
}

fn setup_output_dir(args: &Args) -> anyhow::Result<PathBuf> {
    let dir = args.output_dir.clone().unwrap_or_else(|| {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        PathBuf::from(format!("/tmp/avtp-test-{timestamp}"))
    });

    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn log_config(
    args: &Args,
    wav_header: &wav::WavHeader,
    talker: &ValidatedInterface,
    listener: &ValidatedInterface,
    output_dir: &Path,
) {
    eprintln!("=== AVTP Integration Test ===");
    eprintln!(
        "Input WAV: {} ({} Hz, {} ch, {} bits container, {} bits valid)",
        args.wav_file.display(),
        wav_header.sample_rate,
        wav_header.channels,
        wav_header.bits_per_sample,
        wav_header.valid_bits_per_sample
    );
    eprintln!(
        "Talker interface: {} ({}) [{}]",
        talker.interface_name(),
        talker.mac_address(),
        talker.clock_source()
    );
    eprintln!(
        "Listener interface: {} ({}) [{}]",
        listener.interface_name(),
        listener.mac_address(),
        listener.clock_source()
    );
    eprintln!("Playback delay: {}ms", args.playback_delay_ms);
    eprintln!("Output dir: {}", output_dir.display());
    eprintln!();
}

// ============================================================================
// Test Execution
// ============================================================================

fn run_test(
    args: &Args,
    wav: &LoadedWav,
    talker: &ValidatedInterface,
    listener: &ValidatedInterface,
) -> anyhow::Result<(Vec<TalkerRecord>, Vec<ListenerRecord>, Vec<u8>)> {
    let samples_per_packet = args
        .samples_per_packet
        .unwrap_or_else(|| (wav.header.sample_rate / 1_000).max(1));

    let (event_tx, event_rx) = mpsc::channel::<ReceivedEvent>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let (talker_result_tx, talker_result_rx) = mpsc::channel::<anyhow::Result<Vec<TalkerRecord>>>();

    let listener_name = listener.interface_name().to_string();
    let listener_clock_source = listener.clock_source().clone();
    let stream_uid = args.stream_uid;
    let talker_mac = talker.mac_address();
    let _receiver_handle = thread::spawn(move || {
        if let Err(e) = receiver_thread(
            &listener_name,
            &listener_clock_source,
            talker_mac,
            stream_uid,
            event_tx,
            ready_tx,
        ) {
            eprintln!("Receiver thread error: {e}");
        }
    });

    match ready_rx.recv() {
        Ok(Ok(())) => eprintln!("[Receiver ready]"),
        Ok(Err(error)) => return Err(anyhow::anyhow!("Receiver thread failed: {error}")),
        Err(_) => return Err(anyhow::anyhow!("Receiver thread panicked or was killed")),
    }

    let bytes_per_sample = usize::from(wav.header.bits_per_sample / 8);
    let bytes_per_second = wav.header.sample_rate as usize * usize::from(wav.header.channels) * bytes_per_sample;
    let total_audio_ns = wav.pcm_le.len() as u128 * 1_000_000_000u128 / bytes_per_second as u128;
    let total_audio_duration = Duration::from_nanos(
        u64::try_from(total_audio_ns).map_err(|_| anyhow::anyhow!("Input WAV is too large to schedule"))?,
    );

    let talker_name = talker.interface_name().to_string();
    let talker_clock_source = talker.clock_source().clone();
    let wav_header = wav.header;
    let wav_pcm_for_talker = Arc::clone(&wav.pcm_le);
    let dst_mac = listener.mac_address();
    let playback_delay_ms = args.playback_delay_ms;
    let _talker_handle = thread::spawn(move || {
        let result = talker_thread(
            &talker_name,
            &talker_clock_source,
            dst_mac,
            stream_uid,
            playback_delay_ms,
            samples_per_packet,
            &wav_header,
            wav_pcm_for_talker.as_ref(),
        );
        let _ = talker_result_tx.send(result);
    });
    eprintln!("[Talker: ~{:.2}s of audio]", total_audio_duration.as_secs_f64());

    let drain_deadline = Instant::now()
        + total_audio_duration
        + Duration::from_secs(1)
        + Duration::from_millis(u64::from(playback_delay_ms) * 3)
        + Duration::from_secs(args.timeout_secs);

    drain_loop(event_rx, talker_result_rx, listener.clock_source(), drain_deadline)
}

// ============================================================================
// Receiver Thread
// ============================================================================

#[allow(clippy::needless_pass_by_value)]
fn receiver_thread(
    iface_name: &str,
    clock_source: &ClockSource,
    talker_mac: MacAddr,
    stream_id_uid: u16,
    event_tx: mpsc::Sender<ReceivedEvent>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) -> anyhow::Result<()> {
    let iface =
        bin_utils::find_interface(iface_name).ok_or_else(|| anyhow::anyhow!("Interface '{iface_name}' not found"))?;

    let mut receiver = match EthernetAvtpReceiver::new(&iface) {
        Ok(receiver) => receiver,
        Err(error) => {
            let _ = ready_tx.send(Err(format!("Failed to open datalink channel on {iface_name}: {error}")));
            return Ok(());
        }
    };

    let ptp_clock = clock_source
        .open_clock()
        .map_err(|e| anyhow::anyhow!("Failed to open listener clock: {e}"))?;

    let stream_id = StreamID {
        mac_address: talker_mac,
        unique_id: stream_id_uid,
    };
    let mut listener = AafPcmListener::new(Some(stream_id));

    let _ = ready_tx.send(Ok(()));

    loop {
        let (phc_time_at_receive, avtpdu) = match receiver.recv_next_with(|| ptp_clock.time()) {
            Ok((Ok(timestamp), avtpdu)) => (timestamp, avtpdu),
            Ok((Err(error), _)) => {
                return Err(anyhow::anyhow!("Failed to read listener clock at receive: {error}"));
            }
            Err(error) => {
                return Err(anyhow::anyhow!("Receiver failed on {iface_name}: {error}"));
            }
        };

        if let Ok(Some(pcm)) = listener.process(&avtpdu)
            && event_tx
                .send(ReceivedEvent::Packet {
                    pcm,
                    phc_time_at_receive,
                })
                .is_err()
        {
            return Ok(());
        }
    }
}

// ============================================================================
// Talker Thread
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn talker_thread(
    iface_name: &str,
    clock_source: &ClockSource,
    dst_mac: MacAddr,
    stream_id_uid: u16,
    playback_delay_ms: u32,
    samples_per_packet: u32,
    wav_header: &wav::WavHeader,
    wav_pcm_le: &[u8],
) -> anyhow::Result<Vec<TalkerRecord>> {
    let iface =
        bin_utils::find_interface(iface_name).ok_or_else(|| anyhow::anyhow!("Interface '{iface_name}' not found"))?;

    let mut sender =
        EthernetAvtpSender::new(&iface, dst_mac).map_err(|e| anyhow::anyhow!("Failed to open sender: {e}"))?;

    let ptp_sync = PtpSynchronizedClock::new(
        clock_source
            .open_clock()
            .map_err(|e| anyhow::anyhow!("Failed to open talker clock: {e}"))?,
    )
    .map_err(|e| anyhow::anyhow!("Failed to create synchronized clock: {e}"))?;

    let ptp_clock = clock_source
        .open_clock()
        .map_err(|e| anyhow::anyhow!("Failed to open talker validation clock: {e}"))?;

    let stream_id = StreamID {
        mac_address: iface
            .mac
            .ok_or_else(|| anyhow::anyhow!("Interface has no MAC address"))?,
        unique_id: stream_id_uid,
    };

    let bit_depth = u8::try_from(wav_header.valid_bits_per_sample)
        .map_err(|_| anyhow::anyhow!("Invalid WAV bit depth: {}", wav_header.valid_bits_per_sample))?;
    let pcm_format =
        PcmFormat::from_wav_header(wav_header).map_err(|e| anyhow::anyhow!("Unsupported WAV format: {e}"))?;
    let sample_rate = SampleRate::from_hz(wav_header.sample_rate)
        .ok_or_else(|| anyhow::anyhow!("Unsupported sample rate: {}", wav_header.sample_rate))?;

    let mut talker = AafPcmTalker::new(
        stream_id,
        pcm_format,
        sample_rate,
        u10::new(wav_header.channels),
        bit_depth,
        ptp_sync,
        playback_delay_ms,
    )
    .map_err(|e| anyhow::anyhow!("Failed to create talker: {e}"))?;

    let bytes_per_sample = usize::from(wav_header.bits_per_sample / 8);
    let frame_size = usize::from(wav_header.channels) * bytes_per_sample;
    let chunk_size = usize::try_from(samples_per_packet)
        .ok()
        .and_then(|samples| samples.checked_mul(frame_size))
        .ok_or_else(|| anyhow::anyhow!("Packet payload size would overflow"))?;

    let packet_duration_ns = u64::from(samples_per_packet) * 1_000_000_000 / u64::from(wav_header.sample_rate);
    let start_pacing = Instant::now();
    let mut records = Vec::new();
    let mut seq_num = 0u8;

    for (frame_idx, chunk) in wav_pcm_le.chunks(chunk_size).enumerate() {
        let target_time = start_pacing + Duration::from_nanos(frame_idx as u64 * packet_duration_ns);
        if let Some(sleep_duration) = target_time.checked_duration_since(Instant::now()) {
            thread::sleep(sleep_duration);
        }

        let phc_time_at_send = ptp_clock
            .time()
            .map_err(|e| anyhow::anyhow!("Failed to read talker clock: {e}"))?;

        let mut payload_be = chunk.to_vec();
        swap_byte_order(&mut payload_be, bytes_per_sample);

        let avtpdu = talker
            .build_packet(Arc::from(payload_be))
            .map_err(|e| anyhow::anyhow!("Failed to build packet: {e}"))?;

        sender
            .send(&avtpdu)
            .map_err(|e| anyhow::anyhow!("Failed to send packet: {e}"))?;

        let avtp_timestamp_embedded = if let Avtpdu::Stream(stream_header) = &avtpdu {
            stream_header.generic.avtp_timestamp().as_u32()
        } else {
            unreachable!("AafPcmTalker always produces stream AVTPDUs");
        };

        records.push(TalkerRecord {
            frame_idx: frame_idx as u64,
            seq_num,
            phc_time_at_send,
            avtp_timestamp_embedded,
        });

        seq_num = seq_num.wrapping_add(1);
    }

    Ok(records)
}

// ============================================================================
// Drain Loop (Main Thread)
// ============================================================================

#[allow(clippy::needless_pass_by_value)]
fn drain_loop(
    event_rx: mpsc::Receiver<ReceivedEvent>,
    talker_result_rx: mpsc::Receiver<anyhow::Result<Vec<TalkerRecord>>>,
    clock_source: &ClockSource,
    deadline: Instant,
) -> anyhow::Result<(Vec<TalkerRecord>, Vec<ListenerRecord>, Vec<u8>)> {
    let ptp_clock = clock_source
        .open_clock()
        .map_err(|e| anyhow::anyhow!("Failed to open presentation clock: {e}"))?;

    let mut jitter_buf: BinaryHeap<Reverse<Buffered>> = BinaryHeap::new();
    let mut listener_records = Vec::new();
    let mut received_pcm_le = Vec::new();
    let mut frame_idx = 0u64;
    let mut talker_result: Option<anyhow::Result<Vec<TalkerRecord>>> = None;

    loop {
        while let Ok(ReceivedEvent::Packet {
            pcm,
            phc_time_at_receive,
        }) = event_rx.try_recv()
        {
            jitter_buf.push(Reverse(Buffered {
                presentation_time: pcm.avtp_timestamp.expand_near(phc_time_at_receive),
                phc_time_at_receive,
                pcm,
            }));
        }

        if talker_result.is_none() {
            match talker_result_rx.try_recv() {
                Ok(result) => talker_result = Some(result),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    talker_result = Some(Err(anyhow::anyhow!("Talker thread panicked")));
                }
            }
        }

        let current_ptp_time = ptp_clock.time()?;

        while let Some(Reverse(buffered)) = jitter_buf.peek() {
            let is_ready = current_ptp_time >= buffered.presentation_time;
            if !is_ready {
                break;
            }

            let buffered = jitter_buf.pop().expect("peeked item must still be present").0;
            let phc_time_at_present = ptp_clock.time()?;

            let bytes_per_sample = usize::from(buffered.pcm.bit_depth.get() / 8);
            let mut pcm_le = buffered.pcm.payload_be.to_vec();
            swap_byte_order(&mut pcm_le, bytes_per_sample);
            received_pcm_le.extend_from_slice(&pcm_le);

            listener_records.push(ListenerRecord {
                frame_idx,
                seq_num: buffered.pcm.seq_num,
                phc_time_at_receive: buffered.phc_time_at_receive,
                avtp_timestamp_embedded: buffered.pcm.avtp_timestamp.as_u32(),
                phc_time_at_present,
            });
            frame_idx += 1;
        }

        if talker_result.is_some() && jitter_buf.is_empty() {
            break;
        }
        if Instant::now() > deadline {
            eprintln!("[Timeout exceeded, exiting drain loop]");
            break;
        }

        thread::sleep(Duration::from_millis(1));
    }

    let talker_records = talker_result
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("Talker thread did not complete before the drain deadline"))?;

    Ok((talker_records, listener_records, received_pcm_le))
}

// ============================================================================
// Analysis
// ============================================================================

#[allow(clippy::cast_precision_loss)]
fn analyze_and_report(
    talker_records: &[TalkerRecord],
    listener_records: &[ListenerRecord],
    sent_pcm_le: &[u8],
    received_pcm_le: &[u8],
) -> anyhow::Result<()> {
    let packets_lost = talker_records.len().saturating_sub(listener_records.len());
    let packet_loss_pct = if talker_records.is_empty() {
        0.0
    } else {
        100.0 * packets_lost as f64 / talker_records.len() as f64
    };

    eprintln!("\n=== Results ===");
    eprintln!("Packets sent: {}", talker_records.len());
    eprintln!("Packets received: {}", listener_records.len());
    eprintln!("Packets lost: {packets_lost} ({packet_loss_pct:.1}%)");
    eprintln!("PCM bytes sent: {}", sent_pcm_le.len());
    eprintln!("PCM bytes received: {}", received_pcm_le.len());

    let mut failures = Vec::new();

    if talker_records.len() != listener_records.len() {
        failures.push(format!(
            "packet count mismatch: sent {}, received {}",
            talker_records.len(),
            listener_records.len()
        ));
    }

    if let Some((frame_idx, talker_record, listener_record)) =
        talker_records.iter().zip(listener_records).enumerate().find_map(
            |(frame_idx, (talker_record, listener_record))| {
                (talker_record.seq_num != listener_record.seq_num
                    || talker_record.avtp_timestamp_embedded != listener_record.avtp_timestamp_embedded)
                    .then_some((frame_idx, talker_record, listener_record))
            },
        )
    {
        failures.push(format!(
            "packet metadata mismatch at frame {frame_idx}: sent seq {} ts {}, received seq {} ts {}",
            talker_record.seq_num,
            talker_record.avtp_timestamp_embedded,
            listener_record.seq_num,
            listener_record.avtp_timestamp_embedded
        ));
    }

    if sent_pcm_le.len() != received_pcm_le.len() {
        failures.push(format!(
            "PCM length mismatch: sent {} bytes, received {} bytes",
            sent_pcm_le.len(),
            received_pcm_le.len()
        ));
    }

    if let Some(first_mismatch) = sent_pcm_le
        .iter()
        .zip(received_pcm_le)
        .position(|(sent, received)| sent != received)
    {
        failures.push(format!(
            "PCM payload mismatch at byte {}: sent 0x{:02x}, received 0x{:02x}",
            first_mismatch, sent_pcm_le[first_mismatch], received_pcm_le[first_mismatch]
        ));
    }

    if failures.is_empty() {
        eprintln!("Validation: PASS");
        return Ok(());
    }

    eprintln!("Validation: FAIL");
    for failure in &failures {
        eprintln!("  - {failure}");
    }

    anyhow::bail!("Integration test validation failed")
}

// ============================================================================
// File I/O
// ============================================================================

fn save_outputs(
    output_dir: &Path,
    talker_records: &[TalkerRecord],
    listener_records: &[ListenerRecord],
    received_pcm_le: &[u8],
    wav_header: &wav::WavHeader,
) -> anyhow::Result<()> {
    let talker_csv_path = output_dir.join("talker.csv");
    csv::write_csv(&talker_csv_path, talker_records)
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", talker_csv_path.display()))?;

    let listener_csv_path = output_dir.join("listener.csv");
    csv::write_csv(&listener_csv_path, listener_records)
        .map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", listener_csv_path.display()))?;

    let wav_path = output_dir.join("received.wav");
    let mut wav_file = File::create(&wav_path)?;
    wav::Wav::from_header_and_data(*wav_header, received_pcm_le.to_vec()).write(&mut wav_file)?;

    eprintln!("Talker CSV: {}", talker_csv_path.display());
    eprintln!("Listener CSV: {}", listener_csv_path.display());
    eprintln!("Received WAV: {}", wav_path.display());

    Ok(())
}

// ============================================================================
// Utilities
// ============================================================================

fn swap_byte_order(buf: &mut [u8], word_size: usize) {
    for chunk in buf.chunks_exact_mut(word_size) {
        chunk.reverse();
    }
}
