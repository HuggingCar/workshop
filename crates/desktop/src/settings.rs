use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use posnet::{Connection, validate_name};
use serde::{Deserialize, Serialize};

pub const SERVICES: &[&str] = &[
    "Usługa własna",
    "Naprawa auta",
    "Naprawa silnika",
    "Naprawa elektryki",
    "Naprawa układu paliwowego",
    "Naprawa układu wydechowego",
    "Naprawa układu hamulcowego",
    "Naprawa układu chłodzenia",
    "Naprawa układu kierowniczego",
    "Naprawa układu klimatyzacji",
    "Serwis opon",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub address: String,
    pub baudrate: u32,
    pub services: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            address: String::new(),
            baudrate: 9600,
            services: SERVICES.iter().map(|s| (*s).into()).collect(),
        }
    }
}

impl Settings {
    pub fn connection(&self) -> Connection {
        Connection {
            address: self.address.clone(),
            baudrate: self.baudrate,
            ..Connection::default()
        }
    }

    pub fn normalized(&self) -> Result<Self, String> {
        if self.baudrate == 0 {
            return Err("Nieprawidłowa prędkość transmisji.".into());
        }
        let mut config = self.clone();
        config.address = config.address.trim().to_owned();
        if !config.address.is_empty() {
            config.connection().validate().map_err(|e| e.to_string())?;
        }
        config.services.clear();
        for name in &self.services {
            let name = validate_name(name).map_err(|e| e.to_string())?;
            if !config.services.contains(&name) {
                config.services.push(name);
            }
        }
        if config.services.is_empty() {
            return Err("Dodaj przynajmniej jedną usługę.".into());
        }
        Ok(config)
    }

    pub fn load_file(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| e.to_string())?;
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Nie można odczytać ustawień: {e}"))?;
        config.normalized()
    }

    /// Import Qt once; leave its original settings untouched for a safe rollback.
    pub fn load(data: &Path) -> Result<Self, String> {
        let path = data.join("settings.json");
        if path.exists() {
            return Self::load_file(&path);
        }
        let config = legacy_settings()?.unwrap_or_default().normalized()?;
        config.save(&path)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let config = self.normalized()?;
        let parent = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut file, &config).map_err(|e| e.to_string())?;
        file.flush()
            .and_then(|_| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        file.persist(path).map_err(|e| e.to_string())?;
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    pub fn from_qt_ini(text: &str) -> Self {
        let mut result = Self::default();
        let mut section = "";
        for raw in text.lines() {
            let line = raw.trim();
            if line.starts_with('[') && line.ends_with(']') {
                section = &line[1..line.len() - 1];
                continue;
            }
            let Some((key, raw)) = line.split_once('=') else {
                continue;
            };
            let values = qt_list(raw);
            let value = values.first().cloned().unwrap_or_default();
            match (section, key.trim()) {
                ("connection", "address") | ("General", "connection\\address") => {
                    result.address = qt_string(value)
                }
                ("connection", "baudrate") | ("General", "connection\\baudrate") => {
                    result.baudrate = value.parse().ok().filter(|n| *n > 0).unwrap_or(9600)
                }
                ("General", "services") => {
                    result.services = qt_catalog(values);
                }
                _ => {}
            }
        }
        result
    }
}

/// QSettings INI strings: commas outside quotes delimit QStringList, backslash escapes
/// protect quotes and Unicode. A one-element list is deliberately not JSON in Qt.
fn qt_list(raw: &str) -> Vec<String> {
    if raw.trim() == "@Invalid()" {
        return vec![];
    }
    let mut parts = Vec::new();
    let mut part = String::new();
    let mut quoted = false;
    let mut chars = raw.trim().chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                parts.push(part.trim().to_string());
                part.clear();
            }
            '\\' => match chars.next() {
                Some('n') => part.push('\n'),
                Some('r') => part.push('\r'),
                Some('t') => part.push('\t'),
                Some('x') => {
                    let mut hex = String::new();
                    while hex.len() < 4 && chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                        hex.push(chars.next().expect("peeked character"));
                    }
                    if let Ok(number) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(number)
                    {
                        part.push(ch);
                    }
                }
                Some(ch) => part.push(ch),
                None => part.push('\\'),
            },
            ch => part.push(ch),
        }
    }
    parts.push(part.trim().to_string());
    parts
}

fn qt_string(mut value: String) -> String {
    if value.starts_with("@@") {
        value.remove(0);
    }
    value
}

