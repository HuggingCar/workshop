import json
from decimal import Decimal, InvalidOperation
from pathlib import Path

from posnet.models import Line, money
from posnet.printer import Printer
from posnet.protocol import Connection
from PySide6.QtCore import QDate, QLocale, QSettings, Qt, QThread, QTimer
from PySide6.QtGui import QBrush, QColor, QIcon
from PySide6.QtWidgets import (
    QAbstractItemView,
    QCheckBox,
    QComboBox,
    QDateEdit,
    QDialog,
    QFormLayout,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QLayout,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QMainWindow,
    QMessageBox,
    QPushButton,
    QTableWidget,
    QTableWidgetItem,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)
from serial.tools import list_ports

PAYMENTS = {0: "Gotówka", 2: "Karta płatnicza"}
HISTORY_LIMIT = 500
RIGHT = Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter
REPROBE_MS = 10_000  # retry the printer this often while it is unavailable

SERVICES = [
    "Usługa własna",
    "Naprawa auta",
    "Naprawa silnika",
    "Naprawa elektryki",
    "Naprawa układu paliwowego",
    "Naprawa układu wydechowego",
    "Naprawa układu hamulcowego",
    "Naprawa układu chłodzenia",
    "Naprawa układu kierowniczego",
    "Naprawa układu klimatyzacji",
    "Serwis opon",
]


def settings():
    return QSettings("HuggingCar", "Fiscal")


def saved_connection():
    saved = settings()
    return Connection(
        address=saved.value("connection/address", ""),
        baudrate=int(saved.value("connection/baudrate", 9600)),
    )


def serial_ports():
    """Serial devices, Temo's USB (CDC-ACM) first so a scan finds it before probing anything else."""
    return sorted((port.device for port in list_ports.comports()), key=lambda d: "ttyACM" not in d)


class Operation(QThread):
    def __init__(self, printer, method, args, parent):
        super().__init__(parent)
        self.printer = printer
        self.method = method
        self.args = args
        self.result = None
        self.error = None
        self.pending = None

    def run(self):
        try:
            self.result = getattr(self.printer, self.method)(*self.args)
        except Exception as error:  # noqa: BLE001 - contain errors at the GUI worker boundary.
            self.error = str(error) or type(error).__name__
        try:
            self.pending = self.printer.pending()
        except Exception as error:  # noqa: BLE001 - unreadable journal must block fiscal operations.
            self.error = f"Nie można odczytać stanu ostatniej operacji: {error}"
            self.pending = {"operation": "unknown"}


def saved_services():
    # Stored as JSON: QSettings hands a one-element list back as a bare str on cold read.
    try:
        names = json.loads(settings().value("services", "null"))
    except TypeError, ValueError:
        names = None
    return names if isinstance(names, list) and names else SERVICES


