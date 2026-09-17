import errno
import json
import os
import re
import tempfile
from contextlib import contextmanager
from dataclasses import dataclass, replace
from datetime import date, datetime
from decimal import Decimal, InvalidOperation
from pathlib import Path

from .models import MAX_CENTS, VAT_COUNT, Line, VatRate, sanitize
from .protocol import Connection, ProtocolError, Session

if os.name == "nt":
    import msvcrt

    def _try_lock(file):
        try:
            msvcrt.locking(file.fileno(), msvcrt.LK_NBLCK, 1)
        except OSError as exc:
            raise BlockingIOError from exc

else:
    import fcntl

    def _try_lock(file):
        fcntl.flock(file, fcntl.LOCK_EX | fcntl.LOCK_NB)


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


def vat_percent(raw: str) -> Decimal:
    """One rate out of `vatget`; a non-numeric or non-finite value is not a usable rate."""
    percent = Decimal(raw.replace(",", "."))
    if not percent.is_finite():
        raise ValueError("Nieprawidłowa stawka VAT w odpowiedzi drukarki.")
    return percent


SCAN_TIMEOUT = 0.5  # a Temo answers getrealid well within this; silent ports cost no more
MAX_LINES = 500


def _denied_port(exc: BaseException) -> bool:
    """True when the OS refused to open the port; the usual cause is a missing dialout group."""
    cause = exc.__cause__ if isinstance(exc, ProtocolError) else exc
    return isinstance(cause, OSError) and cause.errno in (errno.EACCES, errno.EPERM)


def receipt_commands(lines: list[Line], payment: int, total: int, footer: str) -> list[tuple]:
    """The command sequence for one receipt; a non-empty `footer` adds a branded footer line."""
    commands = [("trinit", {"bm": "0"}, 10)]
    commands += [
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
        for line in lines
    ]
    commands.append(("trpayment", {"ty": payment, "wa": total}, 10))
    if footer:
        commands += [
            ("trend", {"to": total, "fp": total, "fe": "0"}, 30),
            ("trftrln", {"id": "25", "na": footer}, 10),
            ("trftrend", {}, 30),
        ]
    else:
        commands.append(("trend", {"to": total, "fp": total}, 30))
    return commands


