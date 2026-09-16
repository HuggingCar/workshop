# HuggingCar workshop

Software that runs on the workshop PC, next to the Posnet Temo Online fiscal printer.
One uv workspace, three packages:

| Package | Import | What it is |
| --- | --- | --- |
| `packages/posnet` | `posnet` | Driver for the Posnet protocol over USB/serial: framing and CRC, CP1250, status probe, receipts, reports, intent journal, plus a socket simulator for tests. Uses pySerial and anyascii. |
| `packages/desktop` | `fiscal_desktop` | **HuggingCar Fiscal** — Polish desktop app (PySide6): receipt editor, daily/monthly/periodic reports, local history. Ships as `.exe`, `.dmg`, `.deb`. |
| `packages/agent` | `workshop_agent` | Agent with a setup window sharing the desktop app's Ant Design-style theme, plus optional headless mode. Polls the manager API, prints receipts and reports their fiscal numbers. Standalone `.exe` and `.tar.gz` builds. |

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

Python 3.14 or newer. `bash packages/desktop/build.sh` and `bash packages/agent/build.sh`
produce bundles for the host OS and CPU in their respective `release/` directories.
CI builds both on five runners and publishes them with checksums when the root version
increases on `master`. A manual **Release** workflow run builds downloadable Actions
artifacts without publishing a GitHub release.

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
desktop app applies one selected rate to the whole receipt. The agent uses the highest
active non-exempt rate (or the exempt slot when that is the only active rate).
The agent does not support mixed-rate receipts. Verify that this rule fits the workshop
before printing. When the fiscal header does not contain the company name, it is added
as a footer line.

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

Receipt text is sanitized by the driver, not the API: unsupported characters are
transliterated, control characters removed, and names shortened to the device limit.
The desktop editor also checks names before printing.

State: journal and history in `$XDG_STATE_HOME/huggingcar-fiscal/`
(`~/.local/state/huggingcar-fiscal/`), settings in `~/.config/HuggingCar/Fiscal.conf`.

## Agent

### Standalone agent (0.1.2)

No Python, uv, or desktop app is required. Download the matching agent asset from the
[release page](https://github.com/HuggingCar/workshop/releases/latest):

| Platform | Asset |
| --- | --- |
| Windows x64 | `huggingcar-agent-win-x64.exe` |
| macOS Apple Silicon | `huggingcar-agent-mac-arm64.tar.gz` |
| macOS Intel | `huggingcar-agent-mac-x64.tar.gz` |
| Linux x64 | `huggingcar-agent-linux-x64.tar.gz` |
| Linux ARM64 | `huggingcar-agent-linux-arm64.tar.gz` |

Agent assets appear after the 0.1.2 release is published. For an unpublished build,
use the manual workflow's Actions artifacts or build locally with `bash packages/agent/build.sh`.
Linux CI builds target Ubuntu 24.04 or newer; a local build requires a compatible
host libc. Builds are unsigned, as with the desktop application.

1. Deploy the matching API first. Version 0.1.2 requires the new session endpoint;
   older agents that send a permanent token to job endpoints must be upgraded.
2. In the web app (director): Ustawienia → Drukarki fiskalne → Dodaj drukarkę.
   Copy the credential; it is shown only once. Existing printer credentials also work.
3. Extract the Unix archive. Double-click the Windows `.exe` or macOS `.app`;
   on Linux launch `./huggingcar-agent` with no arguments.
4. Enter **Adres API**, paste the credential into **Token**, and select or type the
   printer port. Leave the speed at 9600 unless the printer uses another speed.
5. Click **Zapisz** to save without printing. Click **Uruchom** to connect and print
   queued jobs. The window shows connection and error messages.
   **Zatrzymaj** and closing the window wait for the current operation to finish.
   Keep the window open while the agent is running. Do not run the desktop app
   against the same printer.

Optional command-line mode remains available:

```bash
./huggingcar-agent fiscal --serial /dev/ttyACM0 --api-url https://<manager-api> --token <TOKEN>
```

For command-line use, prefer `WORKSHOP_AGENT_TOKEN` over `--token` to keep the
credential out of process arguments. Ctrl+C stops after the current operation.

The API URL and permanent credential are saved in
`$XDG_STATE_HOME/workshop-agent/fiscal.json` (default `~/.local/state/workshop-agent/`).
Use `--data-dir` to choose another location. Unix credentials are saved with mode 0600;
on Windows, keep the directory in your private user profile and restrict its ACL.
Reopening the window restores the URL, masked token, port and speed. It does not
start printing until you click **Uruchom**.

The agent sends `Authorization: Agent <credential>` only to
`POST /integrations/fiscal/agent/session/`, with the printer serial and telemetry headers.
The server pins that serial atomically and returns a signed bearer token valid for
15 minutes. Job calls use `Authorization: Bearer <access_token>`. Session tokens
remain in memory, renew before expiry, and are invalidated immediately by credential
rotation or service/company deletion. A 401 causes one session refresh and one HTTP
retry; network errors never trigger a physical reprint.

HTTPS is required, except for loopback development URLs (`localhost`, `127.0.0.1`, `::1`).
Redirects are rejected; use the final manager API URL, including any path prefix.
Serial binding prevents accidental device swaps; it is not hardware attestation.

Polls every 3 seconds. Queued jobs older than 10 minutes expire rather than print late.
A crash after printing but before reporting is reconciled from the journal on restart,
including when the server has marked the job's outcome unknown.

## Tests

Driver: the CRC vector from the Posnet documentation, CP1250 encoding, exact amounts,
VAT-in-gross rounding, protocol error forms, port autodetection and the branding footer
against the simulator. Desktop: value validation, the Qt editor, reports, lock release
after a failed connection. Agent: real HTTP session exchange, renewal, revocation
responses and redirect rejection; receipt jobs and crash recovery against the simulator.
Real printouts still need an attached printer.

The protocol source is Posnet's POT-I-DEV-37 specification (v5406, 2022-06-13, Temo
Online 2.01). No vendor SDK is used, and the specification is not redistributed here.
