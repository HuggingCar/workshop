//! Offline Temo peer. Every command crosses an actual TCP connection.
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use chrono::Local;
use encoding_rs::WINDOWS_1250;
use parking_lot::Mutex;
use rust_decimal::{Decimal, RoundingStrategy, prelude::ToPrimitive};
use serde_json::{Value, json};

use crate::protocol::{Connection, MAX_FRAME, Params, decode_frame};

#[derive(Clone, Debug)]
pub struct SimulatorState {
    pub receipts: Vec<Value>,
    pub reports: Vec<Value>,
    pub footers: Vec<Value>,
    pub header: String,
    pub transaction_open: bool,
    pub footer_open: bool,
    pub lines: Vec<Value>,
    pub payment: Option<Params>,
    /// Complete replacement responses, including malformed status replies.
    pub responses: BTreeMap<String, Params>,
    /// Execution errors, before any mutation.
    pub errors: BTreeMap<String, i64>,
    /// Execute but send no acknowledgement.
    pub drop_responses: BTreeSet<String>,
    /// Execute and close the connection before acknowledging.
    pub disconnect_after: BTreeSet<String>,
    /// Delay before execution; commands are recorded on arrival.
    pub delays: BTreeMap<String, Duration>,
    pub commands: Vec<Value>,
}

impl Default for SimulatorState {
    fn default() -> Self {
        Self {
            receipts: vec![],
            reports: vec![],
            footers: vec![],
            header: "&c&1Warsztat Kowalski&1&c\nul. Testowa 1\n00-001 Warszawa".into(),
            transaction_open: false,
            footer_open: false,
            lines: vec![],
            payment: None,
            responses: BTreeMap::new(),
            errors: BTreeMap::new(),
            drop_responses: BTreeSet::new(),
            disconnect_after: BTreeSet::new(),
            delays: BTreeMap::new(),
            commands: vec![],
        }
    }
}

pub struct Simulator {
    pub connection: Connection,
    state: Arc<Mutex<SimulatorState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Simulator {
    pub fn start() -> io::Result<Self> {
        Self::start_on(0)
    }

    pub fn start_on(port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        let connection = Connection::new(format!("sim://{}", listener.local_addr()?));
        let state = Arc::new(Mutex::new(SimulatorState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = Arc::clone(&state);
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("Temo simulator".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((peer, _)) => serve(peer, &worker_state, &worker_stop),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            connection,
            state,
            stop,
            thread: Some(thread),
        })
    }

    pub fn snapshot(&self) -> SimulatorState {
        self.state.lock().clone()
    }

    pub fn update(&self, update: impl FnOnce(&mut SimulatorState)) {
        update(&mut self.state.lock());
    }
}

impl Drop for Simulator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut peer: TcpStream, state: &Mutex<SimulatorState>, stop: &AtomicBool) {
    if peer
        .set_read_timeout(Some(Duration::from_millis(20)))
        .is_err()
        || peer
            .set_write_timeout(Some(Duration::from_millis(100)))
            .is_err()
    {
        return;
    }
    let mut frame = Vec::new();
    let mut oversized = false;
    while !stop.load(Ordering::Relaxed) {
        let mut buffer = [0; 4096];
        let count = match peer.read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => count,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return,
        };
        for byte in &buffer[..count] {
            if !oversized {
                frame.push(*byte);
            }
            if frame.len() > MAX_FRAME {
                oversized = true;
            }
            if *byte != 3 {
                continue;
            }
            let decoded = if oversized {
                None
            } else {
                decode_frame(&frame).ok()
            };
            frame.clear();
            oversized = false;
            let Some((command, params)) = decoded else {
                if reply(&mut peer, "ERR\t?2000\t").is_err() {
                    return;
                }
                continue;
            };
            let delay = {
                let mut state = state.lock();
                state
                    .commands
                    .push(json!({"command":command,"params":params}));
                state.delays.get(&command).copied().unwrap_or_default()
            };
            let started = Instant::now();
            while started.elapsed() < delay {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                thread::sleep(
                    (delay - started.elapsed().min(delay)).min(Duration::from_millis(10)),
                );
            }
            let (response, drop_reply, disconnect) = {
                let mut state = state.lock();
                let result = if let Some(code) = state.errors.get(&command) {
                    Err(*code)
                } else if let Some(response) = state.responses.get(&command) {
                    Ok(response.clone())
                } else {
                    handle(&mut state, &command, params).ok_or(2000)
                };
                (
                    result,
                    state.drop_responses.contains(&command),
                    state.disconnect_after.contains(&command),
                )
            };
            if disconnect {
                return;
            }
            if drop_reply {
                continue;
            }
            let payload = match response {
                Ok(params) => {
                    let mut text = format!("{command}\t");
                    for (key, value) in params {
                        text.push_str(&key);
                        text.push_str(&value);
                        text.push('\t');
                    }
                    text
                }
                Err(code) => format!("{command}\t?{code}\t"),
            };
            if reply(&mut peer, &payload).is_err() {
                return;
            }
        }
    }
}

