import binascii
import os
import pty
import tty

import pytest
from posnet.protocol import (
    Connection,
    DeviceError,
    ProtocolError,
    ResponseTimeout,
    Session,
    decode_frame,
    encode_frame,
)


def reply(payload):
    raw = payload.encode("cp1250")
    return b"\x02" + raw + b"#%04X\x03" % binascii.crc_hqx(raw, 0)


def test_published_posnet_crc_vector():
    assert encode_frame("trinit", {"bm": "0"}) == b"\x02trinit\tbm0\t#4825\x03"


def test_polish_text_encoded_before_checksum():
    encoded = encode_frame("trline", {"na": "Koło", "vt": "0", "pr": "100"})
    assert b"Ko\xb3o" in encoded
    assert decode_frame(encoded) == ("trline", {"na": "Koło", "vt": "0", "pr": "100"})


@pytest.mark.parametrize("value", ["x\ty", "x\x03", "x\n", "🔧"])
def test_rejects_control_characters_and_unencodable_text(value):
    with pytest.raises(ValueError):
        encode_frame("trline", {"na": value})


def test_rejects_bad_crc():
    with pytest.raises(ProtocolError):
        decode_frame(b"\x02trinit\tbm0\t#0000\x03")


@pytest.mark.parametrize("payload", ["trline\t?2000", "ERR\t?2000\tcmtrline\tfdvt\t"])
def test_both_documented_error_forms(payload):
    with pytest.raises(DeviceError) as error:
        decode_frame(reply(payload))
    assert error.value.code == 2000


def test_lost_reply_never_retries_mutation():
    """An unresponsive port must surface a timeout, never resend a fiscal command."""
    master, slave = pty.openpty()
    tty.setraw(slave)
    try:
        with (
            Session(Connection(os.ttyname(slave), timeout=0.2)) as session,
            pytest.raises(ResponseTimeout),
        ):
            session.command("trinit", {"bm": "0"})
        os.set_blocking(master, False)
        assert os.read(master, 4096) == encode_frame("trinit", {"bm": "0"})
    finally:
        os.close(master)
        os.close(slave)
