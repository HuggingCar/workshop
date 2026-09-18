# HuggingCar workshop

Software that runs on the workshop PC, next to the Posnet Temo Online fiscal printer.
One Rust workspace, three crates:

| Crate | What it is |
| --- | --- |
| `crates/posnet` | Posnet serial driver: framing, CRC, CP1250, receipts, reports, status and persistent intent journal. The optional `sim` feature provides a native TCP simulator for development. |
| `crates/desktop` | **HuggingCar Fiscal** — Polish desktop app: receipt editor, reports, service catalog and local history. |
| `crates/agent` | **HuggingCar Agent** — setup window and system tray, or headless API polling and fiscal job execution. |

Both applications use the same driver. They ship as native binaries with no interpreter.

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
cargo run -p fiscal-desktop -- --serial /dev/ttyACM0 --probe
```

## Development

```bash
cargo build --workspace --locked
cargo run -p fiscal-desktop                       # desktop app
cargo run -p workshop-agent                      # agent setup window
cargo run -p workshop-agent -- --headless         # saved agent configuration
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +nightly-2026-03-05 fmt --all --check
```

The pinned toolchain is Rust **1.94.0**, edition **2024**, workspace resolver **3**.
Nightly rustfmt (`rustup toolchain install nightly-2026-03-05 --component rustfmt`)
is used only for the import grouping settings in `.rustfmt.toml`.
For Linux release packages, use Ubuntu 24.04 and the build dependencies listed in
`.github/workflows/release.yml`; this keeps the supported glibc baseline.

`bash scripts/build.sh` builds release bundles for the host OS and CPU in `dist/`.
CI builds both applications on five runners. A push to `master` publishes only when
the workspace version in `Cargo.toml` increases above every published release.
A manual **Release** run builds downloadable Actions artifacts without publishing.
To retry a failed release, rerun its original workflow run.

To exercise the desktop without a printer, run these in separate terminals:

```bash
cargo run -p posnet --features sim --bin posnet-simulator -- --port 29000
cargo run -p fiscal-desktop --features sim -- --serial sim://127.0.0.1:29000 --data-dir /tmp/fiscal-demo
```

Keep simulator state separate from production data. Release packages omit the `sim`
feature and reject simulator addresses.

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

Three tabs, printer status and recovery controls. The native interface uses egui.

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

Receipt text is validated by the driver before any fiscal command is sent.
The desktop editor also checks names before printing.

Journal, history and `settings.json` share the application state directory:

| OS | Default desktop state directory |
| --- | --- |
| Linux | `$XDG_STATE_HOME/huggingcar-fiscal` or `~/.local/state/huggingcar-fiscal` |
| macOS | `~/Library/Preferences/State/huggingcar-fiscal` |
| Windows | `%LOCALAPPDATA%/State/huggingcar-fiscal` |

Existing Qt settings are imported on first start; existing journals and history stay
in place. `--data-dir PATH` selects an explicit state directory for either application.
Do not use a different directory to bypass an unresolved fiscal operation.

## Agent

### Standalone agent

Download the matching agent asset from the
[release page](https://github.com/HuggingCar/workshop/releases/latest):

| Platform | Asset |
| --- | --- |
| Windows x64 | `huggingcar-agent-win-x64.exe` |
| macOS Apple Silicon | `huggingcar-agent-mac-arm64.dmg` |
| macOS Intel | `huggingcar-agent-mac-x64.dmg` |
| Linux x64 | `huggingcar-agent-linux-x64.deb` |
| Linux ARM64 | `huggingcar-agent-linux-arm64.deb` |

Linux and macOS also have `.tar.gz` archives. For an unpublished build, use the
manual workflow's Actions artifacts or build locally with `bash scripts/build.sh`.
Linux CI builds target Ubuntu 24.04 or newer. Builds are unsigned.

1. Deploy the matching API first. Version 0.1.2 requires the new session endpoint;
   older agents that send a permanent token to job endpoints must be upgraded.
2. In the web app (director): Ustawienia → Drukarki fiskalne → Dodaj drukarkę.
   Copy the credential; it is shown only once. Existing printer credentials also work.
3. Install the Linux package, open the macOS disk image, or run the Windows `.exe`.
   Archive users can launch the extracted application directly.
4. Enter **Adres API**, paste the credential into **Token**, and select or type the
   printer port. Leave the speed at 9600 unless the printer uses another speed.
5. Click **Zapisz** to save without printing. Click **Uruchom** to connect and print
   queued jobs. The window shows connection and error messages.
   Closing the window hides it in the system tray; the agent keeps processing jobs.
   Use **Otwórz** in the tray menu to reopen it. **Zatrzymaj** stops processing;
   **Zakończ**, in the window or tray menu, exits the app. Both wait for the current
   operation to finish. If no system tray is available, closing keeps the window
   visible; use **Zakończ** to exit. Do not run the desktop app against the same printer.

The API URL and permanent credential are saved in
`fiscal.json` in the agent state directory. On Linux this is
`$XDG_STATE_HOME/workshop-agent` (default `~/.local/state/workshop-agent`).
Other platforms use their local application-data directory. Existing legacy state
directories are retained; conflicting old and new state locations fail closed.
Unix credentials are saved with mode 0600;
on Windows, keep the directory in your private user profile and restrict its ACL.
Launching the app restores the URL, masked token, port and speed. It does not
start printing until you click **Uruchom**. Reopening the window from the tray does
not interrupt or restart the agent.

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

Driver tests cover independent CRC vectors, CP1250 encoding, exact amounts,
VAT rounding, protocol errors, autodetection, reports, branding and uncertain
fiscal outcomes against the native simulator. Desktop tests cover receipt validation,
settings and operation state. Agent tests cover HTTP sessions, authorization,
receipt jobs and journal reconciliation. No test needs a physical printer.

Real printouts still need an attached printer. Simulator success is not hardware certification.

The protocol source is Posnet's POT-I-DEV-37 specification (v5406, 2022-06-13, Temo
Online 2.01). No vendor SDK is used, and the specification is not redistributed here.
