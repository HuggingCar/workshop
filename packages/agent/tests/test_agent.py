"""Agent loop against the simulator and an in-memory stand-in for the HuggingCar API."""

import contextlib
import json
import os
import signal
import time
from http import HTTPStatus

import pytest
from posnet.printer import Printer
from posnet.protocol import ProtocolError
from posnet.simulator import Simulator
from workshop_agent.agent import DONE, FAILED, TAKEN, UNKNOWN, Agent, Api, main

JOB = {
    "pk": 7,
    "payment": 2,
    "payload": {
        "company_name": "Warsztat Kowalski",
        "vat": 0,
        "lines": [
            {"name": "Wymiana oleju", "quantity": "2", "unit_price": "100.00"},
            {"name": "Klocki hamulcowe", "quantity": "1", "unit_price": "50.00"},
        ],
    },
}


class FakeApi:
    def __init__(self, jobs):
        self.jobs = list(jobs)
        self.remote = {}
        self.reports = []

    def take(self):
        if not self.jobs:
            return None
        job = self.jobs.pop(0)
        self.remote[job["pk"]] = TAKEN
        return job

    def status(self, job_id):
        return {"status": self.remote[job_id]}

    def report(self, job_id, status, receipt_number="", result=""):
        self.remote[job_id] = status
        self.reports.append((job_id, status, receipt_number, result))


def test_prints_job_forwarding_brand_and_reports_receipt_number(tmp_path):
    with Simulator() as sim:
        sim.header = "&c&1Firma Inna&1&c\nul. Testowa 1"  # the brand must reach the footer
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB])
        Agent(printer, api).step()

        assert len(sim.receipts) == 1
        assert sim.receipts[0]["total_cents"] == 25000
        assert sim.receipts[0]["payment"] == 2
        assert sim.footers == [{"id": "25", "na": "Warsztat Kowalski"}]
        assert not sim.footer_open
        assert api.reports == [(7, DONE, "1", "")]
        assert printer.last_record()["state"] == "acknowledged"


def test_invalid_payload_fails_without_touching_printer(tmp_path):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        bad = {**JOB, "payload": {**JOB["payload"], "lines": [{"name": "x"}]}}
        api = FakeApi([bad])
        Agent(printer, api).step()

        assert sim.receipts == []
        assert api.reports[0][1] == FAILED
        assert printer.last_record() is None


def test_not_ready_printer_takes_no_job(tmp_path, monkeypatch):
    with Simulator() as sim:
        handle = sim._handle
        monkeypatch.setattr(
            sim, "_handle", lambda c, p: {"pr": "5"} if c == "sprn" else handle(c, p)
        )
        monkeypatch.setattr("workshop_agent.agent.time.sleep", lambda _: None)
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB, JOB])
        Agent(printer, api).step()

        assert len(api.jobs) == 2  # out of paper: the queue is left for later
        assert api.reports == []
        assert sim.receipts == []


def test_failure_mid_receipt_is_reported_unknown_and_blocks(tmp_path, monkeypatch):
    with Simulator() as sim:
        handle = sim._handle

        def dying(command, params):
            if command == "trend":
                raise ValueError  # ?2000 after the lines went through: paper state unknown
            return handle(command, params)

        monkeypatch.setattr(sim, "_handle", dying)
        monkeypatch.setattr("workshop_agent.agent.time.sleep", lambda _: None)
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB, JOB])
        agent = Agent(printer, api)
        agent.step()

        assert api.reports[0][:2] == (7, UNKNOWN)
        assert printer.pending()["stage"] == "trend"
        agent.step()  # still blocked: nothing taken, nothing printed until a human decides
        assert len(api.jobs) == 1
        assert sim.receipts == []


def test_malformed_server_replies_do_not_kill_the_loop_and_sigterm_stops_it(tmp_path, monkeypatch):
    with Simulator() as sim:
        # Stop the moment the queue is drained: what systemd would do at the end of a day.
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([{"payload": None}, {"pk": 8}, {**JOB, "pk": 9}])
        monkeypatch.setattr(
            time, "sleep", lambda _: api.jobs or os.kill(os.getpid(), signal.SIGTERM)
        )
        handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
        agent = Agent(printer, api)
        try:
            agent.run_forever()
        finally:
            for sig, handler in handlers.items():
                signal.signal(sig, handler)

        assert (8, FAILED) in [r[:2] for r in api.reports]
        assert (9, DONE) in [r[:2] for r in api.reports]
        assert len(sim.receipts) == 1


def test_crash_after_print_before_report_is_settled_on_restart(tmp_path):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB])

        # First run: printed, but the process died before reporting.
        class DyingApi(FakeApi):
            def report(self, *_args, **_kwargs):
                raise ProtocolError("network down")

        dying = DyingApi([JOB])
        with contextlib.suppress(ProtocolError):
            Agent(printer, dying).step()
        assert len(sim.receipts) == 1
        assert printer.last_record()["state"] == "completed"

        # Restart: the completed journal record is reported, never re-printed.
        api.remote = dying.remote
        assert Agent(printer, api)._settle_last_operation() is False
        assert api.reports == [(7, DONE, "1", "")]
        assert len(sim.receipts) == 1


def test_pending_journal_reports_unknown_and_blocks_until_manager_resolves(tmp_path):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        printer._save(
            {
                "state": "pending",
                "operation": "receipt",
                "stage": "trend",
                "job_id": 7,
                "unique_number": "DEMO0000001",
            }
        )
        api = FakeApi([])
        api.remote[7] = TAKEN
        agent = Agent(printer, api)

        assert agent._settle_last_operation() is True
        assert api.reports[0][1] == UNKNOWN
        assert agent._settle_last_operation() is True  # still waiting for the manager

        api.remote[7] = FAILED  # manager looked at the paper: nothing printed
        assert agent._settle_last_operation() is False
        assert printer.pending() is None
        assert sim.receipts == []


def test_api_rejects_a_non_http_base_url_and_sends_bounded_authorized_requests(monkeypatch):
    with pytest.raises(ValueError, match="http"):
        Api("ftp://server", "secret", "DEMO0000001")

    sent = []

    class Response:
        status = HTTPStatus.NO_CONTENT

        def __enter__(self):
            return self

        def __exit__(self, *_):
            pass

    monkeypatch.setattr(
        "workshop_agent.agent.urlopen",
        lambda request, timeout: sent.append(request) or Response(),
    )
    api = Api("https://server/", "secret", "DEMO0000001")
    assert api.take() is None
    assert api.report(7, FAILED, result="x" * 5000) is None
    take, report = sent
    assert take.full_url == "https://server/integrations/fiscal/agent/jobs/take/"
    assert take.headers["Authorization"] == "Agent secret"
    assert take.headers["X-device-serial"] == "DEMO0000001"
    assert len(json.loads(report.data)["result"]) == 2000  # the server rejects longer results


def test_credentials_are_remembered_owner_only_and_missing_ones_stop_the_agent(
    tmp_path, monkeypatch
):
    config = tmp_path / "fiscal.json"
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        assert main(printer, config, None, None) == 2
        assert not config.exists()

        monkeypatch.setattr(Agent, "run_forever", lambda _self: None)
        assert main(printer, config, "https://server", "secret") == 0
        assert json.loads(config.read_text()) == {"api_url": "https://server", "token": "secret"}
        assert config.stat().st_mode & 0o777 == 0o600
        assert main(printer, config, None, None) == 0  # later runs need only --serial
        assert main(printer, config, "ftp://server", "secret") == 2
