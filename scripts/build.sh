#!/usr/bin/env bash
# Native Rust bundles: dist/*.deb (Linux), *.exe (Windows), *.dmg (macOS).
# Agent tarballs are retained for existing Linux/macOS download links.
# Run with Bash (Git Bash on Windows), Rust 1.94.0 and jq.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=mac; export MACOSX_DEPLOYMENT_TARGET=11.0 ;;
  MINGW* | MSYS* | CYGWIN*) os=win ;;
  *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | AMD64) arch=x64 ;;
  arm64 | aarch64) arch=arm64 ;;
  *) echo "Unsupported CPU: $(uname -m)" >&2; exit 1 ;;
esac
if [[ $os == win && $arch != x64 ]]; then
  echo 'Windows packages support x64 only.' >&2
  exit 1
fi

export CARGO_TARGET_DIR="$PWD/target"
host=$(rustc +1.94.0 -vV | sed -n 's/^host: //p')
if [[ $os == win ]]; then
  [[ $host == x86_64-pc-windows-msvc ]] || { echo "Windows releases require the x64 MSVC toolchain." >&2; exit 1; }
  command -v dumpbin >/dev/null || { echo "Run from an MSVC developer environment with dumpbin on PATH." >&2; exit 1; }
  # The single-file download must not require the Visual C++ Redistributable.
  # Encoded flags take precedence over RUSTFLAGS, including when explicitly empty.
  if [[ ${CARGO_ENCODED_RUSTFLAGS+x} ]]; then
    export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+${CARGO_ENCODED_RUSTFLAGS}$'\x1f'}-C"$'\x1f'"target-feature=+crt-static"
  else
    export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
  fi
fi
version=$(cargo +1.94.0 metadata --locked --no-deps --format-version 1 | jq -er '
  [.packages[] | select(.name == "fiscal-desktop" or .name == "workshop-agent") | .version]
  | if length == 2 and .[0] == .[1] then .[0] else error("application versions differ") end')
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Not a release version: $version" >&2; exit 1; }
# Explicit native target prevents a local Cargo cross-target setting from mislabelling packages.
# Simulator support is never enabled in release binaries.
cargo +1.94.0 build --locked --release --workspace --bins --no-default-features --target "$host"
mkdir -p dist target
stage=$(mktemp -d "$PWD/target/packaging.XXXXXX")
trap 'rm -rf "$stage"' EXIT

for product in fiscal agent; do
  binary="huggingcar-$product"
  stem="$binary-$os-$arch"
  name="HuggingCar Fiscal"
  [[ $product == agent ]] && name="HuggingCar Agent"
  exe="$CARGO_TARGET_DIR/$host/release/$binary"
  [[ $os == win ]] && exe+=.exe
  if [[ $os == win ]]; then
    imports=$(MSYS_NO_PATHCONV=1 dumpbin /nologo /dependents "$(cygpath -w "$exe")")
    printf '%s\n' "$imports"
    if [[ ${imports^^} =~ (VCRUNTIME|MSVCP|MSVCR|CONCRT|VCOMP)[0-9][[:alnum:]_]*\.DLL ]]; then
      echo "Standalone executable depends on the Visual C++ Redistributable: $exe" >&2
      exit 1
    fi
  fi

  if [[ $os == linux ]]; then
    dbus-run-session -- xvfb-run -a bash packaging/smoke.sh "$exe"
  else
    bash packaging/smoke.sh "$exe"
  fi

  case $os in
    win)
      cp "$exe" "dist/$stem.exe"
      ;;
    mac)
      app="$name.app"
      [[ $product == agent ]] && app=huggingcar-agent.app
      volume="$stage/$binary"
      bundle="$volume/$app/Contents"
      mkdir -p "$bundle/MacOS" "$bundle/Resources"
      cp "$exe" "$bundle/MacOS/$binary"
      cp "packaging/$binary.icns" "$bundle/Resources/"
      sed -e "s/@NAME@/$name/g" -e "s/@BINARY@/$binary/g" \
        -e "s/@PRODUCT@/$product/g" -e "s/@VERSION@/$version/g" \
        packaging/Info.plist >"$bundle/Info.plist"
      codesign --force --deep --sign - "$volume/$app"
      bash packaging/smoke.sh "$bundle/MacOS/$binary"
      ln -s /Applications "$volume/Applications"
      hdiutil create -volname "$name" -srcfolder "$volume" -ov -format UDZO "dist/$stem.dmg"
      if [[ $product == agent ]]; then
        tar -czf "dist/$stem.tar.gz" -C "$volume" "$app"
      fi
      ;;
    linux)
      package="$stage/$binary"
      mkdir -p "$package/DEBIAN" "$package/usr/bin" "$package/usr/share/applications" \
        "$package/usr/share/icons/hicolor/scalable/apps"
      chmod 755 "$package"
      install -m 755 "$exe" "$package/usr/bin/$binary"
      install -m 644 "packaging/$binary.svg" "$package/usr/share/icons/hicolor/scalable/apps/"
      install -m 644 "packaging/$binary.desktop" "$package/usr/share/applications/"
      deb_arch=amd64
      [[ $arch == arm64 ]] && deb_arch=arm64
      cat >"$package/DEBIAN/control" <<EOF
Package: $binary
Version: $version
Section: misc
Priority: optional
Architecture: $deb_arch
Maintainer: HuggingCar <huggingcar@users.noreply.github.com>
Depends: libc6 (>= 2.39), libgcc-s1, libegl1, libgl1, libx11-6, libxcursor1, libxi6, libxrandr2, libxkbcommon0, libxkbcommon-x11-0, libxcb1, libxcb-render0, libxcb-shape0, libxcb-xfixes0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libdbus-1-3
Homepage: https://github.com/HuggingCar/workshop
Description: $name for Posnet fiscal printers
 Prints receipts and fiscal reports on a Posnet printer over USB serial.
EOF
      dpkg-deb --root-owner-group -Zxz --build "$package" "dist/$stem.deb"
      if [[ $product == agent ]]; then
        tar -czf "dist/$stem.tar.gz" -C "$package/usr/bin" "$binary"
      fi
      ;;
  esac
done
printf 'Release artifacts: %s/dist/\n' "$PWD"
