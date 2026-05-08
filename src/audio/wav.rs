//! RIFF/WAVE parsing and writing utilities.
//!
//! This module supports two complementary use cases:
//! - parsing enough of a WAV stream to expose the `b"fmt "` metadata and begin
//!   streaming the `b"data"` payload from the same reader
//! - reading or writing a whole WAV value in memory while preserving chunk order
//!
//! When parsing a preamble, the reader is left positioned at the first byte of
//! the `b"data"` payload so callers can continue consuming sample bytes
//! directly.

use std::{
    io::{self, Read, Write},
    ops::RangeInclusive,
};

use num_enum::{FromPrimitive, IntoPrimitive};
use thiserror::Error;

const RIFF: [u8; 4] = *b"RIFF";
const WAVE: [u8; 4] = *b"WAVE";
const FMT: [u8; 4] = *b"fmt ";
const DATA: [u8; 4] = *b"data";
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const KS_SUBFORMAT_GUID_SUFFIX: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

#[derive(Debug, Error)]
/// Errors returned while reading or writing WAV data.
pub enum WavError {
    #[error("Invalid WAV magic: {found:?} (expected {expected:?})")]
    /// The RIFF or WAVE signature did not match the expected 4-byte tag.
    InvalidMagic { found: [u8; 4], expected: [u8; 4] },

    #[error("Invalid chunk size for {chunk_id:?}: found {found_size} bytes, expected {expected_size:?}")]
    /// A chunk size did not satisfy the bounds required by its format.
    InvalidChunkSize {
        /// Four-byte RIFF chunk identifier.
        chunk_id: [u8; 4],
        /// Size declared for the chunk in the stream.
        found_size: u32,
        /// Inclusive range of valid sizes for the chunk.
        expected_size: RangeInclusive<u32>,
    },

    #[error("Data chunk found before b\"fmt \" chunk")]
    /// A `b"data"` chunk was encountered before any `b"fmt "` chunk had been parsed.
    DataBeforeFmt,

    #[error("b\"fmt \" chunk not found")]
    /// The stream ended before a `b"fmt "` chunk was found.
    MissingFmtChunk,

    #[error("b\"data\" chunk not found")]
    /// The stream ended before a `b"data"` chunk was found.
    MissingDataChunk,

    #[error(transparent)]
    /// Reading from or writing to the underlying stream failed.
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Generic RIFF/WAVE chunk representation.
pub struct WavChunk {
    /// Four-byte RIFF chunk identifier.
    pub chunk_id: [u8; 4],

    /// Raw chunk payload bytes, excluding the 8-byte chunk header and any pad byte.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
/// Internal helper structure containing the header of a RIFF/WAVE chunk.
///
/// This is a useful abstraction that allows us to get a structured beginning
/// of the chunk without reading it's data yet. E.g. a precursor to reading
/// [`WavChunk`].
struct ChunkHeader {
    chunk_id: [u8; 4],
    chunk_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
/// Encoding tag stored in the WAV `b"fmt "` chunk's `wFormatTag` field.
pub enum WavAudioFormat {
    /// Linear PCM integer samples (`WAVE_FORMAT_PCM`, 0x0001).
    Pcm = 0x0001,

    /// IEEE-754 floating-point samples (`WAVE_FORMAT_IEEE_FLOAT`, 0x0003).
    IeeeFloat = 0x0003,

