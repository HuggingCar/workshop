"""Agent loop against the simulator and an in-memory stand-in for the HuggingCar API."""

import contextlib

from posnet.printer import Printer
from posnet.protocol import ProtocolError
from posnet.simulator import Simulator
from workshop_agent.agent import DONE, FAILED, TAKEN, UNKNOWN, Agent

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


def test_prints_job_and_reports_receipt_number_without_footer_when_header_has_brand(tmp_path):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB])
        Agent(printer, api).step()

        assert len(sim.receipts) == 1
        assert sim.receipts[0]["total_cents"] == 25000
        assert sim.receipts[0]["payment"] == 2
        assert sim.footers == []
        assert api.reports == [(7, DONE, "1", "")]
        assert printer.last_record()["state"] == "acknowledged"


def test_adds_brand_footer_when_header_lacks_company_name(tmp_path):
    with Simulator() as sim:
        sim.header = "&c&1Firma Inna&1&c\nul. Testowa 1"
        printer = Printer(sim.connection, tmp_path / "operation.json")
        api = FakeApi([JOB])
        Agent(printer, api).step()

        assert sim.footers == [{"id": "25", "na": "Warsztat Kowalski"}]
        assert not sim.footer_open
        assert api.reports[0][1] == DONE


def test_invalid_payload_fails_without_touching_printer(tmp_path):
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        bad = {**JOB, "payload": {**JOB["payload"], "lines": [{"name": "x"}]}}
        api = FakeApi([bad])
        Agent(printer, api).step()

        assert sim.receipts == []
        assert api.reports[0][1] == FAILED
        assert printer.last_record() is None


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
