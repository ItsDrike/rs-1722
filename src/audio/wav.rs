use std::io::{self, Read, Write};

use thiserror::Error;

#[derive(Debug, Clone, Copy)]
pub struct WavHeader {
    pub channels: u16,
    pub sample_rate: u32,
    pub bits_per_sample: u16,
    pub audio_format: u16,
}

#[derive(Debug, Error)]
pub enum WavError {
    #[error("Invalid WAV magic: {0:?}")]
    InvalidMagic([u8; 4]),

    #[error("Invalid chunk ID")]
    InvalidChunkId,

    #[error("Unsupported audio format: {0}")]
    UnsupportedFormat(u16),

    #[error("Data chunk found before fmt chunk")]
    DataBeforeFmt,

    #[error("fmt chunk not found")]
    MissingFmtChunk,

    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Reads a WAV header from a reader without requiring seeking.
///
/// Parses RIFF/WAVE format and reads the fmt and data chunks.
/// Audio data follows the returned header in the same reader.
///
/// # Errors
///
/// Returns `WavError` if the format is invalid, required chunks are missing,
/// or an I/O error occurs.
pub fn read_wav_header<R: Read>(reader: &mut R) -> Result<WavHeader, WavError> {
    let mut buf = [0u8; 4];

    // RIFF magic
    reader.read_exact(&mut buf)?;
    if &buf != b"RIFF" {
        return Err(WavError::InvalidMagic(buf));
    }

    // Skip RIFF chunk size (4 bytes)
    reader.read_exact(&mut buf)?;

    // WAVE format
    reader.read_exact(&mut buf)?;
    if &buf != b"WAVE" {
        return Err(WavError::InvalidMagic(buf));
    }

    let mut header = None;

    // Read chunks until we find fmt and data
    loop {
        reader.read_exact(&mut buf)?;
        let chunk_id = buf;

        // Read chunk size (LE)
        let mut size_buf = [0u8; 4];
        reader.read_exact(&mut size_buf)?;
        let chunk_size = u32::from_le_bytes(size_buf) as usize;

        match &chunk_id {
            b"fmt " => {
                if chunk_size < 16 {
                    return Err(WavError::InvalidChunkId);
                }

                let mut fmt_buf = vec![0u8; chunk_size];
                reader.read_exact(&mut fmt_buf)?;

                let audio_format = u16::from_le_bytes([fmt_buf[0], fmt_buf[1]]);
                let channels = u16::from_le_bytes([fmt_buf[2], fmt_buf[3]]);
                let sample_rate = u32::from_le_bytes([fmt_buf[4], fmt_buf[5], fmt_buf[6], fmt_buf[7]]);
                let bits_per_sample = u16::from_le_bytes([fmt_buf[14], fmt_buf[15]]);

                header = Some(WavHeader {
                    channels,
                    sample_rate,
                    bits_per_sample,
                    audio_format,
                });
            }
            b"data" => {
                return header.ok_or(WavError::DataBeforeFmt);
            }
            _ => {
                // Skip unknown chunks
                let mut skip_buf = vec![0u8; chunk_size];
                reader.read_exact(&mut skip_buf)?;
            }
        }
    }
}

/// Writes a WAV header to a writer for streaming audio.
///
/// Writes a 44-byte WAV header with the data chunk size set to `0xFFFF_FFFF`,
/// which signals the streaming WAV format. Audio samples should follow this header.
///
/// # Errors
///
/// Returns an I/O error if writing to the writer fails.
pub fn write_wav_header<W: Write>(writer: &mut W, header: &WavHeader) -> io::Result<()> {
    // RIFF chunk
    writer.write_all(b"RIFF")?;
    writer.write_all(&0xFFFF_FFFFu32.to_le_bytes())?;
    writer.write_all(b"WAVE")?;

    // fmt sub-chunk
    writer.write_all(b"fmt ")?;
    writer.write_all(&16u32.to_le_bytes())?; // PCM fmt size
    writer.write_all(&header.audio_format.to_le_bytes())?;
    writer.write_all(&header.channels.to_le_bytes())?;
    writer.write_all(&header.sample_rate.to_le_bytes())?;

    let byte_rate = header.sample_rate * u32::from(header.channels) * u32::from(header.bits_per_sample / 8);
    writer.write_all(&byte_rate.to_le_bytes())?;

    let block_align = header.channels * (header.bits_per_sample / 8);
    writer.write_all(&block_align.to_le_bytes())?;

    writer.write_all(&header.bits_per_sample.to_le_bytes())?;

    // data sub-chunk
    writer.write_all(b"data")?;
    writer.write_all(&0xFFFF_FFFFu32.to_le_bytes())?;

    Ok(())
}
