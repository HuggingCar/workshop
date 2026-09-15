import threading
import time
from decimal import Decimal
from types import SimpleNamespace

import pytest
from posnet.models import VatRate
from posnet.protocol import Connection
from PySide6.QtCore import QSettings
from PySide6.QtWidgets import QApplication, QDialog


def isolate_settings(monkeypatch, tmp_path):
    """Route QSettings to a file under tmp_path; setPath() is ignored once Qt has cached it."""
    from fiscal_desktop import app as app_module

    ini = str(tmp_path / "settings.ini")
    monkeypatch.setattr(app_module, "settings", lambda: QSettings(ini, QSettings.Format.IniFormat))


def wait_for(app, condition, window=None):
    deadline = time.monotonic() + 5
    while not condition() and time.monotonic() < deadline:
        app.processEvents()
        time.sleep(0.005)
    assert condition(), window and window.message_label.text()


@pytest.fixture
def editor(tmp_path, monkeypatch):
    from fiscal_desktop.app import MainWindow

    app = QApplication.instance() or QApplication([])
    isolate_settings(monkeypatch, tmp_path)
    gate = threading.Event()
    gate.set()

    class Service:
        connection = Connection("/dev/null", 9600)
        journal_path = tmp_path / "pending.json"
        submitted = None
        fail = False
        offline = False

        def detect(self, _candidates):
            return self.probe()

        def probe(self):
            if self.offline:
                raise OSError("Brak drukarki")
            return SimpleNamespace(
                name="TEMO ONLINE",
                version="2.01",
                unique_number="ABC1234567",
                ready=True,
                description="Gotowa",
                vat_rates=[VatRate(0, Decimal(23))],
            )

        def pending(self):
            return None

        def print_receipt(self, lines, payment=0):
            assert gate.wait(3)
            if self.fail:
                raise RuntimeError("Brak odpowiedzi drukarki")
            self.submitted = (lines, payment)
            return {
                "timestamp": "2026-09-14T09:12:00+02:00",
                "receipt_number": "7",
                "total_cents": sum(line.total_cents for line in lines),
                "payment": payment,
                "lines": [{"name": line.name} for line in lines],
            }

        def periodic_report(self, start, end, summary=False):
            self.submitted = ("periodic", start, end, summary)
            return {}

        def monthly_report(self, year, month):
            self.submitted = ("monthly", year, month)
            return {}

    service = Service()
    monkeypatch.setattr("fiscal_desktop.app.serial_ports", list)
    window = MainWindow(service)
    window.show()
    wait_for(app, lambda: window.status is not None and not window.busy, window)
    yield app, window, service, gate
    gate.set()
    wait_for(app, lambda: not window.busy)
    window.close()
    app.processEvents()


def test_decimal_totals_and_unpriced_rows(editor):
    _, window, _, _ = editor
    window.table.item(0, 1).setText("2")
    window.table.item(0, 2).setText("12,34")
    window.table.item(1, 1).setText("0,5")
    window.table.item(1, 2).setText("19,99")
    assert window.total_cents == 3468
    assert len(window.receipt_lines()) == 2
    window.table.item(1, 2).setText("")
    assert window.total_cents == 2468
    window.table.item(0, 1).setText("1")
    window.table.item(0, 2).setText("100")
    assert window.breakdown_label.text() == "Netto 81,30 zł  ·  VAT 23% 18,70 zł"


def test_receipt_serializes_work_and_clears_only_after_success(editor, monkeypatch):
    app, window, service, gate = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.table.item(0, 0).setText("Usługa własna")
    window.table.item(0, 2).setText("19,99")
    window.payment_combo.setCurrentIndex(1)
    gate.clear()
    window.print_button.click()
    assert window.busy
    assert not window.table.isEnabled()
    assert not window.print_button.isEnabled()
    assert not window.close()
    assert window.table.item(0, 2).text() == "19,99"
    gate.set()
    wait_for(app, lambda: service.submitted is not None and not window.busy)
    assert service.submitted[1] == 2
    assert service.submitted[0][0].total_cents == 1999
    assert window.table.item(0, 2).text() == ""


def test_bad_quantity_rejected_and_failed_receipt_keeps_form(editor, monkeypatch):
    app, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.table.item(0, 2).setText("10")
    window.table.item(0, 1).setText("niewłaściwa")
    assert not window.print_button.isEnabled()
    with pytest.raises(ValueError):
        window.receipt_lines()
    window.table.item(0, 1).setText("1")
    service.fail = True
    window.print_button.click()
    wait_for(app, lambda: not window.busy)
    assert window.table.item(0, 2).text() == "10"
    assert not window.print_button.isEnabled()
    assert window.status is None


def test_printed_receipt_lands_in_history_with_journal_timestamp(editor, monkeypatch):
    app, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.table.item(0, 2).setText("150")
    window.print_button.click()
    wait_for(app, lambda: service.submitted is not None and not window.busy)
    assert "nr 7" in window.message_label.text()
    window.tabs.setCurrentWidget(window.history_page)
    row = [window.history_table.item(0, col).text() for col in range(5)]
    assert row == ["Usługa własna", "2026-09-14 09:12", "7", "150,00 zł", "Gotówka"]