    /// Any other WAV format tag not modeled explicitly by this crate.
    #[num_enum(catch_all)]
    Other(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Metadata decoded from a WAV `b"fmt "` chunk.
pub struct WavHeader {
    /// Number of interleaved audio channels in the stream.
    pub channels: u16,

    /// Sampling frequency in hertz.
    pub sample_rate: u32,

    /// Width of each stored sample container, in bits.
    ///
    /// This is the on-disk container size. In `WAVE_FORMAT_EXTENSIBLE`, it is
    /// byte-aligned and may be larger than [`Self::valid_bits_per_sample`].
    pub bits_per_sample: u16,

    /// Number of meaningful bits stored in each sample container.
    ///
    /// For classic PCM/IEEE-float WAV headers this is typically equal to
    /// [`Self::bits_per_sample`]. In `WAVE_FORMAT_EXTENSIBLE`, it can be any
    /// value up to the container size.
    pub valid_bits_per_sample: u16,

    /// Sample encoding described by the WAV `wFormatTag` field.
    pub audio_format: WavAudioFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed WAV state up to the start of the `b"data"` payload.
///
/// This type exists for streaming use cases where the caller needs the WAV
/// metadata and chunk ordering information before the sample payload, but does
/// not want to read the payload into memory yet. After [`Self::read`] returns,
/// the underlying reader is positioned at the first byte of the `b"data"`
/// payload, and [`Self::data_size`] describes how many payload bytes remain in
/// that chunk.
///
/// [`Wav`] should be preferred when the caller wants a complete in-memory file
/// representation, including the payload and any chunks that follow it.
pub struct WavPreamble {
    /// Chunks encountered before the `b"fmt "` chunk, in file order.
    pub chunks_before_fmt: Vec<WavChunk>,

    /// Decoded metadata from the WAV `b"fmt "` chunk.
    pub header: WavHeader,

    /// Raw `b"fmt "` extension bytes after the base 16-byte header.
    pub fmt_extension: Vec<u8>,

    /// Chunks encountered after `b"fmt "` and before `b"data"`, in file order.
    pub chunks_before_data: Vec<WavChunk>,

    /// Declared size of the `b"data"` payload in bytes.
    pub data_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// In-memory WAV value preserving chunk order while exposing decoded format metadata directly.
pub struct Wav {
    /// Chunks encountered before the `b"fmt "` chunk, in file order.
    pub chunks_before_fmt: Vec<WavChunk>,

    /// Decoded metadata from the WAV `b"fmt "` chunk.
    pub header: WavHeader,

    /// Raw `b"fmt "` extension bytes after the base 16-byte header.
    pub fmt_extension: Vec<u8>,

    /// Chunks encountered after `b"fmt "` and before `b"data"`, in file order.
    pub chunks_before_data: Vec<WavChunk>,

    /// Raw `b"data"` payload bytes.
    pub data: Vec<u8>,

    /// Chunks encountered after the first `b"data"` chunk, in file order.
    pub chunks_after_data: Vec<WavChunk>,
}

impl ChunkHeader {
    fn read<R: Read>(reader: &mut R) -> Result<Option<Self>, WavError> {
        let mut chunk_id = [0u8; 4];
        match reader.read_exact(&mut chunk_id) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(WavError::Io(err)),
        }

        let mut size_buf = [0u8; 4];
        reader.read_exact(&mut size_buf)?;

        Ok(Some(Self {
            chunk_id,
            chunk_size: u32::from_le_bytes(size_buf),
        }))
    }
}

impl WavHeader {
    /// Decodes a WAV header from a `b"fmt "` chunk without checking the chunk ID.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `chunk.chunk_id == b"fmt "`. This
    /// function assumes the payload belongs to a WAV format chunk and only
    /// validates the payload size.
    unsafe fn from_fmt_chunk_unchecked(chunk: &WavChunk) -> Result<Self, WavError> {
        debug_assert_eq!(chunk.chunk_id, FMT, "expected a b\"fmt \" chunk");

        if chunk.data.len() < 16 {
            let found_size = u32::try_from(chunk.data.len()).unwrap_or(u32::MAX);
            return Err(WavError::InvalidChunkSize {
                chunk_id: chunk.chunk_id,
                found_size,
                expected_size: 16..=u32::MAX,
            });
        }

        let raw_audio_format = WavAudioFormat::from(u16::from_le_bytes([chunk.data[0], chunk.data[1]]));
        let bits_per_sample = u16::from_le_bytes([chunk.data[14], chunk.data[15]]);
        let (audio_format, valid_bits_per_sample) =
            if matches!(raw_audio_format, WavAudioFormat::Other(WAVE_FORMAT_EXTENSIBLE)) {
                if chunk.data.len() < 40 {
                    let found_size = u32::try_from(chunk.data.len()).unwrap_or(u32::MAX);
                    return Err(WavError::InvalidChunkSize {
                        chunk_id: chunk.chunk_id,
                        found_size,
                        expected_size: 40..=u32::MAX,
                    });
                }

                let raw_valid_bits = u16::from_le_bytes([chunk.data[18], chunk.data[19]]);
                let valid_bits_per_sample = if raw_valid_bits == 0 {
                    bits_per_sample
                } else {
                    raw_valid_bits
                };

                let subformat = &chunk.data[24..40];
                let audio_format = if subformat[2..] == KS_SUBFORMAT_GUID_SUFFIX {
                    WavAudioFormat::from(u16::from_le_bytes([subformat[0], subformat[1]]))
                } else {
                    WavAudioFormat::Other(u16::from_le_bytes([subformat[0], subformat[1]]))
                };

                (audio_format, valid_bits_per_sample)
            } else {
                (raw_audio_format, bits_per_sample)
            };

        Ok(Self {
            audio_format,
            channels: u16::from_le_bytes([chunk.data[2], chunk.data[3]]),
            sample_rate: u32::from_le_bytes([chunk.data[4], chunk.data[5], chunk.data[6], chunk.data[7]]),
            bits_per_sample,
            valid_bits_per_sample,
        })
    }

    /// Encodes this header as a WAV `b"fmt "` chunk using the provided raw
    /// extension bytes after the base 16-byte format header.
    fn into_wav_chunk(self, fmt_extension: &[u8]) -> WavChunk {
        let needs_extensible = fmt_extension.len() >= 24 || self.needs_extensible_format();
        let extension = if fmt_extension.is_empty() && needs_extensible {
            self.extensible_fmt_extension()
        } else {
            fmt_extension.to_vec()
        };

        let mut data = Vec::with_capacity(16 + extension.len());
        let format_tag = if needs_extensible {
            WAVE_FORMAT_EXTENSIBLE
        } else {
            u16::from(self.audio_format)
        };
        data.extend_from_slice(&format_tag.to_le_bytes());
        data.extend_from_slice(&self.channels.to_le_bytes());
        data.extend_from_slice(&self.sample_rate.to_le_bytes());

        let byte_rate = self.sample_rate * u32::from(self.channels) * u32::from(self.bits_per_sample / 8);
        data.extend_from_slice(&byte_rate.to_le_bytes());

        let block_align = self.channels * (self.bits_per_sample / 8);
        data.extend_from_slice(&block_align.to_le_bytes());
        data.extend_from_slice(&self.bits_per_sample.to_le_bytes());
        data.extend_from_slice(&extension);

        WavChunk::new(FMT, data)
    }

    /// Returns whether this header requires a `WAVE_FORMAT_EXTENSIBLE`
    /// `b"fmt "` chunk for unambiguous representation.
    const fn needs_extensible_format(&self) -> bool {
        self.valid_bits_per_sample != self.bits_per_sample
            || self.channels > 2
            || matches!(self.audio_format, WavAudioFormat::Pcm) && self.bits_per_sample > 16
    }

    /// Builds the canonical 24-byte extension payload used by
    /// `WAVE_FORMAT_EXTENSIBLE`.
    fn extensible_fmt_extension(&self) -> Vec<u8> {
        let mut extension = Vec::with_capacity(24);
        extension.extend_from_slice(&22u16.to_le_bytes());
        extension.extend_from_slice(&self.valid_bits_per_sample.to_le_bytes());
        extension.extend_from_slice(&0u32.to_le_bytes());
        extension.extend_from_slice(&u16::from(self.audio_format).to_le_bytes());
        extension.extend_from_slice(&KS_SUBFORMAT_GUID_SUFFIX);
        extension
    }
}

impl WavChunk {
    /// Construct a chunk from its identifier and raw payload bytes.
    #[must_use]
    pub const fn new(chunk_id: [u8; 4], data: Vec<u8>) -> Self {
        Self { chunk_id, data }
    }

    fn read<R: Read>(reader: &mut R, header: ChunkHeader) -> Result<Self, WavError> {
        let mut data = vec![0u8; header.chunk_size as usize];
        reader.read_exact(&mut data)?;
        skip_chunk_padding(reader, header.chunk_size)?;

        Ok(Self::new(header.chunk_id, data))
    }

    fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let data_len = u32::try_from(self.data.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "chunk payload is too large to fit into a RIFF chunk size field",
            )
        })?;
        writer.write_all(&self.chunk_id)?;
        writer.write_all(&data_len.to_le_bytes())?;
        writer.write_all(&self.data)?;

        if self.data.len() % 2 == 1 {
            writer.write_all(&[0u8; 1])?;
        }

        Ok(())
    }

