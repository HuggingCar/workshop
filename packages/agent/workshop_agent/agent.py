"""Workshop-PC agent: polls HuggingCar for receipt jobs and prints them.

Safety rules match the desktop app: an operation is journaled before the first
command reaches the printer, nothing is ever retried on the device, and a job
whose outcome is unknown is reported as such, never re-printed.
"""

import json
import logging
import signal
import time
from decimal import Decimal, InvalidOperation
from http import HTTPStatus
from ipaddress import ip_address
from threading import current_thread, main_thread
from typing import TYPE_CHECKING
from urllib.error import HTTPError
from urllib.parse import quote, urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

from posnet.models import EXEMPT_PERCENT, Line
from posnet.protocol import ProtocolError

if TYPE_CHECKING:
    from pathlib import Path

    from posnet.printer import Printer, Status


log = logging.getLogger("posnet.agent")

POLL_SECONDS = 3
RETRY_SECONDS = 15

DONE, FAILED, UNKNOWN = 3, 4, 6
TAKEN = 2


def validated_url(value: str) -> str:
    url = urlsplit(value)
    try:
        loopback = ip_address(url.hostname or "").is_loopback
    except ValueError:
        loopback = url.hostname == "localhost"
    if (
        not url.hostname
        or url.username is not None
        or url.password is not None
        or url.query
        or url.fragment
        or not (url.scheme == "https" or (url.scheme == "http" and loopback))
    ):
        raise ValueError(
            "Adres API wymaga https (http tylko dla localhost), "
            "bez danych logowania, zapytania i fragmentu."
        )
    return value.rstrip("/")


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        # urllib normally forwards Authorization to the redirect target.
        return None


class Api:
    def __init__(self, base_url: str, token: str, device_serial: str):
        self.base_url = validated_url(base_url)
        self.token = token
        self.access_token = ""
        self.refresh_at = 0
        self.opener = build_opener(NoRedirect())
        self.headers = {
            "X-Device-Serial": device_serial,
            "X-Device-Vendor": "posnet",
            "Content-Type": "application/json",
        }

    def describe(self, status: Status) -> None:
        """Carry the device's own model, firmware and active VAT rates on every later call.

        Percent-encoded: header values must stay ASCII, device text need not be.
        """
        rates = [
            {
                "index": vat.index,
                "percent": "zw"
                if vat.percent == EXEMPT_PERCENT
                else f"{vat.percent.normalize():f}",
            }
            for vat in status.vat_rates
            if vat.active
        ]
        self.headers["X-Device-Model"] = quote(status.name)
        self.headers["X-Device-Firmware"] = quote(status.version)
        self.headers["X-Device-Vat-Rates"] = quote(json.dumps(rates, separators=(",", ":")))

    def _send(self, method: str, path: str, authorization: str, body: dict | None = None):
        request = Request(  # noqa: S310 — validated HTTPS or loopback HTTP; redirects disabled
            f"{self.base_url}/integrations/fiscal/agent/{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers={**self.headers, "Authorization": authorization},
            method=method,
        )
        with self.opener.open(request, timeout=30) as response:
            if response.status == HTTPStatus.NO_CONTENT:
                return None
            return json.loads(response.read() or b"null")

    def connect(self):
        started = time.monotonic()
        session = self._send("POST", "session/", f"Agent {self.token}")
        if (
            not isinstance(session, dict)
            or session.get("token_type") != "Bearer"
            or not isinstance(session.get("access_token"), str)
            or not session["access_token"]
            or type(session.get("expires_in")) is not int
            or session["expires_in"] <= 0
        ):
            raise ValueError("Nieprawidłowa odpowiedź sesji API.")
        self.access_token = session["access_token"]
        self.refresh_at = started + session["expires_in"] * 0.9

    def _call(self, method: str, path: str, body: dict | None = None):
        if time.monotonic() >= self.refresh_at:
            self.connect()
        try:
            return self._send(method, f"jobs/{path}", f"Bearer {self.access_token}", body)
        except HTTPError as exc:
            if exc.code != HTTPStatus.UNAUTHORIZED:
                raise
            exc.close()
        # Only an authentication rejection is safe to retry: no job handler ran.
        self.refresh_at = 0
        self.connect()
        return self._send(method, f"jobs/{path}", f"Bearer {self.access_token}", body)

    def take(self) -> dict | None:
        return self._call("POST", "take/")

    def status(self, job_id: int) -> dict:
        return self._call("GET", f"{job_id}/")

    def report(self, job_id: int, status: int, receipt_number: str = "", result: str = ""):
        return self._call(
            "POST",
            f"{job_id}/result/",
            {"status": status, "receipt_number": receipt_number, "result": result[:2000]},
        )


