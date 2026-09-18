#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Anything a person can correct: configuration, payload, printer refusal.
    #[error("{0}")]
    Value(String),
    /// The API answered with a status outside 2xx.
    #[error("API odpowiedziało błędem {0}. {1}")]
    Status(u16, String),
    /// The call never produced an answer: DNS, TLS, timeout, redirect.
    #[error("Błąd połączenia z API: {0}")]
    Request(String),
    #[error("{0}")]
    Printer(#[from] posnet::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn value(message: impl Into<String>) -> Self {
        Self::Value(message.into())
    }
}
