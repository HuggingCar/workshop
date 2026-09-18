//! Polls HuggingCar for receipt jobs and prints them.
//!
//! Every fiscal rule lives in the driver: intent is journaled before the first command
//! reaches the printer, nothing is retried on the device, and an uncertain outcome blocks
//! the agent until a person resolves it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use posnet::{Line, Printer, Record, models::exempt_percent};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::{
    api::Api,
    error::{Error, Result},
};

pub const POLL_SECONDS: u64 = 3;
pub const RETRY_SECONDS: u64 = 15;

pub const DONE: i64 = 3;
pub const FAILED: i64 = 4;
pub const UNKNOWN: i64 = 6;
pub const TAKEN: i64 = 2;

/// Where the agent reports what it is doing; the GUI shows it, a headless run logs it.
pub type Notify = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub struct Agent {
    pub printer: Printer,
    pub api: Api,
    pub stopping: Arc<AtomicBool>,
    pub notify: Notify,
}

impl Agent {
    pub fn new(printer: Printer, api: Api, notify: Notify) -> Self {
        Self {
            printer,
            api,
            stopping: Arc::new(AtomicBool::new(false)),
            notify,
        }
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    /// Sleeps in short slices so a stop request is noticed without abandoning a receipt.
    fn rest(&self, seconds: u64) {
        for _ in 0..seconds * 10 {
            if self.stopping() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn run_forever(&mut self) {
        // A stop request waits for the job in progress: interrupting a receipt
        // would leave a pending journal that only a human can resolve.
        while !self.stopping() {
            if let Err(error) = self.step() {
                (self.notify)(
                    &format!("{error} — ponowna próba za {RETRY_SECONDS}s"),
                    "error",
                );
                self.rest(RETRY_SECONDS);
            }
        }
        (self.notify)("Zatrzymano", "");
    }

    pub fn step(&mut self) -> Result<()> {
        if self.settle_last_operation()? {
            self.rest(RETRY_SECONDS);
            return Ok(());
        }
        // Taking a job the printer cannot print would fail it for good, so check first.
        let status = self.printer.probe()?;
        self.api.describe(&status);
        if !status.ready {
            (self.notify)(
                &format!(
                    "Drukarka niegotowa: {} — ponowna próba za {RETRY_SECONDS}s",
                    status.description
                ),
                "error",
            );
            self.rest(RETRY_SECONDS);
            return Ok(());
        }
        match self.api.take()? {
            Some(job) => self.execute(&job),
            None => {
                self.rest(POLL_SECONDS);
                Ok(())
            }
        }
    }

    /// Reconcile the journal with the server after a crash. `true` = still blocked.
    fn settle_last_operation(&mut self) -> Result<bool> {
        let Some(mut record) = self.printer.last_record()? else {
            return Ok(false);
        };
        let state = record.get("state").and_then(Value::as_str).unwrap_or("");
        let Some(job_id) = record.get("job_id").and_then(Value::as_i64) else {
            return Ok(false);
        };
        if state == "acknowledged" {
            return Ok(false);
        }
        let remote = self.api.job_status(job_id)?;
        if state == "completed" {
            if remote == TAKEN || remote == UNKNOWN {
                let number = record
                    .get("receipt_number")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.api.report(job_id, DONE, &number, "")?;
            }
            self.printer.acknowledge(&mut record)?;
            return Ok(false);
        }
        // pending: the printer may or may not have printed; a human must look at the paper
        if remote == TAKEN {
            self.api.report(
                job_id,
                UNKNOWN,
                "",
                "Przerwano w trakcie drukowania. Sprawdź drukarkę.",
            )?;
            return Ok(true);
        }
        if remote == UNKNOWN {
            return Ok(true);
        }
        self.printer.acknowledge_pending(false)?;
        Ok(false)
    }

    /// The slot of the printer's standard rate.
    ///
    /// Automotive work is single-rate per company (PL: 23% on labour and parts), so the rate
    /// programmed in the device is the right one; per-item rates would need the server to
    /// send one per line.
    fn standard_vat_slot(&self) -> Result<usize> {
        let rates = self
            .printer
            .last_status
            .as_ref()
            .map(|status| status.vat_rates.as_slice())
            .unwrap_or_default();
        rates
            .iter()
            .filter(|vat| vat.active() && vat.percent != exempt_percent())
            .max_by_key(|vat| vat.percent)
            .or_else(|| rates.iter().find(|vat| vat.active()))
            .map(|vat| vat.index)
            .ok_or_else(|| Error::value("Drukarka nie ma zaprogramowanej żadnej stawki VAT."))
    }

    pub fn execute(&mut self, job: &Value) -> Result<()> {
        let job_id = job
            .get("pk")
            .and_then(Value::as_i64)
            .ok_or_else(|| Error::value("Zlecenie bez identyfikatora."))?;
        let lines = match self.job_lines(job) {
            Ok(lines) => lines,
            Err(error) => return self.api.report(job_id, FAILED, "", &error.to_string()),
        };
        let Some(payment) = job.get("payment").and_then(Value::as_i64) else {
            return self
                .api
                .report(job_id, FAILED, "", "Brak prawidłowej płatności.");
        };
        let brand = match job.pointer("/payload/company_name") {
            None => "",
            Some(Value::String(brand)) => brand,
            Some(_) => {
                return self
                    .api
                    .report(job_id, FAILED, "", "Nieprawidłowa nazwa firmy.");
            }
        };

        let printed = self.printer.print_receipt(
            &lines,
            payment,
            brand,
            vec![("job_id", Value::from(job_id))],
        );
        let mut record: Record = match printed {
            Ok(record) => record,
            Err(error) => {
                // Journal writes can fail after paper was printed. Error types alone
                // cannot prove that a receipt failed before the first fiscal command.
                let record = self.printer.last_record()?;
                let state = record
                    .as_ref()
                    .filter(|record| record.get("job_id").and_then(Value::as_i64) == Some(job_id))
                    .and_then(|record| record.get("state"))
                    .and_then(Value::as_str);
                if state == Some("completed") {
                    return Err(error.into()); // reconciliation reports DONE, never reprints
                }
                let status = if state == Some("pending") {
                    UNKNOWN
                } else {
                    FAILED
                };
                return self.api.report(job_id, status, "", &error.to_string());
            }
        };
        let number = record
            .get("receipt_number")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.api.report(job_id, DONE, &number, "")?;
        self.printer.acknowledge(&mut record)?;
        (self.notify)(
            &format!("Paragon {number} dla zlecenia (job {job_id})"),
            "success",
        );
        Ok(())
    }

    fn job_lines(&self, job: &Value) -> Result<Vec<Line>> {
        let vat = self.standard_vat_slot()?;
        let items = job
            .pointer("/payload/lines")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::value("Zlecenie bez pozycji paragonu."))?;
        let number = |item: &Value, key: &str| -> Result<Decimal> {
            let raw = match item.get(key) {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Number(value)) => value.to_string(),
                _ => return Err(Error::value(format!("Brak wartości {key} w pozycji."))),
            };
            let raw = raw.trim();
            let parsed = if let Some((base, _)) = raw.split_once(['e', 'E']) {
                // from_scientific parses its mantissa lossily; reject rounding first.
                Decimal::from_str_exact(base).and_then(|_| Decimal::from_scientific(raw))
            } else {
                Decimal::from_str_exact(raw)
            };
            parsed.map_err(|_| Error::value(format!("Nieprawidłowa wartość {key}: {raw}.")))
        };
        items
            .iter()
            .map(|item| {
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::value("Pozycja bez nazwy."))?;
                Ok(Line::new(
                    name,
                    number(item, "quantity")?,
                    number(item, "unit_price")?,
                    vat,
                )?)
            })
            .collect()
    }
}
