use std::{
    fs, thread,
    time::{Duration, Instant},
};

use fiscal_desktop::{Controller, Job, Row, Settings, completed_months, receipt_lines};
use posnet::simulator::Simulator;
use rust_decimal::Decimal;
use serde_json::json;

fn settle(app: &mut Controller) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while app.busy() {
        app.poll();
        assert!(Instant::now() < deadline, "{}", app.message);
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn receipt_rounding_and_unpriced_name_validation() {
    let mut rows = vec![Row::new("Naprawa"), Row::new("Olej"), Row::new("Inna")];
    rows[0].quantity = "2".into();
    rows[0].price = "12,34".into();
    rows[1].quantity = "0,5".into();
    rows[1].price = "19,99".into();
    rows[2].quantity = "nie jest liczbą".into();
    let lines = receipt_lines(&rows, Some(0)).unwrap();
    assert_eq!(
        lines.iter().map(posnet::Line::total_cents).sum::<i64>(),
        3468
    );
    let rate = posnet::VatRate {
        index: 0,
        percent: Decimal::from(23),
    };
    assert_eq!(rate.tax_cents(10000), 1870);
    rows[2].name = " \t".into();
    assert!(receipt_lines(&rows, Some(0)).is_err());
    rows[2].name = "Inna".into();
    rows[1].quantity = "nie jest liczbą".into();
    assert!(receipt_lines(&rows, Some(0)).is_err());
}

#[test]
fn excessive_decimal_precision_cannot_round_into_a_valid_receipt() {
    let mut row = Row::new("Naprawa");
    for price in [
        "99.99999999999999999999999999999",
        "99.99999999999999999999999999999e0",
    ] {
        row.price = price.into();
        assert!(row.line(0).is_err(), "accepted rounded price {price}");
    }
    row.price = "1e2".into();
    assert_eq!(row.line(0).unwrap().unwrap().total_cents(), 10000);
    row.quantity = "0.999999999999999999999999999999".into();
    assert!(row.line(0).is_err());
}

#[test]
fn qt_settings_migrate_lists_escapes_json_and_invalid_baudrate() {
    for (raw, expected) in [
        ("Tylko jedna", vec!["Tylko jedna"]),
        ("Geometria, Wulkanizacja", vec!["Geometria", "Wulkanizacja"]),
        (r#""[\"Z wersji\", \"0.1.0\"]""#, vec!["Z wersji", "0.1.0"]),
        (
            r#""Serwis, opon", Geometria k\x00f3ł"#,
            vec!["Serwis, opon", "Geometria kół"],
        ),
    ] {
        let config = Settings::from_qt_ini(&format!(
            "[General]\nservices={raw}\n[connection]\naddress=/dev/ttyACM0\nbaudrate=nonsense\n"
        ));
        assert_eq!(config.services, expected);
        assert_eq!(config.baudrate, 9600);
        assert_eq!(config.address, "/dev/ttyACM0");
    }
    let empty = Settings::from_qt_ini("[General]\nservices=@Invalid()\n");
    assert_eq!(empty.services, Settings::default().services);
    let temp = tempfile::tempdir().unwrap();
    let config = Settings {
        services: vec![" Geometria ko\u{301}ł ".into(), "Geometria kół".into()],
        ..Settings::default()
    };
    config.save(&temp.path().join("settings.json")).unwrap();
    assert_eq!(
        Settings::load_file(&temp.path().join("settings.json"))
            .unwrap()
            .services,
        ["Geometria kół"]
    );
}

#[test]
fn completed_months_excludes_current_and_crosses_year() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
    let months = completed_months(today);
    assert_eq!(months.len(), 24);
    assert_eq!(months[0], (2025, 12));
    assert_eq!(months[23], (2024, 1));
}

#[test]
fn worker_serializes_prints_preserves_hidden_rows_and_keeps_history() {
    let sim = Simulator::start().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings {
        address: sim.connection.address.clone(),
        ..Settings::default()
    };
    let mut app = Controller::new(settings, temp.path().to_path_buf()).unwrap();
    assert!(app.start(Job::Probe { scan: false }));
    settle(&mut app);
    app.rows[0].price = "19,99".into();
    app.rows[1].price = "10".into();
    app.search = "opon".into();
    app.payment = 2;
    let lines = app.receipt_lines().unwrap();
    assert_eq!(lines.len(), 2);
    assert!(app.start(Job::Receipt(lines, 2)));
    assert!(!app.start(Job::Daily));
    settle(&mut app);
    assert_eq!(sim.snapshot().receipts.len(), 1);
    assert!(app.rows.iter().all(|row| row.price.is_empty()));
    let history = app.history().unwrap();
    assert_eq!(history[0]["total_cents"], 2999);
    assert_eq!(history[0]["payment"], 2);
    assert!(app.status.as_ref().unwrap().ready);
}

#[test]
fn uncertain_receipt_never_retries_and_recovery_clears_prices() {
    let sim = Simulator::start().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings {
        address: sim.connection.address.clone(),
        ..Settings::default()
    };
    let mut app = Controller::new(settings, temp.path().to_path_buf()).unwrap();
    app.start(Job::Probe { scan: false });
    settle(&mut app);
    app.rows[0].price = "10".into();
    sim.update(|state| {
        state.disconnect_after.insert("trend".into());
    });
    assert!(app.start(Job::Receipt(app.receipt_lines().unwrap(), 0)));
    settle(&mut app);
    assert!(app.pending.is_some());
    assert_eq!(app.rows[0].price, "10");
    assert!(!app.start(Job::Receipt(app.receipt_lines().unwrap(), 0)));
    assert_eq!(sim.snapshot().receipts.len(), 1);
    sim.update(|state| {
        state.disconnect_after.clear();
    });
    assert!(app.start(Job::Recover { force: false }));
    settle(&mut app);
    assert!(app.pending.is_none());
    assert!(app.rows[0].price.is_empty());
    assert_eq!(sim.snapshot().receipts.len(), 1);
}

#[test]
fn wrong_device_requires_explicit_force_and_corrupt_journal_stays_blocked() {
    let sim = Simulator::start().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings {
        address: sim.connection.address.clone(),
        ..Settings::default()
    };
    fs::write(
        temp.path().join("operation.json"),
        json!({"state":"pending","unique_number":"OTHER","operation":"daily_report"}).to_string(),
    )
    .unwrap();
    let mut app = Controller::new(settings, temp.path().to_path_buf()).unwrap();
    app.start(Job::Probe { scan: false });
    settle(&mut app);
    assert!(app.pending.is_some());
    assert!(!app.start(Job::Daily));
    app.start(Job::Recover { force: false });
    settle(&mut app);
    assert!(app.offer_force);
    assert!(app.pending.is_some());
    app.start(Job::Recover { force: true });
    settle(&mut app);
    assert!(app.pending.is_none());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("operation.json")).unwrap()).unwrap();
    assert_eq!(record["resolution"], "manual");
    fs::write(temp.path().join("operation.json"), "not json").unwrap();
    app.start(Job::Probe { scan: false });
    settle(&mut app);
    assert!(app.pending.is_some());
    assert!(!app.start(Job::Daily));
    app.start(Job::Recover { force: true });
    settle(&mut app);
    assert!(app.pending.is_some());
    assert_eq!(
        fs::read_to_string(temp.path().join("operation.json")).unwrap(),
        "not json"
    );
}

