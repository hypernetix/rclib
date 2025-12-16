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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_generic_display() {
        let err = Error::Generic("test error".to_string());
        assert_eq!(format!("{}", err), "test error");
    }

    #[test]
    fn test_error_io_display() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = Error::Io(io_err);
        assert!(format!("{}", err).contains("IO error"));
    }

    #[test]
    fn test_error_cli_display() {
        let err = Error::CliError("invalid argument".to_string());
        assert_eq!(format!("{}", err), "CLI error: invalid argument");
    }

    #[test]
    fn test_error_source_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = Error::Io(io_err);
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_error_source_generic() {
        let err = Error::Generic("test".to_string());
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn test_error_source_cli() {
        let err = Error::CliError("test".to_string());
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn test_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn test_error_debug() {
        let err = Error::Generic("debug test".to_string());
        let debug_str = format!("{:?}", err);
        assert!(debug_str.contains("Generic"));
        assert!(debug_str.contains("debug test"));
    }
}
