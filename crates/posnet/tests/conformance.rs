//! Fiscal safety over a real TCP peer, with no interpreter or external service.
use std::{path::Path, str::FromStr};

use chrono::NaiveDate;
use posnet::{Connection, Error, Line, Printer, protocol::Params, simulator::Simulator};
use rust_decimal::Decimal;
use serde_json::json;

fn line() -> Line {
    Line::new(
        "Naprawa",
        Decimal::from_str("0.5").unwrap(),
        Decimal::from_str("19.99").unwrap(),
        0,
    )
    .unwrap()
}
fn printer(sim: &Simulator, path: &Path) -> Printer {
    Printer::new(sim.connection.clone(), path.join("operation.json")).unwrap()
}
fn params(values: &[(&str, &str)]) -> Params {
    values
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
fn override_reply(sim: &Simulator, command: &str, values: &[(&str, &str)]) {
    sim.update(|s| {
        s.responses.insert(command.into(), params(values));
    });
}

#[test]
fn receipt_rounding_brand_footer_and_journal_survive_reopening() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut p = printer(&sim, dir.path());
    assert!(p.probe().unwrap().ready);
    let mut record = p
        .print_receipt(
            &[line()],
            0,
            "warsztat kowalski",
            vec![("job_id", json!(7))],
        )
        .unwrap();
    assert_eq!(record["receipt_number"], "1");
    assert_eq!(record["job_id"], 7);
    assert_eq!(record["total_cents"], 1000);
    assert!(sim.snapshot().footers.is_empty());
    p.acknowledge(&mut record).unwrap();
    sim.update(|s| s.header = "&cInny Serwis&c".into());
    p.print_receipt(&[line()], 2, "Warsztat Kowalski", vec![])
        .unwrap();
    let state = sim.snapshot();
    assert_eq!(
        state.receipts[1],
        json!({"total_cents":1000,"payment":2,"lines":[{"na":"Naprawa","vt":"0","pr":"1999","il":"0.5","wa":"1000"}]})
    );
    assert_eq!(
        state.footers,
        vec![json!({"id":"25","na":"Warsztat Kowalski"})]
    );
    assert!(!state.footer_open);
    let reopened = printer(&sim, dir.path()).last_record().unwrap().unwrap();
    assert_eq!(reopened["state"], "completed");
    assert_eq!(reopened["operation"], "receipt");
    assert_eq!(reopened["lines"][0]["quantity"], "0.5");
    assert_eq!(reopened["lines"][0]["price_cents"], 1999);
}

#[test]
fn failed_payment_stays_pending_and_recovery_requires_same_ready_device() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    sim.update(|s| {
        s.errors.insert("trpayment".into(), 2000);
    });
    assert!(matches!(
        p.print_receipt(&[line()], 0, "", vec![]),
        Err(Error::Protocol(_))
    ));
    assert_eq!(p.pending().unwrap().unwrap()["stage"], "trpayment");
    sim.update(|s| s.errors.clear());
    let count = sim.snapshot().commands.len();
    assert!(p.print_receipt(&[line()], 0, "", vec![]).is_err());
    assert_eq!(sim.snapshot().commands.len(), count);
    assert!(p.acknowledge_pending(false).is_err());
    sim.update(|s| s.transaction_open = false);
    override_reply(
        &sim,
        "getrealid",
        &[("nm", "POSNET TEMO ONLINE"), ("vr", "1"), ("nu", "OTHER")],
    );
    assert!(p.acknowledge_pending(false).is_err());
    p.acknowledge_pending(true).unwrap();
    assert_eq!(p.last_record().unwrap().unwrap()["resolution"], "manual");
    assert!(p.pending().unwrap().is_none());
    assert!(sim.snapshot().receipts.is_empty());
}

#[test]
fn lost_fiscal_or_footer_ack_is_not_retried_or_marked_completed() {
    for command in ["trend", "trftrln", "trftrend", "dailyrep"] {
        let sim = Simulator::start().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = printer(&sim, dir.path());
        sim.update(|s| {
            s.disconnect_after.insert(command.into());
        });
        let result = if command == "dailyrep" {
            p.daily_report()
        } else {
            p.print_receipt(&[line()], 0, "Another company", vec![])
        };
        assert!(matches!(result, Err(Error::Protocol(_))));
        assert_eq!(p.pending().unwrap().unwrap()["stage"], command);
        let state = sim.snapshot();
        assert_eq!(
            state
                .commands
                .iter()
                .filter(|v| v["command"] == command)
                .count(),
            1
        );
        assert_eq!(state.receipts.len() + state.reports.len(), 1);
        assert!(p.daily_report().is_err());
        if state.footer_open {
            assert!(p.acknowledge_pending(false).is_err());
        }
    }
}