#[test]
fn settings_rebuild_only_changed_catalog_and_reject_blank_names() {
    let temp = tempfile::tempdir().unwrap();
    let mut app = Controller::new(Settings::default(), temp.path().into()).unwrap();
    app.rows[0].price = "50".into();
    app.save_settings(app.settings.clone()).unwrap();
    assert_eq!(app.rows[0].price, "50");
    let mut config = app.settings.clone();
    config.services = vec![" \t".into()];
    assert!(app.save_settings(config.clone()).is_err());
    assert_eq!(app.rows[0].price, "50");
    config.services = vec![
        " Wulkanizacja ".into(),
        "Geometria".into(),
        "Wulkanizacja".into(),
    ];
    app.save_settings(config).unwrap();
    assert_eq!(
        app.rows
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["Wulkanizacja", "Geometria"]
    );
    assert!(app.rows.iter().all(|row| row.price.is_empty()));
    assert_eq!(
        Settings::load_file(&temp.path().join("settings.json")).unwrap(),
        app.settings
    );
}

#[test]
fn history_is_compatible_bounded_and_newest_first() {
    let temp = tempfile::tempdir().unwrap();
    let app = Controller::new(Settings::default(), temp.path().into()).unwrap();
    let rows: String = (0..502).map(|number| format!("{}\n", json!({"receipt_number":number.to_string(),"total_cents":100,"timestamp":"2026-09-14T09:12:00+02:00","lines":[{"name":"Olej"}],"payment":2}))).collect();
    fs::write(temp.path().join("history.jsonl"), rows).unwrap();
    let history = app.history().unwrap();
    assert_eq!(history.len(), 500);
    assert_eq!(history[0]["receipt_number"], "501");
    assert_eq!(history[499]["receipt_number"], "2");
}

