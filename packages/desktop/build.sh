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

version=$(uv version --short --package workshop)  # the root version: what release.yml tags
stem="huggingcar-fiscal-${os}-${arch}"  # no version: releases/latest/download/<stem> stays a stable link
name="HuggingCar Fiscal"
[[ $os == linux ]] && name=huggingcar-fiscal  # no spaces in Unix paths
rm -rf build dist release
mkdir -p build release

uv sync --locked --group build
icon=()
if [[ $os != linux ]]; then  # PyInstaller ignores --icon on Linux; the .deb ships the SVG
  icon=(--icon build/icon.png)  # PyInstaller + Pillow turn it into a multi-size .ico / .icns
  # A missing or broken SVG yields a null pixmap, which refuses to save.
  QT_QPA_PLATFORM=offscreen uv run python -c "
from PySide6.QtGui import QGuiApplication, QIcon
QGuiApplication([])
assert QIcon('fiscal_desktop/icon.svg').pixmap(512, 512).save('build/icon.png'), 'icon not rendered'"
fi
printf 'import sys\nfrom fiscal_desktop.__main__ import main\nsys.exit(main())\n' > build/launcher.py

mode=--onedir
[[ $os == win ]] && mode=--onefile
# No MSYS argument rewriting: Git Bash must hand the arguments to PyInstaller untouched.
MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' \
uv run pyinstaller --noconfirm --clean --windowed "$mode" --name "$name" --specpath build \
  "${icon[@]}" --osx-bundle-identifier com.huggingcar.fiscal \
  --collect-data fiscal_desktop \
  build/launcher.py

# The data files (theme, icons) are only read when the window is built, so start the
# bundle headless and require it to still be running after a few seconds.
exe="dist/$name/$name"
[[ $os == win ]] && exe="dist/$name.exe"
[[ $os == mac ]] && exe="dist/$name.app/Contents/MacOS/$name"  # the bundle the .dmg ships
QT_QPA_PLATFORM=offscreen uv run python -c "
import subprocess, sys, tempfile
try:
    run = subprocess.run([sys.argv[1], '--data-dir', tempfile.mkdtemp()], timeout=8)
except subprocess.TimeoutExpired:
    sys.exit(0)  # still up after 8 s: the window was built
sys.exit(f'bundle exited with {run.returncode} before the window was up')" "$exe"

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