class ConnectionDialog(QDialog):
    def __init__(self, connection, parent):
        super().__init__(parent)
        self.setWindowTitle("Ustawienia")
        self.setMinimumWidth(450)
        layout = QVBoxLayout(self)
        tabs = QTabWidget()
        tabs.setDocumentMode(True)
        layout.addWidget(tabs)

        services = QWidget()
        services_layout = QVBoxLayout(services)
        services_layout.setContentsMargins(0, 16, 0, 0)
        title_row = QHBoxLayout()
        title_row.addWidget(
            label("Dwuklik edytuje nazwę, przeciąganie zmienia kolejność.", "muted")
        )
        title_row.addStretch()
        for text, slot in (("Dodaj", self._add_service), ("Usuń", self._remove_service)):
            button = QPushButton(text)
            button.setObjectName("linkButton")
            button.setAutoDefault(False)
            button.clicked.connect(slot)
            title_row.addWidget(button)
        title_row.setSpacing(16)
        services_layout.addLayout(title_row)
        self.list = QListWidget()
        self.list.setSelectionMode(QAbstractItemView.SelectionMode.ExtendedSelection)
        self.list.setDragDropMode(QAbstractItemView.DragDropMode.InternalMove)
        for name in saved_services():
            item = QListWidgetItem(name)
            item.setFlags(item.flags() | Qt.ItemFlag.ItemIsEditable)
            self.list.addItem(item)
        services_layout.addWidget(self.list)
        tabs.addTab(services, "Usługi")

        printer = QWidget()
        printer_layout = QVBoxLayout(printer)
        printer_layout.setContentsMargins(0, 16, 0, 0)
        form = QFormLayout()
        form.setSpacing(16)
        self.address = QComboBox()
        self.address.setEditable(True)
        self.address.addItems([port.device for port in list_ports.comports()])
        self.address.setCurrentText(connection.address)
        self.address.setMinimumWidth(270)
        self.baudrate = QComboBox()
        self.baudrate.addItems(["9600", "19200", "38400", "57600", "115200"])
        self.baudrate.setCurrentText(str(connection.baudrate))
        form.addRow("Port drukarki", self.address)
        form.addRow("Prędkość transmisji", self.baudrate)
        printer_layout.addLayout(form)
        note = label(
            "Drukarka jest wykrywana automatycznie. Wskaż port ręcznie tylko, gdy wykrywanie zawodzi.",
            "muted",
        )
        note.setWordWrap(True)
        printer_layout.addWidget(note)
        printer_layout.addStretch()
        tabs.addTab(printer, "Drukarka")

        layout.addSpacing(8)
        layout.addLayout(dialog_footer(self, "Zapisz"))

    def accept(self):
        if not self.address.currentText().strip():
            QMessageBox.warning(self, "Brak portu", "Podaj port szeregowy drukarki.")
            return
        if not self.services():
            QMessageBox.warning(self, "Brak usług", "Dodaj przynajmniej jedną usługę.")
            return
        super().accept()

    def services(self):
        names = (self.list.item(i).text().strip() for i in range(self.list.count()))
        return list(dict.fromkeys(name for name in names if name))

    def _add_service(self):
        item = QListWidgetItem("Nowa usługa")
        item.setFlags(item.flags() | Qt.ItemFlag.ItemIsEditable)
        self.list.addItem(item)
        self.list.setCurrentItem(item)
        self.list.editItem(item)

    def _remove_service(self):
        for item in self.list.selectedItems():
            self.list.takeItem(self.list.row(item))

    def connection(self):
        return Connection(
            address=self.address.currentText().strip(),
            baudrate=int(self.baudrate.currentText()),
        )


def dialog_footer(dialog, accept_text):
    """antd Modal footer: Anuluj, then the primary action; Enter always means the action."""
    footer = QHBoxLayout()
    footer.addStretch()
    cancel = QPushButton("Anuluj")
    cancel.setAutoDefault(False)
    cancel.clicked.connect(dialog.reject)
    accept = QPushButton(accept_text)
    accept.setObjectName("okButton")
    accept.setDefault(True)
    accept.clicked.connect(dialog.accept)
    footer.addWidget(cancel)
    footer.addWidget(accept)
    return footer


def label(text, name):
    widget = QLabel(text)
    widget.setObjectName(name)
    return widget


def set_state(widget, state):
    """Switch a QSS `[state=…]` selector on an already-shown widget."""
    if widget.property("state") != state:
        widget.setProperty("state", state)
        widget.style().unpolish(widget)
        widget.style().polish(widget)


