//! Native printer simulation and real HTTP exchanges exercise fiscal and API boundaries.

use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use posnet::simulator::Simulator;
use serde_json::{Value, json};
use workshop_agent::{
    Api, Config, Error,
    agent::{Agent, DONE, FAILED, TAKEN, UNKNOWN},
    build, validated_url,
};

type Reply = (u16, Value);
type Request = (String, HashMap<String, String>, Value);

#[derive(Default)]
struct Served {
    jobs: VecDeque<Value>,
    remote: HashMap<i64, i64>,
    reports: Vec<(i64, i64, String, String)>,
    requests: Vec<Request>,
    replies: Option<VecDeque<Reply>>,
    fail_report: bool,
}

struct Server {
    url: String,
    served: Arc<Mutex<Served>>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn start(jobs: Vec<Value>) -> Self {
        Self::serve(Served {
            jobs: jobs.into(),
            ..Served::default()
        })
    }

    fn scripted(replies: Vec<Reply>) -> Self {
        Self::serve(Served {
            replies: Some(replies.into()),
            ..Served::default()
        })
    }

    fn serve(served: Served) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let served = Arc::new(Mutex::new(served));
        let state = Arc::clone(&served);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((peer, _)) => handle(peer, &state),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("HTTP accept: {error}"),
                }
            }
        });
        Self {
            url,
            served,
            stopping,
            thread: Some(thread),
        }
    }

    fn reports(&self) -> Vec<(i64, i64, String, String)> {
        self.served.lock().reports.clone()
    }

    fn requests(&self) -> Vec<Request> {
        self.served.lock().requests.clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn handle(mut peer: TcpStream, state: &Arc<Mutex<Served>>) {
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut reader = BufReader::new(peer.try_clone().unwrap());
    let mut request = String::new();
    if reader.read_line(&mut request).unwrap_or(0) == 0 {
        return;
    }
    let path = request.split_whitespace().nth(1).unwrap().to_string();
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim_end().is_empty() {
            break;
        }
        let (key, value) = header.trim_end().split_once(':').unwrap();
        headers.insert(key.to_lowercase(), value.trim().to_string());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut served = state.lock();
    served.requests.push((path.clone(), headers, body.clone()));
    let (code, reply) = if let Some(replies) = &mut served.replies {
        replies.pop_front().expect("unexpected extra API request")
    } else if path.ends_with("/session/") {
        session("session-token")
    } else if path.ends_with("/jobs/take/") {
        match served.jobs.pop_front() {
            Some(job) => {
                if let Some(id) = job["pk"].as_i64() {
                    served.remote.insert(id, TAKEN);
                }
                (200, job)
            }
            None => (204, Value::Null),
        }
    } else if path.ends_with("/result/") {
        if served.fail_report {
            (500, json!({}))
        } else {
            let id = path.split('/').nth_back(2).unwrap().parse().unwrap();
            let status = body["status"].as_i64().unwrap();
            served.remote.insert(id, status);
            served.reports.push((
                id,
                status,
                body["receipt_number"].as_str().unwrap().into(),
                body["result"].as_str().unwrap().into(),
            ));
            (204, Value::Null)
        }
    } else {
        let id = path.split('/').nth_back(1).unwrap().parse::<i64>().unwrap();
        (
            200,
            json!({"status": served.remote.get(&id).copied().unwrap_or(0)}),
        )
    };
    drop(served);
    let body = if code == 204 {
        String::new()
    } else {
        reply.to_string()
    };
    let response = format!(
        "HTTP/1.1 {code} Response\r\nContent-Type: application/json\r\nLocation: /steal-credential\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    peer.write_all(response.as_bytes()).unwrap();
}

fn session(token: &str) -> Reply {
    (200, json!({"access_token": token, "expires_in": 900}))
}

fn job() -> Value {
    json!({"pk": 7, "payment": 2, "payload": {
    "company_name": "Warsztat Kowalski", "lines": [
        {"name": "Wymiana oleju", "quantity": "2", "unit_price": "100.00"},
        {"name": "Klocki hamulcowe", "quantity": "1", "unit_price": "50.00"}
    ]}})
}

fn agent(sim: &Simulator, server: &Server, data: &Path) -> Agent {
    build(
        &Config {
            api_url: server.url.clone(),
            token: "secret".into(),
            serial: sim.connection.address.clone(),
            baudrate: 9600,
        },
        data,
        Arc::new(|_, _| {}),
    )
    .unwrap()
}

fn no_wait(agent: &Agent) {
    agent.stopping.store(true, Ordering::Relaxed);
}

#[test]
fn prints_job_and_uses_probed_identity_on_every_request() {
    let sim = Simulator::start().unwrap();
    sim.update(|s| s.header = "&c&1Firma Inna&1&c\nul. Testowa 1".into());
    let server = Server::start(vec![job()]);
    let data = tempfile::tempdir().unwrap();
    let mut agent = agent(&sim, &server, data.path());
    agent.api.connect().unwrap();
    agent.step().unwrap();
    let state = sim.snapshot();
    assert_eq!(state.receipts.len(), 1);
    assert_eq!(state.receipts[0]["total_cents"], 25000);
    assert_eq!(state.receipts[0]["payment"], 2);
    assert!(
        state.receipts[0]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|line| line["vt"] == "0")
    );
    assert_eq!(
        state.footers,
        [json!({"id":"25", "na":"Warsztat Kowalski"})]
    );
    assert!(!state.footer_open);
    assert_eq!(server.reports(), [(7, DONE, "1".into(), String::new())]);
    assert_eq!(
        agent.printer.last_record().unwrap().unwrap()["state"],
        "acknowledged"
    );
    for (_, headers, _) in server.requests() {
        assert_eq!(headers["x-device-serial"], "DEMO0000001");
        assert_eq!(headers["x-device-model"], "POSNET%20TEMO%20ONLINE");
        assert_eq!(headers["x-device-firmware"], "32.01");
        assert!(headers.values().all(|value| value.is_ascii()));
        let decoded = percent_encoding::percent_decode_str(&headers["x-device-vat-rates"])
            .decode_utf8()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&decoded).unwrap(),
            json!([
            {"index":0,"percent":"23"}, {"index":1,"percent":"8"},
            {"index":2,"percent":"0"}, {"index":6,"percent":"zw"}])
        );
    }
}

