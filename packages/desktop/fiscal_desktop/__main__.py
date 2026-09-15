import argparse
import json
import sys
from dataclasses import asdict, replace
from pathlib import Path

from posnet.printer import Printer
from posnet.protocol import Connection
from PySide6.QtCore import QStandardPaths


def main():
    parser = argparse.ArgumentParser(description="HuggingCar Fiscal · Posnet Temo Online")
    parser.add_argument(
        "--probe", action="store_true", help="Read printer identity, status and VAT, then exit"
    )
    parser.add_argument("--serial", help="USB serial device, e.g. /dev/serial/by-id/...")
    parser.add_argument("--baudrate", type=int, help="Override the saved baudrate (default 9600)")
    parser.add_argument("--data-dir", type=Path, help="Override application state directory")
    args = parser.parse_args()
    data = args.data_dir or (
        Path(QStandardPaths.writableLocation(QStandardPaths.StandardLocation.GenericStateLocation))
        / "huggingcar-fiscal"
    )
    if args.serial:
        connection = Connection(args.serial)
    else:
        from .app import saved_connection

        connection = saved_connection()
    if args.baudrate:
        connection = replace(connection, baudrate=args.baudrate)
    journal = data / "operation.json"
    if args.probe:
        try:
            status = Printer(connection, journal).probe()
        except (OSError, ValueError, RuntimeError) as exc:
            print(str(exc), file=sys.stderr)
            return 1
        print(json.dumps(asdict(status), ensure_ascii=False, indent=2, default=str))
        return 0 if status.ready else 1
    from PySide6.QtCore import QLocale
    from PySide6.QtGui import QIcon
    from PySide6.QtWidgets import QApplication, QMessageBox

    from .app import MainWindow

    QLocale.setDefault(QLocale("pl_PL"))
    app = QApplication(sys.argv[:1])
    app.setStyle("Fusion")
    app.setApplicationName("HuggingCar Fiscal")
    app.setOrganizationName("HuggingCar")
    app.setDesktopFileName("huggingcar-fiscal")
    app.setWindowIcon(QIcon(str(Path(__file__).with_name("icon.svg"))))
    try:
        window = MainWindow(Printer(connection, journal))
    except (OSError, ValueError) as exc:  # windowed bundles have no stderr to read
        QMessageBox.critical(None, "HuggingCar Fiscal", f"Nie można uruchomić aplikacji:\n{exc}")
        return 1
    window.show()
    return app.exec()


if __name__ == "__main__":
    sys.exit(main())
