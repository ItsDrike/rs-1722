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
pub(crate) trait BitDecode: Sized {
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
pub(crate) trait BitEncode {
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

impl<E> IOWrapError<E> {
    /// Transforms one [`IOWrapError`] into another, mapping the [`Self::Specific`] variant, while
    /// leaving [`Self::Io`] variants unchanged.
    ///
    /// This is useful when you need to lift a lower-level domain error into a higher-level
    /// error type that wraps it, while preserving any I/O errors that may have occurred.
    ///
    /// # Example
    /// ```rust
    /// use std::io;
    ///
    /// #[derive(Debug, thiserror::Error)]
    /// #[error("parse error")]
    /// struct ParseError;
    ///
    /// #[derive(Debug, thiserror::Error)]
    /// enum DomainError {
    ///     #[error(transparent)]
    ///     Parse(#[from] ParseError),
    ///
    ///     #[error("validation failed")]
    ///     Validation,
    /// }
    ///
    /// let io_err: IOWrapError<ParseError> = IOWrapError::Io(io::Error::last_os_error());
    /// let specific_err: IOWrapError<ParseError> = IOWrapError::Specific(ParseError);
    ///
    /// // Map to higher-level DomainError
    /// let mapped_io: IOWrapError<DomainError> = io_err.map_specific(DomainError::from);
    /// let mapped_specific: IOWrapError<DomainError> = specific_err.map_specific(DomainError::from);
    ///
    /// assert!(matches!(mapped_io, IOWrapError::Io(_)));
    /// assert!(matches!(mapped_specific, IOWrapError::Specific(DomainError::Parse(_))));
    /// ```
    ///
    /// # Note
    /// This method only transforms the error contained in the `Specific` variant.
    /// If you need to transform an `IOWrapError<E>` into `IOWrapError<F>` where `E: Into<F>`,
    /// consider implementing `From<IOWrapError<E>> for IOWrapError<F>` instead.
    ///
    /// Unfortunately, a generic impl for this is not possible as Rust doesn't allow us to enforce a
    /// bound of `E: NotSame<F>`.
    pub fn map_specific<F, NewE>(self, f: F) -> IOWrapError<NewE>
    where
        F: FnOnce(E) -> NewE,
    {
        match self {
            Self::Io(e) => IOWrapError::Io(e),
            Self::Specific(e) => IOWrapError::Specific(f(e)),
        }
    }
}
