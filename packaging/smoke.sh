#!/usr/bin/env bash
# The setup window must stay alive without a printer or existing configuration.
set -euo pipefail
exe=$1
shift
state=$(mktemp -d)
pid=
cleanup() {
  if [[ -n $pid ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$state"
}
trap cleanup EXIT
export XDG_STATE_HOME="$state/state"
export XDG_CONFIG_HOME="$state/config"
export LIBGL_ALWAYS_SOFTWARE=1
unset WAYLAND_DISPLAY
case "$exe" in
  *huggingcar-agent | *huggingcar-agent.exe) set -- --data-dir "$state/agent" "$@" ;;
  *huggingcar-fiscal | *huggingcar-fiscal.exe)
    mkdir -p "$state/fiscal"
    # Existing settings prevent legacy registry/plist import from the build user's profile.
    printf '{}\n' >"$state/fiscal/settings.json"
    set -- --data-dir "$state/fiscal" "$@"
    ;;
esac
"$exe" "$@" >"$state/output.log" 2>&1 &
pid=$!
sleep 8
if ! kill -0 "$pid" 2>/dev/null; then
  status=0
  wait "$pid" || status=$?
  pid=
  cat "$state/output.log"
  echo "Application exited with status $status before setup was ready: $exe" >&2
  exit 1
fi
echo "Startup smoke passed: $exe stayed alive for eight seconds."
