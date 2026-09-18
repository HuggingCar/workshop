//! Independent wire vectors and fiscal numeric boundaries.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    str::FromStr,
    time::Duration,
};

use posnet::{
    error::Error,
    models::{Line, VatRate, money, sanitize},
    protocol::{Connection, Session, decode_frame, encode_frame},
};
use rust_decimal::Decimal;

fn decimal(value: &str) -> Decimal {
    Decimal::from_str(value).expect("test literal")
}

fn param(key: &'static str, value: &str) -> Vec<(&'static str, String)> {
    vec![(key, value.to_string())]
}

/// Spec p.13 worked example: the full frame for trinit bm0 (CRC16-CCITT, poly 0x1021, init 0).
#[test]
fn published_posnet_crc_vector() {
    let frame = encode_frame("trinit", &[("bm", "0".into())]).unwrap();
    assert_eq!(frame, b"\x02trinit\tbm0\t#4825\x03");
}

#[test]
fn polish_text_encoded_before_checksum() {
    let frame = encode_frame(
        "trline",
        &[
            ("na", "Koło".into()),
            ("vt", "0".into()),
            ("pr", "100".into()),
        ],
    )
    .unwrap();
    assert!(frame.windows(4).any(|window| window == b"Ko\xb3o"));
    let (command, params) = decode_frame(&frame).unwrap();
    assert_eq!(command, "trline");
    assert_eq!(params["na"], "Koło");
    assert_eq!(params["pr"], "100");
}

#[test]
fn rejects_control_characters_and_unencodable_text() {
    for value in ["x\ty", "x\x03", "x\n", "🔧"] {
        let error = encode_frame("trline", &[("na", value.into())]).unwrap_err();
        assert!(matches!(error, Error::Value(_)), "{value:?} accepted");
    }
}

fn reply(payload: &str) -> Vec<u8> {
    let encoded = encoding_rs::WINDOWS_1250.encode(payload).0.into_owned();
    let crc = reference_crc(&encoded);
    let mut frame = vec![0x02];
    frame.extend_from_slice(&encoded);
    frame.extend_from_slice(format!("#{crc:04X}").as_bytes());
    frame.push(0x03);
    frame
}