#[test]
fn failed_counter_lookup_does_not_make_completed_receipt_uncertain() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    sim.update(|s| {
        s.errors.insert("scnt".into(), 2000);
    });
    let record = p.print_receipt(&[line()], 0, "", vec![]).unwrap();
    assert_eq!(record["state"], "completed");
    assert_eq!(record["receipt_number"], "");
    assert!(p.pending().unwrap().is_none());
    assert_eq!(sim.snapshot().receipts.len(), 1);
}

#[test]
fn corrupt_journal_blocks_receipts_reports_and_recovery() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    for content in ["", "{not json", "{\"state\":\"odd\"}", "[]"] {
        std::fs::write(dir.path().join("operation.json"), content).unwrap();
        assert!(p.last_record().is_err());
        assert!(p.print_receipt(&[line()], 0, "", vec![]).is_err());
        assert!(p.daily_report().is_err());
        assert!(p.acknowledge_pending(true).is_err());
    }
    assert!(sim.snapshot().commands.is_empty());
}

#[test]
fn reports_use_device_clock_and_validate_date_boundaries() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    override_reply(&sim, "rtcget", &[("da", "2020-02-29;23:59")]);
    p.daily_report().unwrap();
    p.monthly_report(2026, 8).unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
    p.periodic_report(start, end, true).unwrap();
    assert_eq!(
        sim.snapshot().reports,
        vec![
            json!({"command":"dailyrep","params":{"da":"2020-02-29"}}),
            json!({"command":"monthlyrep","params":{"da":"2026-08-01","su":"0"}}),
            json!({"command":"periodicrepbydates","params":{"fd":"2026-08-01","td":"2026-08-31","su":"1"}}),
        ]
    );
    let count = sim.snapshot().commands.len();
    assert!(p.periodic_report(end, start, false).is_err());
    for (year, month) in [(0, 1), (10000, 1), (2026, 0), (2026, 13)] {
        assert!(p.monthly_report(year, month).is_err());
    }
    assert_eq!(sim.snapshot().commands.len(), count);
    override_reply(&sim, "rtcget", &[("da", "2026-02-30;00:00")]);
    assert!(p.daily_report().is_err());
    assert_eq!(sim.snapshot().reports.len(), 3);
}

#[test]
fn invalid_printer_clock_never_starts_a_fiscal_report() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    for clock in ["0000-01-01;00:00", "-001-01-01;00:00", "2026-02-30;00:00"] {
        override_reply(&sim, "rtcget", &[("da", clock)]);
        assert!(matches!(p.daily_report(), Err(Error::Protocol(_))));
        assert!(p.last_record().unwrap().is_none());
    }
    assert!(
        !sim.snapshot()
            .commands
            .iter()
            .any(|entry| entry["command"] == "dailyrep")
    );
    assert!(sim.snapshot().reports.is_empty());
}

#[test]
fn preflight_rejects_device_problems_and_malformed_status() {
    let cases = [
        (
            "getrealid",
            params(&[("nm", "POSNET THERMAL HD"), ("vr", "1"), ("nu", "X")]),
        ),
        ("scomm", params(&[("fs", "0"), ("hr", "1"), ("ts", "0")])),
        ("scomm", params(&[("fs", "1"), ("hr", "0"), ("ts", "0")])),
        ("sdev", params(&[("ds", "1"), ("qe", "1")])),
        ("sdev", params(&[("ds", "0"), ("qe", "0")])),
        ("sprn", params(&[("pr", "5")])),
        ("strns", params(&[("to", "0"), ("fe", "1")])),
        (
            "vatget",
            params(&[
                ("va", "101"),
                ("vb", "101"),
                ("vc", "101"),
                ("vd", "101"),
                ("ve", "101"),
                ("vf", "101"),
                ("vg", "101"),
            ]),
        ),
        ("getrealid", params(&[("vr", "1"), ("nu", "X")])),
        (
            "scomm",
            params(&[("fs", "maybe"), ("hr", "1"), ("ts", "0")]),
        ),
        ("vatget", params(&[("va", "Infinity")])),
    ];
    for (command, response) in cases {
        let sim = Simulator::start().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut p = printer(&sim, dir.path());
        sim.update(|s| {
            s.responses.insert(command.into(), response);
        });
        assert!(!p.probe().is_ok_and(|s| s.ready));
        assert!(p.print_receipt(&[line()], 0, "", vec![]).is_err());
        assert!(p.last_record().unwrap().is_none());
        assert!(sim.snapshot().receipts.is_empty());
    }
}