class Printer:
    def __init__(self, connection: Connection, journal_path: str | Path):
        self.connection = connection
        self.journal_path = Path(journal_path)
        self.journal_path.parent.mkdir(parents=True, exist_ok=True)
        self.last_status = None

    @contextmanager
    def _locked(self):
        with self.journal_path.with_suffix(".lock").open("ab") as lock:
            try:
                _try_lock(lock)
            except BlockingIOError as exc:
                raise ValueError("Inna operacja drukarki jest już wykonywana.") from exc
            yield

    def _save(self, record):
        # Persist intent before touching the printer, including across power loss.
        fd, name = tempfile.mkstemp(dir=self.journal_path.parent, prefix=".operation-")
        temporary = Path(name)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as file:
                json.dump(record, file, ensure_ascii=False, indent=2)
                file.flush()
                os.fsync(file.fileno())
            temporary.replace(self.journal_path)
            if hasattr(os, "O_DIRECTORY"):  # Windows cannot fsync a directory
                directory = os.open(self.journal_path.parent, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    os.fsync(directory)
                finally:
                    os.close(directory)
        finally:
            temporary.unlink(missing_ok=True)

    def last_record(self) -> dict | None:
        """Whatever the journal holds, regardless of state; None when there is none.

        A journal that exists but cannot be read is never "nothing": callers must stop.
        """
        try:
            record = json.loads(self.journal_path.read_text(encoding="utf-8"))
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
        return record

    def pending(self) -> dict | None:
        record = self.last_record()
        return record if record and record["state"] == "pending" else None

    def _require_resolved(self):
        if self.pending():
            raise ValueError(
                "Poprzednia operacja jest nierozstrzygnięta. Sprawdź drukarkę i rozstrzygnij ją "
                "przed kolejnym wydrukiem."
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
                VatRate(i, vat_percent(vats["v" + chr(ord("a") + i)])) for i in range(VAT_COUNT)
            ]
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
        denied = self.connection.address if _denied_port(error) else ""
        for address in candidates:
            if address == self.connection.address:
                continue
            connection = replace(self.connection, address=address)
            try:
                with self._locked(), Session(connection) as session:
                    identity = session.command("getrealid", timeout=SCAN_TIMEOUT)
                    if identity.get("nm") != "POSNET TEMO ONLINE":
                        continue
                    status = self._probe(session)
            except (ProtocolError, ValueError, OSError) as exc:
                denied = denied or (address if _denied_port(exc) else "")
                continue
            self.connection = connection
            self.last_status = status
            return status
        if denied:
            raise ValueError(
                f"Brak uprawnień do portu {denied}. Na Linuksie dodaj użytkownika do grupy "
                "dialout i zaloguj się ponownie."
            ) from error
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
                    f"{exc}\nWynik operacji wymaga sprawdzenia. "
                    "Nie wysyłaj jej ponownie przed sprawdzeniem drukarki."
                ) from exc
        record["state"] = "completed"
        self._save(record)
        return record

    def _check_rates(self, status: Status, lines: list[Line]):
        """Refuse to print on a rate that is inactive or has changed since the last probe."""
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

    def print_receipt(
        self, lines: list[Line], payment: int = 0, brand: str = "", **details
    ) -> dict:
        """Print one fiscal receipt.

        `brand` is the company name: when the printer header does not already
        show it, it is added as a footer line so the receipt carries the brand.
        Extra `details` (e.g. a job id) are journaled with the operation.
        """
        lines = list(lines)
        if not 1 <= len(lines) <= MAX_LINES:
            raise ValueError("Paragon musi zawierać od 1 do 500 pozycji.")
        if any(not isinstance(line, Line) for line in lines):
            raise ValueError("Nieprawidłowe pozycje paragonu.")
        if payment not in (0, 2):
            raise ValueError("Wybierz płatność gotówką lub kartą.")
        brand = sanitize(brand, 40)
        total = sum(line.total_cents for line in lines)
        if total > MAX_CENTS:
            raise ValueError("Suma paragonu przekracza zakres drukarki.")
        with self._locked():
            self._require_resolved()
            with Session(self.connection) as session:
                status = self._check_ready(session)
                self._check_rates(status, lines)
                header = header_text(session.command("hdrget").get("tx", "")) if brand else ""
                footer = brand if brand.casefold() not in header.casefold() else ""
                record = self._run(
                    session,
                    status,
                    "receipt",
                    receipt_commands(lines, payment, total, footer),
                    total_cents=total,
                    payment=payment,
                    footer=footer,
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

    def periodic_report(self, start: date, end: date, *, summary: bool = False) -> dict:
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

    def acknowledge_pending(self, *, force: bool = False):
        """Close the unresolved record once a human has checked the paper.

        The device it happened on must be connected and ready, so the check was possible;
        `force` skips that for a dead or replaced printer and records the fact.
        """
        with self._locked():
            pending = self.pending()
            if not pending:
                return
            if force:
                pending["resolution"] = "manual"
            else:
                with Session(self.connection) as session:
                    status = self._check_ready(session)
                if pending.get("unique_number") != status.unique_number:
                    raise ValueError("Podłącz drukarkę, której dotyczy nierozstrzygnięta operacja.")
            self._acknowledge(pending)

    def acknowledge(self, record: dict):
        """Close a journal record once its outcome is known and reported."""
        with self._locked():
            self._acknowledge(record)

    def _acknowledge(self, record: dict):
        record["state"] = "acknowledged"
        record["acknowledged_at"] = datetime.now().astimezone().isoformat()
        self._save(record)
