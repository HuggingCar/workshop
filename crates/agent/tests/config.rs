use workshop_agent::Config;

#[test]
fn saved_settings_round_trip_without_starting_or_exposing_token() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("fiscal.json");
    let config = Config {
        api_url: " https://api.example/ ".into(),
        token: " secret ".into(),
        serial: " /dev/custom-printer ".into(),
        baudrate: 57600,
    };
    config.save(&path).unwrap();
    let loaded = Config::load(&path);
    assert_eq!(loaded.api_url, "https://api.example");
    assert_eq!(loaded.token, "secret");
    assert_eq!(loaded.serial, "/dev/custom-printer");
    assert_eq!(loaded.baudrate, 57600);
    assert!(!data.path().join("fiscal-operation.json").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        config.save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn invalid_settings_do_not_replace_existing_credentials() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("fiscal.json");
    let mut config = Config {
        api_url: "https://api.example".into(),
        token: "secret".into(),
        serial: "/dev/test".into(),
        baudrate: 9600,
    };
    config.save(&path).unwrap();
    for token in [
        "",
        "not secret",
        "zażółć",
        "invalid\u{1c}token",
        "invalid\0token",
    ] {
        config.token = token.into();
        assert!(config.save(&path).is_err());
        assert_eq!(Config::load(&path).token, "secret");
    }
}

#[test]
fn invalid_or_missing_config_opens_setup_without_printing() {
    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("fiscal.json");
    assert!(Config::load(&path).token.is_empty());
    std::fs::write(&path, "{broken").unwrap();
    assert!(Config::load(&path).token.is_empty());
}

#[cfg(unix)]
#[test]
fn atomic_config_save_replaces_symlink_without_overwriting_target() {
    let data = tempfile::tempdir().unwrap();
    let target = data.path().join("unrelated");
    let path = data.path().join("fiscal.json");
    std::fs::write(&target, "keep").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    Config {
        api_url: "https://api.example".into(),
        token: "secret".into(),
        serial: "/dev/test".into(),
        baudrate: 9600,
    }
    .save(&path)
    .unwrap();
    assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    assert!(
        !std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(Config::load(&path).token, "secret");
}
