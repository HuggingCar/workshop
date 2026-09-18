use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{Datelike, Local, SecondsFormat};
use rust_decimal::prelude::*;
use serde_json::{Map, Value, json};

use crate::{
    error::{Error, Result},
    models::{Line, MAX_CENTS, VAT_COUNT, VatRate, sanitize},
    protocol::{Connection, Params, Session},
};

/// A Temo answers `getrealid` well within this; silent ports cost no more.
pub const SCAN_TIMEOUT: f64 = 0.5;
pub const MAX_LINES: usize = 500;

pub type Record = Map<String, Value>;
/// One protocol step of an operation: command, parameters, timeout in seconds.
pub type Step = (&'static str, Vec<(&'static str, String)>, f64);

/// Plain header text: `&&` is a literal `&`, any other `&x` is a formatting mark.
pub fn header_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(char) = chars.next() {
        if char != '&' {
            out.push(char);
        } else if matches!(chars.peek(), None | Some('\n')) || chars.next() == Some('&') {
            out.push('&');
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub name: String,
    pub version: String,
    pub unique_number: String,
    pub ready: bool,
    pub description: String,
    pub vat_rates: Vec<VatRate>,
}

pub fn boolean(value: &str) -> Result<bool> {
    match value.to_lowercase().as_str() {
        "1" | "y" | "t" => Ok(true),
        "0" | "n" | "f" => Ok(false),
        _ => Err(Error::protocol("Nieprawidłowy status logiczny drukarki.")),
    }
}

/// One rate out of `vatget`; a non-numeric or non-finite value is not a usable rate.
pub fn vat_percent(raw: &str) -> Result<Decimal> {
    Decimal::from_str_exact(&raw.replace(',', "."))
        .map_err(|_| Error::value("Nieprawidłowa stawka VAT w odpowiedzi drukarki."))
}

/// The command sequence for one receipt; a non-empty `footer` adds a branded footer line.
pub fn receipt_commands(lines: &[Line], payment: i64, total: i64, footer: &str) -> Vec<Step> {
    let mut commands = vec![("trinit", vec![("bm", "0".to_string())], 10.0)];
    commands.extend(lines.iter().map(|line| {
        (
            "trline",
            vec![
                ("na", line.name().to_string()),
                ("vt", line.vat().to_string()),
                ("pr", line.price_cents().to_string()),
                ("il", line.quantity_text()),
                ("wa", line.total_cents().to_string()),
            ],
            10.0,
        )
    }));
    commands.push((
        "trpayment",
        vec![("ty", payment.to_string()), ("wa", total.to_string())],
        10.0,
    ));
    if footer.is_empty() {
        commands.push((
            "trend",
            vec![("to", total.to_string()), ("fp", total.to_string())],
            30.0,
        ));
    } else {
        commands.push((
            "trend",
            vec![
                ("to", total.to_string()),
                ("fp", total.to_string()),
                ("fe", "0".to_string()),
            ],
            30.0,
        ));
        commands.push((
            "trftrln",
            vec![("id", "25".to_string()), ("na", footer.to_string())],
            10.0,
        ));
        commands.push(("trftrend", Vec::new(), 30.0));
    }
    commands
}

pub struct Printer {
    pub connection: Connection,
    journal_path: PathBuf,
    pub last_status: Option<Status>,
}

impl Printer {
    /// Hold the cross-process operation lock until `work` returns.
    fn locked<T>(&self, work: impl FnOnce() -> Result<T>) -> Result<T> {
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.journal_path.with_extension("lock"))
            .map_err(|err| Error::value(format!("Nie można otworzyć blokady operacji: {err}")))?;
        let mut lock = fd_lock::RwLock::new(file);
        let _guard = lock
            .try_write()
            .map_err(|_| Error::value("Inna operacja drukarki jest już wykonywana."))?;
        work()
    }
}

