//! Error type for the Juggler driver.

use camoufox_core::error::CamoufoxError;
use thiserror::Error;

/// Convenience alias.
pub type Result<T> = std::result::Result<T, JugglerError>;

/// Every error raised by the Juggler driver.
#[derive(Debug, Error)]
pub enum JugglerError {
    /// Underlying camoufox failure (preparation, install, config…).
    #[error("{0}")]
    Camoufox(#[from] CamoufoxError),
    /// The browser answered with a protocol error.
    #[error("juggler protocol error: {0}")]
    Protocol(String),
    /// A command did not complete in time.
    #[error("timeout: {0}")]
    Timeout(String),
    /// The pipe/connection was closed before the answer arrived.
    #[error("connection closed")]
    Disconnected,
    /// The pipe transport is Unix-only for now.
    #[error("unsupported OS for the juggler pipe transport: {0}")]
    UnsupportedOs(String),
    /// IO failure.
    #[error("io error: {0}")]
    Io(String),
}

impl From<std::io::Error> for JugglerError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for JugglerError {
    fn from(value: serde_json::Error) -> Self {
        Self::Protocol(format!("json: {value}"))
    }
}
