use std::collections::HashMap;
use thiserror::Error;

/// Represents the state of a PTP port as defined by IEEE 1588 (`portDS.portState`).
///
/// This corresponds to the `portState` field from the `PORT_DATA_SET`.
/// It reflects the internal state machine of the PTP protocol.
///
/// The most important states are:
/// - [`PortState::Master`] -> this node is acting as the grandmaster or a master
/// - [`PortState::Slave`] -> this node is synchronized to a master
/// - [`PortState::Listening`] -> no master selected yet
/// - [`PortState::Uncalibrated`] -> master selected but synchronization not complete
///
/// For synchronization detection, [`PortState::Slave`] is the authoritative signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortState {
    /// Initial state during startup.
    Initializing,
    /// Fault detected.
    Faulty,
    /// Port is disabled.
    Disabled,
    /// Waiting for Announce messages from a master.
    Listening,
    /// Preparing to become master.
    PreMaster,
    /// Acting as a master.
    Master,
    /// Passive (not participating in synchronization actively).
    Passive,
    /// Master selected, but servo not yet locked.
    Uncalibrated,
    /// Fully synchronized to a master.
    Slave,
    /// Unknown or unrecognized state.
    Unknown(String),
}

impl PortState {
    /// Parses a `portState` string from `pmc` output.
    #[must_use]
    fn parse(s: &str) -> Self {
        match s {
            "INITIALIZING" => Self::Initializing,
            "FAULTY" => Self::Faulty,
            "DISABLED" => Self::Disabled,
            "LISTENING" => Self::Listening,
            "PRE_MASTER" => Self::PreMaster,
            "MASTER" => Self::Master,
            "PASSIVE" => Self::Passive,
            "UNCALIBRATED" => Self::Uncalibrated,
            "SLAVE" => Self::Slave,
            s => Self::Unknown(s.to_owned()),
        }
    }
}

/// Represents the `TIME_STATUS_NP` dataset.
///
/// This dataset provides runtime information about synchronization state,
/// including offset, grandmaster presence, and timing metadata.
#[derive(Debug, Clone)]
pub struct TimeStatusNp {
    /// Current offset from the master clock in nanoseconds.
    ///
    /// This value is meaningful primarily when the port state is [`PortState::Slave`].
    pub master_offset_ns: i64,

    /// Timestamp of the last ingress event.
    pub ingress_time: i128,

    /// Frequency offset relative to the master.
    pub cumulative_scaled_rate_offset: f64,

    /// Indicates whether a grandmaster is known.
    ///
    /// `true` means a master has been discovered, but does not guarantee synchronization.
    pub gm_present: bool,

    /// Identity of the current grandmaster clock.
    ///
    /// Typically derived from the MAC address of the master.
    pub gm_identity: String,
}

/// Represents the `PORT_DATA_SET`.
///
/// This dataset contains configuration and state information for a PTP port.
/// It includes the authoritative `portState`, which determines synchronization status.
///
/// Unlike [`TimeStatusNp`], this dataset reflects protocol state rather than
/// runtime measurements.
#[derive(Debug, Clone)]
pub struct PortDataSet {
    /// Unique identifier of the port.
    pub port_identity: String,

    /// Current state of the port state machine.
    pub port_state: PortState,

    /// Minimum delay request interval (log2 seconds).
    pub log_min_delay_req_interval: i8,

    /// Mean path delay to peer (nanoseconds).
    pub peer_mean_path_delay: i64,

    /// Announce message interval (log2 seconds).
    pub log_announce_interval: i8,

    /// Number of missed announces before timeout.
    pub announce_receipt_timeout: u8,

    /// Sync message interval (log2 seconds).
    pub log_sync_interval: i8,

    /// Delay mechanism in use.
    ///
    /// Typically:
    /// - `1` = E2E
    /// - `2` = P2P
    pub delay_mechanism: u8,

    /// Peer delay request interval (log2 seconds).
    pub log_min_pdelay_req_interval: i8,

    /// PTP version number.
    pub version_number: u8,
}

/// Represents the `CURRENT_DATA_SET`.
///
/// This dataset provides information about the current synchronization relationship,
/// including topology distance and measured delay.
#[derive(Debug, Clone)]
pub struct CurrentDataSet {
    /// Number of hops between this clock and the grandmaster.
    pub steps_removed: u32,

