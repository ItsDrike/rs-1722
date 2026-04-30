mod alternative;
mod common;
mod control;
mod stream;

pub use alternative::AlternativeHeader;
pub use common::CommonHeader;
pub use control::ControlHeader;
pub use stream::{GenericStreamData, SpecificStreamData, StreamDataError, StreamHeader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderType {
    Control,
    Stream,
    Alternative,
}
