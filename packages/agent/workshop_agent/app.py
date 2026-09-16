"""Small setup window; printing stays in the existing agent loop."""

import logging
import sys
from pathlib import Path

import fiscal_desktop
from fiscal_desktop.app import label, set_state
from posnet.printer import Printer
from posnet.protocol import Connection
from PySide6.QtCore import QThread, Signal
from PySide6.QtWidgets import (
    QApplication,
    QComboBox,
    QFormLayout,
    QFrame,
    QHBoxLayout,
    QLineEdit,
    QPushButton,
    QVBoxLayout,
    QWidget,
)
from serial.tools import list_ports

from .agent import Agent, Api, load_config, log, save_config, validated_url


class StatusLog(logging.Handler):
    def __init__(self, status):
        super().__init__()
        self.status = status

    def emit(self, record):
        self.status.emit(self.format(record), "error" if record.levelno >= logging.WARNING else "")


class Worker(QThread):
    status = Signal(str, str)

    def __init__(self, config, data, parent):
        super().__init__(parent)
        self.config = config
        self.data = data
        self.agent = None

    def stop(self):
        self.requestInterruption()
        if self.agent is not None:
            self.agent.stopping = True

    def run(self):
        handler = StatusLog(self.status)
        log.addHandler(handler)
        try:
            printer = Printer(
                Connection(self.config["serial"], self.config["baudrate"]),
                self.data / "fiscal-operation.json",
            )
            device = printer.probe()
            api = Api(self.config["api_url"], self.config["token"], device.unique_number)
            api.describe(device)
            self.agent = Agent(printer, api)
            if self.isInterruptionRequested():
                return
            api.connect()
            self.status.emit(
                f"Połączono: {device.unique_number}. Oczekiwanie na zlecenia.", "success"
            )
            if not self.isInterruptionRequested():
                self.agent.run_forever()
        except Exception as exc:  # noqa: BLE001 — contain errors at the GUI worker boundary
            self.status.emit(f"Błąd: {exc}", "error")
        finally:
            log.removeHandler(handler)

            if self.isInterruptionRequested():
                self.status.emit("Zatrzymano.", "")


class AgentWindow(QWidget):
    def __init__(self, data: Path):
        super().__init__()
        self.data = data
        self.worker = None
        self.closing = False
        self.setWindowTitle("HuggingCar Agent")
        self.setMinimumWidth(620)
        self.setObjectName("root")
        theme = Path(fiscal_desktop.__file__).with_name("theme.qss")
        self.setStyleSheet(
            theme.read_text(encoding="utf-8").replace("{dir}", theme.parent.as_posix())
        )
        config = load_config(data / "fiscal.json")
        root = QVBoxLayout(self)
        root.setContentsMargins(24, 20, 24, 16)
        root.setSpacing(8)
        card = QFrame()
        card.setObjectName("card")
        root.addWidget(card)
        layout = QVBoxLayout(card)
        layout.setContentsMargins(24, 20, 24, 24)
        layout.setSpacing(16)
        layout.addWidget(label("HuggingCar Agent", "cardTitle"))
        layout.addWidget(label("Połącz drukarkę fiskalną z HuggingCar.", "secondary"))
        self._build_form(config)
        layout.addWidget(self.form)
        buttons = QHBoxLayout()
        self.save_button = QPushButton("Zapisz")
        self.start_button = QPushButton("Uruchom")
        self.start_button.setObjectName("primaryButton")
        self.stop_button = QPushButton("Zatrzymaj")
        self.stop_button.setEnabled(False)
        self.save_button.clicked.connect(self.save)
        self.start_button.clicked.connect(self.start)
        self.stop_button.clicked.connect(self.stop)
        for button in (self.save_button, self.start_button, self.stop_button):
            buttons.addWidget(button)
        layout.addLayout(buttons)
        self.status = label("Zatrzymano. Uzupełnij ustawienia i kliknij Uruchom.", "message")
        self.status.setWordWrap(True)
        layout.addWidget(self.status)
        note = label(
            "Uruchom pobiera i drukuje oczekujące paragony. Pozostaw okno otwarte.", "muted"
        )
        note.setWordWrap(True)
        root.addWidget(note)

    def _build_form(self, config):
        self.form = QWidget()
        form = QFormLayout(self.form)
        form.setContentsMargins(0, 0, 0, 0)
        form.setSpacing(12)
        self.api_url = QLineEdit(config.get("api_url", ""))
        self.api_url.setPlaceholderText("https://api-manager.example.com")
        self.token = QLineEdit(config.get("token", ""))
        self.token.setEchoMode(QLineEdit.EchoMode.Password)
        self.token.setPlaceholderText("Token z ustawień drukarki w HuggingCar")
        self.port = QComboBox()
        self.port.setEditable(True)
        self.port.addItems(sorted(port.device for port in list_ports.comports()))
        self.port.setCurrentText(config.get("serial", self.port.currentText()))
        self.port.lineEdit().setPlaceholderText("COM3 lub /dev/ttyACM0")
        self.baudrate = QComboBox()
        self.baudrate.addItems(["9600", "19200", "38400", "57600", "115200"])
        self.baudrate.setCurrentText(str(config.get("baudrate", 9600)))
        form.addRow("Adres API", self.api_url)
        form.addRow("Token", self.token)
        form.addRow("Port drukarki", self.port)
        form.addRow("Prędkość transmisji", self.baudrate)

    def show_status(self, text, state=""):
        self.status.setText(text)
        set_state(self.status, state)

    def save(self):
        token = self.token.text().strip()
        if not token or not token.isascii() or any(c.isspace() for c in token):
            self.show_status("Wklej token drukarki z HuggingCar.", "error")
            return None
        try:
            config = {
                "api_url": validated_url(self.api_url.text().strip()),
                "token": token,
                "serial": self.port.currentText().strip(),
                "baudrate": int(self.baudrate.currentText()),
            }
            save_config(self.data / "fiscal.json", config)
        except (OSError, ValueError) as exc:
            self.show_status(f"Błąd: {exc}", "error")
            return None
        self.show_status("Zapisano ustawienia.", "success")
        return config

    def start(self):
        config = self.save()
        if config is None:
            return
        if not config["serial"]:
            self.show_status("Wybierz lub wpisz port drukarki.", "error")
            return
        self.form.setEnabled(False)
        self.save_button.setEnabled(False)
        self.start_button.setEnabled(False)
        self.stop_button.setEnabled(True)
        self.show_status("Łączenie z drukarką i API…")
        self.worker = Worker(config, self.data, self)
        self.worker.status.connect(self.show_status)
        self.worker.finished.connect(self.finished)
        self.worker.start()

    def stop(self):
        self.stop_button.setEnabled(False)
        self.show_status("Zatrzymywanie po zakończeniu bieżącej operacji…")
        if self.worker is not None:
            self.worker.stop()

    def finished(self):
        self.form.setEnabled(True)
        self.save_button.setEnabled(True)
        self.start_button.setEnabled(True)
        self.stop_button.setEnabled(False)
        self.worker.deleteLater()
        self.worker = None
        if self.closing:
            self.close()

    def closeEvent(self, event):  # noqa: N802 — Qt API
        if self.worker is not None:
            self.closing = True
            self.stop()
            event.ignore()
        else:
            event.accept()


def run(data: Path) -> int:
    app = QApplication(sys.argv[:1])
    app.setStyle("Fusion")
    app.setApplicationName("HuggingCar Agent")
    app.setOrganizationName("HuggingCar")
    log.setLevel(logging.INFO)
    window = AgentWindow(data)
    window.show()
    return app.exec()