// Device replies permit LF (hdrget); outgoing host frames deliberately do not.
fn reply(peer: &mut TcpStream, payload: &str) -> io::Result<()> {
    let (payload, _, malformed) = WINDOWS_1250.encode(payload);
    if malformed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Simulator response is not CP1250",
        ));
    }
    let crc = crc::Crc::<u16>::new(&crc::CRC_16_XMODEM).checksum(&payload);
    let mut frame = vec![2];
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(format!("#{crc:04X}\x03").as_bytes());
    peer.write_all(&frame)
}

fn params(values: &[(&str, &str)]) -> Params {
    values
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn handle(state: &mut SimulatorState, command: &str, input: Params) -> Option<Params> {
    let response = match command {
        "getrealid" => params(&[
            ("nm", "POSNET TEMO ONLINE"),
            ("vr", "32.01"),
            ("nu", "DEMO0000001"),
        ]),
        "sdev" => params(&[("ds", "0"), ("cp", "1"), ("qe", "1"), ("pe", "0")]),
        "sprn" => params(&[("pr", "0")]),
        "scomm" => params(&[
            ("fs", "1"),
            ("tz", "0"),
            ("ts", if state.transaction_open { "16" } else { "0" }),
            ("hr", "1"),
            ("nu", "DEMO0000001"),
        ]),
        "strns" => params(&[
            ("to", if state.transaction_open { "1" } else { "0" }),
            ("ts", "16"),
            ("fe", if state.footer_open { "1" } else { "0" }),
        ]),
        "hdrget" => params(&[("tx", &state.header)]),
        "scnt" => params(&[("bt", &state.receipts.len().to_string())]),
        "vatget" => params(&[
            ("va", "23,00"),
            ("vb", "8,00"),
            ("vc", "0,00"),
            ("vd", "101,00"),
            ("ve", "101,00"),
            ("vf", "101,00"),
            ("vg", "100,00"),
        ]),
        "rtcget" => params(&[("da", &Local::now().format("%Y-%m-%d;%H:%M").to_string())]),
        "trinit" if !state.transaction_open && !state.footer_open => {
            state.transaction_open = true;
            state.lines.clear();
            state.payment = None;
            Params::new()
        }
        "trline" if state.transaction_open => {
            let quantity = input.get("il")?.parse::<Decimal>().ok()?;
            let price = input.get("pr")?.parse::<i64>().ok()?;
            let value = quantity
                .checked_mul(Decimal::from(price))?
                .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
                .to_i64()?;
            if quantity <= Decimal::ZERO
                || price <= 0
                || input.get("wa")?.parse::<i64>().ok()? != value
                || ![0, 1, 2, 6].contains(&input.get("vt")?.parse::<usize>().ok()?)
            {
                return None;
            }
            state.lines.push(json!(input));
            Params::new()
        }
        "trpayment" if state.transaction_open => {
            if ![0, 2].contains(&input.get("ty")?.parse::<i64>().ok()?) {
                return None;
            }
            state.payment = Some(input);
            Params::new()
        }
        "trend" if state.transaction_open && !state.lines.is_empty() => {
            let payment = state.payment.as_ref()?;
            let total = state.lines.iter().try_fold(0i64, |sum, line| {
                sum.checked_add(line["wa"].as_str()?.parse::<i64>().ok()?)
            })?;
            if input.get("to")?.parse::<i64>().ok()? != total
                || input.get("fp")?.parse::<i64>().ok()? != total
                || payment.get("wa")?.parse::<i64>().ok()? != total
            {
                return None;
            }
            state.receipts.push(json!({"total_cents":total,"payment":payment.get("ty")?.parse::<i64>().ok()?,"lines":state.lines}));
            state.transaction_open = false;
            state.footer_open = input.get("fe").is_some_and(|v| v == "0");
            Params::new()
        }
        "trftrln" if state.footer_open => {
            state.footers.push(json!(input));
            Params::new()
        }
        "trftrend" if state.footer_open => {
            state.footer_open = false;
            Params::new()
        }
        "dailyrep" | "monthlyrep" | "periodicrepbydates"
            if !state.transaction_open && !state.footer_open =>
        {
            state
                .reports
                .push(json!({"command":command,"params":input}));
            Params::new()
        }
        _ => return None,
    };
    Some(response)
}
