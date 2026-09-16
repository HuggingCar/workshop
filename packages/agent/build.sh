#!/usr/bin/env bash
# Standalone agent with a setup window and optional command-line mode.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=mac ;;
  MINGW* | MSYS* | CYGWIN*) os=win ;;
  *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | AMD64) arch=x64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) echo "Unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac

stem="huggingcar-agent-${os}-${arch}"
rm -rf build dist release
mkdir -p build release
uv sync --locked --package workshop-agent --group build --inexact
mode=(--console --onefile)
[[ $os == win ]] && mode=(--windowed --onefile)
[[ $os == mac ]] && mode=(--windowed --onedir)
printf 'import sys\nfrom workshop_agent.__main__ import main\nsys.exit(main())\n' > build/launcher.py
MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' \
uv run --no-sync pyinstaller --noconfirm --clean "${mode[@]}" \
  --name huggingcar-agent --specpath build --copy-metadata workshop-agent \
  --collect-data anyascii --collect-data fiscal_desktop --collect-data workshop_agent build/launcher.py

exe=dist/huggingcar-agent
[[ $os == win ]] && exe+=.exe
[[ $os == mac ]] && exe=dist/huggingcar-agent.app/Contents/MacOS/huggingcar-agent
"$exe" --version
"$exe" fiscal --help
QT_QPA_PLATFORM=offscreen uv run --no-sync python -c "
import subprocess, sys, tempfile
with tempfile.TemporaryDirectory() as state:
    try:
        run = subprocess.run([sys.argv[1], '--data-dir', state], timeout=8)
    except subprocess.TimeoutExpired:
        sys.exit(0)  # the setup window is open and waiting for input
    sys.exit(f'agent window exited with {run.returncode} before setup was ready')" "$exe"
if [[ $os == win ]]; then
  mv "$exe" "release/$stem.exe"
elif [[ $os == mac ]]; then
  tar -czf "release/$stem.tar.gz" -C dist huggingcar-agent.app
else
  tar -czf "release/$stem.tar.gz" -C dist huggingcar-agent
fi
