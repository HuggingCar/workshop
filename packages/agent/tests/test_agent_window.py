import json
import threading
import time

from posnet.simulator import Simulator
from PySide6.QtCore import Qt
from PySide6.QtTest import QTest
from PySide6.QtWidgets import QApplication, QLineEdit
from test_api import server, session
from workshop_agent.app import AgentWindow


def test_saved_setup_and_close_waits_for_in_flight_receipt(tmp_path, monkeypatch):
    app = QApplication.instance() or QApplication([])
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
            assert window.isVisible()  # closing must not kill the thread mid-receipt
        finally:
            finish.set()
            window.stop()
            deadline = time.monotonic() + 5
            while window.worker is not None and time.monotonic() < deadline:
                app.processEvents()
                time.sleep(0.01)
        assert window.worker is None
        assert not window.isVisible()
        assert len(sim.receipts) == 1
        assert requests[-1][2]["status"] == 3
        assert requests[-1][2]["receipt_number"] == "1"
        assert (
            json.loads((tmp_path / "fiscal-operation.json").read_text())["state"] == "acknowledged"
        )
