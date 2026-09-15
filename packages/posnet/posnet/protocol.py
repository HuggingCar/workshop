import binascii
import re
import time
from dataclasses import dataclass

import serial

from .models import printable

MAX_FRAME = 16384
MAX_TIMEOUT = 120
KEY_LENGTH = 2  # every response parameter is keyed by two letters
STX = b"\x02"
ETX = b"\x03"
CORRUPT = "Uszkodzona odpowiedź drukarki (CRC lub kodowanie)."


class ProtocolError(RuntimeError):
    pass


class ResponseTimeoutError(ProtocolError):
    pass


class DeviceError(ProtocolError):
    def __init__(self, code: int, command: str, field: str = ""):
        self.code = code
        self.command = command
        self.field = field
        detail = f", parametr {field}" if field else ""
        super().__init__(f"Drukarka zwróciła błąd {code} ({command}{detail}).")


@dataclass(frozen=True)
class Connection:
    address: str = ""
    baudrate: int = 9600
    timeout: float = 3.0

    def validate(self):
        if not self.address.strip():
            raise ValueError("Nie wykryto drukarki.")
        simulator = (
            self.address.startswith("sim://") and "posnet" in serial.protocol_handler_packages
        )
        if "://" in self.address and not simulator:
            # pyserial URLs: loop:// would echo our own valid frames back as "replies", and
            # sim:// only exists in a process that imported posnet.simulator.
            raise ValueError("Adres drukarki musi być portem szeregowym.")
        if not 0 < self.timeout <= MAX_TIMEOUT:
            raise ValueError("Nieprawidłowy limit czasu połączenia.")


def encode_frame(command: str, params: dict | None = None) -> bytes:
    if not re.fullmatch(r"!?[a-z][a-z0-9]*", command):
        raise ValueError("Nieprawidłowe polecenie drukarki.")
    fields = [command]
    for key, value in (params or {}).items():
        if not re.fullmatch(r"[a-z]{2}|@", key):
            raise ValueError("Nieprawidłowy parametr drukarki.")
        text = str(value)
        printable(text)
        fields.append(key + text)
    payload = ("\t".join(fields) + "\t").encode("cp1250")
    frame = STX + payload + b"#%04X" % binascii.crc_hqx(payload, 0) + ETX
    if len(frame) > MAX_FRAME:
        raise ValueError("Polecenie drukarki jest zbyt długie.")
    return frame


def _frame_params(fields: list[str]) -> tuple[dict[str, str], int | None]:
    """Response parameters plus the error code the printer reported, if any."""
    params: dict[str, str] = {}
    error = None
    for field in fields:
        if not field:
            continue
        if field.startswith("?"):
            try:
                error = int(field[1:])
            except ValueError as exc:
                raise ProtocolError("Nieprawidłowy kod błędu drukarki.") from exc
        elif field.startswith("@"):
            params["@"] = field[1:]
        elif len(field) >= KEY_LENGTH:
            key = field[:KEY_LENGTH]
            if key in params:
                raise ProtocolError("Powtórzony parametr w odpowiedzi drukarki.")
            params[key] = field[KEY_LENGTH:]
        else:
            raise ProtocolError("Niepełny parametr odpowiedzi drukarki.")
    return params, error


def decode_frame(frame: bytes) -> tuple[str, dict[str, str]]:
    if len(frame) > MAX_FRAME or not frame.startswith(STX) or not frame.endswith(ETX):
        raise ProtocolError("Nieprawidłowa ramka odpowiedzi drukarki.")
    payload, separator, checksum = frame[1:-1].rpartition(b"#")
    if (
        not separator
        or not re.fullmatch(rb"[0-9a-fA-F]{4}", checksum)
        or binascii.crc_hqx(payload, 0) != int(checksum, 16)
    ):
        raise ProtocolError(CORRUPT)
    try:
        fields = payload.decode("cp1250").split("\t")
    except UnicodeDecodeError as exc:
        raise ProtocolError(CORRUPT) from exc
    command = fields.pop(0)
    params, error = _frame_params(fields)
    if error is not None:
        raise DeviceError(error, params.get("cm", command), params.get("fd", ""))
    if command == "ERR":
        raise ProtocolError("Drukarka odrzuciła ramkę bez kodu błędu.")
    return command, params


class Session:
    """One exclusive serial connection; no command is ever automatically retried."""

    def __init__(self, connection: Connection):
        self.connection = connection
        self.stream = None

    def __enter__(self):
        self.connection.validate()
        try:
            self.stream = serial.serial_for_url(
                self.connection.address,
                self.connection.baudrate,
                timeout=0.1,
                write_timeout=self.connection.timeout,
                exclusive=True,
            )
        except OSError as exc:
            raise ProtocolError(f"Nie można połączyć z drukarką: {exc}") from exc
        return self

    def __exit__(self, *_):
        if self.stream is not None:
            self.stream.close()

    def command(
        self, command: str, params: dict | None = None, timeout: float | None = None
    ) -> dict:
        frame = encode_frame(command, params)
        deadline = time.monotonic() + (timeout if timeout is not None else self.connection.timeout)
        try:
            if self.stream.write(frame) != len(frame):
                raise ProtocolError("Niepełne wysłanie polecenia do drukarki.")
            while (remaining := deadline - time.monotonic()) > 0:
                self.stream.timeout = remaining
                raw = self.stream.read_until(ETX, MAX_FRAME)
                if not raw.endswith(ETX):
                    if len(raw) >= MAX_FRAME:
                        raise ProtocolError("Zbyt długa odpowiedź drukarki.")
                    break
                if STX not in raw:
                    continue  # line noise; the CRC guards the frame that follows
                response_command, response = decode_frame(raw[raw.rindex(STX) :])
                if response_command != command:
                    raise ProtocolError(
                        f"Oczekiwano odpowiedzi {command}, otrzymano {response_command}."
                    )
                return response
        except (TimeoutError, serial.SerialTimeoutException) as exc:
            raise ResponseTimeoutError(
                f"Brak odpowiedzi na {command}. Polecenie nie zostało ponowione."
            ) from exc
        except OSError as exc:
            raise ProtocolError(f"Przerwano połączenie z drukarką: {exc}") from exc
        raise ResponseTimeoutError(
            f"Brak odpowiedzi na {command}. Polecenie nie zostało ponowione."
        )
