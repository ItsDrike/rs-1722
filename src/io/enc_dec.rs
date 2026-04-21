use std::io::{self, Read, Write};

use bitstream_io::{BigEndian, BitReader, BitWriter};

/// Trait for decoding a value from a bit-level reader.
///
/// Implementations are expected to read exactly the number of bits required
/// to reconstruct `Self` from the provided [`BitReader`], using big-endian
/// bit ordering.
///
/// This is primarily intended for parsing binary protocols with non-byte-aligned
/// fields (e.g., AVTP headers).
pub trait BitDecode: Sized {
    /// Error type returned when decoding fails.
    type Error;

    /// Decodes an instance of `Self` from the given [`BitReader`].
    ///
    /// # Errors
    ///
    /// Implementations may return an error if:
    /// - the underlying reader produces an I/O error
    /// - the input data is malformed or invalid for the type
    fn decode<R: Read>(reader: &mut BitReader<R, BigEndian>) -> Result<Self, Self::Error>;
}

/// Trait for encoding a value into a bit-level writer.
///
/// Implementations are expected to write the exact bit representation of `Self`
/// into the provided [`BitWriter`], using big-endian bit ordering.
///
/// This is primarily intended for serializing binary protocols with non-byte-aligned
/// fields (e.g., AVTP headers).
pub trait BitEncode {
    /// Error type returned when encoding fails.
    type Error;

    /// Encodes `self` into the given [`BitWriter`].
    ///
    /// # Errors
    ///
    /// Implementations may return an error if:
    /// - the underlying writer produces an I/O error
    /// - the value cannot be represented within the required bit width
    fn encode<W: Write>(&self, writer: &mut BitWriter<W, BigEndian>) -> Result<(), Self::Error>;
}

/// A helper error type that combines I/O errors with type-specific decoding errors.
///
/// This is useful for [`BitDecode`] implementations where both:
/// - low-level I/O failures (e.g., read errors), and
/// - higher-level parsing or validation errors
///   need to be represented.
///
/// The [`From<io::Error>`] implementation allows seamless use of the `?` operator
/// when reading from a [`BitReader`].
#[derive(Debug, thiserror::Error)]
pub enum IOWrapError<E> {
    /// An error originating from the underlying I/O source.
    #[error("io error")]
    Io(#[from] io::Error),

    /// A type-specific error produced during decoding or validation.
    #[error(transparent)]
    Specific(E),
}
