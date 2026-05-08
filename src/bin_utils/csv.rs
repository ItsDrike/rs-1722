//! CSV output utilities for CLI applications.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use thiserror::Error;

/// Error type for CSV operations.
#[derive(Debug, Error)]
pub enum CsvError {
    /// Failed to create or write to file.
    #[error("CSV file operation failed: {0}")]
    IoError(#[from] io::Error),
}

/// A record that can be written to CSV.
pub trait CsvRecord {
    /// Get the header line for the CSV file.
    fn csv_header() -> &'static str;

    /// Serialize this record as a CSV line.
    fn to_csv_line(&self) -> String;
}

/// Write CSV records to a file.
///
/// # Arguments
/// * `path` - Path where the CSV file should be written
/// * `records` - Slice of records to write
///
/// # Errors
/// Returns a `CsvError` if the file cannot be created or written.
pub fn write_csv<T: CsvRecord>(path: &Path, records: &[T]) -> Result<(), CsvError> {
    let mut file = File::create(path)?;

    writeln!(file, "{}", T::csv_header())?;

    for record in records {
        writeln!(file, "{}", record.to_csv_line())?;
    }

    Ok(())
}
