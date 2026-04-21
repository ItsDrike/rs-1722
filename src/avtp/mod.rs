// OCTET: byte
// QUADLET: 4 octets, big endian
//

mod headers;
mod stream_id;
mod subtype;

pub use headers::{AvtpAlternativeHeader, AvtpCommonHeader, AvtpControlHeader, AvtpStreamHeader, HeaderType};
pub use subtype::{EncapsulationStyle, Subtype, UnknownSubtype};

pub const ETHER_TYPE: u16 = 0x22F0;