class Agent:
    def __init__(self, printer: Printer, api: Api):
        self.printer = printer
        self.api = api
        self.stopping = False

    def run_forever(self):
        # A stop request waits for the job in progress: killing the process mid-receipt
        # would leave a pending journal that only a human can resolve.
        if current_thread() is main_thread():
            for sig in (signal.SIGINT, signal.SIGTERM):
                signal.signal(sig, lambda *_: setattr(self, "stopping", True))
        while not self.stopping:
            try:
                self.step()
            except (OSError, ProtocolError, ValueError, LookupError, TypeError) as exc:
                log.warning("%s — ponowna próba za %ss", exc, RETRY_SECONDS)
                time.sleep(RETRY_SECONDS)
        log.info("Zatrzymano")

    def step(self):
        if self._settle_last_operation():
            time.sleep(RETRY_SECONDS)
            return
        # Taking a job the printer cannot print would fail it for good, so check first.
        status = self.printer.probe()
        self.api.describe(status)
        if not status.ready:
            log.warning(
                "Drukarka niegotowa: %s — ponowna próba za %ss", status.description, RETRY_SECONDS
            )
            time.sleep(RETRY_SECONDS)
            return
        job = self.api.take()
        if job is None:
            time.sleep(POLL_SECONDS)
            return
        self.execute(job)

    def _settle_last_operation(self) -> bool:
        """Reconcile the journal with the server after a crash. True = still blocked."""
        record = self.printer.last_record()
        if not record or "job_id" not in record or record.get("state") == "acknowledged":
            return False
        job_id = record["job_id"]
        remote = self.api.status(job_id)["status"]
        if record["state"] == "completed":
            if remote in (TAKEN, UNKNOWN):
                self.api.report(job_id, DONE, record.get("receipt_number", ""))
            self.printer.acknowledge(record)
            return False
        # pending: the printer may or may not have printed; a human must look at the paper
        if remote == TAKEN:
            self.api.report(
                job_id, UNKNOWN, result="Przerwano w trakcie drukowania. Sprawdź drukarkę."
            )
            return True
        if remote == UNKNOWN:
            return True
        self.printer.acknowledge_pending()
        return False

    def _standard_vat_slot(self) -> int:
        """The slot of the printer's standard rate.

        Automotive work is single-rate per company (PL: 23% on labour and parts), so the
        rate programmed in the device is the right one; per-item rates would need the
        server to send one per line.
        """
        status = self.printer.last_status
        active = [vat for vat in (status.vat_rates if status else ()) if vat.active]
        taxed = [vat for vat in active if vat.percent != EXEMPT_PERCENT]
        if taxed:
            return max(taxed, key=lambda vat: vat.percent).index
        if active:
            return active[0].index  # VAT-exempt workshop: the receipt prints "zwolniona"
        raise ValueError("Drukarka nie ma zaprogramowanej żadnej stawki VAT.")

    def execute(self, job: dict):
        job_id = job["pk"]
        try:
            payload = job["payload"]
            payment = job["payment"]
            vat = self._standard_vat_slot()
            lines = [
                Line(
                    item["name"],
                    Decimal(item["quantity"]),
                    Decimal(item["unit_price"]),
                    vat,
                )
                for item in payload["lines"]
            ]
        except (KeyError, ValueError, InvalidOperation, TypeError) as exc:
            self.api.report(job_id, FAILED, result=str(exc))
            return
        try:
            record = self.printer.print_receipt(
                lines, payment, brand=payload.get("company_name", ""), job_id=job_id
            )
        except ValueError as exc:
            # Rejected before any command reached the printer (pre-flight, VAT, lock).
            self.api.report(job_id, FAILED, result=str(exc))
            return
        except ProtocolError as exc:
            if self.printer.pending():
                self.api.report(job_id, UNKNOWN, result=str(exc))
            else:
                self.api.report(job_id, FAILED, result=str(exc))
            return
        self.api.report(job_id, DONE, record.get("receipt_number", ""))
        self.printer.acknowledge(record)
        log.info("Paragon %s dla zlecenia (job %s)", record.get("receipt_number"), job_id)


def load_config(path: Path) -> dict:
    try:
        return json.loads(path.read_text())
    except OSError, ValueError:
        return {}


def save_config(path: Path, config: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch(mode=0o600)
    path.chmod(0o600)
    path.write_text(json.dumps(config, indent=2))


def main(printer: Printer, config_path: Path, api_url: str | None, token: str | None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    config = load_config(config_path)
    if api_url:
        config["api_url"] = api_url
    if token:
        config["token"] = token
    if not config.get("api_url") or not config.get("token"):
        log.error("Podaj --api-url i --token (zapamiętywane po pierwszym uruchomieniu).")
        return 2
    try:
        config["api_url"] = validated_url(config["api_url"])
    except ValueError as exc:
        log.error("%s", exc)  # noqa: TRY400 — invalid user configuration, no traceback needed
        return 2
    save_config(config_path, config)
    while True:
        try:
            serial = printer.probe().unique_number
            break
        except (OSError, ValueError, ProtocolError) as exc:
            log.warning("Drukarka niedostępna: %s — ponowna próba za %ss", exc, RETRY_SECONDS)
            time.sleep(RETRY_SECONDS)
    log.info("Drukarka %s, serwer %s", serial, config["api_url"])
    Agent(printer, Api(config["api_url"], config["token"], serial)).run_forever()
    return 0