#[test]
fn invalid_jobs_fail_before_any_fiscal_command() {
    let sim = Simulator::start().unwrap();
    let server = Server::start(vec![]);
    let data = tempfile::tempdir().unwrap();
    let mut agent = agent(&sim, &server, data.path());
    let mut missing_payment = job();
    missing_payment.as_object_mut().unwrap().remove("payment");
    let mut bad_quantity = job();
    bad_quantity["payload"]["lines"][0]["quantity"] = json!("-1");
    let mut bad_brand = job();
    bad_brand["payload"]["company_name"] = json!({});
    let mut rounded_price = job();
    rounded_price["payload"]["lines"][0]["unit_price"] =
        json!("1.00000000000000000000000000000000001");
    for invalid in [
        missing_payment,
        bad_quantity,
        bad_brand,
        rounded_price,
        json!({"pk":7}),
        json!({"pk":7,"payment":"2","payload":job()["payload"]}),
    ] {
        agent.execute(&invalid).unwrap();
    }
    assert_eq!(server.reports().len(), 6);
    assert!(server.reports().iter().all(|report| report.1 == FAILED));
    assert!(sim.snapshot().receipts.is_empty());
    assert!(agent.printer.last_record().unwrap().is_none());
}

#[test]
fn malformed_jobs_do_not_prevent_later_printing() {
    let sim = Simulator::start().unwrap();
    let server = Server::start(vec![json!({"payload":null}), json!({"pk":8}), job()]);
    let data = tempfile::tempdir().unwrap();
    let mut agent = agent(&sim, &server, data.path());
    assert!(agent.step().is_err());
    agent.step().unwrap();
    agent.step().unwrap();
    assert_eq!(
        server
            .reports()
            .iter()
            .map(|r| (r.0, r.1))
            .collect::<Vec<_>>(),
        [(8, FAILED), (7, DONE)]
    );
    assert_eq!(sim.snapshot().receipts.len(), 1);
}

#[test]
fn exempt_vat_and_unready_printers() {
    let sim = Simulator::start().unwrap();
    sim.update(|s| {
        s.responses.insert(
            "vatget".into(),
            ('a'..='g')
                .map(|letter| {
                    (
                        format!("v{letter}"),
                        if letter == 'g' { "100,00" } else { "101,00" }.into(),
                    )
                })
                .collect(),
        );
    });
    let server = Server::start(vec![job()]);
    let data = tempfile::tempdir().unwrap();
    let mut agent = agent(&sim, &server, data.path());
    agent.step().unwrap();
    assert!(
        sim.snapshot().receipts[0]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|line| line["vt"] == "6")
    );
    sim.update(|s| {
        s.responses
            .get_mut("vatget")
            .unwrap()
            .insert("vg".into(), "101,00".into());
    });
    no_wait(&agent);
    let count = server.requests().len();
    agent.step().unwrap();
    assert_eq!(
        server.requests().len(),
        count,
        "no active rate must leave jobs queued"
    );
    agent.execute(&job()).unwrap();
    assert_eq!(server.reports().last().unwrap().1, FAILED);
    sim.update(|s| {
        s.responses.remove("vatget");
        s.responses
            .insert("sprn".into(), [("pr".into(), "5".into())].into());
    });
    let count = server.requests().len();
    agent.step().unwrap();
    assert_eq!(
        server.requests().len(),
        count,
        "out of paper must leave jobs queued"
    );
}