    /// Returns the on-disk size of the chunk, including its 8-byte chunk
    /// header and any RIFF pad byte required for odd-length payloads.
    const fn serialized_size(&self) -> usize {
        8 + self.data.len() + (self.data.len() % 2)
    }
}

impl Wav {
    /// Construct a normalized WAV value containing `b"fmt "` and `b"data"` chunks.
    #[must_use]
    pub const fn from_header_and_data(header: WavHeader, data: Vec<u8>) -> Self {
        Self {
            chunks_before_fmt: Vec::new(),
            header,
            fmt_extension: Vec::new(),
            chunks_before_data: Vec::new(),
            data,
            chunks_after_data: Vec::new(),
        }
    }

    /// Reads a complete WAV value into memory.
    ///
    /// This preserves chunk order across the whole RIFF/WAVE file, including
    /// chunks that appear after the first `b"data"` chunk.
    ///
    /// # Errors
    ///
    /// Returns [`WavError`] if the stream is not a valid WAV file, if a
    /// required chunk is missing, or if reading from the underlying stream fails.
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, WavError> {
        let preamble = WavPreamble::read(reader)?;
        let mut data = vec![0u8; preamble.data_size as usize];
        reader.read_exact(&mut data)?;
        skip_chunk_padding(reader, preamble.data_size)?;

        let mut chunks_after_data = Vec::new();
        while let Some(header) = ChunkHeader::read(reader)? {
            chunks_after_data.push(WavChunk::read(reader, header)?);
        }

        Ok(Self {
            chunks_before_fmt: preamble.chunks_before_fmt,
            header: preamble.header,
            fmt_extension: preamble.fmt_extension,
            chunks_before_data: preamble.chunks_before_data,
            data,
            chunks_after_data,
        })
    }

