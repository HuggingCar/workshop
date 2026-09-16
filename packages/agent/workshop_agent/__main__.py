import argparse
import os
import sys
from importlib.metadata import version
from pathlib import Path

from posnet.printer import Printer
from posnet.protocol import Connection

from .agent import main as run_fiscal


def main():
    parser = argparse.ArgumentParser(
        prog="huggingcar-agent",
        description="HuggingCar agent for the workshop PC",
    )
    parser.add_argument("--version", action="version", version=version("workshop-agent"))
    parser.add_argument("--data-dir", type=Path, help="Override application state directory")
    commands = parser.add_subparsers(dest="command")

    fiscal = commands.add_parser("fiscal", help="Print queued receipts on a Posnet Temo Online")
    fiscal.add_argument("--serial", required=True, help="USB serial device, e.g. /dev/ttyACM0")
    fiscal.add_argument("--baudrate", type=int, default=9600)
    fiscal.add_argument("--api-url", help="Manager API base URL (remembered after first run)")
    fiscal.add_argument(
        "--token",
        default=os.environ.get("WORKSHOP_AGENT_TOKEN"),
        help="Printer agent token from HuggingCar (remembered); or $WORKSHOP_AGENT_TOKEN",
    )
    fiscal.add_argument(
        "--data-dir", type=Path, default=argparse.SUPPRESS, help="Override state directory"
    )

    args = parser.parse_args()
    data = args.data_dir or (
        Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state") / "workshop-agent"
    )
    if args.command is None:
        from .app import run

        return run(data)
    printer = Printer(Connection(args.serial, args.baudrate), data / "fiscal-operation.json")
    return run_fiscal(printer, data / "fiscal.json", args.api_url, args.token)


if __name__ == "__main__":
    sys.exit(main())
