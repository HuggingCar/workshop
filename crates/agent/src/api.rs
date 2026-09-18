use std::time::{Duration, Instant};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use posnet::{Status, models::exempt_percent};
use serde_json::{Value, json};
use url::Url;

use crate::error::{Error, Result};

const TIMEOUT: Duration = Duration::from_secs(30);
/// Percent-encode device metadata, keeping unreserved characters and `/`.
const QUOTE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'_')
    .remove(b'.')
    .remove(b'-')
    .remove(b'~')
    .remove(b'/');

pub fn validated_url(value: &str) -> Result<String> {
    let rejected = || {
        Error::value(
            "Adres API wymaga https (http tylko dla localhost), \
             bez danych logowania, zapytania i fragmentu.",
        )
    };
    let url = Url::parse(value).map_err(|_| rejected())?;
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    let host = url.host_str().unwrap_or_default();
    let loopback = match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(address) => address.is_loopback(),
        Err(_) => host == "localhost",
    };
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if host.is_empty()
        || authority.is_empty()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !scheme_ok
    {
        return Err(rejected());
    }
    Ok(value.trim_end_matches('/').to_string())
}

pub struct Api {
    base_url: String,
    token: String,
    access_token: String,
    /// `None` until a session is fetched, then the moment to renew it.
    refresh_at: Option<Instant>,
    headers: Vec<(&'static str, String)>,
    http: ureq::Agent,
}

impl Api {
    pub fn new(base_url: &str, token: &str, device_serial: &str) -> Result<Self> {
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // urllib would forward Authorization to a redirect target; neither client follows one.
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            base_url: validated_url(base_url)?,
            token: token.to_string(),
            access_token: String::new(),
            refresh_at: None,
            headers: vec![
                ("X-Device-Serial", device_serial.to_string()),
                ("X-Device-Vendor", "posnet".to_string()),
                ("Content-Type", "application/json".to_string()),
            ],
            http,
        })
    }

    fn set_header(&mut self, name: &'static str, value: String) {
        match self.headers.iter_mut().find(|(key, _)| *key == name) {
            Some(header) => header.1 = value,
            None => self.headers.push((name, value)),
        }
    }

    /// Carry the device's own model, firmware and active VAT rates on every later call.
    ///
    /// Percent-encoded: header values must stay ASCII, device text need not be.
    pub fn describe(&mut self, status: &Status) {
        let rates: Vec<Value> = status
            .vat_rates
            .iter()
            .filter(|vat| vat.active())
            .map(|vat| {
                let percent = if vat.percent == exempt_percent() {
                    "zw".to_string()
                } else {
                    vat.percent.normalize().to_string()
                };
                json!({"index": vat.index, "percent": percent})
            })
            .collect();
        let rates = serde_json::to_string(&rates).unwrap_or_else(|_| "[]".into());
        self.set_header("X-Device-Model", quote(&status.name));
        self.set_header("X-Device-Firmware", quote(&status.version));
        self.set_header("X-Device-Vat-Rates", quote(&rates));
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        authorization: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        let url = format!("{}/integrations/fiscal/agent/{path}", self.base_url);
        let mut response = if method == "POST" {
            let mut request = self.http.post(&url).header("Authorization", authorization);
            for (key, value) in &self.headers {
                request = request.header(*key, value);
            }
            match body {
                Some(body) => request.send_json(body),
                None => request.send_empty(),
            }
        } else {
            let mut request = self.http.get(&url).header("Authorization", authorization);
            for (key, value) in &self.headers {
                request = request.header(*key, value);
            }
            request.call()
        }
        .map_err(|err| Error::Request(err.to_string()))?;

        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(Error::Status(
                status,
                "Serwer przekierował żądanie. Podaj docelowy adres API.".into(),
            ));
        }
        if !(200..300).contains(&status) {
            let detail = response.body_mut().read_to_string().unwrap_or_default();
            return Err(Error::Status(status, detail.chars().take(500).collect()));
        }
        if status == 204 {
            return Ok(None);
        }
        response
            .body_mut()
            .read_json::<Value>()
            .map(Some)
            .map_err(|err| Error::Request(err.to_string()))
    }

    pub fn connect(&mut self) -> Result<()> {
        let started = Instant::now();
        let session = self.send("POST", "session/", &format!("Agent {}", self.token), None)?;
        let access_token = session
            .as_ref()
            .and_then(|session| session.get("access_token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty());
        let expires_in = session
            .as_ref()
            .and_then(|session| session.get("expires_in"))
            .and_then(Value::as_u64)
            .filter(|seconds| *seconds > 0);
        let (Some(access_token), Some(expires_in)) = (access_token, expires_in) else {
            return Err(Error::value("Nieprawidłowa odpowiedź sesji API."));
        };
        let renewal = Duration::try_from_secs_f64(expires_in as f64 * 0.9)
            .ok()
            .and_then(|duration| started.checked_add(duration))
            .ok_or_else(|| Error::value("Nieprawidłowa odpowiedź sesji API."))?;
        self.access_token = access_token.to_string();
        self.refresh_at = Some(renewal);
        Ok(())
    }

    fn call(&mut self, method: &str, path: &str, body: Option<Value>) -> Result<Option<Value>> {
        if self.refresh_at.is_none_or(|at| Instant::now() >= at) {
            self.connect()?;
        }
        let authorization = format!("Bearer {}", self.access_token);
        match self.send(
            method,
            &format!("jobs/{path}"),
            &authorization,
            body.clone(),
        ) {
            Err(Error::Status(401, _)) => {}
            other => return other,
        }
        // Only an authentication rejection is safe to retry: no job handler ran.
        self.refresh_at = None;
        self.connect()?;
        let authorization = format!("Bearer {}", self.access_token);
        self.send(method, &format!("jobs/{path}"), &authorization, body)
    }

    pub fn take(&mut self) -> Result<Option<Value>> {
        self.call("POST", "take/", None)
    }

    pub fn job_status(&mut self, job_id: i64) -> Result<i64> {
        self.call("GET", &format!("{job_id}/"), None)?
            .and_then(|job| job.get("status").and_then(Value::as_i64))
            .ok_or_else(|| Error::value("Nieprawidłowa odpowiedź statusu zlecenia."))
    }

    pub fn report(
        &mut self,
        job_id: i64,
        status: i64,
        receipt_number: &str,
        result: &str,
    ) -> Result<()> {
        let result: String = result.chars().take(2000).collect();
        self.call(
            "POST",
            &format!("{job_id}/result/"),
            Some(json!({
                "status": status,
                "receipt_number": receipt_number,
                "result": result,
            })),
        )?;
        Ok(())
    }
}

fn quote(value: &str) -> String {
    utf8_percent_encode(value, QUOTE).to_string()
}