#[test]
fn uncertain_print_blocks_until_manager_resolves() {
    let sim = Simulator::start().unwrap();
    sim.update(|s| {
        s.errors.insert("trend".into(), 2000);
    });
    let server = Server::start(vec![job(), job()]);
    let data = tempfile::tempdir().unwrap();
    let mut agent = agent(&sim, &server, data.path());
    no_wait(&agent);
    agent.step().unwrap();
    assert_eq!(server.reports()[0].1, UNKNOWN);
    assert_eq!(agent.printer.pending().unwrap().unwrap()["stage"], "trend");
    agent.step().unwrap();
    assert_eq!(server.served.lock().jobs.len(), 1);
    assert!(sim.snapshot().receipts.is_empty());
    server.served.lock().remote.insert(7, FAILED);
    sim.update(|s| {
        s.errors.clear();
        s.transaction_open = false;
    });
    agent.step().unwrap();
    assert_eq!(sim.snapshot().receipts.len(), 1);
    assert_eq!(server.reports().last().unwrap().1, DONE);
}

#[test]
fn pending_restart_reports_unknown_and_waits_for_human() {
    let sim = Simulator::start().unwrap();
    let server = Server::start(vec![]);
    server.served.lock().remote.insert(7, TAKEN);
    let data = tempfile::tempdir().unwrap();
    std::fs::write(
        data.path().join("fiscal-operation.json"),
        json!({
        "state":"pending","operation":"receipt","stage":"trend","job_id":7,
        "unique_number":"DEMO0000001"})
        .to_string(),
    )
    .unwrap();
    let mut agent = agent(&sim, &server, data.path());
    no_wait(&agent);
    agent.step().unwrap();
    agent.step().unwrap();
    assert_eq!(server.reports().len(), 1);
    assert_eq!(server.reports()[0].1, UNKNOWN);
    assert!(agent.printer.pending().unwrap().is_some());
    server.served.lock().remote.insert(7, FAILED);
    agent.step().unwrap();
    assert!(agent.printer.pending().unwrap().is_none());
    assert!(sim.snapshot().receipts.is_empty());
}

#[test]
fn completed_receipt_is_reported_after_restart_never_reprinted() {
    for remote in [TAKEN, UNKNOWN, FAILED] {
        let sim = Simulator::start().unwrap();
        let server = Server::start(vec![job()]);
        server.served.lock().fail_report = true;
        let data = tempfile::tempdir().unwrap();
        let mut first = agent(&sim, &server, data.path());
        assert!(first.step().is_err());
        assert_eq!(
            first.printer.last_record().unwrap().unwrap()["state"],
            "completed"
        );
        drop(first);
        server.served.lock().fail_report = false;
        server.served.lock().remote.insert(7, remote);
        let mut restarted = agent(&sim, &server, data.path());
        no_wait(&restarted);
        restarted.step().unwrap();
        assert_eq!(sim.snapshot().receipts.len(), 1);
        assert_eq!(server.reports().len(), usize::from(remote != FAILED));
        assert_eq!(
            restarted.printer.last_record().unwrap().unwrap()["state"],
            "acknowledged"
        );
    }
}

#[test]
fn stop_waits_for_fiscal_completion_and_acknowledgement() {
    let sim = Simulator::start().unwrap();
    sim.update(|s| {
        s.delays.insert("trend".into(), Duration::from_millis(300));
    });
    let server = Server::start(vec![job()]);
    let data = tempfile::tempdir().unwrap();
    let mut agent = agent(&sim, &server, data.path());
    let stopping = Arc::clone(&agent.stopping);
    let thread = std::thread::spawn(move || {
        agent.run_forever();
        agent
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !data.path().join("fiscal-operation.json").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    stopping.store(true, Ordering::Relaxed);
    let agent = thread.join().unwrap();
    assert_eq!(sim.snapshot().receipts.len(), 1);
    assert_eq!(server.reports()[0].1, DONE);
    assert_eq!(
        agent.printer.last_record().unwrap().unwrap()["state"],
        "acknowledged"
    );
}

#[test]
fn sessions_reuse_credentials_and_bound_unicode_results() {
    let server = Server::scripted(vec![session("one"), (204, Value::Null), (204, Value::Null)]);
    let mut api = Api::new(&server.url, "secret", "DEMO0000001").unwrap();
    assert!(api.take().unwrap().is_none());
    api.report(7, FAILED, "", &"ą".repeat(5000)).unwrap();
    let requests = server.requests();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.1["authorization"].as_str())
            .collect::<Vec<_>>(),
        ["Agent secret", "Bearer one", "Bearer one"]
    );
    assert_eq!(
        requests[2].2["result"].as_str().unwrap().chars().count(),
        2000
    );
}

