use std::{
    collections::BTreeMap,
    io::{BufReader, Read, Write},
    time::{Duration, Instant},
};

use encoding_rs::WINDOWS_1250;

use crate::{
    error::{Error, Result},
    models::printable,
};

pub const MAX_FRAME: usize = 16384;
pub const MAX_TIMEOUT: f64 = 120.0;
/// Every response parameter is keyed by two letters.
pub const KEY_LENGTH: usize = 2;
pub const STX: u8 = 0x02;
pub const ETX: u8 = 0x03;
pub const CORRUPT: &str = "Uszkodzona odpowiedź drukarki (CRC lub kodowanie).";

const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_XMODEM);

pub type Params = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    pub address: String,
    pub baudrate: u32,
    pub timeout: f64,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            address: String::new(),
            baudrate: 9600,
            timeout: 3.0,
        }
    }
}

impl Connection {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            ..Self::default()
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.address.trim().is_empty() {
            return Err(Error::value("Nie wykryto drukarki."));
        }
        // Only explicitly simulator-enabled builds may use a non-serial transport.
        if self.address.contains("://") && !(cfg!(feature = "sim") && self.is_simulator()) {
            return Err(Error::value("Adres drukarki musi być portem szeregowym."));
        }
        if !(self.timeout > 0.0 && self.timeout <= MAX_TIMEOUT) {
            return Err(Error::value("Nieprawidłowy limit czasu połączenia."));
        }
        Ok(())
    }

    fn is_simulator(&self) -> bool {
        self.address.starts_with("sim://")
    }
}

fn valid_command(command: &str) -> bool {
    let body = command.strip_prefix('!').unwrap_or(command);
    let mut chars = body.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|char| char.is_ascii_lowercase() || char.is_ascii_digit())
}

fn valid_key(key: &str) -> bool {
    key == "@" || (key.len() == KEY_LENGTH && key.chars().all(|char| char.is_ascii_lowercase()))
}

fn to_cp1250(text: &str) -> Vec<u8> {
    WINDOWS_1250.encode(text).0.into_owned()
}

pub fn encode_frame(command: &str, params: &[(&str, String)]) -> Result<Vec<u8>> {
    if !valid_command(command) {
        return Err(Error::value("Nieprawidłowe polecenie drukarki."));
    }
    let mut payload = to_cp1250(command);
    for (key, value) in params {
        if !valid_key(key) {
            return Err(Error::value("Nieprawidłowy parametr drukarki."));
        }
        printable(value)?;
        payload.push(b'\t');
        payload.extend_from_slice(&to_cp1250(key));
        payload.extend_from_slice(&to_cp1250(value));
    }
    payload.push(b'\t');
    let mut frame = Vec::with_capacity(payload.len() + 7);
    frame.push(STX);
    frame.extend_from_slice(&payload);
    frame.extend_from_slice(format!("#{:04X}", CRC.checksum(&payload)).as_bytes());
    frame.push(ETX);
    if frame.len() > MAX_FRAME {
        return Err(Error::value("Polecenie drukarki jest zbyt długie."));
    }
    Ok(frame)
}

/// Response parameters plus the error code the printer reported, if any.
fn frame_params(fields: &[&str]) -> Result<(Params, Option<i64>)> {
    let mut params = Params::new();
    let mut error = None;
    for field in fields {
        if field.is_empty() {
            continue;
        }
        if let Some(code) = field.strip_prefix('?') {
            error = Some(
                code.parse::<i64>()
                    .map_err(|_| Error::protocol("Nieprawidłowy kod błędu drukarki."))?,
            );
        } else if let Some(value) = field.strip_prefix('@') {
            params.insert("@".into(), value.into());
        } else if field.chars().count() >= KEY_LENGTH {
            let split = field
                .char_indices()
                .nth(KEY_LENGTH)
                .map_or(field.len(), |(index, _)| index);
            let (key, value) = field.split_at(split);
            if params.insert(key.into(), value.into()).is_some() {
                return Err(Error::protocol(
                    "Powtórzony parametr w odpowiedzi drukarki.",
                ));
            }
        } else {
            return Err(Error::protocol("Niepełny parametr odpowiedzi drukarki."));
        }
    }
    Ok((params, error))
}

pub fn decode_frame(frame: &[u8]) -> Result<(String, Params)> {
    if frame.len() > MAX_FRAME || frame.first() != Some(&STX) || frame.last() != Some(&ETX) {
        return Err(Error::protocol("Nieprawidłowa ramka odpowiedzi drukarki."));
    }
    let body = &frame[1..frame.len() - 1];
    let Some(hash) = body.iter().rposition(|byte| *byte == b'#') else {
        return Err(Error::protocol(CORRUPT));
    };
    let (payload, checksum) = (&body[..hash], &body[hash + 1..]);
    let valid = checksum.len() == 4
        && checksum.iter().all(u8::is_ascii_hexdigit)
        && u16::from_str_radix(std::str::from_utf8(checksum).unwrap_or(""), 16)
            == Ok(CRC.checksum(payload));
    if !valid {
        return Err(Error::protocol(CORRUPT));
    }
    let (text, _, malformed) = WINDOWS_1250.decode(payload);
    if malformed
        || payload
            .iter()
            .any(|byte| matches!(byte, 0x81 | 0x83 | 0x88 | 0x90 | 0x98))
    {
        return Err(Error::protocol(CORRUPT));
    }
    let fields: Vec<&str> = text.split('\t').collect();
    let command = fields[0].to_string();
    let (params, error) = frame_params(&fields[1..])?;
    if let Some(code) = error {
        return Err(Error::Device {
            code,
            command: params.get("cm").unwrap_or(&command).clone(),
            field: params.get("fd").cloned().unwrap_or_default(),
        });
    }
    if command == "ERR" {
        return Err(Error::protocol("Drukarka odrzuciła ramkę bez kodu błędu."));
    }
    Ok((command, params))
}

