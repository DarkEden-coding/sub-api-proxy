#!/bin/sh
set -eu

usage() {
    echo "Usage: $0 [--role router|server|both] [--bin-dir PATH] [--dry-run]"
}
role=both
bin_dir=./target/release
dry_run=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --role|--bin-dir)
            [ "$#" -ge 2 ] || { usage >&2; exit 2; }
            case "$1" in --role) role=$2 ;; --bin-dir) bin_dir=$2 ;; esac
            shift 2 ;;
        --dry-run) dry_run=true; shift ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
case "$role" in router|server|both) ;; *) usage >&2; exit 2 ;; esac
: "${HOME:?HOME must be set}"
case "$HOME" in /*) ;; *) echo "HOME must be absolute" >&2; exit 2 ;; esac
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
os=$(uname -s)
case "$os" in Darwin|Linux) ;; *) echo "Unsupported OS: $os" >&2; exit 1 ;; esac
run() {
    if "$dry_run"; then
        printf 'Would run:'
        for arg in "$@"; do printf ' [%s]' "$arg"; done
        printf '\n'
    else
        "$@"
    fi
}
roles=$role
[ "$role" != both ] || roles='server router'
binaries=sap
for service in $roles; do
    case "$service" in server) binaries="$binaries cliproxy" ;; router) binaries="$binaries sap-router" ;; esac
done
if ! "$dry_run"; then
    for binary in $binaries; do
        [ -x "$bin_dir/$binary" ] || { echo "Missing executable: $bin_dir/$binary" >&2; exit 1; }
    done
    for service in $roles; do
        case "$service" in
            router) configs=router.yaml ;;
            server) configs='config.yaml sap.yaml' ;;
        esac
        for config in $configs; do
            [ -f "$HOME/.config/sub-api-proxy/$config" ] || { echo "Create $HOME/.config/sub-api-proxy/$config before installing" >&2; exit 1; }
        done
    done
    if [ "$os" = Darwin ]; then command -v python3 >/dev/null || { echo 'python3 is required to render launchd templates' >&2; exit 1; }; fi
fi
run mkdir -p "$HOME/.local/bin" "$HOME/.config/sub-api-proxy" "$HOME/.local/state/sub-api-proxy"
for binary in $binaries; do
    # Replace via rename so a running Linux executable can be upgraded safely.
    run install -m 755 "$bin_dir/$binary" "$HOME/.local/bin/$binary.sap-install"
    run mv -f "$HOME/.local/bin/$binary.sap-install" "$HOME/.local/bin/$binary"
done
if [ "$os" = Darwin ]; then
    run mkdir -p "$HOME/Library/LaunchAgents"
    for service in $roles; do
        label=io.sub-api-proxy.$service
        target=$HOME/Library/LaunchAgents/$label.plist
        if "$dry_run"; then
            printf 'Would render %s with HOME into %s\n' "$script_dir/$label.plist" "$target"
        else
            python3 - "$script_dir/$label.plist" "$target" "$HOME" <<'PY'
import os, sys
from xml.sax.saxutils import escape
source, target, home = sys.argv[1:]
with open(source) as f:
    text = f.read().replace('@HOME@', escape(home))
with open(target + '.tmp', 'w') as f:
    f.write(text)
os.chmod(target + '.tmp', 0o600)
os.replace(target + '.tmp', target)
PY
        fi
        run launchctl bootout "gui/$(id -u)/$label" || true
        # launchd can briefly retain a service after bootout while it exits.
        run launchctl bootstrap "gui/$(id -u)" "$target" || {
            run sleep 1
            run launchctl bootstrap "gui/$(id -u)" "$target"
        }
    done
else
    run mkdir -p "$HOME/.config/systemd/user"
    for service in $roles; do
        run install -m 644 "$script_dir/sap-$service.service" "$HOME/.config/systemd/user/sap-$service.service"
    done
    run systemctl --user daemon-reload
    for service in $roles; do
        run systemctl --user enable "sap-$service.service"
        run systemctl --user restart "sap-$service.service"
    done
fi
printf 'Add %s/.local/bin to PATH if needed.\n' "$HOME"