class MainWindow(QMainWindow):
    def __init__(self, printer: Printer):
        super().__init__()
        self.printer = printer
        self.history_path = Path(printer.journal_path).with_name("history.jsonl")
        self.busy = False
        self.status = None
        self.pending_operation = None
        self.total_cents = 0
        self.vat_rates = {}
        self._valid = True
        self._worker = None
        self._done = ""  # last success message, repeated after the follow-up status check
        self.setWindowTitle("HuggingCar Fiscal")
        self.resize(1180, 820)
        self.setMinimumSize(1040, 780)
        theme = Path(__file__).with_name("theme.qss")
        self.setStyleSheet(theme.read_text().replace("{dir}", theme.parent.as_posix()))
        self._reprobe = QTimer(self)
        self._reprobe.setInterval(REPROBE_MS)
        self._reprobe.timeout.connect(self._reprobe_tick)
        self._build_ui()
        self._update_totals()
        QTimer.singleShot(0, self.refresh_status)

    @staticmethod
    def _card(title=None):
        card = QFrame()
        card.setObjectName("card")
        layout = QVBoxLayout(card)
        layout.setContentsMargins(24, 20, 24, 24)
        layout.setSpacing(16)
        if title:
            layout.addWidget(label(title, "cardTitle"))
        return card, layout

    @staticmethod
    def _page():
        page = QWidget()
        page.setObjectName("page")
        layout = QHBoxLayout(page)
        layout.setContentsMargins(0, 16, 0, 0)
        layout.setSpacing(16)
        return page, layout

    @staticmethod
    def _table(headers, widths, numeric=()):
        table = QTableWidget(0, len(headers))
        table.setHorizontalHeaderLabels(headers)
        table.horizontalHeader().setDefaultAlignment(
            Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter
        )
        for col in numeric:
            table.horizontalHeaderItem(col).setTextAlignment(RIGHT)
        table.verticalHeader().hide()
        table.verticalHeader().setDefaultSectionSize(40)
        table.horizontalHeader().setSectionResizeMode(0, QHeaderView.ResizeMode.Stretch)
        for col, width in widths.items():
            table.horizontalHeader().setSectionResizeMode(col, QHeaderView.ResizeMode.Fixed)
            table.setColumnWidth(col, width)
        table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        table.setShowGrid(False)
        return table

    def _build_ui(self):
        root = QWidget()
        root.setObjectName("root")
        self.setCentralWidget(root)
        layout = QVBoxLayout(root)
        layout.setContentsMargins(24, 20, 24, 16)
        layout.setSpacing(8)
        self.tabs = QTabWidget()
        self.tabs.setDocumentMode(True)
        self.tabs.addTab(self._build_sales(), "Sprzedaż")
        self.tabs.addTab(self._build_reports(), "Raporty")
        self.history_page = self._build_history()
        self.tabs.addTab(self.history_page, "Historia")
        self.tabs.currentChanged.connect(self._tab_changed)
        # Settings in the tab bar's right corner
        corner = QWidget()
        actions = QHBoxLayout(corner)
        actions.setContentsMargins(0, 0, 0, 4)
        actions.setSpacing(8)
        self.settings_button = QPushButton()
        self.settings_button.setObjectName("iconButton")
        self.settings_button.setToolTip("Ustawienia")
        self.settings_button.setIcon(QIcon(str(Path(__file__).with_name("settings.svg"))))
        self.settings_button.clicked.connect(self.configure_connection)
        actions.addWidget(self.settings_button)
        self.tabs.setCornerWidget(corner, Qt.Corner.TopRightCorner)
        layout.addWidget(self.tabs, 1)
        # Footer: last message and recovery
        footer = QHBoxLayout()
        self.message_label = label("Sprawdzanie drukarki…", "message")
        self.message_label.setWordWrap(True)
        footer.addWidget(self.message_label, 1)
        self.recovery_button = QPushButton("Wyjaśnij ostatnią operację")
        self.recovery_button.clicked.connect(self.resolve_pending)
        self.recovery_button.hide()
        footer.addWidget(self.recovery_button)
        layout.addLayout(footer)

    def _build_sales(self):
        page, body = self._page()
        services, service_layout = self._card()
        title_row = QHBoxLayout()
        title_row.addWidget(label("Pozycje paragonu", "cardTitle"))
        title_row.addStretch()
        self.clear_button = QPushButton("Wyczyść ceny")
        self.clear_button.setObjectName("linkButton")
        self.clear_button.clicked.connect(self.clear_prices)
        title_row.addWidget(self.clear_button)
        service_layout.addLayout(title_row)
        toolbar = QHBoxLayout()
        self.validation_label = label(
            "Do paragonu trafią tylko usługi z ceną większą od zera.", "muted"
        )
        self.validation_label.setWordWrap(True)
        toolbar.addWidget(self.validation_label, 1)
        self.search = QLineEdit()
        self.search.setPlaceholderText("Szukaj usługi")
        self.search.setClearButtonEnabled(True)
        self.search.setFixedWidth(240)
        self.search.textChanged.connect(self._filter_rows)
        toolbar.addWidget(self.search)
        service_layout.addLayout(toolbar)
        self.table = self._table(
            ["Nazwa usługi", "Ilość", "Cena brutto", "Wartość"], {1: 80, 2: 120, 3: 120}, (1, 2, 3)
        )
        self.table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectItems)
        self.table.itemChanged.connect(self._update_totals)
        self._fill_services(saved_services())
        service_layout.addWidget(self.table, 1)
        body.addWidget(services, 1)
        summary, summary_layout = self._card("Podsumowanie")
        summary.setFixedWidth(320)
        summary_layout.addWidget(label("Do zapłaty brutto", "secondary"))
        self.total_label = label("0,00 zł", "statistic")
        summary_layout.addWidget(self.total_label)
        self.breakdown_label = label("", "muted")
        summary_layout.addWidget(self.breakdown_label)
        self.count_label = label("Pozycji na paragonie: 0", "muted")
        summary_layout.addWidget(self.count_label)
        summary_layout.addSpacing(8)
        summary_layout.addWidget(QLabel("Stawka VAT"))
        self.vat_combo = QComboBox()
        self.vat_combo.currentIndexChanged.connect(self._update_totals)
        summary_layout.addWidget(self.vat_combo)
        # Rates come from the printer; until it answers there is nothing to select.
        self.vat_note = label("Oczekiwanie na drukarkę", "muted")
        summary_layout.addWidget(self.vat_note)
        summary_layout.addWidget(QLabel("Forma płatności"))
        self.payment_combo = QComboBox()
        for value, name in PAYMENTS.items():
            self.payment_combo.addItem(name, value)
        summary_layout.addWidget(self.payment_combo)
        summary_layout.addStretch()
        self.print_button = QPushButton("Drukuj paragon")
        self.print_button.setObjectName("primaryButton")
        self.print_button.clicked.connect(self.submit_receipt)
        summary_layout.addWidget(self.print_button)
        body.addWidget(summary)
        return page

    def _report_row(self, layout, title, description, *controls):
        """antd List item: title + description on the left, controls on the right."""
        row = QHBoxLayout()
        row.setSpacing(12)
        text = QVBoxLayout()
        text.setSpacing(2)
        text.addWidget(QLabel(title))
        hint = label(description, "muted")
        hint.setWordWrap(True)
        text.addWidget(hint)
        row.addLayout(text, 1)
        for control in controls:
            row.addWidget(control)
        layout.addLayout(row)

    @staticmethod
    def _divider(layout):
        line = QFrame()
        line.setObjectName("divider")
        line.setFixedHeight(1)
        layout.addWidget(line)

    def _build_reports(self):
        page, body = self._page()
        today = QDate.currentDate()
        card, layout = self._card("Raporty fiskalne")
        card.setMaximumWidth(900)
        self.daily_button = QPushButton("Drukuj raport dobowy")
        self.daily_button.clicked.connect(self.submit_daily_report)
        self._report_row(
            layout,
            "Raport dobowy",
            "Zamyka sprzedaż bieżącego dnia w pamięci fiskalnej. Wykonuj po zakończeniu sprzedaży.",
            self.daily_button,
        )
        self._divider(layout)
        self.month_picker = QComboBox()
        self.month_picker.setMinimumWidth(180)
        locale = QLocale(QLocale.Language.Polish)
        for back in range(1, 25):  # previous month first: the one a report is normally due for
            month = today.addMonths(-back)
            self.month_picker.addItem(
                f"{locale.standaloneMonthName(month.month())} {month.year()}",
                (month.year(), month.month()),
            )
        self.monthly_button = QPushButton("Drukuj raport miesięczny")
        self.monthly_button.clicked.connect(self.submit_monthly_report)
        self._report_row(
            layout,
            "Raport miesięczny",
            "Łączny raport fiskalny za wybrany miesiąc.",
            self.month_picker,
            self.monthly_button,
        )
        self._divider(layout)
        self.period_start = QDateEdit(today.addDays(-7))
        self.period_end = QDateEdit(today)
        for picker in (self.period_start, self.period_end):
            picker.setDisplayFormat("dd.MM.yyyy")
            picker.setCalendarPopup(True)
            picker.setMaximumDate(today)
            picker.setFixedWidth(140)
        self.period_summary = QCheckBox("Skrócony")
        self.periodic_button = QPushButton("Drukuj raport okresowy")
        self.periodic_button.clicked.connect(self.submit_periodic_report)
        self._report_row(
            layout,
            "Raport okresowy",
            "Za dowolny zakres dat: szczegółowy lub skrócony (łączny).",
            self.period_start,
            label("–", "secondary"),
            self.period_end,
            self.period_summary,
            self.periodic_button,
        )
        body.addWidget(card, 1, Qt.AlignmentFlag.AlignTop)
        return page

    def _build_history(self):
        page, body = self._page()
        card, layout = self._card()
        title_row = QHBoxLayout()
        title_row.addWidget(label("Wydrukowane paragony", "cardTitle"))
        title_row.addStretch()
        self.history_hint = label("Lista z tego komputera. Numer nadaje drukarka.", "muted")
        title_row.addWidget(self.history_hint)
        layout.addLayout(title_row)
        self.history_table = self._table(
            ["Pozycje", "Data", "Nr paragonu", "Kwota", "Płatność"],
            {1: 150, 2: 110, 3: 120, 4: 130},
            (3,),
        )
        self.history_table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
        self.history_table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        layout.addWidget(self.history_table, 1)
        body.addWidget(card, 1)
        return page

    def _tab_changed(self, index):
        if self.tabs.widget(index) is self.history_page:
            self.load_history()

    def load_history(self):
        try:
            rows = [json.loads(line) for line in self.history_path.read_text().splitlines() if line]
        except OSError, ValueError:
            rows = []
        rows = rows[-HISTORY_LIMIT:][::-1]
        self.history_table.setRowCount(len(rows))
        for row, record in enumerate(rows):
            when = record.get("timestamp", "")[:16].replace("T", " ")
            values = [
                ", ".join(line["name"] for line in record.get("lines", [])),
                when,
                record.get("receipt_number") or "—",
                money(record.get("total_cents", 0)),
                PAYMENTS.get(record.get("payment"), "—"),
            ]
            for col, value in enumerate(values):
                item = QTableWidgetItem(value)
                if col == 3:
                    item.setTextAlignment(RIGHT)
                self.history_table.setItem(row, col, item)
        self.history_hint.setText(
            "Brak paragonów wydrukowanych z tego komputera."
            if not rows
            else "Lista z tego komputera. Numer nadaje drukarka."
        )

    def _append_history(self, record):
        try:
            self.history_path.parent.mkdir(parents=True, exist_ok=True)
            with self.history_path.open("a") as file:
                file.write(json.dumps(record, ensure_ascii=False) + "\n")
        except OSError, TypeError:
            pass  # convenience log only; the fiscal journal is the source of truth

    def _row_line(self, row, vat):
        raw_price = self.table.item(row, 2).text().strip()
        if not raw_price:
            return None
        try:
            price = Decimal(raw_price.replace(",", "."))
            if not price.is_finite() or price < 0:
                raise ValueError("Cena musi być nieujemną liczbą.")
            if price == 0:
                return None
            quantity = Decimal(self.table.item(row, 1).text().strip().replace(",", "."))
            if not quantity.is_finite() or quantity <= 0:
                raise ValueError("Ilość musi być większa od zera.")
            return Line(self.table.item(row, 0).text().strip(), quantity, price, vat)
        except (InvalidOperation, ValueError) as error:
            detail = "" if isinstance(error, InvalidOperation) else f" {error}"
            raise ValueError(f"Wiersz {row + 1}: sprawdź nazwę, ilość i cenę.{detail}") from error

    def receipt_lines(self):
        vat = self.vat_combo.currentData()
        if vat is None:
            raise ValueError("Najpierw połącz drukarkę i wybierz aktywną stawkę VAT.")
        return [line for row in range(self.table.rowCount()) if (line := self._row_line(row, vat))]

    def _update_totals(self, *_):
        total, count, first_error = 0, 0, ""
        self.table.blockSignals(True)
        for row in range(self.table.rowCount()):
            try:
                line = self._row_line(row, self.vat_combo.currentData() or 0)
                cents = line.total_cents if line else 0
                total += cents
                count += bool(line)
                self.table.item(row, 3).setText(money(cents))
                self.table.item(row, 3).setForeground(
                    QBrush() if line else QColor(0, 0, 0, 64)  # default / colorTextQuaternary
                )
            except ValueError as error:
                first_error = first_error or str(error)
                self.table.item(row, 3).setText("Sprawdź")
                self.table.item(row, 3).setForeground(QColor("#ff4d4f"))  # colorError
        self.table.blockSignals(False)
        self.total_cents = total
        self._valid = not first_error
        self.total_label.setText(money(total))
        rate = self.vat_rates.get(self.vat_combo.currentData())
        tax = rate.tax_cents(total) if rate else 0
        self.breakdown_label.setText(
            f"Netto {money(total - tax)}  ·  VAT {rate.rate_label} {money(tax)}" if rate else ""
        )
        self.count_label.setText(f"Pozycji na paragonie: {count}")
        self.validation_label.setText(
            first_error or "Do paragonu trafią tylko usługi z ceną większą od zera."
        )
        set_state(self.validation_label, "error" if first_error else "")
        self._update_controls()

    def _update_controls(self):
        editable = not self.busy and not self.pending_operation
        ready = bool(self.status and self.status.ready)
        for widget in [
            self.table,
            self.clear_button,
            self.vat_combo,
            self.payment_combo,
            self.month_picker,
            self.period_start,
            self.period_end,
            self.period_summary,
        ]:
            widget.setEnabled(editable)
        self.vat_combo.setVisible(self.vat_combo.count() > 0)
        self.vat_note.setVisible(self.vat_combo.count() == 0)
        self.settings_button.setEnabled(not self.busy)
        self.print_button.setEnabled(
            editable
            and ready
            and self._valid
            and self.total_cents > 0
            and self.vat_combo.currentData() is not None
        )
        for button in (self.daily_button, self.monthly_button, self.periodic_button):
            button.setEnabled(editable and ready)
        self.recovery_button.setVisible(bool(self.pending_operation))
        self.recovery_button.setEnabled(not self.busy)
        # Printer off at launch or unplugged: keep probing until it answers.
        if ready or self.busy or self.pending_operation:
            self._reprobe.stop()
        elif not self._reprobe.isActive():
            self._reprobe.start()

    def _say(self, text, state=""):
        self.message_label.setText(text)
        set_state(self.message_label, state)

    def _start(self, method, *args):
        if self.busy:
            return
        self.busy = True
        self._say("Trwa komunikacja z drukarką. Poczekaj na wynik operacji.")
        self._update_controls()
        self._worker = Operation(self.printer, method, args, self)
        self._worker.finished.connect(self._operation_done)
        self._worker.start()

    def _operation_done(self):
        worker = self._worker
        self._worker = None
        self.busy = False
        self.pending_operation = worker.pending
        if worker.error:
            self._done = ""
            self.status = None
            hint = ""
            if worker.method == "detect":
                hint = " Podłącz drukarkę lub wskaż port w Ustawieniach."
                self.vat_note.setText("Brak połączenia z drukarką")
            self._say(f"Błąd: {worker.error}{hint}", "error")
        elif worker.method in ("probe", "detect"):
            if worker.method == "detect":
                self._remember_connection()
            self.status = worker.result
            selected = self.vat_combo.currentData()
            self.vat_combo.blockSignals(True)
            self.vat_combo.clear()
            self.vat_rates = {rate.index: rate for rate in self.status.vat_rates if rate.active}
            for rate in self.vat_rates.values():
                self.vat_combo.addItem(rate.label, rate.index)
            index = self.vat_combo.findData(selected)
            self.vat_combo.setCurrentIndex(max(index, 0))
            self.vat_combo.blockSignals(False)
            self._say(
                f"{self._done}{self.status.name} · {self.status.unique_number} · {self.status.description}",
                "success" if self._done else "",
            )
        else:
            if worker.method == "print_receipt":
                self._append_history(worker.result)
                self.clear_prices()
                number = (worker.result or {}).get("receipt_number")
                message = f"Paragon nr {number} wydrukowany." if number else "Paragon wydrukowany."
            elif worker.method == "acknowledge_pending":
                message = (
                    "Zapis poprzedniej operacji potwierdzony. Sprawdź połączenie przed kontynuacją."
                )
            else:
                message = "Raport wydrukowany."
            self.status = None
            self._say(message, "success")
            self._done = message + " "
        if self.pending_operation:
            self._say(
                "Wynik ostatniej operacji jest niepotwierdzony. Sprawdź wydruk i stan drukarki, "
                "a następnie wybierz „Wyjaśnij ostatnią operację”.",
                "error",
            )
        self._update_totals()
        worker.deleteLater()
        if (
            not worker.error
            and worker.method not in ("probe", "detect")
            and not self.pending_operation
        ):
            self._start("probe")

    def _reprobe_tick(self):
        """Retry the saved port only; rescanning every port each tick would hammer the machine."""
        self.refresh_status(scan=not self.printer.connection.address)

    def refresh_status(self, scan=True):
        """Check the printer; `scan` also tries other ports if the saved one does not answer."""
        self._done = ""
        if scan:
            self._start("detect", serial_ports())
        else:
            self._start("probe")

    def _fill_services(self, names):
        """Rebuild the receipt table; edits in progress are discarded."""
        self.services = list(names)
        self.table.blockSignals(True)
        self.table.setRowCount(0)
        self.table.setRowCount(len(self.services))
        for row, service in enumerate(self.services):
            for col, value in enumerate([service, "1", "", "0,00 zł"]):
                item = QTableWidgetItem(value)
                if col:
                    item.setTextAlignment(RIGHT)
                if col == 3:
                    item.setFlags(item.flags() & ~Qt.ItemFlag.ItemIsEditable)
                self.table.setItem(row, col, item)
        self.table.blockSignals(False)
        self._filter_rows(self.search.text())

    def _filter_rows(self, text):
        """Hide rows whose name does not contain the search text; hidden rows stay on the receipt."""
        needle = text.strip().casefold()
        for row in range(self.table.rowCount()):
            self.table.setRowHidden(row, needle not in self.table.item(row, 0).text().casefold())

    def clear_prices(self):
        self.table.blockSignals(True)
        for row in range(self.table.rowCount()):
            self.table.item(row, 2).setText("")
        self.table.blockSignals(False)
        self._update_totals()

    def _confirm(self, title, text, action="Drukuj"):
        """antd Modal.confirm: bold title, body, Anuluj + primary action."""
        dialog = QDialog(self)
        dialog.setWindowTitle(title)
        dialog.setFixedWidth(416)  # antd confirm width
        layout = QVBoxLayout(dialog)
        layout.setContentsMargins(24, 20, 24, 20)
        layout.setSpacing(8)
        layout.addWidget(label(title, "cardTitle"))
        body = QLabel(text)
        body.setWordWrap(True)
        body.setFixedWidth(416 - 48)
        layout.setSizeConstraint(QLayout.SizeConstraint.SetMinimumSize)
        layout.addWidget(body)
        layout.addSpacing(16)
        layout.addLayout(dialog_footer(dialog, action))
        return dialog.exec() == QDialog.DialogCode.Accepted

    def submit_receipt(self):
        try:
            lines = self.receipt_lines()
        except ValueError as error:
            self._say(str(error), "error")
            return
        warning = "Zatwierdzenie wystawi paragon fiskalny. Sprawdź dane przed drukowaniem."
        if self._confirm(
            "Potwierdź paragon",
            (
                f"Pozycji: {len(lines)}\nDo zapłaty: {money(self.total_cents)}\n"
                f"Płatność: {self.payment_combo.currentText()}\n\n{warning}"
            ),
        ):
            self._start("print_receipt", lines, self.payment_combo.currentData())

    def submit_daily_report(self):
        text = "Raport dobowy zamknie bieżącą sprzedaż dnia w pamięci fiskalnej. Wydrukować raport?"
        if self._confirm("Raport dobowy", text):
            self._start("daily_report")

    def submit_monthly_report(self):
        year, month = self.month_picker.currentData()
        if self._confirm(
            "Raport miesięczny",
            f"Wydrukować raport miesięczny za {self.month_picker.currentText()}?",
        ):
            self._start("monthly_report", year, month)

    def submit_periodic_report(self):
        start, end = self.period_start.date(), self.period_end.date()
        if start > end:
            self._say("Data początkowa nie może być późniejsza niż końcowa.", "error")
            return
        kind = "skrócony" if self.period_summary.isChecked() else "szczegółowy"
        if self._confirm(
            "Raport okresowy",
            f"Wydrukować raport {kind} za okres "
            f"{start.toString('dd.MM.yyyy')} – {end.toString('dd.MM.yyyy')}?",
        ):
            self._start(
                "periodic_report",
                start.toPython(),
                end.toPython(),
                self.period_summary.isChecked(),
            )

    def resolve_pending(self):
        if self.busy or not self.pending_operation:
            return
        if self._confirm(
            "Potwierdź sprawdzenie drukarki",
            (
                "Sprawdź fizyczny wydruk i stan transakcji na drukarce. Operacja mogła się zakończyć "
                "mimo braku odpowiedzi. Nie powtarzaj jej bez sprawdzenia.\n\n"
                "Czy ustalono wynik operacji i można usunąć lokalną blokadę? "
                "To potwierdzenie nie anuluje transakcji ani nie ponawia wydruku."
            ),
            "Usuń blokadę",
        ):
            self._start("acknowledge_pending")

    def configure_connection(self):
        if self.busy:
            return
        dialog = ConnectionDialog(self.printer.connection, self)
        if dialog.exec() != QDialog.DialogCode.Accepted:
            return
        connection = dialog.connection()
        services = dialog.services()
        settings().setValue("services", json.dumps(services, ensure_ascii=False))
        if services != self.services:
            self._fill_services(services)
            self._update_totals()
        if connection != self.printer.connection:
            self.printer = Printer(connection, self.printer.journal_path)
            self._remember_connection()
            self.status = None
            self.refresh_status(scan=False)

    def _remember_connection(self):
        saved = settings()
        for field in ("address", "baudrate"):
            saved.setValue(f"connection/{field}", getattr(self.printer.connection, field))

    def closeEvent(self, event):
        if self.busy:
            event.ignore()
            self._say("Poczekaj na zakończenie operacji przed zamknięciem aplikacji.", "error")
        else:
            event.accept()
