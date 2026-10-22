#!/bin/sh
set -eu
source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
app=${1:-"$source_dir/../../../target/macos/Sub-Api-Proxy.app"}
[ "$(uname -s)" = Darwin ] || { echo 'macOS is required' >&2; exit 1; }
[ -x "$app/Contents/MacOS/Sub-Api-Proxy" ] || { echo 'Build the menu bar app first' >&2; exit 1; }
: "${HOME:?HOME must be set}"
case "$HOME" in /*) ;; *) echo 'HOME must be absolute' >&2; exit 1 ;; esac
target=$HOME/Applications/Sub-Api-Proxy.app
label=io.sub-api-proxy.menubar
mkdir -p "$HOME/Applications" "$HOME/Library/LaunchAgents" "$HOME/.local/state/sub-api-proxy"
ditto "$app" "$target.installing"
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
if [ -e "$target" ]; then
    mv "$target" "$target.backup-$(date +%Y%m%d%H%M%S)"
fi
mv "$target.installing" "$target"
python3 - "$HOME" "$target" <<'PY'
import os, plistlib, sys
from pathlib import Path
home, app = map(Path, sys.argv[1:])
path = home / 'Library/LaunchAgents/io.sub-api-proxy.menubar.plist'
value = dict(Label='io.sub-api-proxy.menubar',
             ProgramArguments=[str(app / 'Contents/MacOS/Sub-Api-Proxy')],
             RunAtLoad=True, KeepAlive=dict(SuccessfulExit=False),
             ProcessType='Interactive', ThrottleInterval=5,
             StandardOutPath=str(home / '.local/state/sub-api-proxy/menubar.log'),
             StandardErrorPath=str(home / '.local/state/sub-api-proxy/menubar.log'))
with path.open('wb') as f:
    plistlib.dump(value, f)
os.chmod(path, 0o600)
PY
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$label.plist"
printf 'Installed %s and started the menu bar app.\n' "$target"
