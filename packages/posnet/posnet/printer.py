import fcntl
import json
import os
import re
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import date, datetime
from decimal import Decimal, InvalidOperation
from pathlib import Path

from .models import MAX_CENTS, Line, VatRate, printable
from .protocol import Connection, ProtocolError, Session


def header_text(raw: str) -> str:
    """Plain header text: `&&` is a literal `&`, any other `&x` is a formatting mark."""
    return re.sub(r"&(.)", lambda m: "&" if m.group(1) == "&" else "", raw)


@dataclass(frozen=True)
class Status:
    name: str
    version: str
    unique_number: str
    ready: bool
    description: str
    vat_rates: list[VatRate]


def boolean(value: str) -> bool:
    if value.lower() not in ("0", "1", "y", "n", "t", "f"):
        raise ProtocolError("Nieprawidłowy status logiczny drukarki.")
    return value.lower() in ("1", "y", "t")


SCAN_TIMEOUT = 0.5  # a Temo answers getrealid well within this; silent ports cost no more


class Printer:
    def __init__(self, connection: Connection, journal_path: str | Path):
        self.connection = connection
        self.journal_path = Path(journal_path)
        self.journal_path.parent.mkdir(parents=True, exist_ok=True)
        self.last_status = None

    @contextmanager
    def _locked(self):
        with self.journal_path.with_suffix(".lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise ValueError("Inna operacja drukarki jest już wykonywana.") from exc
            yield

    def _save(self, record):
        # Persist intent before touching the printer, including across power loss.
        fd, path = tempfile.mkstemp(dir=self.journal_path.parent, prefix=".operation-")
        try:
            with os.fdopen(fd, "w") as file:
                json.dump(record, file, ensure_ascii=False, indent=2)
                file.flush()
                os.fsync(file.fileno())
            os.replace(path, self.journal_path)
            directory = os.open(self.journal_path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if os.path.exists(path):
                os.unlink(path)

    def last_record(self) -> dict | None:
        """Whatever the journal holds, regardless of state; None when empty or unreadable."""
        try:
            record = json.loads(self.journal_path.read_text())
        except OSError, ValueError:
            return None
        return record if isinstance(record, dict) else None

    def pending(self) -> dict | None:
        try:
            record = json.loads(self.journal_path.read_text())
        except FileNotFoundError:
            return None
        except (OSError, ValueError) as exc:
            raise ValueError(
                "Nie można odczytać rejestru operacji. Drukowanie zablokowane."
            ) from exc
        if not isinstance(record, dict) or record.get("state") not in (
            "pending",
            "completed",
            "acknowledged",
        ):
            raise ValueError("Uszkodzony rejestr operacji. Drukowanie zablokowane.")
        return record if record["state"] == "pending" else None

    def _require_resolved(self):
        if self.pending():
            raise ValueError(
                "Poprzednia operacja jest nierozstrzygnięta. Sprawdź drukarkę i rozstrzygnij ją przed kolejnym wydrukiem."
            )

    def _probe(self, session: Session) -> Status:
        identity = session.command("getrealid")
        device = session.command("sdev")
        mechanism = session.command("sprn")
        common = session.command("scomm")
        transaction = session.command("strns")
        vats = session.command("vatget")
        try:
            name = identity["nm"]
            version = identity["vr"]
            number = identity["nu"]
            fiscal = boolean(common["fs"])
            transaction_open = transaction["to"] != "0" or boolean(transaction["fe"])
            queued = not boolean(device["qe"])
            rates = [
                VatRate(i, Decimal(vats["v" + chr(97 + i)].replace(",", "."))) for i in range(7)
            ]
            if any(not rate.percent.is_finite() for rate in rates):
                raise ValueError
            problems = []
            if name != "POSNET TEMO ONLINE" or not number.strip():
                problems.append("Wybrane urządzenie nie jest drukarką Temo Online")
            if not fiscal:
                problems.append("Drukarka nie jest w trybie fiskalnym")
            if not boolean(common["hr"]):
                problems.append("Drukarka nie ma zaprogramowanego nagłówka")
            if device["ds"] != "0":
                problems.append("Drukarka oczekuje na operatora lub jest w menu")
            if queued:
                problems.append("Drukarka ma oczekujące polecenia")
            errors = {
                "1": "Podniesiona dźwignia",
                "3": "Otwarta pokrywa",
                "5": "Brak papieru",
                "6": "Nieprawidłowa temperatura lub zasilanie",
            }
            if mechanism["pr"] != "0":
                problems.append(errors.get(mechanism["pr"], f"Błąd mechanizmu: {mechanism['pr']}"))
            if transaction_open or common["ts"] != "0":
                problems.append("Otwarta transakcja. Dokończ lub anuluj ją na drukarce")
            if not any(rate.active for rate in rates):
                problems.append("Brak aktywnych stawek VAT")
        except (KeyError, ValueError, InvalidOperation) as exc:
            raise ProtocolError("Niepełna lub nieprawidłowa odpowiedź statusu drukarki.") from exc
        return Status(
            name,
            version,
            number,
            not problems,
            ". ".join(problems) or "Gotowa do drukowania",
            rates,
        )

    def probe(self) -> Status:
        with self._locked(), Session(self.connection) as session:
            status = self._probe(session)
        self.last_status = status
        return status

    def detect(self, candidates: list[str]) -> Status:
        """Probe the saved port; if it does not answer, try each candidate and adopt the first.

        Only `getrealid` is sent to strangers, and a port is adopted solely when it identifies
        itself as a Temo Online, so a scan can never disturb another serial device. While an
        operation is unresolved the device must not change, so no scan happens then.
        """
        try:
            return self.probe()
        except (ProtocolError, ValueError, OSError) as saved_error:
            error = saved_error
        if self.pending():
            raise error
        for address in candidates:
            if address == self.connection.address:
                continue
            connection = Connection(address, self.connection.baudrate)
            try:
                with self._locked(), Session(connection) as session:
                    identity = session.command("getrealid", timeout=SCAN_TIMEOUT)
                    if identity.get("nm") != "POSNET TEMO ONLINE":
                        continue
                    status = self._probe(session)
            except ProtocolError, ValueError, OSError:
                continue
            self.connection = connection
            self.last_status = status
            return status
        raise error

    def _check_ready(self, session):
        status = self._probe(session)
        if not status.ready:
            raise ValueError(status.description)
        return status

    def _run(self, session, status, operation, commands, **details):
        record = dict(
            state="pending",
            operation=operation,
            timestamp=datetime.now().astimezone().isoformat(),
            unique_number=status.unique_number,
            **details,
        )
        for command, params, timeout in commands:
            record["stage"] = command
            self._save(record)
            try:
                session.command(command, params, timeout=timeout)
            except Exception as exc:
                raise ProtocolError(
                    f"{exc}\nWynik operacji wymaga sprawdzenia. Nie wysyłaj jej ponownie przed sprawdzeniem drukarki."
                ) from exc
        record["state"] = "completed"
        self._save(record)
        return record

    def print_receipt(
        self, lines: list[Line], payment: int = 0, brand: str = "", **details
    ) -> dict:
        """Print one fiscal receipt.

        `brand` is the company name: when the printer header does not already
        show it, it is added as a footer line so the receipt carries the brand.
        Extra `details` (e.g. a job id) are journaled with the operation.
        """
        lines = list(lines)
        if not 1 <= len(lines) <= 500:
            raise ValueError("Paragon musi zawierać od 1 do 500 pozycji.")
        if any(not isinstance(line, Line) for line in lines):
            raise ValueError("Nieprawidłowe pozycje paragonu.")
        if payment not in (0, 2):
            raise ValueError("Wybierz płatność gotówką lub kartą.")
        brand = brand.strip()[:40]
        if brand:
            printable(brand)
        total = sum(line.total_cents for line in lines)
        if total > MAX_CENTS:
            raise ValueError("Suma paragonu przekracza zakres drukarki.")
        with self._locked():
            self._require_resolved()
            with Session(self.connection) as session:
                status = self._check_ready(session)
                for line in lines:
                    if not status.vat_rates[line.vat].active:
                        raise ValueError("Wybrana stawka VAT jest nieaktywna na drukarce.")
                    if (
                        self.last_status
                        and status.vat_rates[line.vat] != self.last_status.vat_rates[line.vat]
                    ):
                        raise ValueError(
                            "Stawka VAT zmieniła się na drukarce. Odśwież połączenie i sprawdź stawki."
                        )
                footer = (
                    brand
                    and brand.casefold()
                    not in header_text(session.command("hdrget").get("tx", "")).casefold()
                )
                commands = [("trinit", {"bm": "0"}, 10)]
                for line in lines:
                    commands.append(
                        (
                            "trline",
                            {
                                "na": line.name,
                                "vt": line.vat,
                                "pr": line.price_cents,
                                "il": format(line.quantity, "f"),
                                "wa": line.total_cents,
                            },
                            10,
                        )
                    )
                commands.append(("trpayment", {"ty": payment, "wa": total}, 10))
                if footer:
                    commands += [
                        ("trend", {"to": total, "fp": total, "fe": "0"}, 30),
                        ("trftrln", {"id": "25", "na": brand}, 10),
                        ("trftrend", {}, 30),
                    ]
                else:
                    commands.append(("trend", {"to": total, "fp": total}, 30))
                record = self._run(
                    session,
                    status,
                    "receipt",
                    commands,
                    total_cents=total,
                    payment=payment,
                    footer=brand if footer else "",
                    lines=[
                        {
                            "name": line.name,
                            "quantity": str(line.quantity),
                            "price_cents": line.price_cents,
                            "vat": line.vat,
                            "total_cents": line.total_cents,
                        }
                        for line in lines
                    ],
                    **details,
                )
                try:
                    record["receipt_number"] = session.command("scnt").get("bt", "")
                except ProtocolError:
                    record["receipt_number"] = ""
                self._save(record)
                return record

    def daily_report(self) -> dict:
        with self._locked():
            self._require_resolved()
            with Session(self.connection) as session:
                status = self._check_ready(session)
                clock = session.command("rtcget")
                try:
                    day = date.fromisoformat(clock["da"][:10])
                except (KeyError, ValueError) as exc:
                    raise ProtocolError("Nie można odczytać daty drukarki.") from exc
                return self._run(
                    session, status, "daily_report", [("dailyrep", {"da": day.isoformat()}, 120)]
                )

    def monthly_report(self, year: int, month: int) -> dict:
        day = date(year, month, 1)
        with self._locked():
            self._require_resolved()
            with Session(self.connection) as session:
                status = self._check_ready(session)
                return self._run(
                    session,
                    status,
                    "monthly_report",
                    [("monthlyrep", {"da": day.isoformat(), "su": "0"}, 120)],
                )

    def periodic_report(self, start: date, end: date, summary: bool = False) -> dict:
        if start > end:
            raise ValueError("Data początkowa nie może być późniejsza niż końcowa.")
        with self._locked():
            self._require_resolved()
            with Session(self.connection) as session:
                status = self._check_ready(session)
                return self._run(
                    session,
                    status,
                    "periodic_report",
                    [
                        (
                            "periodicrepbydates",
                            {
                                "fd": start.isoformat(),
                                "td": end.isoformat(),
                                "su": "1" if summary else "0",
                            },
                            300,
                        )
                    ],
                )

    def acknowledge_pending(self):
        with self._locked():
            pending = self.pending()
            if not pending:
                return
            with Session(self.connection) as session:
                status = self._check_ready(session)
                if pending.get("unique_number") != status.unique_number:
                    raise ValueError("Podłącz drukarkę, której dotyczy nierozstrzygnięta operacja.")
            self.acknowledge(pending)

    def acknowledge(self, record: dict):
        """Close a journal record once its outcome is known and reported."""
        record["state"] = "acknowledged"
        record["acknowledged_at"] = datetime.now().astimezone().isoformat()
        self._save(record)
