# HuggingCar workshop

Software that runs on the workshop PC, next to the Posnet Temo Online fiscal printer.
One uv workspace, three packages:

| Package | Import | What it is |
| --- | --- | --- |
| `packages/posnet` | `posnet` | Driver for the Posnet protocol over USB/serial: framing and CRC, CP1250, status probe, receipts, reports, intent journal, plus a pseudo-terminal simulator for tests. pySerial only. |
| `packages/desktop` | `fiscal_desktop` | **HuggingCar Fiscal** — Polish desktop app (PySide6): receipt editor, daily/monthly/periodic reports, local history. Ships as `.exe`, `.dmg`, `.deb`. |
| `packages/agent` | `workshop_agent` | Headless agent: polls the HuggingCar manager API for receipt jobs queued from the web app, prints them, reports the fiscal number back. |

The desktop app and the agent import the same `posnet` source; a driver fix lands once.

## Install HuggingCar Fiscal

Download the build for your machine; each link always points at the
[latest release](https://github.com/HuggingCar/workshop/releases/latest):

| Platform | File |
| --- | --- |
| Windows x64 | [`huggingcar-fiscal-win-x64.exe`](https://github.com/HuggingCar/workshop/releases/latest/download/huggingcar-fiscal-win-x64.exe) — single file, no installation |
| macOS Apple Silicon | [`huggingcar-fiscal-mac-arm64.dmg`](https://github.com/HuggingCar/workshop/releases/latest/download/huggingcar-fiscal-mac-arm64.dmg) |
| macOS Intel | [`huggingcar-fiscal-mac-x64.dmg`](https://github.com/HuggingCar/workshop/releases/latest/download/huggingcar-fiscal-mac-x64.dmg) |
| Ubuntu / Debian x64 | [`huggingcar-fiscal-linux-x64.deb`](https://github.com/HuggingCar/workshop/releases/latest/download/huggingcar-fiscal-linux-x64.deb) |
| Ubuntu / Debian ARM | [`huggingcar-fiscal-linux-arm64.deb`](https://github.com/HuggingCar/workshop/releases/latest/download/huggingcar-fiscal-linux-arm64.deb) |

File names carry no version so the links stay stable; the version is in the release tag,
in `SHA256SUMS.txt`, and in the app itself.

Builds are unsigned: Windows SmartScreen and macOS Gatekeeper ask once before the first
start (macOS: right-click → Open). The `.deb` installs `huggingcar-fiscal` and a launcher
entry.

## Printer

Printer side: **PC services → USB → POSNET → Windows 1250**, 9600 baud. On Linux the
port is usually `/dev/ttyACM0` (prefer a stable `/dev/serial/by-id/...` name when one
exists), on Windows `COM3`-style, on macOS `/dev/cu.usbmodem…`. The user needs read and
write access to the port (`dialout` group on Linux). Never run through sudo.

The desktop app scans the serial ports on start and adopts the one answering as a Temo;
the port can also be set by hand under Ustawienia → Drukarka. Only serial connections
are supported.

Read-only check that issues no receipt:

```bash
uv run fiscal-desktop --serial /dev/ttyACM0 --probe
```

## Development

```bash
curl -LsSf https://astral.sh/uv/install.sh | sh   # once, if uv is missing
uv sync --locked                                   # every package, one .venv
uv run fiscal-desktop                              # the desktop app
uv run workshop-agent fiscal --serial /dev/ttyACM0 --api-url https://<manager-api> --token <TOKEN>
QT_QPA_PLATFORM=offscreen uv run pytest -q         # all packages
uv run ruff check packages && uv run ruff format --check packages
```

Python 3.14 or newer. `packages/desktop/build.sh` produces the bundle for the host OS
and CPU into `packages/desktop/release/`; CI runs it on five runners and publishes a
release whenever `version` in the root `pyproject.toml` is raised on `master`.

## Fiscal safety

Both the desktop app and the agent follow the same rules, in the driver:

- Before every operation the Temo identity, fiscal mode, header, mechanism, command
  queue, transaction state and VAT rates are verified.
- Intent is journaled before the first command reaches the printer. Nothing is ever
  retried on the device and nothing is ever reprinted.
- An uncertain outcome (no answer mid-transaction, process death) blocks further
  operations, including across restarts, until a person checks the paper and the
  printer state and acknowledges: in the desktop app "Wyjaśnij ostatnią operację", for
  the agent the job is reported as *unknown* and resolved in the web app.
  Acknowledging unblocks; it does not reissue the receipt.

Prices are gross; the printer computes and prints the VAT block. A receipt contains only
lines priced above zero. VAT rates come from the printer's own table (`vatget`); the
desktop app applies one selected rate to the whole receipt, the agent uses the rate
configured for the printer in the web app. When the fiscal header does not already
contain the company name, it is added as a footer line.

## Desktop app

Three tabs and a message line at the bottom; styling follows Ant Design v5 default tokens
(`theme.qss`).

- **Sprzedaż** — the receipt editor (editable names, quantities to 8 decimals, gross
  prices to 2), a search box that filters the service list without dropping priced rows,
  and the summary with the net and VAT contained in the gross total. Prices are cleared
  after a successful print and kept after an error.
- **Raporty** — daily report, monthly report (previous month preselected), periodic
  report by dates (full or summary). All three confirm before printing.
- **Historia** — receipts printed from this computer, newest first, with the number
  assigned by the printer. A convenience log; the printer's fiscal memory is the record.
- **Ustawienia** (gear) — the service list shown on Sprzedaż (add, rename by
  double-click, delete, drag to reorder) and, on the second tab, printer port and speed.

Service names must be nonblank, printable in Windows-1250, and at most 80 characters.
Both name editors reject invalid edits and keep the previous name. Settings are checked
again before saving, and invalid names block printing even on rows without a price.
Equivalent accented characters are normalized to NFC; names are never transliterated
or truncated.

State: journal and history in `$XDG_STATE_HOME/huggingcar-fiscal/`
(`~/.local/state/huggingcar-fiscal/`), settings in `~/.config/HuggingCar/Fiscal.conf`.

## Agent

1. In the web app (director): Ustawienia → Drukarki fiskalne → Dodaj drukarkę. Copy the
   token — it is shown once.
2. On the workshop PC run the command above once with `--api-url` and `--token` (or
   `WORKSHOP_AGENT_TOKEN` in the environment, which keeps the token out of `ps`); they
   are stored owner-only in `$XDG_STATE_HOME/workshop-agent/fiscal.json` and later runs
   need only `--serial`. On first contact the server pins the printer's unique number to
   the token; another device on the same token is rejected.

Polls every 3 s. Queued jobs older than 10 minutes expire server-side rather than print
late. A crash after printing but before reporting is reconciled from the journal on
restart.

## Tests

Driver: the CRC vector from the Posnet documentation, CP1250 encoding, exact amounts,
VAT-in-gross rounding, protocol error forms, port autodetection and the branding footer
against the simulator. Desktop: value validation, the Qt editor, reports, lock release
after a failed connection. Agent: the poll loop against the simulator and an in-memory
API stand-in — receipt number, invalid payloads, crash recovery, the unknown-outcome
hold. Real printouts still need an attached printer.

The protocol source is Posnet's POT-I-DEV-37 specification (v5406, 2022-06-13, Temo
Online 2.01). No vendor SDK is used, and the specification is not redistributed here.
