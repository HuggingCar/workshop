#!/usr/bin/env bash
# Builds a self-contained HuggingCar Fiscal bundle for the current OS and CPU into release/:
# Windows → .exe, macOS → .dmg, Linux → .deb. Runs on Git Bash on Windows.
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

version=$(uv version --short)
stem="huggingcar-fiscal-${version}-${os}-${arch}"
name="HuggingCar Fiscal"
[[ $os == linux ]] && name=huggingcar-fiscal  # no spaces in Unix paths
rm -rf build dist release
mkdir -p build release

uv sync --locked --group build
icon=()
if [[ $os != linux ]]; then  # PyInstaller ignores --icon on Linux; the .deb ships the SVG
  icon=(--icon build/icon.png)
  QT_QPA_PLATFORM=offscreen uv run python - <<'PY'
from PySide6.QtGui import QGuiApplication, QImage, QPainter
from PySide6.QtSvg import QSvgRenderer

app = QGuiApplication([])
image = QImage(512, 512, QImage.Format.Format_ARGB32)
image.fill(0)
painter = QPainter(image)
QSvgRenderer("fiscal_desktop/icon.svg").render(painter)
painter.end()
image.save("build/icon.png")
PY
fi
printf 'import sys\nfrom fiscal_desktop.__main__ import main\nsys.exit(main())\n' > build/launcher.py

sep=:
[[ $os == win ]] && sep=';'
mode=--onedir
[[ $os == win ]] && mode=--onefile
# Relative paths only, and no MSYS argument rewriting: Git Bash would otherwise turn the
# "src;dest" data-file arguments into path lists before PyInstaller sees them.
MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' \
uv run pyinstaller --noconfirm --clean --windowed "$mode" --name "$name" \
  "${icon[@]}" --osx-bundle-identifier com.huggingcar.fiscal \
  --add-data "fiscal_desktop/theme.qss${sep}fiscal_desktop" \
  --add-data "fiscal_desktop/*.svg${sep}fiscal_desktop" \
  build/launcher.py
rm -f "$name.spec"

# The data files (theme, icons) are only read when the window is built, so start the
# bundle headless and require it to still be running after a few seconds.
exe="dist/$name/$name"
[[ $os == win ]] && exe="dist/$name.exe"
QT_QPA_PLATFORM=offscreen uv run python - "$exe" <<'PY'
import subprocess, sys, tempfile, time

app = subprocess.Popen([sys.argv[1], "--data-dir", tempfile.mkdtemp()])
time.sleep(8)
alive = app.poll() is None
app.terminate()
sys.exit(0 if alive else f"bundle exited with {app.returncode} before the window was up")
PY

case $os in
  win)
    mv "dist/$name.exe" "release/$stem.exe"
    ;;
  mac)
    hdiutil create -volname "$name" -srcfolder "dist/$name.app" -ov -format UDZO "release/$stem.dmg"
    ;;
  linux)
    package=$(mktemp -d)
    trap 'rm -rf "$package"' EXIT
    chmod 755 "$package"
    install -d "$package/DEBIAN" "$package/opt" "$package/usr/bin" \
      "$package/usr/share/applications" "$package/usr/share/icons/hicolor/scalable/apps"
    cp -r "dist/$name" "$package/opt/$name"
    ln -s "/opt/$name/$name" "$package/usr/bin/$name"
    install -m 644 fiscal_desktop/icon.svg "$package/usr/share/icons/hicolor/scalable/apps/$name.svg"
    cat >"$package/usr/share/applications/$name.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=HuggingCar Fiscal
Comment=Paragony i raporty Posnet Temo Online
Exec=/usr/bin/$name
Icon=$name
Terminal=false
Categories=Office;Finance;
StartupNotify=true
EOF
    cat >"$package/DEBIAN/control" <<EOF
Package: $name
Version: $version
Section: misc
Priority: optional
Architecture: $(dpkg --print-architecture)
Maintainer: HuggingCar <huggingcar@users.noreply.github.com>
Depends: libglib2.0-0, libegl1, libgl1, libfontconfig1, libdbus-1-3, libxkbcommon-x11-0, libxcb-cursor0, libxcb-icccm4, libxcb-keysyms1, libxcb-shape0, libwayland-cursor0
Homepage: https://github.com/HuggingCar/workshop
Description: Polish receipt application for Posnet Temo Online
 Prints receipts and fiscal reports on a Posnet Temo Online printer over USB.
EOF
    dpkg-deb --root-owner-group -Zxz --build "$package" "release/$stem.deb"
    ;;
esac
ls -l release
