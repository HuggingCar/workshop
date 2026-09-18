//! HuggingCar workshop agent: takes fiscal jobs from the API and prints them locally.

pub mod agent;
pub mod api;
pub mod config;
pub mod error;

pub use agent::{Agent, Notify};
pub use api::{Api, validated_url};
pub use config::{Config, state_dir};
pub use error::{Error, Result};
use posnet::{Connection, Printer};

/// Build a ready-to-run agent from saved settings, sharing `stopping` with the caller.
pub fn build(config: &Config, data: &std::path::Path, notify: Notify) -> Result<Agent> {
    let config = config.validated()?;
    if config.serial.is_empty() {
        return Err(Error::value("Wybierz lub wpisz port drukarki."));
    }
    let mut printer = Printer::new(
        Connection {
            address: config.serial.clone(),
            baudrate: config.baudrate,
            ..Connection::default()
        },
        data.join("fiscal-operation.json"),
    )?;
    let status = printer.probe()?;
    let mut api = Api::new(&config.api_url, &config.token, &status.unique_number)?;
    api.describe(&status);
    Ok(Agent::new(printer, api, notify))
}
