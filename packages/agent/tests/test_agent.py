"""Agent loop against the simulator and an in-memory stand-in for the HuggingCar API."""

import contextlib
import json
import time
from urllib.parse import unquote

import pytest
from posnet.printer import Printer
from posnet.protocol import ProtocolError
from posnet.simulator import Simulator
from workshop_agent.agent import DONE, FAILED, TAKEN, UNKNOWN, Agent, Api

JOB = {
    "pk": 7,
    "payment": 2,
    "payload": {
        "company_name": "Warsztat Kowalski",
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
        self.described = []

    def describe(self, status):
        self.described.append((status.name, status.version))

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
        assert {line["vt"] for line in sim.receipts[0]["lines"]} == {"0"}  # slot A, 23%
        assert sim.footers == [{"id": "25", "na": "Warsztat Kowalski"}]
        assert not sim.footer_open
        assert api.reports == [(7, DONE, "1", "")]
        assert printer.last_record()["state"] == "acknowledged"
        assert api.described == [("POSNET TEMO ONLINE", "32.01")]  # from the device, not configured


def test_invalid_payload_fails_without_touching_printer(tmp_path):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        bad = {**JOB, "payload": {**JOB["payload"], "lines": [{"name": "x"}]}}
        api = FakeApi([bad])
        Agent(printer, api).step()

        assert sim.receipts == []
        assert api.reports[0][1] == FAILED
        assert printer.last_record() is None


def test_api_reports_the_device_as_ascii_headers(tmp_path):
    with Simulator() as sim:
        status = Printer(sim.connection, tmp_path / "operation.json").probe()
    api = Api("https://api.example", "token", status.unique_number)
    api.describe(status)

    assert all(value.isascii() for value in api.headers.values())
    assert unquote(api.headers["X-Device-Model"]) == "POSNET TEMO ONLINE"
    assert unquote(api.headers["X-Device-Firmware"]) == "32.01"
    assert json.loads(unquote(api.headers["X-Device-Vat-Rates"])) == [
        {"index": 0, "percent": "23"},
        {"index": 1, "percent": "8"},
        {"index": 2, "percent": "0"},
        {"index": 6, "percent": "zw"},
    ]  # only active slots, the exempt sentinel spelled out


def test_vat_exempt_workshop_prints_on_the_exempt_slot(tmp_path, monkeypatch):
    with Simulator() as sim:
        handle = sim._handle
        rates = dict.fromkeys((f"v{letter}" for letter in "abcdef"), "101,00") | {"vg": "100,00"}
        monkeypatch.setattr(sim, "_handle", lambda c, p: rates if c == "vatget" else handle(c, p))
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB])
        Agent(printer, api).step()

        assert api.reports[0][:2] == (7, DONE)
        assert {line["vt"] for line in sim.receipts[0]["lines"]} == {"6"}  # slot G, zwolniona


def test_printer_without_any_active_rate_fails_without_printing(tmp_path, monkeypatch):
    with Simulator() as sim:
        handle = sim._handle
        rates = dict.fromkeys((f"v{letter}" for letter in "abcdefg"), "101,00")
        monkeypatch.setattr(sim, "_handle", lambda c, p: rates if c == "vatget" else handle(c, p))
        monkeypatch.setattr("workshop_agent.agent.time.sleep", lambda _: None)
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB])
        agent = Agent(printer, api)
        agent.step()

        assert api.jobs == [JOB]  # such a device is not ready: the job is left in the queue
        agent.execute(JOB)  # and if it is handed one anyway, it fails before printing
        assert api.reports[0][:2] == (7, FAILED)
        assert "stawki VAT" in api.reports[0][3]
        assert sim.receipts == []
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


def test_malformed_server_replies_do_not_kill_the_loop(tmp_path, monkeypatch):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([{"payload": None}, {"pk": 8}, {**JOB, "pk": 9}])
        agent = Agent(printer, api)
        monkeypatch.setattr(time, "sleep", lambda _: setattr(agent, "stopping", not api.jobs))
        agent.run_forever()

        assert (8, FAILED) in [r[:2] for r in api.reports]
        assert (9, DONE) in [r[:2] for r in api.reports]
        assert len(sim.receipts) == 1


@pytest.mark.parametrize("remote_status", [TAKEN, UNKNOWN])
def test_crash_after_print_before_report_is_settled_on_restart(tmp_path, remote_status):
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
        api.remote[7] = remote_status
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
