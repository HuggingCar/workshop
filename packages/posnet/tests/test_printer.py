import fcntl
from decimal import Decimal

import pytest
from posnet.models import Line
from posnet.printer import Printer, header_text
from posnet.protocol import Connection, ProtocolError
from posnet.simulator import Simulator


def test_printer_lock_rejects_overlap_and_releases_after_connection_failure(tmp_path):
    printer = Printer(Connection(str(tmp_path / "missing-port")), tmp_path / "operation.json")
    with (tmp_path / "operation.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(ValueError):
            printer.probe()
        fcntl.flock(lock, fcntl.LOCK_UN)

        with pytest.raises(ProtocolError):
            printer.probe()
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)


def test_header_text_strips_formatting_and_keeps_literal_ampersand():
    assert header_text("&c&1Kowalski && Syn&1&c\nul. X") == "Kowalski & Syn\nul. X"


def test_brand_footer_only_when_header_lacks_company_name(tmp_path):
    lines = [Line("Wymiana oleju", Decimal(1), Decimal("100.00"), 0)]
    with Simulator() as sim:
        printer = Printer(sim.connection, tmp_path / "operation.json")
        record = printer.print_receipt(lines, 0, brand="warsztat kowalski", job_id=7)
        assert sim.footers == []
        assert record["receipt_number"] == "1"
        assert record["job_id"] == 7
        assert record["state"] == "completed"

        sim.header = "&cInny Serwis&c"
        printer.acknowledge(record)
        printer.print_receipt(lines, 2, brand="Warsztat Kowalski")
        assert sim.footers == [{"id": "25", "na": "Warsztat Kowalski"}]
        assert not sim.footer_open
        assert printer.last_record()["footer"] == "Warsztat Kowalski"


def test_detect_adopts_the_port_that_identifies_as_temo(tmp_path):
    with Simulator() as sim:
        printer = Printer(Connection("", 9600), tmp_path / "operation.json")
        status = printer.detect(["/dev/null", sim.connection.address])
        assert status.unique_number == "DEMO0000001"
        assert printer.connection.address == sim.connection.address
        assert printer.probe().ready  # the adopted port is now the saved one


def test_detect_reports_the_original_error_when_nothing_answers(tmp_path):
    printer = Printer(Connection("", 9600), tmp_path / "operation.json")
    with pytest.raises(ValueError, match="Nie wykryto drukarki"):
        printer.detect(["/dev/null"])
    assert printer.connection.address == ""


def test_detect_never_switches_device_while_an_operation_is_unresolved(tmp_path):
    with Simulator() as sim:
        printer = Printer(Connection("", 9600), tmp_path / "operation.json")
        (tmp_path / "operation.json").write_text(
            '{"state": "pending", "operation": "receipt", "unique_number": "OTHER"}'
        )
        with pytest.raises(ValueError, match="Nie wykryto drukarki"):
            printer.detect([sim.connection.address])
        assert printer.connection.address == ""
        assert sim.receipts == []
