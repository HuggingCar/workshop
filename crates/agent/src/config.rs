use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    api::validated_url,
    error::{Error, Result},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default = "default_baudrate")]
    pub baudrate: u32,
}

fn default_baudrate() -> u32 {
    9600
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: String::new(),
            token: String::new(),
            serial: String::new(),
            baudrate: default_baudrate(),
        }
    }
}

/// Preserve pre-migration journals before choosing an OS-native location for new installs.
pub fn state_dir() -> Result<PathBuf> {
    let legacy = std::env::var_os("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        .map(|base| base.join("workshop-agent"));
    let native = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .filter(|path| path.is_absolute())
        .map(|base| base.join("workshop-agent"));
    select_state_dir(legacy, native)
}

fn contains_state(path: &Path) -> Result<bool> {
    for name in ["fiscal-operation.json", "fiscal.json"] {
        match fs::symlink_metadata(path.join(name)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::value(format!(
                    "Nie można sprawdzić {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(false)
}

fn select_state_dir(legacy: Option<PathBuf>, native: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(legacy) = legacy.as_ref()
        && contains_state(legacy)?
    {
        if let Some(native) = native.as_ref()
            && native != legacy
            && contains_state(native)?
            && fs::canonicalize(native)
                .ok()
                .zip(fs::canonicalize(legacy).ok())
                .is_none_or(|(native, legacy)| native != legacy)
        {
            return Err(Error::value(
                "Znaleziono dwa katalogi danych agenta. Sprawdź rejestry operacji i wskaż katalog przez --data-dir.",
            ));
        }
        return Ok(legacy.clone());
    }
    native
        .or(legacy)
        .ok_or_else(|| Error::value("Nie można ustalić katalogu danych użytkownika."))
}

impl Config {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Validate once for both GUI saves and headless startup.
    pub fn validated(&self) -> Result<Self> {
        let checked = Self {
            api_url: validated_url(self.api_url.trim())?,
            token: self.token.trim().to_string(),
            serial: self.serial.trim().to_string(),
            baudrate: self.baudrate,
        };
        if checked.token.is_empty()
            || !checked.token.is_ascii()
            || checked
                .token
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(Error::value("Wklej token drukarki z HuggingCar."));
        }
        if checked.baudrate == 0 {
            return Err(Error::value("Nieprawidłowa prędkość transmisji."));
        }
        Ok(checked)
    }

    /// Replace atomically; the token is owner-only before any bytes are written.
    pub fn save(&self, path: &Path) -> Result<()> {
        let checked = self.validated()?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .map_err(|err| Error::value(format!("Nie można utworzyć katalogu: {err}")))?;
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let write = || -> std::io::Result<()> {
            let mut temporary = tempfile::Builder::new()
                .prefix(".fiscal-")
                .tempfile_in(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                temporary
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            serde_json::to_writer_pretty(temporary.as_file_mut(), &checked)?;
            temporary.as_file_mut().flush()?;
            temporary.as_file().sync_all()?;
            temporary.persist(path).map_err(|error| error.error)?;
            #[cfg(unix)]
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        };
        write().map_err(|err| Error::value(format!("Nie można zapisać: {err}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_legacy_journal_cannot_be_bypassed_by_os_native_default() {
        let data = tempfile::tempdir().unwrap();
        let legacy = data.path().join("legacy");
        let native = data.path().join("native");
        fs::create_dir(&legacy).unwrap();
        fs::write(
            legacy.join("fiscal-operation.json"),
            r#"{"state":"pending"}"#,
        )
        .unwrap();
        assert_eq!(
            select_state_dir(Some(legacy.clone()), Some(native.clone())).unwrap(),
            legacy
        );
        fs::create_dir(&native).unwrap();
        fs::write(native.join("fiscal.json"), "{}").unwrap();
        assert!(select_state_dir(Some(legacy), Some(native)).is_err());
    }

    #[test]
    fn fresh_install_uses_native_directory_but_saved_legacy_config_stays_put() {
        let data = tempfile::tempdir().unwrap();
        let legacy = data.path().join("legacy");
        let native = data.path().join("native");
        assert_eq!(
            select_state_dir(Some(legacy.clone()), Some(native.clone())).unwrap(),
            native
        );
        fs::create_dir(&legacy).unwrap();
        fs::write(legacy.join("fiscal.json"), "{}").unwrap();
        assert_eq!(
            select_state_dir(Some(legacy.clone()), Some(native)).unwrap(),
            legacy
        );
        assert_eq!(
            select_state_dir(Some(legacy.clone()), Some(legacy.clone())).unwrap(),
            legacy
        );
    }
}
