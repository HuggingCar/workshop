"""pyserial handler for `sim://host:port`: socket:// without the 0.3 s close grace.

pyserial sleeps after closing a socket port "in case of quick reconnects"; the simulator's
peer needs no such grace, and the driver opens a session per operation.
"""

from serial.urlhandler.protocol_socket import Serial as SocketSerial


class Serial(SocketSerial):
    def close(self):
        if self.is_open:
            self._socket.close()
            self._socket = None
            self.is_open = False


def serial_class_for_url(url):
    return url.replace("sim://", "socket://", 1), Serial
