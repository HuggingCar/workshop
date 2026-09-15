"""Offline Temo peer. Commands traverse a real socket via pyserial's URL handlers."""

import binascii
import socket
import threading
from datetime import datetime
from decimal import ROUND_HALF_UP, Decimal

import serial

from .protocol import MAX_FRAME, Connection, DeviceError, ProtocolError, decode_frame

serial.protocol_handler_packages.append("posnet")  # resolves sim:// to posnet.protocol_sim


class Simulator:
    def __init__(self):
        self._server = socket.create_server(("127.0.0.1", 0))
        self._server.settimeout(0.1)  # accept() wakes up to notice close()
        self._peer = None
        self.connection = Connection(address=f"sim://127.0.0.1:{self._server.getsockname()[1]}")
        self.receipts = []
        self.reports = []
        self.footers = []
        self.header = "&c&1Warsztat Kowalski&1&c\nul. Testowa 1\n00-001 Warszawa"
        self.footer_open = False
        self.transaction_open = False
        self.lines = []
        self.payment = None
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._serve, name="Temo simulator", daemon=True)

    def __enter__(self):
        self._thread.start()
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        if self._stop.is_set():
            return
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(timeout=1)
        self._server.close()

    def _serve(self):
        # One session at a time, like a serial port; each Session opens a new connection.
        while not self._stop.is_set():
            try:
                self._peer, _ = self._server.accept()
            except TimeoutError:
                continue
            with self._peer:
                self._peer.settimeout(0.1)
                self._serve_session()

    def _serve_session(self):
        buffer = bytearray()
        while not self._stop.is_set():
            try:
                chunk = self._peer.recv(4096)
            except TimeoutError:
                continue
            except OSError:
                return
            if not chunk:
                return  # the driver closed its session
            buffer.extend(chunk)
            if len(buffer) > MAX_FRAME:
                buffer.clear()
                continue
            while b"\x03" in buffer:
                end = buffer.index(3) + 1
                frame, buffer = bytes(buffer[:end]), buffer[end:]
                try:
                    command, params = decode_frame(frame)
                except ProtocolError:
                    self._reply(b"ERR\t?2000\t")  # frame error: the documented ERR form
                    continue
                try:
                    self._respond(command, self._handle(command, params))
                except ProtocolError, ValueError, KeyError, ArithmeticError:
                    self._reply(command.encode("cp1250") + b"\t?2000\t")  # execution error

    def _reply(self, payload):
        self._peer.sendall(b"\x02" + payload + b"#%04X\x03" % binascii.crc_hqx(payload, 0))

    def _respond(self, command, params):
        # Device replies may carry LF inside values (e.g. the header), which the
        # outgoing sanitizer in encode_frame rightly rejects.
        self._reply(
            ("\t".join([command, *(k + str(v) for k, v in params.items())]) + "\t").encode("cp1250")
        )

    def _handle(self, command, params):
        if command == "getrealid":
            return {"nm": "POSNET TEMO ONLINE", "vr": "32.01", "nu": "DEMO0000001"}
        if command == "sdev":
            return {"ds": "0", "cp": "1", "qe": "1", "pe": "0"}
        if command == "sprn":
            return {"pr": "0"}
        if command == "scomm":
            return {
                "fs": "1",
                "tz": "0",
                "ts": "16" if self.transaction_open else "0",
                "hr": "1",
                "nu": "DEMO0000001",
            }
        if command == "strns":
            return {
                "to": "1" if self.transaction_open else "0",
                "ts": "16",
                "fe": "1" if self.footer_open else "0",
            }
        if command == "hdrget":
            return {"tx": self.header}
        if command == "scnt":
            return {"bt": str(len(self.receipts))}
        if command == "trftrln" and self.footer_open:
            self.footers.append(params)
            return {}
        if command == "trftrend" and self.footer_open:
            self.footer_open = False
            return {}
        if command == "vatget":
            return {
                "va": "23,00",
                "vb": "8,00",
                "vc": "0,00",
                "vd": "101,00",
                "ve": "101,00",
                "vf": "101,00",
                "vg": "100,00",
            }
        if command == "rtcget":
            return {"da": datetime.now().astimezone().strftime("%Y-%m-%d;%H:%M")}
        if command == "trinit" and not self.transaction_open:
            self.transaction_open = True
            self.lines = []
            self.payment = None
            return {}
        if command == "trline" and self.transaction_open:
            value = (Decimal(params["il"]) * int(params["pr"])).quantize(
                Decimal(1), rounding=ROUND_HALF_UP
            )
            if int(params["wa"]) != int(value) or int(params["vt"]) not in (0, 1, 2, 6):
                raise ValueError
            self.lines.append(params)
            return {}
        if command == "trpayment" and self.transaction_open:
            self.payment = params
            return {}
        if command == "trend" and self.transaction_open and self.lines and self.payment:
            total = sum(int(line["wa"]) for line in self.lines)
            if (
                total != int(params["to"])
                or total != int(self.payment["wa"])
                or total != int(params["fp"])
            ):
                raise ValueError
            self.receipts.append(
                {
                    "total_cents": total,
                    "payment": int(self.payment["ty"]),
                    "lines": list(self.lines),
                }
            )
            self.transaction_open = False
            self.footer_open = params.get("fe") == "0"
            return {}
        if (
            command in ("dailyrep", "monthlyrep", "periodicrepbydates")
            and not self.transaction_open
        ):
            self.reports.append({"command": command, "params": params})
            return {}
        raise DeviceError(2000, command)
