//! Error type for the refactor pipeline.
//!
//! Deliberately independent of `anyhow` (and `takopack-core`): the pipeline can
//! be consumed and its errors handled without depending on either. Backend
//! errors from the cargo wrapper are captured as message strings, so this
//! module never needs to name the backend's error type.

use std::fmt;

/// Pipeline error.
#[derive(Debug)]
pub enum Error {
    /// A plain message, typically from a failed validation or precondition.
    Message(String),
    /// An underlying I/O failure.
    Io(std::io::Error),
    /// A failure propagated from the cargo backend, captured as a message.
    Backend(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Build a plain-message error.
    pub fn message(msg: impl Into<String>) -> Self {
        Error::Message(msg.into())
    }

    /// Build a backend error (any cargo/loader failure) from its message.
    pub fn backend(msg: impl Into<String>) -> Self {
        Error::Backend(msg.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Message(m) => write!(f, "{m}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Backend(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Message(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Message(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_roundtrips_message() {
        let err = Error::message("boom");
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn from_string_creates_message_variant() {
        let err: Error = "boom".into();
        assert!(matches!(err, Error::Message(_)));
    }
}