    /// Offset from the master clock in nanoseconds (floating-point representation).
    ///
    /// This may differ slightly from `master_offset_ns` due to representation differences.
    pub offset_from_master_ns: f64,

    /// Mean path delay between this node and the master in nanoseconds.
    pub mean_path_delay_ns: f64,
}

/// Snapshot of the current PTP state.
///
/// This aggregates multiple PTP management datasets into a single
/// structured representation obtained via `pmc`.
///
/// This struct is intended to represent a consistent view of the PTP state
/// at a specific point in time.
///
/// If an answer was not returned for a given dataset request from `pmc`, it
/// will be represented as `None`.
#[derive(Debug, Clone)]
pub struct PtpSnapshot {
    /// Runtime timing and synchronization information.
    pub time_status: TimeStatusNp,

    /// Protocol state and port configuration.
    pub port_data: PortDataSet,

    /// Current topology and delay information.
    pub current_data: CurrentDataSet,
}

impl PtpSnapshot {
    /// Returns `true` if the clock is synchronized to a master.
    ///
    /// This is determined exclusively by the port state:
    /// synchronization is considered established only when the state is [`PortState::Slave`].
    #[must_use]
    pub const fn is_synchronized(&self) -> bool {
        matches!(self.port_data.port_state, PortState::Slave)
    }

    /// Returns `true` if this instance is acting as a master.
    #[must_use]
    pub const fn is_master(&self) -> bool {
        matches!(self.port_data.port_state, PortState::Master)
    }

    /// Returns the current offset from the master clock in nanoseconds.
    ///
    /// This is a convenience wrapper around [`TimeStatusNp::master_offset_ns`].
    #[must_use]
    #[inline]
    pub fn offset_ns(&self) -> Option<i64> {
        self.is_synchronized().then_some(self.time_status.master_offset_ns)
    }

    /// Parses the output of a `pmc` invocation into a [`PtpSnapshot`].
    ///
    /// The input is expected to be the combined output of the following commands:
    /// - `GET TIME_STATUS_NP`
    /// - `GET PORT_DATA_SET`
    /// - `GET CURRENT_DATA_SET`
    ///
    /// The output is processed as a sequence of dataset response blocks. Each block
    /// is identified and delegated to the corresponding dataset parser.
    ///
    /// Datasets that were not present in the `pmc` output are represented as `None`.
    /// (This can happen if `ptp4l` is not running.)
    ///
    /// # Errors
    /// Returns [`DatasetParseError`] if a dataset block is present but malformed,
    /// such as when a required field is missing or cannot be parsed. The error
    /// contains both the dataset in which the failure occurred and the specific
    /// field-level parsing error.
    pub fn parse_pmc_output(text: &str) -> Result<Self, SnapshotParseError> {
        let mut time_status = None;
        let mut port_data = None;
        let mut current_data = None;

        for (kind, lines) in Self::parse_blocks(text) {
            match kind {
                "TIME_STATUS_NP" => {
                    if time_status.is_some() {
                        return Err(SnapshotParseError::DuplicateDataset("TIME_STATUS_NP"));
                    }

                    time_status = Some(TimeStatusNp::parse_block(&lines).map_err(|e| {
                        SnapshotParseError::DatasetError(DatasetParseError {
                            dataset: "TIME_STATUS_NP",
                            field_err: e,
                        })
                    })?);
                }
                "PORT_DATA_SET" => {
                    if port_data.is_some() {
                        return Err(SnapshotParseError::DuplicateDataset("PORT_DATA_SET"));
                    }

                    port_data = Some(PortDataSet::parse_block(&lines).map_err(|e| {
                        SnapshotParseError::DatasetError(DatasetParseError {
                            dataset: "PORT_DATA_SET",
                            field_err: e,
                        })
                    })?);
                }
                "CURRENT_DATA_SET" => {
                    if current_data.is_some() {
                        return Err(SnapshotParseError::DuplicateDataset("CURRENT_DATA_SET"));
                    }

                    current_data = Some(CurrentDataSet::parse_block(&lines).map_err(|e| {
                        SnapshotParseError::DatasetError(DatasetParseError {
                            dataset: "CURRENT_DATA_SET",
                            field_err: e,
                        })
                    })?);
                }
                _ => {}
            }
        }

        Ok(Self {
            time_status: time_status.ok_or(SnapshotParseError::MissingDataset("TIME_STATUS_NP"))?,
            port_data: port_data.ok_or(SnapshotParseError::MissingDataset("PORT_DATA_SET"))?,
            current_data: current_data.ok_or(SnapshotParseError::MissingDataset("CURRENT_DATA_SET"))?,
        })
    }

