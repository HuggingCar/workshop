//! Polish receipt editor and its single-operation printer worker.
mod settings;
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use chrono::{Datelike, NaiveDate};
use posnet::{Line, Printer, Record, Status, models::MAX_CENTS, validate_name};
use rust_decimal::Decimal;
use serde_json::{Value, json};
pub use settings::{SERVICES, Settings, data_dir};

pub const HISTORY_LIMIT: usize = 500;
pub const REPROBE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct Row {
    pub name: String,
    pub quantity: String,
    pub price: String,
}
impl Row {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            quantity: "1".into(),
            price: String::new(),
        }
    }
    pub fn line(&self, vat: usize) -> Result<Option<Line>, String> {
        let name = validate_name(&self.name).map_err(|e| e.to_string())?;
        let price = if self.price.trim().is_empty() {
            Decimal::ZERO
        } else {
            decimal(&self.price)?
        };
        // An unused row does not need a quantity, but every name must be printable.
        if price == Decimal::ZERO {
            return Ok(None);
        }
        Line::new(&name, decimal(&self.quantity)?, price, vat)
            .map(Some)
            .map_err(|e| e.to_string())
    }
}
fn decimal(text: &str) -> Result<Decimal, String> {
    let normalized = text.trim().replace(',', ".");
    let parsed = if let Some((mantissa, _)) = normalized.split_once(['e', 'E']) {
        // from_scientific parses its mantissa with the rounding FromStr implementation.
        Decimal::from_str_exact(mantissa).and_then(|_| Decimal::from_scientific(&normalized))
    } else {
        Decimal::from_str_exact(&normalized)
    };
    parsed.map_err(|_| "Sprawdź ilość i cenę.".into())
}
pub fn receipt_lines(rows: &[Row], vat: Option<usize>) -> Result<Vec<Line>, String> {
    let vat = vat.ok_or("Najpierw połącz drukarkę i wybierz aktywną stawkę VAT.")?;
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if let Some(line) = row
            .line(vat)
            .map_err(|e| format!("Wiersz {}: {e}", index + 1))?
        {
            lines.push(line);
        }
    }
    if lines.len() > 500 || lines.iter().map(Line::total_cents).sum::<i64>() > MAX_CENTS {
        return Err(
            "Paragon przekracza zakres drukarki (500 pozycji lub 99 999 999,99 zł).".into(),
        );
    }
    Ok(lines)
}

pub fn completed_months(today: NaiveDate) -> Vec<(i32, u32)> {
    let current = today.year() * 12 + today.month0() as i32;
    (1..=24)
        .map(|back| {
            let month = current - back;
            (month.div_euclid(12), month.rem_euclid(12) as u32 + 1)
        })
        .collect()
}

#[derive(Debug)]
pub enum Job {
    Probe { scan: bool },
    Receipt(Vec<Line>, i64),
    Daily,
    Monthly(i32, u32),
    Periodic(NaiveDate, NaiveDate, bool),
    Recover { force: bool },
}
impl Job {
    fn fiscal(&self) -> bool {
        matches!(
            self,
            Self::Receipt(..) | Self::Daily | Self::Monthly(..) | Self::Periodic(..)
        )
    }
}
enum Outcome {
    Status(Status),
    Printed(Record),
    Recovered,
}
struct Finished {
    printer: Printer,
    job: Job,
    outcome: Result<Outcome, String>,
    pending: Result<Option<Record>, String>,
}

/// Only one worker owns the Printer at a time. No UI thread locks, no fiscal retries.
pub struct Controller {
    pub settings: Settings,
    pub data: PathBuf,
    pub rows: Vec<Row>,
    pub search: String,
    pub payment: i64,
    pub vat: Option<usize>,
    pub vat_rates: Vec<posnet::VatRate>,
    pub status: Option<Status>,
    pub pending: Option<Value>,
    pub message: String,
    pub offer_force: bool,
    printer: Option<Printer>,
    worker: Option<Receiver<Finished>>,
    retry_at: Instant,
    done: String,
}