    /// Writes a complete WAV value with exact chunk sizes.
    ///
    /// The written chunk order matches the order stored in this value.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if writing to the writer fails.
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let fmt_chunk = self.header.into_wav_chunk(&self.fmt_extension);
        let data_chunk = WavChunk::new(DATA, self.data.clone());
        let riff_size = 4
            + self
                .chunks_before_fmt
                .iter()
                .map(WavChunk::serialized_size)
                .sum::<usize>()
            + fmt_chunk.serialized_size()
            + self
                .chunks_before_data
                .iter()
                .map(WavChunk::serialized_size)
                .sum::<usize>()
            + data_chunk.serialized_size()
            + self
                .chunks_after_data
                .iter()
                .map(WavChunk::serialized_size)
                .sum::<usize>();
        let riff_size = u32::try_from(riff_size).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "WAV file is too large to fit into a RIFF size field",
            )
        })?;
        WavPreamble::write_riff_header(writer, riff_size)?;

        for chunk in &self.chunks_before_fmt {
            chunk.write(writer)?;
        }

        fmt_chunk.write(writer)?;

        for chunk in &self.chunks_before_data {
            chunk.write(writer)?;
        }

        data_chunk.write(writer)?;

        for chunk in &self.chunks_after_data {
            chunk.write(writer)?;
        }

        Ok(())
    }
}

