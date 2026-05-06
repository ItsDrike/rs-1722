use std::io::{self, Cursor};

use pnet::datalink::{self, Channel, Config, DataLinkReceiver, DataLinkSender, NetworkInterface};
use pnet::util::MacAddr;
use thiserror::Error;

use crate::avtp::{Avtpdu, AvtpduError};
use crate::io::enc_dec::IOWrapError;

const ETHER_TYPE: u16 = 0x22F0;

#[derive(Debug, Error)]
pub enum EthernetSenderError {
    #[error("Failed to open datalink channel: {0}")]
    ChannelOpen(io::Error),

    #[error("Interface has no MAC address")]
    NoMac,

    #[error("Packet serialization failed: {0}")]
    Serialize(#[from] IOWrapError<AvtpduError>),

    #[error("Failed to send frame: {0}")]
    Send(io::Error),
}

pub struct EthernetAvtpSender {
    tx: Box<dyn DataLinkSender>,
    src_mac: MacAddr,
    dst_mac: MacAddr,
    frame_buf: Vec<u8>,
}

impl EthernetAvtpSender {
    /// Creates a new Ethernet AVTP sender on the specified interface.
    ///
    /// # Errors
    ///
    /// Returns [`EthernetSenderError::NoMac`] if the interface has no MAC address.
    /// Returns [`EthernetSenderError::ChannelOpen`] if the datalink channel cannot be opened.
    pub fn new(interface: &NetworkInterface, dst_mac: MacAddr) -> Result<Self, EthernetSenderError> {
        let src_mac = interface.mac.ok_or(EthernetSenderError::NoMac)?;
        let (tx, _rx) = match datalink::channel(interface, Config::default()) {
            Ok(Channel::Ethernet(tx, rx)) => (tx, rx),
            Ok(_) => {
                return Err(EthernetSenderError::ChannelOpen(io::Error::other(
                    "unexpected non-ethernet channel type",
                )));
            }
            Err(e) => return Err(EthernetSenderError::ChannelOpen(e)),
        };
        Ok(Self {
            tx,
            src_mac,
            dst_mac,
            frame_buf: Vec::with_capacity(1514),
        })
    }

    /// Sends an AVTP packet as an Ethernet frame.
    ///
    /// The packet is wrapped in an Ethernet II header with the source and destination MACs
    /// and the AVTP `EtherType` (0x22F0).
    ///
    /// # Errors
    ///
    /// Returns [`EthernetSenderError::Serialize`] if the packet cannot be serialized.
    /// Returns [`EthernetSenderError::Send`] if the frame cannot be sent on the interface.
    pub fn send(&mut self, packet: &Avtpdu) -> Result<(), EthernetSenderError> {
        self.frame_buf.clear();

        // Ethernet II header: dst (6) + src (6) + ethertype (2)
        self.frame_buf.extend_from_slice(&self.dst_mac.octets());
        self.frame_buf.extend_from_slice(&self.src_mac.octets());
        self.frame_buf.extend_from_slice(&ETHER_TYPE.to_be_bytes());

        // AVTP payload
        packet.write(&mut self.frame_buf)?;

        self.tx
            .send_to(&self.frame_buf, None)
            .ok_or_else(|| EthernetSenderError::Send(io::Error::other("send returned None")))?
            .map_err(EthernetSenderError::Send)
    }
}

#[derive(Debug, Error)]
pub enum EthernetReceiverError {
    #[error("Failed to open datalink channel: {0}")]
    ChannelOpen(io::Error),

    #[error("Failed to receive frame: {0}")]
    Receive(io::Error),

    #[error("Failed to parse AVTP packet: {0}")]
    Parse(#[from] IOWrapError<AvtpduError>),
}

pub struct EthernetAvtpReceiver {
    rx: Box<dyn DataLinkReceiver>,
}

impl EthernetAvtpReceiver {
    /// Creates a new Ethernet AVTP receiver on the specified interface.
    ///
    /// # Errors
    ///
    /// Returns [`EthernetReceiverError::ChannelOpen`] if the datalink channel cannot be opened.
    pub fn new(interface: &NetworkInterface) -> Result<Self, EthernetReceiverError> {
        let (_tx, rx) = match datalink::channel(interface, Config::default()) {
            Ok(Channel::Ethernet(tx, rx)) => (tx, rx),
            Ok(_) => {
                return Err(EthernetReceiverError::ChannelOpen(io::Error::other(
                    "unexpected non-ethernet channel type",
                )));
            }
            Err(e) => return Err(EthernetReceiverError::ChannelOpen(e)),
        };
        Ok(Self { rx })
    }

    /// Receives the next AVTP packet from the interface.
    ///
    /// Blocks until an AVTP packet (`EtherType` 0x22F0) is received.
    /// Non-AVTP frames are silently filtered out.
    ///
    /// # Errors
    ///
    /// Returns [`EthernetReceiverError::Receive`] if frame reception fails.
    /// Returns [`EthernetReceiverError::Parse`] if the AVTP packet cannot be parsed.
    pub fn recv_next(&mut self) -> Result<Avtpdu, EthernetReceiverError> {
        loop {
            let frame = self.rx.next().map_err(EthernetReceiverError::Receive)?;

            // Ethernet II: dst(6) + src(6) + ethertype(2) = 14 bytes header
            if frame.len() < 14 {
                continue;
            }

            let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
            if ethertype != ETHER_TYPE {
                continue;
            }

            let payload = &frame[14..];
            let mut cursor = Cursor::new(payload);
            return Ok(Avtpdu::read(&mut cursor)?);
        }
    }
}
