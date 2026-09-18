//! Driver for Posnet Temo Online fiscal printers over the Posnet protocol.
//!
//! Durable operation journals and explicit recovery protect uncertain fiscal operations.

pub mod error;
pub mod models;
pub mod printer;
pub mod protocol;
#[cfg(feature = "sim")]
pub mod simulator;

pub use error::{Error, Result};
pub use models::{Line, VatRate, money, sanitize, validate_name};
pub use printer::{Printer, Record, Status, header_text, receipt_commands};
pub use protocol::{Connection, Session, decode_frame, encode_frame};

/// Serial ports worth probing, newest-looking first; the desktop app's scan list.
pub fn candidate_ports() -> Vec<String> {
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|port| port.port_name)
        .collect()
}