    /// Splits raw `pmc` output into dataset response blocks.
    ///
    /// The input is expected to be the textual output of a `pmc` invocation.
    /// This function groups lines into logical blocks, where each block corresponds
    /// to a single `RESPONSE MANAGEMENT <DATASET>` section.
    ///
    /// Each returned entry contains:
    /// - The dataset identifier (e.g., `"TIME_STATUS_NP"`)
    /// - The associated field lines belonging to that dataset
    ///
    /// Lines that are empty or start with `"sending:"` are ignored, as they are
    /// considered protocol noise and not part of any dataset.
    ///
    /// Blocks are detected by identifying header lines containing
    /// `"RESPONSE MANAGEMENT"` that are not indented. All subsequent indented
    /// lines are treated as part of that block until the next header is encountered.
    ///
    /// # Notes
    /// - The order of returned blocks matches the order in the input.
    /// - The function does not validate or parse the contents of blocks; it only
    ///   performs structural grouping.
    /// - Duplicate or missing datasets are not handled here and must be validated
    ///   by the caller.
    fn parse_blocks(text: &str) -> Vec<(&str, Vec<&str>)> {
        let mut blocks = Vec::new();

        let mut current_kind: Option<&str> = None;
        let mut current_lines: Vec<&str> = Vec::new();

        for line in text.lines() {
            let trimmed = line.trim();

            if trimmed.is_empty() || trimmed.starts_with("sending:") {
                continue;
            }

            if trimmed.contains("RESPONSE MANAGEMENT") {
                if let Some(kind) = current_kind.take() {
                    blocks.push((kind, std::mem::take(&mut current_lines)));
                }

                current_kind = trimmed.split_whitespace().last();
                continue;
            }

            if current_kind.is_some() {
                current_lines.push(trimmed);
            }
        }

        if let Some(kind) = current_kind {
            blocks.push((kind, current_lines));
        }

        blocks
    }
}

