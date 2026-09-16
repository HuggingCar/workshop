import json
import threading
import time

import pytest

from posnet.simulator import Simulator
from PySide6.QtCore import Qt
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QLineEdit, QSystemTrayIcon
from test_api import server, session
from workshop_agent.app import AgentWindow


@pytest.mark.parametrize("tray_available", [True, False])
def test_saved_setup_close_keeps_running_and_quit_finishes_receipt(
    tmp_path, monkeypatch, tray_available
):
    app = QApplication.instance() or QApplication([])
    monkeypatch.setattr(QSystemTrayIcon, "isSystemTrayAvailable", lambda: tray_available)
    printing = threading.Event()
    finish = threading.Event()
    job = {
        "pk": 7,
        "payment": 2,
        "payload": {"lines": [{"name": "Oil", "quantity": "1", "unit_price": "100.00"}]},
    }
    with (
        Simulator() as sim,
        server([session("one"), (200, job, {}), (200, {}, {})]) as (url, requests),
    ):
        handle = sim._handle

        def gate(command, params):
            if command == "trend":
                printing.set()
                assert finish.wait(5)
            return handle(command, params)

        monkeypatch.setattr(sim, "_handle", gate)
        window = AgentWindow(tmp_path)
        window.show()
        window.api_url.setText(url)
        window.token.setText("secret")
        window.port.setCurrentText(sim.connection.address)
        QTest.mouseClick(window.save_button, Qt.MouseButton.LeftButton)
        assert requests == []  # saving configuration never starts printing
        assert window.token.echoMode() == QLineEdit.EchoMode.Password
        window.close()
        window.deleteLater()
        window = AgentWindow(tmp_path)
        window.show()
        assert window.api_url.text() == url
        assert window.token.text() == "secret"
        assert window.port.currentText() == sim.connection.address
        QTest.mouseClick(window.start_button, Qt.MouseButton.LeftButton)
        try:
            deadline = time.monotonic() + 5
            while not printing.is_set() and time.monotonic() < deadline:
                app.processEvents()
                time.sleep(0.01)
            assert printing.is_set(), (window.status.text(), requests)
            window.close()
            app.processEvents()
            assert window.isVisible() is not tray_available
            assert not window.worker.isInterruptionRequested()
            if tray_available:
                window.tray.contextMenu().actions()[0].trigger()
                assert window.isVisible()
                window.close()
                window.tray.contextMenu().actions()[-1].trigger()
            else:
                QTest.mouseClick(window.quit_button, Qt.MouseButton.LeftButton)
            assert window.worker.isRunning()  # quit must finish and report the receipt first
            assert window.worker.isInterruptionRequested()
        finally:
            finish.set()
            window.stop()
            deadline = time.monotonic() + 5
            while window.worker is not None and time.monotonic() < deadline:
                app.processEvents()
                time.sleep(0.01)
        assert window.worker is None
        window.close()
        assert not window.isVisible()
        assert len(sim.receipts) == 1
        assert requests[-1][2]["status"] == 3
        assert requests[-1][2]["receipt_number"] == "1"
        assert (
            json.loads((tmp_path / "fiscal-operation.json").read_text())["state"] == "acknowledged"
        )
