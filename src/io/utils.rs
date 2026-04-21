use std::{io, sync::Arc};

use bitstream_io::BitRead;

/// Reads `length` bytes from the given [`BitReader`] into a newly allocated
/// [`Arc<[u8]>`] without intermediate buffering.
///
/// This performs a single allocation and avoids copying by reading directly
/// into the final buffer.
///
/// # Errors
/// Returns any I/O error encountered while reading from the underlying reader.
///
/// # Safety
/// This function uses `unsafe` to write into uninitialized memory. This is
/// sound because the buffer is fully initialized via `read_bytes` before
/// calling `assume_init`, and the `Arc` is uniquely owned at that point.
pub fn read_arc<R: BitRead>(reader: &mut R, length: usize) -> Result<Arc<[u8]>, io::Error> {
    let mut data = Arc::<[u8]>::new_uninit_slice(length);

    let slice = Arc::get_mut(&mut data).unwrap();
    let slice = unsafe { &mut *(std::ptr::from_mut(slice) as *mut [u8]) };
    reader.read_bytes(slice)?;

    let data = unsafe { data.assume_init() };

    Ok(data)
}
