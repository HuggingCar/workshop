import binascii
import os
import select
import socket
import sys

import pytest
from posnet.protocol import (
    Connection,
    DeviceError,
    ProtocolError,
    ResponseTimeoutError,
    Session,
    decode_frame,
    encode_frame,
)


def reply(payload):
    raw = payload.encode("cp1250")
    return b"\x02" + raw + b"#%04X\x03" % binascii.crc_hqx(raw, 0)


def test_published_posnet_crc_vector():
    # Spec p.13 worked example: the full frame for trinit bm0 (CRC16-CCITT, poly 0x1021, init 0).
    assert encode_frame("trinit", {"bm": "0"}) == b"\x02trinit\tbm0\t#4825\x03"


def test_polish_text_encoded_before_checksum():
    encoded = encode_frame("trline", {"na": "Koło", "vt": "0", "pr": "100"})
    assert b"Ko\xb3o" in encoded
    assert decode_frame(encoded) == ("trline", {"na": "Koło", "vt": "0", "pr": "100"})


@pytest.mark.parametrize("value", ["x\ty", "x\x03", "x\n", "🔧"])
def test_rejects_control_characters_and_unencodable_text(value):
    with pytest.raises(ValueError):
        encode_frame("trline", {"na": value})


@pytest.mark.parametrize(
    "frame",
    [
        b"\x02trinit\tbm0\t#0000\x03",  # wrong CRC
        reply("trinit\tbm0\tbm1\t"),  # duplicate parameter
        reply("trinit\tb\t"),  # parameter shorter than its two-letter key
    ],
)
def test_rejects_corrupt_and_ambiguous_frames(frame):
    with pytest.raises(ProtocolError):
        decode_frame(frame)


@pytest.mark.parametrize("payload", ["trline\t?2000", "ERR\t?2000\tcmtrline\tfdvt\t"])
def test_both_documented_error_forms(payload):
    with pytest.raises(DeviceError) as error:
        decode_frame(reply(payload))
    assert error.value.code == 2000


class PtyPeer:
    """socket-like view of a pty master, so the tests read the same over both transports."""

    def __init__(self, master):
        self.master = master
        self.timeout = None

    def settimeout(self, seconds):
        self.timeout = seconds

    def sendall(self, data):
        os.write(self.master, data)

    def recv(self, size):
        if not select.select([self.master], [], [], self.timeout)[0]:
            raise TimeoutError
        return os.read(self.master, size)


@pytest.fixture(params=["pty", "sim"])
def raw_peer(request):
    """A device that answers only with the bytes the test writes to it.

    The pty case is the transport the printer actually uses (termios timeouts, partial reads);
    the socket case is what Windows CI can run.
    """
    if request.param == "pty":
        if sys.platform == "win32":
            pytest.skip("no pseudo-terminals on Windows")
        import pty
        import tty

        master, slave = pty.openpty()
        tty.setraw(slave)
        try:
            with Session(Connection(os.ttyname(slave), timeout=0.5)) as s:
                yield PtyPeer(master), s
        finally:
            os.close(master)
            os.close(slave)
        return
    import posnet.simulator  # noqa: F401 — registers the sim:// handler

    server = socket.create_server(("127.0.0.1", 0))
    with (
        server,
        Session(Connection(f"sim://127.0.0.1:{server.getsockname()[1]}", timeout=0.5)) as s,
    ):
        peer, _ = server.accept()
        with peer:
            yield peer, s


def test_lost_reply_never_retries_mutation(raw_peer):
    """An unresponsive port must surface a timeout, never resend a fiscal command."""
    peer, session = raw_peer
    with pytest.raises(ResponseTimeoutError):
        session.command("trinit", {"bm": "0"})
    peer.settimeout(0.2)
    assert peer.recv(4096) == encode_frame("trinit", {"bm": "0"})
    with pytest.raises(TimeoutError):
        peer.recv(4096)  # nothing else was sent


def test_noise_before_a_frame_is_skipped_but_a_foreign_reply_is_not_a_success(raw_peer):
    peer, session = raw_peer
    peer.sendall(b"\x00\xff" + reply("getrealid\tnmX\t"))
    assert session.command("getrealid") == {"nm": "X"}
    peer.sendall(reply("scnt\tbt1\t"))  # a late reply to something else
    with pytest.raises(ProtocolError, match="Oczekiwano odpowiedzi trend"):
        session.command("trend", {"to": "1"})