enum Transport {
    Serial(Box<dyn serialport::SerialPort>),
    #[cfg(feature = "sim")]
    Socket(std::net::TcpStream),
}

impl Transport {
    fn set_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Self::Serial(port) => port.set_timeout(timeout).map_err(std::io::Error::from),
            #[cfg(feature = "sim")]
            Self::Socket(stream) => {
                stream.set_read_timeout(Some(timeout))?;
                stream.set_write_timeout(Some(timeout))
            }
        }
    }
}

impl Read for Transport {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Serial(port) => port.read(buffer),
            #[cfg(feature = "sim")]
            Self::Socket(stream) => stream.read(buffer),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Serial(port) => port.write(buffer),
            #[cfg(feature = "sim")]
            Self::Socket(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Serial(port) => port.flush(),
            #[cfg(feature = "sim")]
            Self::Socket(stream) => stream.flush(),
        }
    }
}

/// One exclusive serial connection; no command is ever automatically retried.
pub struct Session {
    connection: Connection,
    stream: BufReader<Transport>,
}

impl Session {
    pub fn open(connection: &Connection) -> Result<Self> {
        connection.validate()?;
        let timeout = Duration::from_secs_f64(connection.timeout);
        let transport = Self::connect(connection, timeout)?;
        Ok(Self {
            connection: connection.clone(),
            stream: BufReader::new(transport),
        })
    }

    #[cfg(feature = "sim")]
    fn connect(connection: &Connection, timeout: Duration) -> Result<Transport> {
        if let Some(address) = connection.address.strip_prefix("sim://") {
            let stream = std::net::TcpStream::connect(address).map_err(|err| Error::open(&err))?;
            stream.set_nodelay(true).ok();
            let mut transport = Transport::Socket(stream);
            transport
                .set_timeout(timeout)
                .map_err(|err| Error::open(&err))?;
            return Ok(transport);
        }
        Self::open_serial(connection, timeout)
    }

    #[cfg(not(feature = "sim"))]
    fn connect(connection: &Connection, timeout: Duration) -> Result<Transport> {
        Self::open_serial(connection, timeout)
    }

    fn open_serial(connection: &Connection, timeout: Duration) -> Result<Transport> {
        serialport::new(&connection.address, connection.baudrate)
            .timeout(timeout)
            .open()
            .map(Transport::Serial)
            .map_err(|err| Error::open(&std::io::Error::from(err)))
    }

    pub fn command(
        &mut self,
        command: &str,
        params: &[(&str, String)],
        timeout: Option<f64>,
    ) -> Result<Params> {
        let frame = encode_frame(command, params)?;
        let duration = Duration::try_from_secs_f64(timeout.unwrap_or(self.connection.timeout))
            .ok()
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| Error::value("Nieprawidłowy limit czasu polecenia."))?;
        let deadline = Instant::now()
            .checked_add(duration)
            .ok_or_else(|| Error::value("Nieprawidłowy limit czasu polecenia."))?;
        self.write_frame(&frame, command, deadline)?;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let raw = self.read_frame(deadline)?;
            if raw.last() != Some(&ETX) {
                if raw.len() >= MAX_FRAME {
                    return Err(Error::protocol("Zbyt długa odpowiedź drukarki."));
                }
                break;
            }
            // Line noise before the frame: the CRC guards the frame that follows.
            let Some(start) = raw.iter().rposition(|byte| *byte == STX) else {
                continue;
            };
            let (response_command, response) = decode_frame(&raw[start..])?;
            if response_command != command {
                return Err(Error::protocol(format!(
                    "Oczekiwano odpowiedzi {command}, otrzymano {response_command}."
                )));
            }
            return Ok(response);
        }
        Err(Error::timeout(command))
    }

    fn write_frame(&mut self, mut frame: &[u8], command: &str, deadline: Instant) -> Result<()> {
        let transport = self.stream.get_mut();
        while !frame.is_empty() {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| Error::timeout(command))?;
            transport
                .set_timeout(remaining)
                .map_err(|err| Error::broken(&err))?;
            match transport.write(frame) {
                Ok(0) => return Err(Error::protocol("Niepełne wysłanie polecenia do drukarki.")),
                Ok(written) => frame = &frame[written..],
                Err(err) if timed_out(&err) => return Err(Error::timeout(command)),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(Error::broken(&err)),
            }
        }
        Ok(())
    }

    /// Read one response against the command's absolute deadline, not a per-byte timeout.
    fn read_frame(&mut self, deadline: Instant) -> Result<Vec<u8>> {
        let mut raw = Vec::new();
        while raw.len() < MAX_FRAME {
            let Some(remaining) = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
            else {
                break;
            };
            self.stream
                .get_mut()
                .set_timeout(remaining)
                .map_err(|err| Error::broken(&err))?;
            let mut byte = [0u8; 1];
            match self.stream.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    raw.push(byte[0]);
                    if byte[0] == ETX {
                        break;
                    }
                }
                Err(err) if timed_out(&err) => break,
                Err(err) => return Err(Error::broken(&err)),
            }
        }
        Ok(raw)
    }
}

fn timed_out(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}