#[test]
fn reports_and_status_blocking_use_native_printer() {
    let sim = Simulator::start().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let settings = Settings {
        address: sim.connection.address.clone(),
        ..Settings::default()
    };
    let mut app = Controller::new(settings, temp.path().into()).unwrap();
    app.start(Job::Probe { scan: false });
    settle(&mut app);
    sim.update(|state| {
        state
            .responses
            .insert("sprn".into(), [("pr".into(), "5".into())].into());
    });
    app.start(Job::Probe { scan: false });
    settle(&mut app);
    assert!(!app.ready());
    assert!(app.editable());
    assert!(app.message.contains("Brak papieru"));
    assert!(!app.start(Job::Daily));
    sim.update(|state| {
        state.responses.clear();
    });
    app.start(Job::Probe { scan: false });
    settle(&mut app);
    assert!(app.start(Job::Daily));
    settle(&mut app);
    assert!(app.start(Job::Monthly(2025, 12)));
    settle(&mut app);
    let start = chrono::NaiveDate::from_ymd_opt(2025, 12, 1).unwrap();
    let end = chrono::NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
    assert!(app.start(Job::Periodic(start, end, true)));
    settle(&mut app);
    let snapshot = sim.snapshot();
    assert_eq!(snapshot.reports.len(), 3);
    assert_eq!(snapshot.reports[1]["params"]["da"], "2025-12-01");
    assert_eq!(snapshot.reports[2]["params"]["fd"], "2025-12-01");
    assert_eq!(snapshot.reports[2]["params"]["td"], "2025-12-31");
    assert_eq!(snapshot.reports[2]["params"]["su"], "1");
}

#[test]
fn probe_cli_prints_json_without_a_display_and_uses_exit_status() {
    let sim = Simulator::start().unwrap();
    let temp = tempfile::tempdir().unwrap();
    Settings::default()
        .save(&temp.path().join("settings.json"))
        .unwrap();
    let probe = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_huggingcar-fiscal"))
            .args(["--probe", "--serial", &sim.connection.address, "--data-dir"])
            .arg(temp.path())
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .output()
            .unwrap()
    };
    let output = probe();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["ready"], true);
    assert!(status["vat_rates"][0]["percent"].is_string());
    sim.update(|state| {
        state
            .responses
            .insert("sprn".into(), [("pr".into(), "5".into())].into());
    });
    let output = probe();
    assert_eq!(output.status.code(), Some(1));
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["ready"], false);
    assert!(sim.snapshot().receipts.is_empty());
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
#[test]
fn relative_xdg_paths_cannot_move_the_fiscal_journal_or_settings() {
    let sim = Simulator::start().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join(".local/state/huggingcar-fiscal");
    let config = temp.path().join(".config/HuggingCar");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&config).unwrap();
    let pending = r#"{"state":"pending","operation":"receipt","unique_number":"OLD"}"#;
    fs::write(state.join("operation.json"), pending).unwrap();
    fs::write(
        config.join("Fiscal.conf"),
        "[General]\nservices=@@Naprawa, @@@Serwis\n",
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_huggingcar-fiscal"))
        .args(["--probe", "--serial", &sim.connection.address])
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("XDG_STATE_HOME", "relative-state")
        .env("XDG_CONFIG_HOME", "relative-config")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(state.join("operation.json")).unwrap(),
        pending
    );
    assert_eq!(
        Settings::load_file(&state.join("settings.json"))
            .unwrap()
            .services,
        ["@Naprawa", "@@Serwis"]
    );
    assert!(!temp.path().join("relative-state").exists());
    assert!(!temp.path().join("relative-config").exists());
}
