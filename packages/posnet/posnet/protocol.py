import binascii
import re
import time
from dataclasses import dataclass

import serial

from .models import printable

MAX_FRAME = 16384


class ProtocolError(RuntimeError):
    pass


class ResponseTimeout(ProtocolError):
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
        if not 0 < self.timeout <= 120:
            raise ValueError("Nieprawidłowy limit czasu połączenia.")


def encode_frame(command: str, params: dict | None = None) -> bytes:
    if not re.fullmatch(r"!?[a-z][a-z0-9]*", command):
        raise ValueError("Nieprawidłowe polecenie drukarki.")
    fields = [command]
    for key, value in (params or {}).items():
        if not re.fullmatch(r"[a-z]{2}|@", key):
            raise ValueError("Nieprawidłowy parametr drukarki.")
        value = str(value)
        printable(value)
        fields.append(key + value)
    payload = ("\t".join(fields) + "\t").encode("cp1250")
    frame = b"\x02" + payload + b"#%04X\x03" % binascii.crc_hqx(payload, 0)
    if len(frame) > MAX_FRAME:
        raise ValueError("Polecenie drukarki jest zbyt długie.")
    return frame


def decode_frame(frame: bytes) -> tuple[str, dict[str, str]]:
    if len(frame) > MAX_FRAME or not frame.startswith(b"\x02") or not frame.endswith(b"\x03"):
        raise ProtocolError("Nieprawidłowa ramka odpowiedzi drukarki.")
    try:
        payload, checksum = frame[1:-1].rsplit(b"#", 1)
        if not re.fullmatch(rb"[0-9a-fA-F]{4}", checksum):
            raise ValueError
        if binascii.crc_hqx(payload, 0) != int(checksum, 16):
            raise ValueError
        fields = payload.decode("cp1250").split("\t")
    except ValueError as exc:
        raise ProtocolError("Uszkodzona odpowiedź drukarki (CRC lub kodowanie).") from exc
    command = fields.pop(0)
    params = {}
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
        elif len(field) >= 2:
            key = field[:2]
            if key in params:
                raise ProtocolError("Powtórzony parametr w odpowiedzi drukarki.")
            params[key] = field[2:]
        else:
            raise ProtocolError("Niepełny parametr odpowiedzi drukarki.")
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
            self.stream = serial.Serial(
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
            result = bytearray()
            while (remaining := deadline - time.monotonic()) > 0:
                self.stream.timeout = min(0.1, max(0.001, remaining))
                chunk = self.stream.read(1)
                if not chunk:
                    continue
                result.extend(chunk)
                if result[0] != 2 or len(result) > MAX_FRAME:
                    raise ProtocolError("Nieprawidłowy początek lub rozmiar odpowiedzi drukarki.")
                if chunk == b"\x03":
                    response_command, response = decode_frame(bytes(result))
                    if response_command != command:
                        raise ProtocolError(
                            f"Oczekiwano odpowiedzi {command}, otrzymano {response_command}."
                        )
                    return response
        except (TimeoutError, serial.SerialTimeoutException) as exc:
            raise ResponseTimeout(
                f"Brak odpowiedzi na {command}. Polecenie nie zostało ponowione."
            ) from exc
        except OSError as exc:
            raise ProtocolError(f"Przerwano połączenie z drukarką: {exc}") from exc
        raise ResponseTimeout(f"Brak odpowiedzi na {command}. Polecenie nie zostało ponowione.")