/// Error that can occur while parsing individual fields within a dataset block.
///
/// These errors indicate that a dataset block was found, but its contents
/// were malformed or incomplete.
///
/// This typically means that the `pmc` output is corrupted, truncated, or
/// does not match the expected format.
#[derive(Debug, Error)]
pub enum FieldParseError {
    /// A required field was not present in the dataset block.
    #[error("missing required field `{0}`")]
    MissingField(&'static str),

    /// A field was present but could not be parsed into the expected type.
    #[error("invalid value for field `{0}`")]
    InvalidField(&'static str),
}

/// Error that occurred while parsing a specific dataset.
///
/// This wraps a [`FieldParseError`] and adds context about which dataset
/// the failure occurred in.
///
/// This is useful when parsing multiple datasets from a single `pmc` output.
#[derive(Debug, Error)]
#[error("failed to parse dataset `{dataset}`: {field_err}")]
pub struct DatasetParseError {
    /// Name of the dataset being parsed (e.g., `"TIME_STATUS_NP"`).
    pub dataset: &'static str,

    /// The underlying field-level parsing error.
    pub field_err: FieldParseError,
}

/// Error that can occur while constructing a [`PtpSnapshot`] from `pmc` output.
///
/// This represents higher-level structural issues in the parsed data,
/// such as missing datasets, duplicate datasets, or dataset-level parsing failures.
#[derive(Debug, Error)]
pub enum SnapshotParseError {
    /// A required dataset was not present in the `pmc` output.
    #[error("missing required dataset `{0}`")]
    MissingDataset(&'static str),

    /// A dataset appeared more than once in the `pmc` output.
    ///
    /// This indicates malformed or unexpected output.
    #[error("duplicate dataset `{0}` encountered")]
    DuplicateDataset(&'static str),

    /// A dataset was present but failed to parse correctly.
    #[error(transparent)]
    DatasetError(#[from] DatasetParseError),
}

/// Converts a slice of dataset lines into a key-value map.
///
/// Each input line is expected to follow the format:
/// `<key><whitespace><value>`
///
/// The function splits each line at the first whitespace boundary,
/// treating the left-hand side as the key and the remainder as the value.
///
/// Leading whitespace in the value is trimmed to account for column-aligned
/// `pmc` output formatting.
///
/// # Notes
/// - Lines that do not match the expected format are silently ignored.
/// - If duplicate keys are encountered, the last occurrence wins.
/// - This function performs no validation of keys or values; it is purely
///   a structural transformation used by dataset parsers.
fn to_map<'a>(lines: &[&'a str]) -> HashMap<&'a str, &'a str> {
    let mut map = HashMap::new();

    for line in lines {
        if let Some((key, value)) = line.split_once(char::is_whitespace) {
            // The fields are column aligned, trim the leading whitespaces after the split
            let value = value.trim();
            map.insert(key, value);
        }
    }

    map
}

impl TimeStatusNp {
    fn parse_block(lines: &[&str]) -> Result<Self, FieldParseError> {
        let map = to_map(lines);

        Ok(Self {
            master_offset_ns: map
                .get("master_offset")
                .ok_or(FieldParseError::MissingField("master_offset"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("master_offset"))?,
            ingress_time: map
                .get("ingress_time")
                .ok_or(FieldParseError::MissingField("ingress_time"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("ingress_time"))?,
            cumulative_scaled_rate_offset: map
                .get("cumulativeScaledRateOffset")
                .ok_or(FieldParseError::MissingField("cumulativeScaledRateOffset"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("cumulativeScaledRateOffset"))?,
            gm_present: match *map.get("gmPresent").ok_or(FieldParseError::MissingField("gmPresent"))? {
                "true" => true,
                "false" => false,
                _ => return Err(FieldParseError::InvalidField("gmPresent")),
            },
            gm_identity: map
                .get("gmIdentity")
                .copied() // &&str -> &str
                .ok_or(FieldParseError::MissingField("gmIdentity"))?
                .to_string(),
        })
    }
}

impl PortDataSet {
    fn parse_block(lines: &[&str]) -> Result<Self, FieldParseError> {
        let map = to_map(lines);

        Ok(Self {
            port_identity: map
                .get("portIdentity")
                .copied() // &&str -> &str
                .ok_or(FieldParseError::MissingField("portIdentity"))?
                .to_string(),
            port_state: PortState::parse(map.get("portState").ok_or(FieldParseError::MissingField("portState"))?),
            log_min_delay_req_interval: map
                .get("logMinDelayReqInterval")
                .ok_or(FieldParseError::MissingField("logMinDelayReqInterval"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("logMinDelayReqInterval"))?,
            peer_mean_path_delay: map
                .get("peerMeanPathDelay")
                .ok_or(FieldParseError::MissingField("peerMeanPathDelay"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("peerMeanPathDelay"))?,
            log_announce_interval: map
                .get("logAnnounceInterval")
                .ok_or(FieldParseError::MissingField("logAnnounceInterval"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("logAnnounceInterval"))?,
            announce_receipt_timeout: map
                .get("announceReceiptTimeout")
                .ok_or(FieldParseError::MissingField("announceReceiptTimeout"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("announceReceiptTimeout"))?,
            log_sync_interval: map
                .get("logSyncInterval")
                .ok_or(FieldParseError::MissingField("logSyncInterval"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("logSyncInterval"))?,
            delay_mechanism: map
                .get("delayMechanism")
                .ok_or(FieldParseError::MissingField("delayMechanism"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("delayMechanism"))?,
            log_min_pdelay_req_interval: map
                .get("logMinPdelayReqInterval")
                .ok_or(FieldParseError::MissingField("logMinPdelayReqInterval"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("logMinPdelayReqInterval"))?,
            version_number: map
                .get("versionNumber")
                .ok_or(FieldParseError::MissingField("versionNumber"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("versionNumber"))?,
        })
    }
}

impl CurrentDataSet {
    fn parse_block(lines: &[&str]) -> Result<Self, FieldParseError> {
        let map = to_map(lines);

        Ok(Self {
            steps_removed: map
                .get("stepsRemoved")
                .ok_or(FieldParseError::MissingField("stepsRemoved"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("stepsRemoved"))?,
            offset_from_master_ns: map
                .get("offsetFromMaster")
                .ok_or(FieldParseError::MissingField("offsetFromMaster"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("offsetFromMaster"))?,
            mean_path_delay_ns: map
                .get("meanPathDelay")
                .ok_or(FieldParseError::MissingField("meanPathDelay"))?
                .parse()
                .map_err(|_| FieldParseError::InvalidField("meanPathDelay"))?,
        })
    }
}
