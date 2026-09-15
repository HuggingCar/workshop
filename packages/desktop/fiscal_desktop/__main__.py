import argparse
import json
import os
import sys
from dataclasses import asdict
from pathlib import Path

from posnet.printer import Printer
from posnet.protocol import Connection


def main():
    parser = argparse.ArgumentParser(description="HuggingCar Fiscal · Posnet Temo Online")
    parser.add_argument(
        "--probe", action="store_true", help="Read printer identity, status and VAT, then exit"
    )
    parser.add_argument("--serial", help="USB serial device, e.g. /dev/serial/by-id/...")
    parser.add_argument("--baudrate", type=int, default=9600)
    parser.add_argument("--data-dir", type=Path, help="Override application state directory")
    args = parser.parse_args()
    data = (
        args.data_dir
        or Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))
        / "huggingcar-fiscal"
    )
    if args.serial:
        connection = Connection(args.serial, args.baudrate)
    else:
        from .app import saved_connection

        connection = saved_connection()
    printer = Printer(connection, data / "operation.json")
    if args.probe:
        try:
            status = printer.probe()
            print(json.dumps(asdict(status), ensure_ascii=False, indent=2, default=str))
            return 0 if status.ready else 1
        except (OSError, ValueError, RuntimeError) as exc:
            print(str(exc), file=sys.stderr)
            return 1
    from PySide6.QtCore import QLocale
    from PySide6.QtGui import QIcon
    from PySide6.QtWidgets import QApplication

    from .app import MainWindow

    QLocale.setDefault(QLocale("pl_PL"))
    app = QApplication(sys.argv[:1])
    app.setStyle("Fusion")
    app.setApplicationName("HuggingCar Fiscal")
    app.setOrganizationName("HuggingCar")
    app.setDesktopFileName("huggingcar-fiscal")
    app.setWindowIcon(QIcon(str(Path(__file__).with_name("icon.svg"))))
    window = MainWindow(printer)
    window.show()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