// Bitwise reference implementation, independent of the driver's CRC crate.
fn reference_crc(bytes: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[test]
fn rejects_corrupt_and_ambiguous_frames() {
    let frames = [
        b"\x02trinit\tbm0\t#0000\x03".to_vec(), // wrong CRC
        reply("trinit\tbm0\tbm1\t"),            // duplicate parameter
        reply("trinit\tb\t"),                   // parameter shorter than its two-letter key
    ];
    for frame in frames {
        assert!(matches!(
            decode_frame(&frame).unwrap_err(),
            Error::Protocol(_)
        ));
    }
}

#[test]
fn both_documented_error_forms() {
    for payload in ["trline\t?2000", "ERR\t?2000\tcmtrline\tfdvt\t"] {
        match decode_frame(&reply(payload)).unwrap_err() {
            Error::Device { code, .. } => assert_eq!(code, 2000),
            other => panic!("{payload}: {other}"),
        }
    }
}

/// An unresponsive port must surface a timeout, never resend a fiscal command.
#[test]
fn lost_reply_never_retries_mutation() {
    let (mut peer, mut session) = raw_peer();
    let error = session
        .command("trinit", &[("bm", "0".into())], None)
        .unwrap_err();
    assert!(matches!(error, Error::Timeout(_)), "{error}");
    let mut received = vec![0u8; 64];
    peer.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let read = peer.read(&mut received).unwrap();
    assert_eq!(
        &received[..read],
        encode_frame("trinit", &[("bm", "0".into())]).unwrap()
    );
    assert!(peer.read(&mut received).is_err(), "nothing else was sent");
}

#[test]
fn noise_before_a_frame_is_skipped_but_a_foreign_reply_is_not_a_success() {
    let (mut peer, mut session) = raw_peer();
    let mut noisy = b"\x00\xff".to_vec();
    noisy.extend_from_slice(&reply("getrealid\tnmX\t"));
    peer.write_all(&noisy).unwrap();
    let params = session.command("getrealid", &[], None).unwrap();
    assert_eq!(params["nm"], "X");

    peer.write_all(&reply("scnt\tbt1\t")).unwrap(); // a late reply to something else
    let error = session
        .command("trend", &param("to", "1"), None)
        .unwrap_err();
    assert!(
        error.to_string().starts_with("Oczekiwano odpowiedzi trend"),
        "{error}"
    );
}

/// A device that answers only with the bytes the test writes to it.
fn raw_peer() -> (TcpStream, Session) {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("sim://{}", server.local_addr().unwrap());
    let joiner = std::thread::spawn(move || server.accept().unwrap().0);
    let session = Session::open(&Connection {
        address,
        timeout: 0.5,
        ..Connection::default()
    })
    .unwrap();
    (joiner.join().unwrap(), session)
}

#[test]
fn fractional_quantity_rounds_line_half_up() {
    let line = Line::new("Naprawa", decimal("0.5"), decimal("19.99"), 0).unwrap();
    assert_eq!(line.total_cents(), 1000);
    assert_eq!(money(3468), "34,68 zł");
    assert_eq!(money(123_456_789), "1 234 567,89 zł");
}

#[test]
fn invalid_quantity_and_price_rejected() {
    for quantity in ["0", "-1", "0.000000001"] {
        assert!(Line::new("Naprawa", decimal(quantity), decimal("10"), 0).is_err());
    }
    for price in ["0", "-1", "1.001", "100000000"] {
        assert!(Line::new("Naprawa", decimal("1"), decimal(price), 0).is_err());
    }
    // Decimal cannot hold non-finite fiscal amounts.
    assert!(Decimal::from_str("NaN").is_err());
    assert!(Decimal::from_str("Infinity").is_err());
}

#[test]
fn raw_name_is_sanitized_instead_of_rejected() {
    let cases = [
        ("Кузов", "Kuzov"), // the printer cannot encode it: transliterated, not rejected
        ("Wymiana 🔧 oleju", "Wymiana :wrench: oleju"),
        ("x\tnaInjected", "x naInjected"), // tabs separate protocol fields
        ("Olej\u{ad}", "Olej"),
        ("Olej\u{a0}5W30", "Olej 5W30"),
        (&"x".repeat(100), &"x".repeat(80)),
    ];
    for (name, expected) in cases {
        let line = Line::new(name, decimal("1"), decimal("10"), 0).unwrap();
        assert_eq!(line.name(), expected, "{name:?}");
    }
}

#[test]
fn name_that_sanitizes_to_nothing_is_rejected() {
    for name in ["", " \t", "\u{ad}"] {
        assert!(
            Line::new(name, decimal("1"), decimal("10"), 0).is_err(),
            "{name:?}"
        );
    }
}

#[test]
fn name_is_normalized_before_checking_printer_limit() {
    let raw = format!(" o\u{301}{} ", "ł".repeat(79));
    let line = Line::new(&raw, decimal("1"), decimal("10"), 0).unwrap();
    assert_eq!(line.name(), format!("ó{}", "ł".repeat(79)));
    assert_eq!(sanitize(&raw, 80).chars().count(), 80);
}

#[test]
fn line_value_must_fit_wire_amount_and_vat_slot() {
    assert!(Line::new("Naprawa", decimal("9999999999"), decimal("10"), 0).is_err());
    assert!(Line::new("Naprawa", decimal("1"), decimal("10"), 7).is_err());
}

#[test]
fn vat_contained_in_gross_matches_printer_rounding() {
    let cases = [
        ("23", 10000, 1870),
        ("8", 10000, 741),
        ("0", 10000, 0),
        ("100", 10000, 0),
        ("23", 1, 0),
    ];
    for (percent, gross, tax) in cases {
        let rate = VatRate {
            index: 0,
            percent: decimal(percent),
        };
        assert_eq!(rate.tax_cents(gross), tax, "{percent}% of {gross}");
    }
}

#[test]
fn vat_labels_name_the_slot_the_operator_picks() {
    let cases = [
        (0, "23.00", "A · 23%"),
        (1, "0", "B · 0%"),
        (2, "100", "C · zwolniona"),
        (6, "101", "G · nieaktywna"),
    ];
    for (index, percent, label) in cases {
        let rate = VatRate {
            index,
            percent: decimal(percent),
        };
        assert_eq!(rate.label(), label);
        assert_eq!(rate.active(), percent != "101");
    }
}

#[test]
fn undefined_cp1250_codepoints_are_not_valid_wire_text() {
    for byte in [0x81u8, 0x83, 0x88, 0x90, 0x98] {
        assert!(encode_frame("trline", &param("na", &char::from(byte).to_string())).is_err());
        let mut payload = b"hdrget\ttx".to_vec();
        payload.push(byte);
        payload.push(b'\t');
        let mut frame = vec![2];
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(format!("#{:04X}\x03", reference_crc(&payload)).as_bytes());
        assert!(matches!(decode_frame(&frame), Err(Error::Protocol(_))));
    }
}

#[test]
fn slow_partial_reply_cannot_extend_the_command_deadline() {
    let (mut peer, mut session) = raw_peer();
    let sender = std::thread::spawn(move || {
        for byte in reply("trinit\tbm0\t") {
            if peer.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        session.command("trinit", &[], Some(0.12)),
        Err(Error::Timeout(_))
    ));
    assert!(started.elapsed() < Duration::from_millis(450));
    drop(session);
    sender.join().unwrap();
}

#[test]
fn invalid_command_timeout_never_sends_a_fiscal_command() {
    let (mut peer, mut session) = raw_peer();
    for timeout in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::MAX] {
        assert!(matches!(
            session.command("trinit", &[], Some(timeout)),
            Err(Error::Value(_))
        ));
    }
    peer.set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    assert!(peer.read(&mut [0; 1]).is_err());
}

#[test]
fn header_keeps_literal_and_trailing_ampersands() {
    assert_eq!(
        posnet::header_text("&c&1Kowalski && Syn&1&c\nul. X&"),
        "Kowalski & Syn\nul. X&"
    );
}

#[test]
fn money_handles_entire_signed_range() {
    assert_eq!(money(i64::MIN), "-92 233 720 368 547 758,08 zł");
}

#[cfg(unix)]
#[test]
fn serial_transport_preserves_partial_frames_and_never_retries() {
    use serialport::SerialPort;
    let (mut peer, mut slave) = serialport::TTYPort::pair().unwrap();
    slave.set_exclusive(false).unwrap();
    let connection = Connection {
        address: slave.name().unwrap(),
        timeout: 0.15,
        ..Connection::default()
    };
    drop(slave);
    let mut session = Session::open(&connection).unwrap();
    peer.set_timeout(Duration::from_millis(50)).unwrap();
    let sender = std::thread::spawn(move || {
        let expected = b"\x02trinit\tbm0\t#4825\x03";
        let mut request = vec![0; expected.len()];
        peer.read_exact(&mut request).unwrap();
        assert_eq!(request, expected);
        for chunk in reply("trinit\t").chunks(2) {
            peer.write_all(chunk).unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
        peer.read_exact(&mut request).unwrap();
        assert_eq!(request, expected);
        assert!(peer.read(&mut [0; 1]).is_err());
        // Keep the PTY open through the driver's response deadline.
        std::thread::sleep(Duration::from_millis(200));
    });
    session.command("trinit", &param("bm", "0"), None).unwrap();
    assert!(matches!(
        session.command("trinit", &param("bm", "0"), None),
        Err(Error::Timeout(_))
    ));
    sender.join().unwrap();
}

#[test]
fn simulator_executes_a_dropped_response_once() {
    let sim = posnet::simulator::Simulator::start().unwrap();
    sim.update(|state| {
        state.drop_responses.insert("trinit".into());
    });
    let mut session = Session::open(&sim.connection).unwrap();
    assert!(matches!(
        session.command("trinit", &param("bm", "0"), Some(0.05)),
        Err(Error::Timeout(_))
    ));
    let state = sim.snapshot();
    assert!(state.transaction_open);
    assert_eq!(
        state.commands,
        vec![serde_json::json!({"command":"trinit","params":{"bm":"0"}})]
    );
}

#[test]
fn frame_limits_and_transport_urls_fail_closed() {
    for command in ["", "TRINIT", "trinit\tbm0", "!"] {
        assert!(encode_frame(command, &[]).is_err());
    }
    for key in ["a", "abc", "?"] {
        assert!(encode_frame("trinit", &param(key, "0")).is_err());
    }
    assert!(
        encode_frame(
            "trline",
            &param("na", &"x".repeat(posnet::protocol::MAX_FRAME))
        )
        .is_err()
    );
    for address in ["", "loop://", "socket://127.0.0.1:1", "https://example.org"] {
        assert!(Connection::new(address).validate().is_err());
    }
    for timeout in [0.0, -1.0, 121.0, f64::NAN, f64::INFINITY] {
        assert!(
            Connection {
                address: "COM1".into(),
                timeout,
                ..Connection::default()
            }
            .validate()
            .is_err()
        );
    }
}
