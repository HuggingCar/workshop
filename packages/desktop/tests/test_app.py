import json
import threading
import time
from decimal import Decimal
from types import SimpleNamespace

import pytest
from posnet.models import VatRate
from posnet.protocol import Connection
from PySide6.QtCore import QSettings, Qt
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QDialog, QLineEdit, QTabWidget


def isolate_settings(monkeypatch, tmp_path):
    """Route QSettings to a file under tmp_path; setPath() is ignored once Qt has cached it."""
    from fiscal_desktop import app as app_module

    ini = str(tmp_path / "settings.ini")
    monkeypatch.setattr(app_module, "settings", lambda: QSettings(ini, QSettings.Format.IniFormat))


def wait_for(condition, window=None):
    deadline = time.monotonic() + 5
    while not condition() and time.monotonic() < deadline:
        QTest.qWait(5)  # processes events while waiting
    assert condition(), window and window.message_label.text()


@pytest.fixture
def editor(tmp_path, monkeypatch):  # noqa: C901 — one fake covering the whole printer surface
    from fiscal_desktop.app import MainWindow

    app = QApplication.instance() or QApplication([])
    isolate_settings(monkeypatch, tmp_path)
    gate = threading.Event()
    gate.set()

    class Service:
        connection = Connection("/dev/null", 9600)
        journal_path = tmp_path / "pending.json"
        submitted = None
        fail = False  # True: refused before printing; "pending": died mid-receipt
        offline = False
        ready = True
        pending_record = None
        prints = 0

        def detect(self, _candidates):
            return self.probe()

        def probe(self):
            if self.offline:
                raise OSError("Brak drukarki")
            return SimpleNamespace(
                name="TEMO ONLINE",
                version="2.01",
                unique_number="ABC1234567",
                ready=self.ready,
                description="Gotowa" if self.ready else "Brak papieru",
                vat_rates=[VatRate(0, Decimal(23)), VatRate(1, Decimal(101))],
            )

        def pending(self):
            return self.pending_record

        def print_receipt(self, lines, payment=0):
            assert gate.wait(3)
            self.prints += 1
            if self.fail == "pending":
                self.pending_record = {"operation": "receipt", "stage": "trend"}
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

        def acknowledge_pending(self, **_):
            self.pending_record = None

        def daily_report(self):
            self.submitted = ("daily",)
            return {}

        def periodic_report(self, start, end, *, summary=False):
            self.submitted = ("periodic", start, end, summary)
            return {}

        def monthly_report(self, year, month):
            self.submitted = ("monthly", year, month)
            return {}

    service = Service()
    monkeypatch.setattr("fiscal_desktop.app.serial_ports", list)
    window = MainWindow(service)
    window.show()
    wait_for(lambda: window.status is not None and not window.busy, window)
    yield app, window, service, gate
    gate.set()
    wait_for(lambda: not window.busy)
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
    _, window, service, gate = editor
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
    window.submit_receipt()  # Enter on the default button while busy: no second worker
    gate.set()
    wait_for(lambda: service.submitted is not None and not window.busy)
    assert service.prints == 1
    assert service.submitted[1] == 2
    assert service.submitted[0][0].total_cents == 1999
    assert window.table.item(0, 2).text() == ""


def test_declined_confirmation_prints_nothing(editor, monkeypatch):
    app, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: False)
    window.table.item(0, 2).setText("19,99")
    window.print_button.click()
    window.daily_button.click()
    app.processEvents()
    assert not window.busy
    assert service.prints == 0
    assert service.submitted is None
    assert window.table.item(0, 2).text() == "19,99"


def test_unresolved_receipt_locks_everything_until_acknowledged(editor, monkeypatch):
    _, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.table.item(0, 2).setText("19,99")
    service.fail = "pending"
    window.print_button.click()
    wait_for(lambda: not window.busy)
    assert window.pending_operation == service.pending_record
    assert window.recovery_button.isVisible()
    assert not window.table.isEnabled()
    assert not window.print_button.isEnabled()
    assert not window.daily_button.isEnabled()
    assert "niepotwierdzony" in window.message_label.text()
    assert window.table.item(0, 2).text() == "19,99"  # kept for the operator to compare

    window.resolve_pending()
    wait_for(lambda: window.status is not None and not window.busy)
    assert service.pending_record is None
    assert not window.recovery_button.isVisible()
    assert window.table.isEnabled()
    assert window.table.item(0, 2).text() == ""  # never one click away from a reprint
    assert not window.print_button.isEnabled()


