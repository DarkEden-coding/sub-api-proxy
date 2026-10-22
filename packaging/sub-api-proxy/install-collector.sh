#!/bin/sh
set -eu
[ "$(uname -s)" = Darwin ] || { echo 'This collector service installer requires macOS' >&2; exit 1; }
source_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
python3 - "$source_dir/collect-usage.py" <<'PY'
import os, plistlib, shutil, sys
from pathlib import Path
home = Path.home()
destination = home / '.local/lib/sub-api-proxy/collect-usage.py'
destination.parent.mkdir(parents=True, exist_ok=True)
shutil.copyfile(sys.argv[1], destination)
os.chmod(destination, 0o700)
logs = home / '.local/state/sub-api-proxy'
logs.mkdir(parents=True, exist_ok=True)
logs.chmod(0o700)
path = home / 'Library/LaunchAgents/io.sub-api-proxy.usage.plist'
path.parent.mkdir(parents=True, exist_ok=True)
value = dict(Label='io.sub-api-proxy.usage',
             ProgramArguments=[sys.executable, str(destination)],
             RunAtLoad=True, KeepAlive=True, ThrottleInterval=5, Umask=63,
             StandardOutPath=str(logs / 'collector.log'),
             StandardErrorPath=str(logs / 'collector.log'))
with path.open('wb') as handle:
    plistlib.dump(value, handle)
path.chmod(0o600)
PY
label=io.sub-api-proxy.usage
launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/$label.plist"
echo 'Started protected usage collection.'
