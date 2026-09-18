use std::io;

/// Operator-correctable input errors, wire failures, and explicit device rejections.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid input or an operation blocked by the safety journal.
    #[error("{0}")]
    Value(String),
    /// Invalid response or uncertain fiscal outcome.
    #[error("{0}")]
    Protocol(String),
    /// No response within the deadline; never retried automatically.
    #[error("{0}")]
    Timeout(String),
    /// The printer answered with an error code.
    #[error("Drukarka zwróciła błąd {code} ({command}{}).",
        if field.is_empty() { String::new() } else { format!(", parametr {field}") })]
    Device {
        code: i64,
        command: String,
        field: String,
    },
    /// Opening or losing the port; `denied` identifies an OS permission failure.
    #[error("{message}")]
    Connect { message: String, denied: bool },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub fn timeout(command: &str) -> Self {
        Self::Timeout(format!(
            "Brak odpowiedzi na {command}. Polecenie nie zostało ponowione."
        ))
    }

    pub fn open(error: &io::Error) -> Self {
        Self::Connect {
            message: format!("Nie można połączyć z drukarką: {error}"),
            denied: is_denied(error),
        }
    }

    pub fn broken(error: &io::Error) -> Self {
        Self::Connect {
            message: format!("Przerwano połączenie z drukarką: {error}"),
            denied: is_denied(error),
        }
    }

    /// The OS refused the port; on Linux the usual cause is a missing `dialout` group.
    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Connect { denied: true, .. })
    }

    /// True for everything `detect` treats as "this port is not our printer".
    pub fn is_device_fault(&self) -> bool {
        !matches!(self, Self::Value(_))
    }
}

fn is_denied(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
}