#[test]
fn unauthorized_refreshes_once_but_errors_never_replay_jobs() {
    let server = Server::scripted(vec![
        session("one"),
        (401, json!({})),
        session("two"),
        (401, json!({})),
    ]);
    let mut api = Api::new(&server.url, "secret", "DEMO0000001").unwrap();
    assert!(matches!(api.take(), Err(Error::Status(401, _))));
    assert_eq!(server.requests().len(), 4);
    for code in [302, 500] {
        let server = Server::scripted(vec![session("one"), (code, json!({}))]);
        let mut api = Api::new(&server.url, "secret", "DEMO0000001").unwrap();
        assert!(matches!(api.take(), Err(Error::Status(status, _)) if status == code));
        assert_eq!(server.requests().len(), 2);
    }
}

#[test]
fn invalid_sessions_are_errors_not_panics_or_authenticated_requests() {
    for session in [
        Value::Null,
        json!([]),
        json!({"access_token":"", "expires_in":900}),
        json!({"access_token":"a", "expires_in":true}),
        json!({"access_token":"a", "expires_in":0}),
        json!({"access_token":"a", "expires_in":-1}),
        json!({"access_token":"a", "expires_in":1.5}),
        json!({"access_token":"a", "expires_in":u64::MAX}),
    ] {
        let server = Server::scripted(vec![(200, session)]);
        let mut api = Api::new(&server.url, "secret", "DEMO0000001").unwrap();
        assert!(matches!(api.take(), Err(Error::Value(_))));
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn url_validation_prevents_credential_leaks_and_allows_ipv6_loopback() {
    for url in [
        "ftp://server",
        "http://server",
        "https://user:pass@server",
        "https://@server",
        "https://server?token=secret",
        "https://server/#fragment",
        "https:///missing-host",
    ] {
        assert!(validated_url(url).is_err(), "{url}");
    }
    for url in [
        "https://api.example",
        "http://localhost",
        "http://127.0.0.2",
        "http://[::1]:8000",
    ] {
        assert_eq!(validated_url(url).unwrap(), url);
    }
}

#[test]
fn session_renews_before_its_ttl_expires() {
    let server = Server::scripted(vec![
        (200, json!({"access_token":"one", "expires_in":1})),
        (204, Value::Null),
        session("two"),
        (204, Value::Null),
    ]);
    let mut api = Api::new(&server.url, "secret", "DEMO0000001").unwrap();
    api.take().unwrap();
    std::thread::sleep(Duration::from_millis(950));
    api.take().unwrap();
    assert_eq!(
        server
            .requests()
            .iter()
            .map(|r| r.1["authorization"].clone())
            .collect::<Vec<_>>(),
        ["Agent secret", "Bearer one", "Agent secret", "Bearer two"]
    );
}

#[test]
fn exact_scientific_amounts_print_without_rounding() {
    let sim = Simulator::start().unwrap();
    let mut scientific = job();
    scientific["payload"]["lines"][0]["quantity"] = json!("2e0");
    scientific["payload"]["lines"][0]["unit_price"] = json!("1e2");
    let server = Server::start(vec![scientific]);
    let data = tempfile::tempdir().unwrap();
    agent(&sim, &server, data.path()).step().unwrap();
    assert_eq!(sim.snapshot().receipts[0]["total_cents"], 25000);
}

#[test]
fn failed_startup_probe_never_sends_api_credentials() {
    let sim = Simulator::start().unwrap();
    sim.update(|state| {
        state.errors.insert("getrealid".into(), 2000);
    });
    let server = Server::start(vec![]);
    let data = tempfile::tempdir().unwrap();
    let result = build(
        &Config {
            api_url: server.url.clone(),
            token: "secret".into(),
            serial: sim.connection.address.clone(),
            baudrate: 9600,
        },
        data.path(),
        Arc::new(|_, _| {}),
    );
    assert!(result.is_err());
    assert!(server.requests().is_empty());
    assert!(sim.snapshot().receipts.is_empty());
}