def test_not_ready_printer_blocks_printing_and_inactive_rate_is_hidden(editor):
    _, window, service, _ = editor
    assert [window.vat_combo.itemData(i) for i in range(window.vat_combo.count())] == [0]
    window.table.item(0, 2).setText("10")
    assert window.print_button.isEnabled()
    service.ready = False
    window.refresh_status(scan=False)
    wait_for(lambda: not window.busy)
    assert "Brak papieru" in window.message_label.text()
    assert not window.print_button.isEnabled()
    assert not window.daily_button.isEnabled()
    assert window.table.isEnabled()  # the form stays editable while waiting for paper


def test_daily_report_asks_then_prints(editor, monkeypatch):
    _, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.daily_button.click()
    wait_for(lambda: service.submitted is not None and not window.busy)
    assert service.submitted == ("daily",)
    assert "Raport wydrukowany" in window.message_label.text()


def test_bad_quantity_rejected_and_failed_receipt_keeps_form(editor, monkeypatch):
    _, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.table.item(0, 2).setText("10")
    window.table.item(0, 1).setText("niewłaściwa")
    assert not window.print_button.isEnabled()
    with pytest.raises(ValueError):
        window.receipt_lines()
    window.table.item(0, 1).setText("1")
    service.fail = True
    window.print_button.click()
    wait_for(lambda: not window.busy)
    assert window.table.item(0, 2).text() == "10"
    assert not window.print_button.isEnabled()
    assert window.status is None


def test_printed_receipt_lands_in_history_with_journal_timestamp(editor, monkeypatch):
    _, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.table.item(0, 2).setText("150")
    window.print_button.click()
    wait_for(lambda: service.submitted is not None and not window.busy)
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
    window.table.item(0, 2).setText("50")
    window.configure_connection()
    assert saved_services() == ["Geometria kół", "Wulkanizacja"]
    assert [window.table.item(r, 0).text() for r in range(window.table.rowCount())] == [
        "Geometria kół",
        "Wulkanizacja",
    ]
    assert [window.table.item(r, 2).text() for r in range(2)] == ["", ""]
    assert window.total_cents == 0


@pytest.mark.parametrize(
    ("stored", "expected"),
    [
        ("Tylko jedna", ["Tylko jedna"]),  # what Qt writes for a one-element list
        ("Geometria, Wulkanizacja", ["Geometria", "Wulkanizacja"]),
        ("@Invalid()", None),  # empty list
        ('"[\\"Z wersji\\", \\"0.1.0\\"]"', ["Z wersji", "0.1.0"]),  # v0.1.0 stored JSON
    ],
)
def test_saved_services_survive_a_cold_read_from_disk(tmp_path, monkeypatch, stored, expected):
    """Read straight from the ini text, past Qt's in-process cache, the way a restart does."""
    from fiscal_desktop import app as app_module

    isolate_settings(monkeypatch, tmp_path)
    (tmp_path / "settings.ini").write_text(f"[General]\nservices={stored}\n", encoding="utf-8")
    assert app_module.saved_services() == (expected or app_module.SERVICES)


@pytest.mark.parametrize("target", ["settings", "receipt"])
def test_invalid_name_edit_keeps_previous_value(editor, target):
    from fiscal_desktop.app import ConnectionDialog

    name = " \t "  # sanitization cannot turn a blank name into a printable item
    app, window, service, _ = editor
    dialog = ConnectionDialog(service.connection, window) if target == "settings" else None
    view = dialog.list if dialog else window.table
    if dialog:
        dialog.show()
    item = view.item(0) if dialog else view.item(0, 0)
    original = item.text()
    view.editItem(item)
    app.processEvents()
    field = QApplication.focusWidget()
    assert isinstance(field, QLineEdit)
    field.setText(name)
    QTest.keyClick(field, Qt.Key.Key_Return)
    app.processEvents()
    assert item.text() == original
    message = dialog.name_error if dialog else window.message_label
    assert message.isVisible() and message.text()
    if dialog:
        dialog.close()