impl Controller {
    pub fn new(settings: Settings, data: PathBuf) -> Result<Self, String> {
        let printer = Printer::new(settings.connection(), data.join("operation.json"))
            .map_err(|e| e.to_string())?;
        let (pending, message) = match printer.pending() {
            Ok(pending) => (pending.map(Value::Object), "Sprawdzanie drukarki…".into()),
            Err(e) => (Some(json!({"operation":"unknown"})), e.to_string()),
        };
        Ok(Self {
            rows: settings
                .services
                .iter()
                .map(|name| Row::new(name))
                .collect(),
            settings,
            data,
            search: String::new(),
            payment: 0,
            vat: None,
            vat_rates: vec![],
            status: None,
            pending,
            message,
            offer_force: false,
            printer: Some(printer),
            worker: None,
            retry_at: Instant::now() + REPROBE,
            done: String::new(),
        })
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub fn editable(&self) -> bool {
        !self.busy() && self.pending.is_none()
    }
    pub fn ready(&self) -> bool {
        self.editable() && self.status.as_ref().is_some_and(|s| s.ready)
    }
    pub fn receipt_lines(&self) -> Result<Vec<Line>, String> {
        receipt_lines(&self.rows, self.vat)
    }
    pub fn clear_prices(&mut self) {
        for row in &mut self.rows {
            row.price.clear();
        }
    }
    pub fn retry_due(&self) -> bool {
        !self.busy()
            && self.pending.is_none()
            && !self.status.as_ref().is_some_and(|s| s.ready)
            && Instant::now() >= self.retry_at
    }
    pub fn start(&mut self, job: Job) -> bool {
        if self.busy() || (job.fiscal() && !self.ready()) {
            return false;
        }
        if let Job::Receipt(lines, _) = &job
            && lines.is_empty()
        {
            return false;
        }
        let Some(mut printer) = self.printer.take() else {
            return false;
        };
        self.offer_force = false;
        self.message = "Trwa komunikacja z drukarką. Poczekaj na wynik operacji.".into();
        let (send, receive) = mpsc::channel();
        self.worker = Some(receive);
        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> posnet::Result<Outcome> {
                    match &job {
                        Job::Probe { scan: true } => {
                            let mut ports = posnet::candidate_ports();
                            ports.sort_by_key(|port| !port.contains("ttyACM"));
                            printer.detect(&ports).map(Outcome::Status)
                        }
                        Job::Probe { scan: false } => printer.probe().map(Outcome::Status),
                        Job::Receipt(lines, payment) => printer
                            .print_receipt(lines, *payment, "", vec![])
                            .map(Outcome::Printed),
                        Job::Daily => printer.daily_report().map(Outcome::Printed),
                        Job::Monthly(year, month) => {
                            printer.monthly_report(*year, *month).map(Outcome::Printed)
                        }
                        Job::Periodic(start, end, summary) => printer
                            .periodic_report(*start, *end, *summary)
                            .map(Outcome::Printed),
                        Job::Recover { force } => printer
                            .acknowledge_pending(*force)
                            .map(|_| Outcome::Recovered),
                    }
                },
            ));
            let outcome = match result {
                Ok(result) => result.map_err(|e| e.to_string()),
                Err(_) => Err(
                    "Przerwano komunikację. Sprawdź wydruk i stan drukarki przed kolejną operacją."
                        .into(),
                ),
            };
            let pending = printer.pending().map_err(|e| e.to_string());
            let _ = send.send(Finished {
                printer,
                job,
                outcome,
                pending,
            });
        });
        true
    }
    /// Consume a completed worker. A success queues only a read-only probe.
    pub fn poll(&mut self) {
        let Some(receiver) = &self.worker else {
            return;
        };
        let finished = match receiver.try_recv() {
            Ok(finished) => finished,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.worker = None;
                self.status = None;
                self.pending = Some(json!({"operation":"unknown"}));
                self.message = "Przerwano wątek drukarki. Uruchom aplikację ponownie i sprawdź ostatnią operację.".into();
                return;
            }
        };
        self.worker = None;
        self.settings.address = finished.printer.connection.address.clone();
        self.settings.baudrate = finished.printer.connection.baudrate;
        self.printer = Some(finished.printer);
        self.retry_at = Instant::now() + REPROBE;
        let journal_error = match finished.pending {
            Ok(pending) => {
                self.pending = pending.map(Value::Object);
                None
            }
            Err(error) => {
                self.pending = Some(json!({"operation":"unknown"}));
                Some(error)
            }
        };
        let mut follow_up = false;
        match finished.outcome {
            Err(error) => {
                self.done.clear();
                self.status = None;
                self.message = format!("Błąd: {error}");
                if matches!(finished.job, Job::Probe { scan: true }) {
                    self.message
                        .push_str(" Podłącz drukarkę lub wskaż port w Ustawieniach.");
                }
                self.offer_force = matches!(finished.job, Job::Recover { force: false });
            }
            Ok(Outcome::Status(status)) => {
                // A removed previously selected rate requires a deliberate new selection.
                if let Some(selected) = self.vat {
                    if !status
                        .vat_rates
                        .iter()
                        .any(|rate| rate.index == selected && rate.active())
                    {
                        self.vat = None;
                    }
                } else {
                    self.vat = status
                        .vat_rates
                        .iter()
                        .find(|rate| rate.active())
                        .map(|rate| rate.index);
                }
                self.vat_rates = status
                    .vat_rates
                    .iter()
                    .filter(|rate| rate.active())
                    .cloned()
                    .collect();
                self.message = format!(
                    "{}{} · {} · {}",
                    self.done, status.name, status.unique_number, status.description
                );
                self.status = Some(status);
                if matches!(finished.job, Job::Probe { scan: true })
                    && let Err(e) = self.settings.save(&self.data.join("settings.json"))
                {
                    self.message.push_str(&format!(" Nie zapisano portu: {e}"));
                }
            }
            Ok(Outcome::Printed(record)) => {
                if matches!(finished.job, Job::Receipt(..)) {
                    self.clear_prices();
                    self.done = match record
                        .get("receipt_number")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        Some(number) => format!("Paragon nr {number} wydrukowany. "),
                        None => "Paragon wydrukowany. ".into(),
                    };
                    if let Err(error) = self.append_history(&record) {
                        self.done
                            .push_str(&format!("Nie zapisano historii: {error}. "));
                    }
                } else {
                    self.done = "Raport wydrukowany. ".into();
                }
                self.message = self.done.clone();
                self.status = None;
                follow_up = true;
            }
            Ok(Outcome::Recovered) => {
                self.clear_prices();
                self.status = None;
                self.done = "Zapis poprzedniej operacji potwierdzony. ".into();
                self.message = self.done.clone();
                follow_up = true;
            }
        }
        if let Some(error) = journal_error {
            self.status = None;
            self.message.push_str(&format!(
                " Nie można odczytać stanu ostatniej operacji: {error}"
            ));
        }
        if self.pending.is_some() {
            self.message.push_str("\nWynik ostatniej operacji jest niepotwierdzony. Sprawdź wydruk i stan drukarki, a następnie wybierz „Wyjaśnij ostatnią operację”.");
        } else if follow_up {
            self.start(Job::Probe { scan: false });
        }
    }
    pub fn save_settings(&mut self, settings: Settings) -> Result<(), String> {
        if self.busy() {
            return Err("Poczekaj na zakończenie operacji.".into());
        }
        let settings = settings.normalized()?;
        settings.save(&self.data.join("settings.json"))?;
        let changed_connection = settings.connection() != self.settings.connection();
        if settings.services != self.settings.services {
            self.rows = settings
                .services
                .iter()
                .map(|name| Row::new(name))
                .collect();
        }
        self.settings = settings;
        if changed_connection {
            self.printer = Some(
                Printer::new(self.settings.connection(), self.data.join("operation.json"))
                    .map_err(|e| e.to_string())?,
            );
            self.status = None;
            self.done.clear();
            self.start(Job::Probe { scan: false });
        } else {
            self.message = "Zapisano ustawienia.".into();
        }
        Ok(())
    }
    pub fn history(&self) -> Result<Vec<Value>, String> {
        let file = match fs::File::open(self.data.join("history.jsonl")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let mut rows = std::collections::VecDeque::with_capacity(HISTORY_LIMIT);
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            let record: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if rows.len() == HISTORY_LIMIT {
                rows.pop_front();
            }
            rows.push_back(record);
        }
        Ok(rows.into_iter().rev().collect())
    }
    fn append_history(&self, record: &Record) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.data.join("history.jsonl"))
            .map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, record).map_err(|e| e.to_string())?;
        file.write_all(b"\n")
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())
    }
}

pub fn status_json(status: &Status) -> Value {
    json!({"name": status.name, "version": status.version, "unique_number": status.unique_number,
        "ready": status.ready, "description": status.description,
        "vat_rates": status.vat_rates.iter().map(|rate| json!({"index": rate.index, "percent": rate.percent.to_string()})).collect::<Vec<_>>()})
}