impl Printer {
    pub fn new(connection: Connection, journal_path: impl Into<PathBuf>) -> Result<Self> {
        let journal_path = journal_path.into();
        if let Some(parent) = journal_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|err| {
                Error::value(format!("Nie można utworzyć katalogu rejestru: {err}"))
            })?;
        }
        Ok(Self {
            connection,
            journal_path,
            last_status: None,
        })
    }

    /// Persist intent before touching the printer, including across power loss.
    fn save(&self, record: &Record) -> Result<()> {
        let parent = self
            .journal_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::Builder::new()
            .prefix(".operation-")
            .tempfile_in(parent)
            .map_err(|err| Error::value(format!("Nie można zapisać rejestru operacji: {err}")))?;
        let write = (|| -> std::io::Result<()> {
            serde_json::to_writer_pretty(temporary.as_file_mut(), record)?;
            temporary.as_file_mut().flush()?;
            temporary.as_file().sync_all()
        })();
        write.map_err(|err| Error::value(format!("Nie można zapisać rejestru operacji: {err}")))?;
        temporary
            .persist(&self.journal_path)
            .map_err(|err| Error::value(format!("Nie można zapisać rejestru operacji: {err}")))?;
        // A successful rename is not durable until the directory entry is synced.
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|err| Error::value(format!("Nie można utrwalić rejestru operacji: {err}")))?;
        Ok(())
    }

    /// Whatever the journal holds, regardless of state; None when there is none.
    ///
    /// A journal that exists but cannot be read is never "nothing": callers must stop.
    pub fn last_record(&self) -> Result<Option<Record>> {
        let text = match fs::read_to_string(&self.journal_path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => {
                return Err(Error::value(
                    "Nie można odczytać rejestru operacji. Drukowanie zablokowane.",
                ));
            }
        };
        let record: Value = serde_json::from_str(&text).map_err(|_| {
            Error::value("Nie można odczytać rejestru operacji. Drukowanie zablokowane.")
        })?;
        let record = record.as_object().filter(|record| {
            matches!(
                record.get("state").and_then(Value::as_str),
                Some("pending" | "completed" | "acknowledged")
            )
        });
        match record {
            Some(record) => Ok(Some(record.clone())),
            None => Err(Error::value(
                "Uszkodzony rejestr operacji. Drukowanie zablokowane.",
            )),
        }
    }

    pub fn pending(&self) -> Result<Option<Record>> {
        Ok(self
            .last_record()?
            .filter(|record| record.get("state").and_then(Value::as_str) == Some("pending")))
    }

    fn require_resolved(&self) -> Result<()> {
        if self.pending()?.is_some() {
            return Err(Error::value(
                "Poprzednia operacja jest nierozstrzygnięta. Sprawdź drukarkę i rozstrzygnij ją \
                 przed kolejnym wydrukiem.",
            ));
        }
        Ok(())
    }

    fn probe_session(session: &mut Session) -> Result<Status> {
        let identity = session.command("getrealid", &[], None)?;
        let device = session.command("sdev", &[], None)?;
        let mechanism = session.command("sprn", &[], None)?;
        let common = session.command("scomm", &[], None)?;
        let transaction = session.command("strns", &[], None)?;
        let vats = session.command("vatget", &[], None)?;
        let incomplete =
            || Error::protocol("Niepełna lub nieprawidłowa odpowiedź statusu drukarki.");
        let field = |params: &Params, key: &str| params.get(key).cloned().ok_or_else(incomplete);

        let name = field(&identity, "nm")?;
        let version = field(&identity, "vr")?;
        let number = field(&identity, "nu")?;
        let fiscal = boolean(&field(&common, "fs")?).map_err(|_| incomplete())?;
        let transaction_open = field(&transaction, "to")? != "0"
            || boolean(&field(&transaction, "fe")?).map_err(|_| incomplete())?;
        let queued = !boolean(&field(&device, "qe")?).map_err(|_| incomplete())?;
        let mut rates = Vec::with_capacity(VAT_COUNT);
        for index in 0..VAT_COUNT {
            let key = format!("v{}", char::from(b'a' + index as u8));
            let percent = vat_percent(&field(&vats, &key)?).map_err(|_| incomplete())?;
            rates.push(VatRate { index, percent });
        }

        let mut problems: Vec<String> = Vec::new();
        if name != "POSNET TEMO ONLINE" || number.trim().is_empty() {
            problems.push("Wybrane urządzenie nie jest drukarką Temo Online".into());
        }
        if !fiscal {
            problems.push("Drukarka nie jest w trybie fiskalnym".into());
        }
        if !boolean(&field(&common, "hr")?).map_err(|_| incomplete())? {
            problems.push("Drukarka nie ma zaprogramowanego nagłówka".into());
        }
        if field(&device, "ds")? != "0" {
            problems.push("Drukarka oczekuje na operatora lub jest w menu".into());
        }
        if queued {
            problems.push("Drukarka ma oczekujące polecenia".into());
        }
        let mechanism_error = field(&mechanism, "pr")?;
        if mechanism_error != "0" {
            problems.push(match mechanism_error.as_str() {
                "1" => "Podniesiona dźwignia".to_string(),
                "3" => "Otwarta pokrywa".to_string(),
                "5" => "Brak papieru".to_string(),
                "6" => "Nieprawidłowa temperatura lub zasilanie".to_string(),
                other => format!("Błąd mechanizmu: {other}"),
            });
        }
        if transaction_open || field(&common, "ts")? != "0" {
            problems.push("Otwarta transakcja. Dokończ lub anuluj ją na drukarce".into());
        }
        if !rates.iter().any(VatRate::active) {
            problems.push("Brak aktywnych stawek VAT".into());
        }
        let description = if problems.is_empty() {
            "Gotowa do drukowania".to_string()
        } else {
            problems.join(". ")
        };
        Ok(Status {
            name,
            version,
            unique_number: number,
            ready: problems.is_empty(),
            description,
            vat_rates: rates,
        })
    }

    pub fn probe(&mut self) -> Result<Status> {
        let status = self.locked(|| {
            let mut session = Session::open(&self.connection)?;
            Self::probe_session(&mut session)
        })?;
        self.last_status = Some(status.clone());
        Ok(status)
    }

    /// Probe the saved port; if it does not answer, try each candidate and adopt the first.
    ///
    /// Only `getrealid` is sent to strangers, and a port is adopted solely when it identifies
    /// itself as a Temo Online, so a scan can never disturb another serial device. While an
    /// operation is unresolved the device must not change, so no scan happens then.
    pub fn detect(&mut self, candidates: &[String]) -> Result<Status> {
        let error = match self.probe() {
            Ok(status) => return Ok(status),
            Err(error) => error,
        };
        if self.pending()?.is_some() {
            return Err(error);
        }
        let mut denied = if error.is_denied() {
            self.connection.address.clone()
        } else {
            String::new()
        };
        for address in candidates {
            if *address == self.connection.address {
                continue;
            }
            let connection = Connection {
                address: address.clone(),
                ..self.connection.clone()
            };
            let found = self.locked(|| {
                let mut session = Session::open(&connection)?;
                let identity = session.command("getrealid", &[], Some(SCAN_TIMEOUT))?;
                if identity.get("nm").map(String::as_str) != Some("POSNET TEMO ONLINE") {
                    return Ok(None);
                }
                Self::probe_session(&mut session).map(Some)
            });
            match found {
                Ok(Some(status)) => {
                    self.connection = connection;
                    self.last_status = Some(status.clone());
                    return Ok(status);
                }
                Ok(None) => continue,
                Err(candidate_error) => {
                    if denied.is_empty() && candidate_error.is_denied() {
                        denied = address.clone();
                    }
                }
            }
        }
        if !denied.is_empty() {
            return Err(Error::value(format!(
                "Brak uprawnień do portu {denied}. Na Linuksie dodaj użytkownika do grupy \
                 dialout i zaloguj się ponownie."
            )));
        }
        Err(error)
    }

    fn check_ready(session: &mut Session) -> Result<Status> {
        let status = Self::probe_session(session)?;
        if !status.ready {
            return Err(Error::value(status.description));
        }
        Ok(status)
    }

    fn run(
        &self,
        session: &mut Session,
        status: &Status,
        operation: &str,
        commands: &[Step],
        details: Vec<(&str, Value)>,
    ) -> Result<Record> {
        let mut record = Record::new();
        record.insert("state".into(), json!("pending"));
        record.insert("operation".into(), json!(operation));
        record.insert("timestamp".into(), json!(timestamp()));
        record.insert("unique_number".into(), json!(status.unique_number));
        for (key, value) in details {
            record.insert(key.into(), value);
        }
        for (command, params, timeout) in commands {
            record.insert("stage".into(), json!(command));
            self.save(&record)?;
            if let Err(error) = session.command(command, params, Some(*timeout)) {
                return Err(Error::protocol(format!(
                    "{error}\nWynik operacji wymaga sprawdzenia. \
                     Nie wysyłaj jej ponownie przed sprawdzeniem drukarki."
                )));
            }
        }
        record.insert("state".into(), json!("completed"));
        self.save(&record)?;
        Ok(record)
    }

    /// Refuse to print on a rate that is inactive or has changed since the last probe.
    fn check_rates(&self, status: &Status, lines: &[Line]) -> Result<()> {
        for line in lines {
            if !status.vat_rates[line.vat()].active() {
                return Err(Error::value(
                    "Wybrana stawka VAT jest nieaktywna na drukarce.",
                ));
            }
            if let Some(last) = &self.last_status
                && last.vat_rates[line.vat()] != status.vat_rates[line.vat()]
            {
                return Err(Error::value(
                    "Stawka VAT zmieniła się na drukarce. Odśwież połączenie i sprawdź stawki.",
                ));
            }
        }
        Ok(())
    }

    /// Print one fiscal receipt.
    ///
    /// `brand` is the company name: when the printer header does not already show it, it is
    /// added as a footer line so the receipt carries the brand. Extra `details` (e.g. a job
    /// id) are journaled with the operation.
    pub fn print_receipt(
        &self,
        lines: &[Line],
        payment: i64,
        brand: &str,
        details: Vec<(&str, Value)>,
    ) -> Result<Record> {
        if !(1..=MAX_LINES).contains(&lines.len()) {
            return Err(Error::value("Paragon musi zawierać od 1 do 500 pozycji."));
        }
        if payment != 0 && payment != 2 {
            return Err(Error::value("Wybierz płatność gotówką lub kartą."));
        }
        if details.iter().any(|(key, _)| {
            matches!(
                *key,
                "state"
                    | "operation"
                    | "timestamp"
                    | "unique_number"
                    | "stage"
                    | "total_cents"
                    | "payment"
                    | "footer"
                    | "lines"
                    | "receipt_number"
                    | "resolution"
                    | "acknowledged_at"
            )
        }) {
            return Err(Error::value(
                "Dodatkowe dane nie mogą nadpisywać rejestru fiskalnego.",
            ));
        }
        let brand = sanitize(brand, 40);
        let total: i64 = lines.iter().map(Line::total_cents).sum();
        if total > MAX_CENTS {
            return Err(Error::value("Suma paragonu przekracza zakres drukarki."));
        }
        self.locked(|| {
            self.require_resolved()?;
            let mut session = Session::open(&self.connection)?;
            let status = Self::check_ready(&mut session)?;
            self.check_rates(&status, lines)?;
            let header = if brand.is_empty() {
                String::new()
            } else {
                header_text(
                    session
                        .command("hdrget", &[], None)?
                        .get("tx")
                        .map_or("", String::as_str),
                )
            };
            let footer = if header
                .to_lowercase()
                .replace('ß', "ss")
                .contains(&brand.to_lowercase().replace('ß', "ss"))
            {
                String::new()
            } else {
                brand.clone()
            };
            let journal_lines: Vec<Value> = lines
                .iter()
                .map(|line| {
                    json!({
                        "name": line.name(),
                        "quantity": line.quantity_text(),
                        "price_cents": line.price_cents(),
                        "vat": line.vat(),
                        "total_cents": line.total_cents(),
                    })
                })
                .collect();
            let mut journaled = vec![
                ("total_cents", json!(total)),
                ("payment", json!(payment)),
                ("footer", json!(footer)),
                ("lines", json!(journal_lines)),
            ];
            journaled.extend(details);
            let mut record = self.run(
                &mut session,
                &status,
                "receipt",
                &receipt_commands(lines, payment, total, &footer),
                journaled,
            )?;
            let number = session
                .command("scnt", &[], None)
                .ok()
                .and_then(|params| params.get("bt").cloned())
                .unwrap_or_default();
            record.insert("receipt_number".into(), json!(number));
            self.save(&record)?;
            Ok(record)
        })
    }

    pub fn daily_report(&self) -> Result<Record> {
        self.locked(|| {
            self.require_resolved()?;
            let mut session = Session::open(&self.connection)?;
            let status = Self::check_ready(&mut session)?;
            let clock = session.command("rtcget", &[], None)?;
            let day = clock
                .get("da")
                .map(|value| value.chars().take(10).collect::<String>())
                .and_then(|day| chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").ok())
                .filter(|day| (1..=9999).contains(&day.year()))
                .map(|day| day.format("%Y-%m-%d").to_string())
                .ok_or_else(|| Error::protocol("Nie można odczytać daty drukarki."))?;
            self.run(
                &mut session,
                &status,
                "daily_report",
                &[("dailyrep", vec![("da", day)], 120.0)],
                Vec::new(),
            )
        })
    }

    pub fn monthly_report(&self, year: i32, month: u32) -> Result<Record> {
        if !(1..=9999).contains(&year) {
            return Err(Error::value("Nieprawidłowy miesiąc raportu."));
        }
        let day = chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .ok_or_else(|| Error::value("Nieprawidłowy miesiąc raportu."))?;
        self.locked(|| {
            self.require_resolved()?;
            let mut session = Session::open(&self.connection)?;
            let status = Self::check_ready(&mut session)?;
            self.run(
                &mut session,
                &status,
                "monthly_report",
                &[(
                    "monthlyrep",
                    vec![("da", day.to_string()), ("su", "0".to_string())],
                    120.0,
                )],
                Vec::new(),
            )
        })
    }

    pub fn periodic_report(
        &self,
        start: chrono::NaiveDate,
        end: chrono::NaiveDate,
        summary: bool,
    ) -> Result<Record> {
        if !(1..=9999).contains(&start.year()) || !(1..=9999).contains(&end.year()) {
            return Err(Error::value("Nieprawidłowa data raportu."));
        }
        if start > end {
            return Err(Error::value(
                "Data początkowa nie może być późniejsza niż końcowa.",
            ));
        }
        self.locked(|| {
            self.require_resolved()?;
            let mut session = Session::open(&self.connection)?;
            let status = Self::check_ready(&mut session)?;
            self.run(
                &mut session,
                &status,
                "periodic_report",
                &[(
                    "periodicrepbydates",
                    vec![
                        ("fd", start.to_string()),
                        ("td", end.to_string()),
                        ("su", if summary { "1" } else { "0" }.to_string()),
                    ],
                    300.0,
                )],
                Vec::new(),
            )
        })
    }

    /// Close the unresolved record once a human has checked the paper.
    ///
    /// The device it happened on must be connected and ready, so the check was possible;
    /// `force` skips that for a dead or replaced printer and records the fact.
    pub fn acknowledge_pending(&self, force: bool) -> Result<()> {
        self.locked(|| {
            let Some(mut pending) = self.pending()? else {
                return Ok(());
            };
            if force {
                pending.insert("resolution".into(), json!("manual"));
            } else {
                let status = {
                    let mut session = Session::open(&self.connection)?;
                    Self::check_ready(&mut session)?
                };
                if pending.get("unique_number").and_then(Value::as_str)
                    != Some(&status.unique_number)
                {
                    return Err(Error::value(
                        "Podłącz drukarkę, której dotyczy nierozstrzygnięta operacja.",
                    ));
                }
            }
            self.acknowledge_record(&mut pending)
        })
    }

    /// Close a journal record once its outcome is known and reported.
    pub fn acknowledge(&self, record: &mut Record) -> Result<()> {
        self.locked(|| self.acknowledge_record(record))
    }

    fn acknowledge_record(&self, record: &mut Record) -> Result<()> {
        record.insert("state".into(), json!("acknowledged"));
        record.insert("acknowledged_at".into(), json!(timestamp()));
        self.save(record)
    }
}

fn timestamp() -> String {
    Local::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}