def test_settings_cannot_accept_invalid_saved_name(editor):
    from fiscal_desktop.app import ConnectionDialog

    _, window, service, _ = editor
    dialog = ConnectionDialog(service.connection, window)
    dialog.show()
    dialog.findChild(QTabWidget).setCurrentIndex(1)
    dialog.list.item(0).setText(" \n")
    dialog.accept()
    assert dialog.result() != QDialog.DialogCode.Accepted
    assert dialog.name_error.isVisible()
    dialog.list.item(0).setText("Geometria ko\u0301ł")
    dialog.accept()
    assert dialog.result() == QDialog.DialogCode.Accepted
    assert dialog.services()[0] == "Geometria kół"


def test_invalid_unpriced_name_blocks_receipt(editor):
    _, window, _, _ = editor
    window.table.item(0, 2).setText("10")
    window.table.item(1, 0).setText(" \n")
    assert not window.print_button.isEnabled()
    with pytest.raises(ValueError):
        window.receipt_lines()


def test_name_edit_is_rejected_on_focus_loss(editor):
    app, window, _, _ = editor
    item = window.table.item(0, 0)
    original = item.text()
    window.table.editItem(item)
    app.processEvents()
    field = QApplication.focusWidget()
    assert isinstance(field, QLineEdit)
    field.setText(" \t ")
    window.search.setFocus()
    app.processEvents()
    assert item.text() == original
    assert window.message_label.text()


def test_monthly_report_defaults_to_previous_month(editor, monkeypatch):
    from PySide6.QtCore import QDate

    _, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    previous = QDate.currentDate().addMonths(-1)
    assert window.month_picker.currentData() == (previous.year(), previous.month())
    assert window.month_picker.currentText().endswith(str(previous.year()))
    window.monthly_button.click()
    wait_for(lambda: service.submitted is not None and not window.busy)
    assert service.submitted == ("monthly", previous.year(), previous.month())


def test_periodic_report_sends_dates_and_summary_flag(editor, monkeypatch):
    from datetime import date

    _, window, service, _ = editor
    monkeypatch.setattr(window, "_confirm", lambda *a: True)
    window.period_start.setDate(window.period_end.date().addDays(-3))
    window.period_summary.setChecked(True)
    window.periodic_button.click()
    wait_for(lambda: service.submitted is not None and not window.busy)
    kind, start, end, summary = service.submitted
    assert kind == "periodic"
    assert isinstance(start, date) and (end - start).days == 3
    assert summary is True
    wait_for(lambda: not window.busy and window.status is not None)
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
    wait_for(lambda: service.probes >= 2 and not window.busy)
    assert window.status is None
    assert "Brak drukarki" in window.message_label.text()
    service.offline = False
    wait_for(lambda: window.status is not None and not window.busy)
    assert window.vat_combo.count() == 1
    assert not window._reprobe.isActive()  # the printer is back: no further probes
    window.close()
    app.processEvents()


def test_history_lists_newest_first_and_marks_a_missing_number(editor):
    _, window, _, _ = editor
    window.tabs.setCurrentWidget(window.history_page)
    assert "Brak paragonów" in window.history_hint.text()

    records = [
        {
            "timestamp": "2026-09-13T08:00:00+02:00",
            "receipt_number": "6",
            "total_cents": 1000,
            "payment": 2,
            "lines": [{"name": "Olej"}],
        },
        {  # no receipt_number: scnt did not answer after the receipt printed
            "timestamp": "2026-09-14T09:12:00+02:00",
            "total_cents": 15000,
            "payment": 0,
            "lines": [{"name": "Klocki"}],
        },
    ]
    window.history_path.write_text(
        "".join(json.dumps(record) + "\n" for record in records), encoding="utf-8"
    )
    window.load_history()
    assert [window.history_table.item(row, 2).text() for row in range(2)] == ["—", "6"]
    assert window.history_table.item(0, 1).text() == "2026-09-14 09:12"
    assert window.history_table.item(1, 4).text() == "Karta płatnicza"


def test_corrupt_saved_baudrate_falls_back_to_the_documented_default(tmp_path, monkeypatch):
    from fiscal_desktop import app as app_module

    isolate_settings(monkeypatch, tmp_path)
    app_module.settings().setValue("connection/baudrate", "nonsense")
    assert app_module.saved_connection() == Connection("", 9600)
