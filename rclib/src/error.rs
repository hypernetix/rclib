//! Error handling for rclib.

use std::fmt;

/// The main error type for rclib operations.
#[derive(Debug)]
pub enum Error {
    /// Generic error with a message.
    Generic(String),
    /// IO error wrapper.
    Io(std::io::Error),
    /// CLI parsing error.
    CliError(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Generic(msg) => write!(f, "{}", msg),
            Error::Io(err) => write!(f, "IO error: {}", err),
            Error::CliError(msg) => write!(f, "CLI error: {}", msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io(err)
    }
}

/// A Result type alias for rclib operations.
pub type Result<T> = std::result::Result<T, Error>;
