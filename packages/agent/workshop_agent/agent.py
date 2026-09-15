"""Workshop-PC agent: polls HuggingCar for receipt jobs and prints them.

Safety rules match the desktop app: an operation is journaled before the first
command reaches the printer, nothing is ever retried on the device, and a job
whose outcome is unknown is reported as such, never re-printed.
"""

import json
import logging
import time
from decimal import Decimal, InvalidOperation
from http import HTTPStatus
from typing import TYPE_CHECKING
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from posnet.models import Line
from posnet.protocol import ProtocolError

if TYPE_CHECKING:
    from pathlib import Path

    from posnet.printer import Printer

log = logging.getLogger("posnet.agent")

POLL_SECONDS = 3
RETRY_SECONDS = 15

DONE, FAILED, UNKNOWN = 3, 4, 6
TAKEN = 2


class Api:
    def __init__(self, base_url: str, token: str, device_serial: str):
        self.base_url = base_url.rstrip("/")
        self.headers = {
            "Authorization": f"Agent {token}",
            "X-Device-Serial": device_serial,
            "Content-Type": "application/json",
        }

    def _call(self, method: str, path: str, body: dict | None = None):
        request = Request(
            f"{self.base_url}/integrations/fiscal/agent/jobs/{path}",
            data=json.dumps(body).encode() if body is not None else None,
            headers=self.headers,
            method=method,
        )
        with urlopen(request, timeout=30) as response:
            if response.status == HTTPStatus.NO_CONTENT:
                return None
            return json.loads(response.read() or b"null")

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

    def run_forever(self):
        while True:
            try:
                self.step()
            except (HTTPError, URLError, OSError, ProtocolError, ValueError) as exc:
                log.warning("%s — ponowna próba za %ss", exc, RETRY_SECONDS)
                time.sleep(RETRY_SECONDS)

    def step(self):
        if self._settle_last_operation():
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
            if remote == TAKEN:
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

    def execute(self, job: dict):
        job_id = job["pk"]
        payload = job["payload"]
        try:
            lines = [
                Line(
                    item["name"],
                    Decimal(item["quantity"]),
                    Decimal(item["unit_price"]),
                    payload["vat"],
                )
                for item in payload["lines"]
            ]
        except (KeyError, ValueError, InvalidOperation, TypeError) as exc:
            self.api.report(job_id, FAILED, result=str(exc))
            return
        try:
            record = self.printer.print_receipt(
                lines, job["payment"], brand=payload.get("company_name", ""), job_id=job_id
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
    if not config["api_url"].startswith(("http://", "https://")):
        log.error("Adres API musi zaczynać się od http:// lub https://")
        return 2
    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(json.dumps(config, indent=2))
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