fn qt_catalog(values: Vec<String>) -> Vec<String> {
    let values: Vec<String> = values.into_iter().map(qt_string).collect();
    let values = if values.len() == 1 && values[0].starts_with('[') {
        serde_json::from_str(&values[0]).unwrap_or(values)
    } else {
        values
    };
    if values.is_empty() {
        Settings::default().services
    } else {
        values
    }
}

fn home() -> Result<PathBuf, String> {
    // std also consults the OS user database when HOME is unset. Never use the
    // working directory: that could hide an existing unresolved fiscal journal.
    std::env::home_dir()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "Nie można ustalić katalogu użytkownika. Podaj --data-dir.".into())
}

#[cfg(not(target_os = "macos"))]
fn absolute_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub fn data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let root = match absolute_env("LOCALAPPDATA") {
        Some(path) => path,
        None => home()?.join("AppData/Local"),
    }
    .join("State");
    #[cfg(target_os = "macos")]
    let root = home()?.join("Library/Preferences/State");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = match absolute_env("XDG_STATE_HOME") {
        Some(path) => path,
        None => home()?.join(".local/state"),
    };
    Ok(root.join("huggingcar-fiscal"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn legacy_settings() -> Result<Option<Settings>, String> {
    let root = match absolute_env("XDG_CONFIG_HOME") {
        Some(path) => path,
        None => home()?.join(".config"),
    };
    match fs::read_to_string(root.join("HuggingCar/Fiscal.conf")) {
        Ok(text) => Ok(Some(Settings::from_qt_ini(&text))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Nie można odczytać ustawień Qt: {e}")),
    }
}

#[cfg(target_os = "windows")]
fn legacy_settings() -> Result<Option<Settings>, String> {
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};
    let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\HuggingCar\\Fiscal") {
        Ok(key) => key,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut config = Settings::default();
    if let Ok(connection) = key.open_subkey("connection") {
        config.address = qt_string(connection.get_value("address").unwrap_or_default());
        config.baudrate = connection
            .get_value::<u32, _>("baudrate")
            .ok()
            .or_else(|| {
                connection
                    .get_value::<String, _>("baudrate")
                    .ok()
                    .and_then(|s| s.parse().ok())
            })
            .filter(|n| *n > 0)
            .unwrap_or(9600);
    }
    if let Ok(names) = key.get_value::<Vec<String>, _>("services") {
        config.services = qt_catalog(names);
    } else if let Ok(raw) = key.get_value::<String, _>("services") {
        config.services = qt_catalog(vec![raw]);
    }
    Ok(Some(config))
}

#[cfg(target_os = "macos")]
fn legacy_settings() -> Result<Option<Settings>, String> {
    let path = home()?.join("Library/Preferences/com.huggingcar.Fiscal.plist");
    if !path.exists() {
        return Ok(None);
    }
    let value = plist::Value::from_file(path).map_err(|e| e.to_string())?;
    let Some(map) = value.as_dictionary() else {
        return Err("Uszkodzone ustawienia Qt.".into());
    };
    let mut config = Settings::default();
    if let Some(address) = map
        .get("connection.address")
        .or_else(|| map.get("connection/address"))
        .and_then(plist::Value::as_string)
    {
        config.address = qt_string(address.into());
    }
    if let Some(baud) = map
        .get("connection.baudrate")
        .or_else(|| map.get("connection/baudrate"))
    {
        config.baudrate = baud
            .as_unsigned_integer()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| baud.as_string().and_then(|s| s.parse().ok()))
            .filter(|n| *n > 0)
            .unwrap_or(9600);
    }
    if let Some(names) = map.get("services") {
        if let Some(array) = names.as_array() {
            config.services = qt_catalog(
                array
                    .iter()
                    .filter_map(plist::Value::as_string)
                    .map(str::to_owned)
                    .collect(),
            );
        } else if let Some(raw) = names.as_string() {
            config.services = qt_catalog(vec![raw.into()]);
        }
    }
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::qt_catalog;

    #[test]
    fn native_qt_catalog_decodes_one_escape_before_legacy_json() {
        assert_eq!(
            qt_catalog(vec!["@@Naprawa".into(), "@@@Serwis".into()]),
            ["@Naprawa", "@@Serwis"]
        );
        assert_eq!(
            qt_catalog(vec![r#"["@Naprawa","@@Serwis"]"#.into()]),
            ["@Naprawa", "@@Serwis"]
        );
    }
}
