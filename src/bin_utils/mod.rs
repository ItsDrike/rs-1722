//! Binary utility functions for CLI applications.
//!
//! This module provides reusable helpers for building AVTP/audio streaming binaries,
//! including network interface handling and data output formatting.
//! These utilities are designed to be usable across multiple binaries with minimal assumptions.

pub mod audio;
pub mod csv;
pub mod network;

pub use network::{
    ClockSource, InterfaceValidationError, ValidatedInterface, ValidatedInterfaceError, find_interface,
    require_interface,
};