impl WavPreamble {
    /// Reads and validates the 12-byte RIFF/WAVE file header.
    ///
    /// This consumes the leading `RIFF` tag, the RIFF size field, and the
    /// `WAVE` form type, leaving the reader positioned at the first chunk.
    fn read_riff_header<R: Read>(reader: &mut R) -> Result<(), WavError> {
        let mut buf = [0u8; 4];

        reader.read_exact(&mut buf)?;
        if buf != RIFF {
            return Err(WavError::InvalidMagic {
                found: buf,
                expected: RIFF,
            });
        }

        reader.read_exact(&mut buf)?;

        reader.read_exact(&mut buf)?;
        if buf != WAVE {
            return Err(WavError::InvalidMagic {
                found: buf,
                expected: WAVE,
            });
        }

        Ok(())
    }

    /// Writes the 12-byte RIFF/WAVE file header with the provided RIFF size.
    ///
    /// The `riff_size` field is written exactly as provided and should already
    /// contain the RIFF chunk size semantics required by the caller.
    fn write_riff_header<W: Write>(writer: &mut W, riff_size: u32) -> io::Result<()> {
        writer.write_all(&RIFF)?;
        writer.write_all(&riff_size.to_le_bytes())?;
        writer.write_all(&WAVE)?;
        Ok(())
    }

    /// Reads a WAV preamble from a reader without requiring seeking.
    ///
    /// Parses RIFF/WAVE format until the `b"data"` chunk header is reached.
    /// Audio data follows the returned preamble in the same reader.
    ///
    /// # Errors
    ///
    /// Returns [`WavError`] if the format is invalid, required chunks are missing,
    /// or an I/O error occurs.
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, WavError> {
        Self::read_riff_header(reader)?;
        let mut chunks_before_fmt = Vec::new();
        let mut chunks_before_data = Vec::new();
        let mut header = None;
        let mut fmt_extension = Vec::new();

        loop {
            let Some(chunk_header) = ChunkHeader::read(reader)? else {
                return if header.is_some() {
                    Err(WavError::MissingDataChunk)
                } else {
                    Err(WavError::MissingFmtChunk)
                };
            };

            if chunk_header.chunk_id == DATA {
                return header.map_or(Err(WavError::DataBeforeFmt), |header| {
                    Ok(Self {
                        chunks_before_fmt,
                        header,
                        fmt_extension,
                        chunks_before_data,
                        data_size: chunk_header.chunk_size,
                    })
                });
            }

            let chunk = WavChunk::read(reader, chunk_header)?;
            if chunk.chunk_id == FMT {
                let decoded = unsafe { WavHeader::from_fmt_chunk_unchecked(&chunk)? };
                fmt_extension = chunk.data[16..].to_vec();
                header = Some(decoded);
            } else if header.is_some() {
                chunks_before_data.push(chunk);
            } else {
                chunks_before_fmt.push(chunk);
            }
        }
    }

    /// Writes a WAV preamble for streaming audio.
    ///
    /// This preserves the order of all chunks that precede `b"data"` and writes a
    /// `b"data"` chunk header whose size is set to `0xFFFF_FFFF`. Audio samples
    /// should follow this preamble.
    ///
    /// Note that if you wish to write a non-streaming preamble, you should instead
    /// construct a full [`Wav`] struct and use [`Wav::write`], which will include
    /// the data in a single write.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if writing to the writer fails.
    pub fn write_stream<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        Self::write_riff_header(writer, 0xFFFF_FFFF)?;

        for chunk in &self.chunks_before_fmt {
            chunk.write(writer)?;
        }

        self.header.into_wav_chunk(&self.fmt_extension).write(writer)?;

        for chunk in &self.chunks_before_data {
            chunk.write(writer)?;
        }

        writer.write_all(&DATA)?;
        writer.write_all(&0xFFFF_FFFFu32.to_le_bytes())?;

        Ok(())
    }
}

/// Consume the single RIFF padding byte used to align odd-sized chunks.
fn skip_chunk_padding<R: Read>(reader: &mut R, chunk_size: u32) -> io::Result<()> {
    if chunk_size % 2 == 1 {
        let mut padding = [0u8; 1];
        reader.read_exact(&mut padding)?;
    }

    Ok(())
}
