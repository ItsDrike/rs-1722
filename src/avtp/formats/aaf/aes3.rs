use getset::{CopyGetters, Getters};
use thiserror::Error;

use super::common::{AafFormat, AafSpecificData};

#[derive(Error, Debug)]
pub enum InvalidAes3Aaf {
    #[error("Attempted to initialize an AES3 AAF from a non-AES3 format: {0:?}")]
    NonAes3Format(AafFormat),
}

// TODO: This is currently WIP
// (Acts as a simple container around the AAF specific data)
#[derive(Debug, Clone, PartialEq, Eq, Getters, CopyGetters)]
pub struct AafAes3(AafSpecificData);

impl AafAes3 {
    pub(super) fn from_specific(data: AafSpecificData) -> Result<Self, InvalidAes3Aaf> {
        match data.format {
            AafFormat::User
            | AafFormat::Float32Bit
            | AafFormat::Int32Bit
            | AafFormat::Int24Bit
            | AafFormat::Int16Bit => return Err(InvalidAes3Aaf::NonAes3Format(data.format)),
            AafFormat::AES3_32Bit => {}
        }

        Ok(Self(data))
    }

    pub(super) fn into_specific(self) -> AafSpecificData {
        self.0
    }
}
