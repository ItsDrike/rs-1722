use std::error::Error;

use pnet::util::MacAddr;

use crate::avtp::{Avtpdu, StreamID};

/// Specifies which streams to accept when listening to AVTP packets.
#[derive(Debug, Clone, Copy)]
pub enum StreamFilter {
    /// Accept packets from any stream.
    Any,

    /// Accept packets only from a specific MAC address, regardless of stream UID.
    MacOnly(MacAddr),

    /// Accept packets only from an exact stream ID (MAC + UID combination).
    Exact(StreamID),
}

impl StreamFilter {
    /// Returns `true` if the given stream ID matches this filter.
    #[must_use]
    pub fn matches(&self, stream_id: StreamID) -> bool {
        match self {
            Self::Any => true,
            Self::MacOnly(mac) => stream_id.mac_address == *mac,
            Self::Exact(id) => stream_id == *id,
        }
    }
}

/// Encodes audio into AVTP packets.
///
/// Manages the state of a single outgoing audio stream, including sequence numbering,
/// timestamp generation, and packet construction.
pub trait StreamTalker: Send {
    /// Error type returned by talker operations.
    type Error: Error + Send + 'static;

    /// Builds and returns the next AVTP packet for transmission.
    ///
    /// Updates the sequence number and timestamp on each call.
    ///
    /// # Arguments
    ///
    /// - `payload`: Big-endian audio samples (Arc-wrapped for zero-copy).
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the packet construction fails.
    fn build_packet(&mut self, payload: std::sync::Arc<[u8]>) -> Result<Avtpdu, Self::Error>;
}

/// Decodes audio from AVTP packets.
///
/// Filters incoming packets by stream ID and detects sequence number gaps
/// to report lost packets.
pub trait StreamListener: Send {
    /// The decoded output type produced by processing packets.
    type Output;

    /// Error type returned by listener operations.
    type Error: Error + Send + 'static;

    /// Processes an incoming AVTP packet.
    ///
    /// Returns:
    /// - `Ok(Some(output))` if the packet is accepted and contains valid data
    /// - `Ok(None)` if the packet is filtered out or not applicable to this listener
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] if the packet fails to parse.
    fn process(&mut self, pdu: &Avtpdu) -> Result<Option<Self::Output>, Self::Error>;
}