def test_search_filters_rows_but_hidden_priced_rows_stay_on_receipt(editor):
    _, window, _, _ = editor
    window.table.item(0, 2).setText("100")  # Usługa własna
    window.search.setText("opon")
    visible = [r for r in range(window.table.rowCount()) if not window.table.isRowHidden(r)]
    assert [window.table.item(r, 0).text() for r in visible] == ["Serwis opon"]
    assert window.total_cents == 10000
    window.search.clear()
    assert not any(window.table.isRowHidden(r) for r in range(window.table.rowCount()))


def test_services_edited_in_settings_persist_and_rebuild_table(editor, monkeypatch):
    from fiscal_desktop.app import ConnectionDialog, saved_services

    _, window, _, _ = editor

    def fake_exec(self):
        self.list.clear()
        self._add_service()
        self.list.item(0).setText("  Geometria kół ")
        self._add_service()
        self.list.item(1).setText("Geometria kół")  # duplicate collapses
        self._add_service()
        self.list.item(2).setText("Wulkanizacja")
        return QDialog.DialogCode.Accepted

    monkeypatch.setattr(ConnectionDialog, "exec", fake_exec)
    window.configure_connection()
    assert saved_services() == ["Geometria kół", "Wulkanizacja"]
    assert [window.table.item(r, 0).text() for r in range(window.table.rowCount())] == [
        "Geometria kół",
        "Wulkanizacja",
    ]
    assert window.total_cents == 0


def test_single_saved_service_survives_cold_read(tmp_path, monkeypatch):
    from fiscal_desktop import app as app_module

    isolate_settings(monkeypatch, tmp_path)
    app_module.settings().setValue("services", '["Tylko jedna"]')
    assert app_module.saved_services() == ["Tylko jedna"]
    app_module.settings().setValue("services", "[]")
    assert app_module.saved_services()[0] == "Usługa własna"
    app_module.settings().setValue("services", "Geometria kół, Wulkanizacja")  # pre-JSON format
    assert app_module.saved_services()[0] == "Usługa własna"


def test_monthly_report_defaults_to_previous_month(editor, monkeypatch):
    from PySide6.QtCore import QDate

    app, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    previous = QDate.currentDate().addMonths(-1)
    assert window.month_picker.currentData() == (previous.year(), previous.month())
    assert window.month_picker.currentText().endswith(str(previous.year()))
    window.monthly_button.click()
    wait_for(app, lambda: service.submitted is not None and not window.busy)
    assert service.submitted == ("monthly", previous.year(), previous.month())


def test_periodic_report_sends_dates_and_summary_flag(editor, monkeypatch):
    from datetime import date

    app, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.period_start.setDate(window.period_end.date().addDays(-3))
    window.period_summary.setChecked(True)
    window.periodic_button.click()
    wait_for(app, lambda: service.submitted is not None and not window.busy)
    kind, start, end, summary = service.submitted
    assert kind == "periodic"
    assert isinstance(start, date) and (end - start).days == 3
    assert summary is True
    wait_for(app, lambda: not window.busy and window.status is not None)
    window.period_end.setDate(window.period_start.date().addDays(-1))
    service.submitted = None
    window.periodic_button.click()
    assert service.submitted is None
    assert "późniejsza" in window.message_label.text()


def test_unavailable_printer_is_reprobed_until_it_answers(tmp_path, monkeypatch):
    from fiscal_desktop import app as app_module

    monkeypatch.setattr(app_module, "REPROBE_MS", 50)
    app = QApplication.instance() or QApplication([])
    isolate_settings(monkeypatch, tmp_path)

    class Service:
        connection = Connection("/dev/null", 9600)
        journal_path = tmp_path / "pending.json"
        probes = 0
        offline = True

        def detect(self, _candidates):
            return self.probe()

        def probe(self):
            self.probes += 1
            if self.offline:
                raise OSError("Brak drukarki")
            return SimpleNamespace(
                name="TEMO ONLINE",
                version="2.01",
                unique_number="ABC1234567",
                ready=True,
                description="Gotowa",
                vat_rates=[VatRate(0, Decimal(23))],
            )

        def pending(self):
            return None

    service = Service()
    monkeypatch.setattr("fiscal_desktop.app.serial_ports", list)
    window = app_module.MainWindow(service)
    window.show()
    wait_for(app, lambda: service.probes >= 2 and not window.busy)
    assert window.status is None
    assert "Brak drukarki" in window.message_label.text()
    service.offline = False
    wait_for(app, lambda: window.status is not None and not window.busy)
    assert window.print_button.isEnabled() is False  # nothing priced yet, but the printer is back
    assert window.vat_combo.count() == 1
    probes = service.probes
    for _ in range(20):  # ~200 ms > REPROBE_MS: no further probes once the printer is back
        app.processEvents()
        time.sleep(0.01)
    assert service.probes == probes
    window.close()
    app.processEvents()
