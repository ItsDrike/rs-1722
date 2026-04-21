use std::io;

use bitstream_io::{BigEndian, BitRead, BitReader, BitWrite, BitWriter};
use pnet::util::MacAddr;

use crate::io::enc_dec::{BitDecode, BitEncode};

/// Canonical 64-bit stream identifier used across AVTP/TSN components.
///
/// In practice this is the tuple `(talker_mac, unique_id)`, allowing one
/// talker node to publish multiple distinct streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamID {
    /// MAC address of the talker that originates the stream.
    pub mac_address: MacAddr, // 48-bit

    /// Per-talker discriminator for multiple streams from the same MAC.
    pub unique_id: u16, // 16-bit
}

impl BitEncode for StreamID {
    type Error = io::Error;

    fn encode<W: io::Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error> {
        writer.write_from::<[u8; 6]>(self.mac_address.into())?;
        writer.write_from(self.unique_id)?;

        Ok(())
    }
}

impl BitDecode for StreamID {
    type Error = io::Error;

    fn decode<R: io::Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error> {
        let mac = reader.read_to::<[u8; 6]>()?;
        let unique_id = reader.read_to::<u16>()?;

        Ok(Self {
            mac_address: MacAddr::from(mac),
            unique_id,
        })
    }
}
