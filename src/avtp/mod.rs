// OCTET: byte
// QUADLET: 4 octets, big endian
//

mod avtpdu;
pub mod headers;
mod stream_id;
pub mod subtype;

pub use avtpdu::Avtpdu;

pub const ETHER_TYPE: u16 = 0x22F0;