#[test]
fn inactive_and_changed_vat_require_operator_review() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut p = printer(&sim, dir.path());
    p.probe().unwrap();
    let inactive = Line::new("Olej", Decimal::ONE, Decimal::ONE, 3).unwrap();
    assert!(p.print_receipt(&[inactive], 0, "", vec![]).is_err());
    override_reply(
        &sim,
        "vatget",
        &[
            ("va", "8"),
            ("vb", "8"),
            ("vc", "0"),
            ("vd", "101"),
            ("ve", "101"),
            ("vf", "101"),
            ("vg", "100"),
        ],
    );
    assert!(p.print_receipt(&[line()], 0, "", vec![]).is_err());
    assert!(sim.snapshot().receipts.is_empty());
    p.probe().unwrap();
    p.print_receipt(&[line()], 0, "", vec![]).unwrap();
}

#[test]
fn arguments_cannot_touch_device_or_override_fiscal_journal_fields() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    assert!(p.print_receipt(&[], 0, "", vec![]).is_err());
    assert!(p.print_receipt(&vec![line(); 501], 0, "", vec![]).is_err());
    assert!(p.print_receipt(&[line()], 1, "", vec![]).is_err());
    let biggest = Line::new(
        "Olej",
        Decimal::ONE,
        Decimal::from_str("99999999.99").unwrap(),
        0,
    )
    .unwrap();
    assert!(
        p.print_receipt(&[biggest.clone(), biggest], 0, "", vec![])
            .is_err()
    );
    for key in [
        "state",
        "stage",
        "unique_number",
        "operation",
        "total_cents",
        "lines",
    ] {
        assert!(
            p.print_receipt(&[line()], 0, "", vec![(key, json!("override"))])
                .is_err()
        );
    }
    assert!(sim.snapshot().commands.is_empty());
    assert!(p.last_record().unwrap().is_none());
}

#[test]
fn detection_adopts_only_temo_and_never_changes_unresolved_device() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut p = Printer::new(Connection::default(), dir.path().join("operation.json")).unwrap();
    assert!(
        p.detect(std::slice::from_ref(&sim.connection.address))
            .unwrap()
            .ready
    );
    assert_eq!(p.connection, sim.connection);
    p.connection = Connection::default();
    std::fs::write(
        dir.path().join("operation.json"),
        json!({"state":"pending","operation":"receipt","unique_number":"OTHER"}).to_string(),
    )
    .unwrap();
    let count = sim.snapshot().commands.len();
    assert!(
        p.detect(std::slice::from_ref(&sim.connection.address))
            .is_err()
    );
    assert!(p.connection.address.is_empty());
    assert_eq!(sim.snapshot().commands.len(), count);
}

#[test]
fn lock_rejects_overlap_and_releases_after_failed_connection() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Printer::new(
        Connection::new(dir.path().join("missing").to_string_lossy()),
        dir.path().join("operation.json"),
    )
    .unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.path().join("operation.lock"))
        .unwrap();
    let mut lock = fd_lock::RwLock::new(file);
    let guard = lock.try_write().unwrap();
    assert!(matches!(p.probe(), Err(Error::Value(_))));
    drop(guard);
    assert!(matches!(p.probe(), Err(Error::Connect { .. })));
    assert!(lock.try_write().is_ok());
}

#[test]
fn same_ready_device_resolves_pending_without_manual_override() {
    let sim = Simulator::start().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = printer(&sim, dir.path());
    std::fs::write(dir.path().join("operation.json"), json!({"state":"pending","operation":"receipt","stage":"trend","unique_number":"DEMO0000001"}).to_string()).unwrap();
    p.acknowledge_pending(false).unwrap();
    let record = p.last_record().unwrap().unwrap();
    assert_eq!(record["state"], "acknowledged");
    assert!(!record.contains_key("resolution"));
    p.acknowledge_pending(false).unwrap();
    assert_eq!(p.last_record().unwrap().unwrap(), record);
}
