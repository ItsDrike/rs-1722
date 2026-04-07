use std::{fmt, num::ParseIntError, str::FromStr};
use thiserror::Error;

/// Errors that can occur when parsing a [`MacAddress`] from a string.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum MacParseError {
    /// The input does not have the expected `xx:xx:xx:xx:xx:xx` format.
    #[error("invalid MAC address format")]
    InvalidFormat,
    /// One of the octets is not valid hexadecimal.
    #[error("invalid MAC address hex value")]
    InvalidHex(ParseIntError),
}

/// Represents a 48-bit MAC (Media Access Control) address.
///
/// Internally stored as 6 raw bytes. This type provides parsing and
/// formatting utilities for working with MAC addresses in a type-safe way.
///
/// Common string representation:
/// `"00:1b:21:ed:39:3e"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddress(pub [u8; 6]);

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
        )
    }
}

impl FromStr for MacAddress {
    type Err = MacParseError;

    /// Parses a MAC address from a string in the form `"xx:xx:xx:xx:xx:xx"`.
    ///
    /// # Errors
    /// Returns:
    /// - [`MacParseError::InvalidFormat`] if the string does not contain exactly 6 octets
    /// - [`MacParseError::InvalidHex`] if any octet is not valid hexadecimal
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; 6];
        let mut parts = s.split(':');

        for byte in &mut bytes {
            let part = parts.next().ok_or(MacParseError::InvalidFormat)?;
            *byte = u8::from_str_radix(part, 16).map_err(MacParseError::InvalidHex)?;
        }

        if parts.next().is_some() {
            return Err(MacParseError::InvalidFormat);
        }

        Ok(Self(bytes))
    }
}
